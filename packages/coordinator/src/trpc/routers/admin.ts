import { TRPCError } from "@trpc/server";
import { auditLog, reservation, user } from "@yard/db";
import { AUDIT_ACTION_VALUES } from "@yard/protocol";
import { and, count, desc, eq, gte, ilike, inArray, lte, or } from "drizzle-orm";
import { z } from "zod";
import { audit } from "../../lib/audit.ts";
import { deviceEvents } from "../../lib/events.ts";
import { addObserver, releaseActive } from "../../lib/reservations.ts";
import { adminProcedure, router } from "../init.ts";

export const adminRouter = router({
  users: adminProcedure
    .input(
      z
        .object({
          search: z.string().optional(),
          limit: z.number().int().min(1).max(200).default(50),
          offset: z.number().int().min(0).default(0),
        })
        .default({ limit: 50, offset: 0 }),
    )
    .query(async ({ ctx, input }) => {
      const where = input.search
        ? or(ilike(user.name, `%${input.search}%`), ilike(user.email, `%${input.search}%`))
        : undefined;

      const [rows, [total]] = await Promise.all([
        ctx.db
          .select({
            id: user.id,
            name: user.name,
            email: user.email,
            image: user.image,
            role: user.role,
            banned: user.banned,
            banReason: user.banReason,
            banExpires: user.banExpires,
            createdAt: user.createdAt,
          })
          .from(user)
          .where(where)
          .orderBy(desc(user.createdAt))
          .limit(input.limit)
          .offset(input.offset),
        ctx.db.select({ value: count() }).from(user).where(where),
      ]);

      return { users: rows, total: total?.value ?? 0 };
    }),

  /** Take a device back from whoever holds it. */
  forceRelease: adminProcedure
    .input(z.object({ deviceId: z.string(), reason: z.string().optional() }))
    .mutation(async ({ ctx, input }) => {
      const [released] = await releaseActive(ctx.db, [input.deviceId], {
        actorUserId: ctx.user.id,
        reason: input.reason ?? "force-released by admin",
        auditAction: "device.force_release",
      });
      if (!released) throw new TRPCError({ code: "NOT_FOUND", message: "No active reservation" });
      return released;
    }),

  /**
   * Join somebody else's session, with full control.
   *
   * **The provider needs no change for this.** It matches sessions on
   * `reservationId`, and re-authorizing the same reservation deliberately does
   * not disturb existing viewers — behaviour written for the popout, and tested.
   * So an admin holding a token minted against *the holder's* reservation is
   * accepted exactly like the holder's second tab.
   *
   * Disclosure, not stealth: the observer row is what the holder's UI names, and
   * the join is audited.
   */
  joinSession: adminProcedure
    .input(z.object({ deviceId: z.string() }))
    .mutation(async ({ ctx, input }) => {
      const [held] = await ctx.db
        .select({ id: reservation.id, userId: reservation.userId })
        .from(reservation)
        .where(and(eq(reservation.deviceId, input.deviceId), eq(reservation.state, "active")))
        .limit(1);

      if (!held) {
        throw new TRPCError({ code: "PRECONDITION_FAILED", message: "Device is not reserved" });
      }
      if (held.userId === ctx.user.id) {
        throw new TRPCError({ code: "BAD_REQUEST", message: "You already hold this device" });
      }

      await addObserver(ctx.db, held.id, ctx.user.id);

      await audit(ctx.db, ctx.user.id, "device.session_join", "device", input.deviceId, {
        reservationId: held.id,
        holder: held.userId,
      });
      deviceEvents.publish();
      return { reservationId: held.id };
    }),

  /**
   * The audit log, filtered.
   *
   * Returns a count alongside the page because the UI used to paginate by "was
   * the page full?", which can neither show a total nor recognise the last
   * page. The count runs the same predicate, so the two cannot disagree.
   */
  audit: adminProcedure
    .input(
      z
        .object({
          limit: z.number().int().min(1).max(500).default(100),
          offset: z.number().int().min(0).default(0),
          /** Several at once: "every way a device came free" is one question. */
          action: z.array(z.enum(AUDIT_ACTION_VALUES)).optional(),
          actorUserId: z.string().optional(),
          targetId: z.string().optional(),
          targetType: z.string().optional(),
          from: z.coerce.date().optional(),
          to: z.coerce.date().optional(),
        })
        .default({ limit: 100, offset: 0 }),
    )
    .query(async ({ ctx, input }) => {
      const filters = [
        input.action?.length ? inArray(auditLog.action, input.action) : undefined,
        input.actorUserId ? eq(auditLog.actorUserId, input.actorUserId) : undefined,
        // A prefix match, so a partial device id someone pasted still finds it.
        input.targetId ? ilike(auditLog.targetId, `${input.targetId}%`) : undefined,
        input.targetType ? eq(auditLog.targetType, input.targetType) : undefined,
        input.from ? gte(auditLog.at, input.from) : undefined,
        input.to ? lte(auditLog.at, input.to) : undefined,
      ].filter((f) => f !== undefined);

      const where = filters.length ? and(...filters) : undefined;

      const [items, [total]] = await Promise.all([
        ctx.db
          .select({
            id: auditLog.id,
            action: auditLog.action,
            targetType: auditLog.targetType,
            targetId: auditLog.targetId,
            metadata: auditLog.metadata,
            at: auditLog.at,
            actorUserId: auditLog.actorUserId,
            actorName: user.name,
            actorEmail: user.email,
          })
          .from(auditLog)
          .leftJoin(user, eq(auditLog.actorUserId, user.id))
          .where(where)
          .orderBy(desc(auditLog.at))
          .limit(input.limit)
          .offset(input.offset),
        ctx.db.select({ value: count() }).from(auditLog).where(where),
      ]);

      return { items, total: total?.value ?? 0 };
    }),
});
