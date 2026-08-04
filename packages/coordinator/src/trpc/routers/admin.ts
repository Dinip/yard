import { auditLog, device, reservation, user } from "@farm/db";
import { TRPCError } from "@trpc/server";
import { and, count, desc, eq, ilike, or } from "drizzle-orm";
import { z } from "zod";
import { audit } from "../../lib/audit.ts";
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
      const [released] = await ctx.db
        .update(reservation)
        .set({
          state: "released",
          releasedAt: new Date(),
          releasedBy: ctx.user.id,
          reason: input.reason ?? "force-released by admin",
        })
        .where(and(eq(reservation.deviceId, input.deviceId), eq(reservation.state, "active")))
        .returning();
      if (!released) throw new TRPCError({ code: "NOT_FOUND", message: "No active reservation" });

      await ctx.db.update(device).set({ status: "ready" }).where(eq(device.id, input.deviceId));
      await audit(ctx.db, ctx.user.id, "device.force_release", "device", input.deviceId, {
        reason: input.reason,
        takenFrom: released.userId,
      });
      return released;
    }),

  audit: adminProcedure
    .input(
      z
        .object({
          limit: z.number().int().min(1).max(500).default(100),
          offset: z.number().int().min(0).default(0),
          action: z.string().optional(),
        })
        .default({ limit: 100, offset: 0 }),
    )
    .query(({ ctx, input }) =>
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
        .where(input.action ? eq(auditLog.action, input.action) : undefined)
        .orderBy(desc(auditLog.at))
        .limit(input.limit)
        .offset(input.offset),
    ),
});
