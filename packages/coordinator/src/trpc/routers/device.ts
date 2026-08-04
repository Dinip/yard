import { device, provider, reservation, user } from "@farm/db";
import { TRPCError } from "@trpc/server";
import { and, desc, eq, inArray } from "drizzle-orm";
import { z } from "zod";
import { env } from "../../env.ts";
import { audit } from "../../lib/audit.ts";
import { protectedProcedure, router } from "../init.ts";

const RESERVABLE: ReadonlyArray<"ready" | "present"> = ["ready", "present"];

/** Device rows joined with their provider and current owner, shaped for the UI. */
async function listDevices(db: import("@farm/db").Database) {
  const rows = await db
    .select({
      device,
      providerName: provider.name,
      providerStatus: provider.status,
      providerBaseUrl: provider.publicBaseUrl,
      reservation: reservation,
      ownerName: user.name,
      ownerEmail: user.email,
    })
    .from(device)
    .innerJoin(provider, eq(device.providerId, provider.id))
    .leftJoin(
      reservation,
      and(eq(reservation.deviceId, device.id), eq(reservation.state, "active")),
    )
    .leftJoin(user, eq(reservation.userId, user.id))
    .orderBy(device.platform, device.name);

  return rows.map((r) => ({
    ...r.device,
    provider: {
      id: r.device.providerId,
      name: r.providerName,
      status: r.providerStatus,
      publicBaseUrl: r.providerBaseUrl,
    },
    reservation: r.reservation
      ? {
          id: r.reservation.id,
          userId: r.reservation.userId,
          ownerName: r.ownerName,
          ownerEmail: r.ownerEmail,
          startedAt: r.reservation.startedAt,
          expiresAt: r.reservation.expiresAt,
        }
      : null,
  }));
}

export type DeviceListItem = Awaited<ReturnType<typeof listDevices>>[number];

export const deviceRouter = router({
  list: protectedProcedure.query(({ ctx }) => listDevices(ctx.db)),

  get: protectedProcedure.input(z.object({ id: z.string() })).query(async ({ ctx, input }) => {
    const all = await listDevices(ctx.db);
    const found = all.find((d) => d.id === input.id);
    if (!found) throw new TRPCError({ code: "NOT_FOUND" });
    return found;
  }),

  /**
   * Claim a device. Exclusivity is enforced by the partial unique index on
   * `reservation(device_id) where state = 'active'` — a losing concurrent caller
   * gets a unique-violation from Postgres, which we translate to CONFLICT.
   */
  reserve: protectedProcedure
    .input(z.object({ deviceId: z.string() }))
    .mutation(async ({ ctx, input }) => {
      const [target] = await ctx.db
        .select()
        .from(device)
        .where(eq(device.id, input.deviceId))
        .limit(1);
      if (!target) throw new TRPCError({ code: "NOT_FOUND" });

      // An existing reservation held by this same user is renewed, not rejected:
      // a second tab (or the popout window) must join the same reservation.
      const [existing] = await ctx.db
        .select()
        .from(reservation)
        .where(and(eq(reservation.deviceId, input.deviceId), eq(reservation.state, "active")))
        .limit(1);

      if (existing) {
        if (existing.userId !== ctx.user.id) {
          throw new TRPCError({ code: "CONFLICT", message: "Device is in use" });
        }
        const [renewed] = await ctx.db
          .update(reservation)
          .set({ expiresAt: new Date(Date.now() + env.RESERVATION_TTL * 1000) })
          .where(eq(reservation.id, existing.id))
          .returning();
        return renewed!;
      }

      if (!RESERVABLE.includes(target.status as "ready")) {
        throw new TRPCError({
          code: "PRECONDITION_FAILED",
          message: `Device is ${target.status}`,
        });
      }

      try {
        const [created] = await ctx.db
          .insert(reservation)
          .values({
            id: crypto.randomUUID(),
            deviceId: input.deviceId,
            userId: ctx.user.id,
            state: "active",
            expiresAt: new Date(Date.now() + env.RESERVATION_TTL * 1000),
          })
          .returning();

        await ctx.db.update(device).set({ status: "busy" }).where(eq(device.id, input.deviceId));
        await audit(ctx.db, ctx.user.id, "device.reserve", "device", input.deviceId);
        return created!;
      } catch (err) {
        if (isUniqueViolation(err)) {
          throw new TRPCError({ code: "CONFLICT", message: "Device is in use" });
        }
        throw err;
      }
    }),

  renew: protectedProcedure
    .input(z.object({ reservationId: z.string() }))
    .mutation(async ({ ctx, input }) => {
      const [updated] = await ctx.db
        .update(reservation)
        .set({ expiresAt: new Date(Date.now() + env.RESERVATION_TTL * 1000) })
        .where(
          and(
            eq(reservation.id, input.reservationId),
            eq(reservation.userId, ctx.user.id),
            eq(reservation.state, "active"),
          ),
        )
        .returning();
      if (!updated) throw new TRPCError({ code: "NOT_FOUND" });
      return updated;
    }),

  release: protectedProcedure
    .input(z.object({ deviceId: z.string() }))
    .mutation(async ({ ctx, input }) => {
      const isAdmin = ctx.user.role === "admin";
      const conditions = [
        eq(reservation.deviceId, input.deviceId),
        eq(reservation.state, "active"),
      ];
      if (!isAdmin) conditions.push(eq(reservation.userId, ctx.user.id));

      const [released] = await ctx.db
        .update(reservation)
        .set({ state: "released", releasedAt: new Date(), releasedBy: ctx.user.id })
        .where(and(...conditions))
        .returning();
      if (!released) throw new TRPCError({ code: "NOT_FOUND" });

      await ctx.db
        .update(device)
        .set({ status: "ready" })
        .where(and(eq(device.id, input.deviceId), inArray(device.status, ["busy"])));
      await audit(ctx.db, ctx.user.id, "device.release", "device", input.deviceId);
      return released;
    }),

  myReservations: protectedProcedure.query(({ ctx }) =>
    ctx.db
      .select()
      .from(reservation)
      .where(and(eq(reservation.userId, ctx.user.id), eq(reservation.state, "active")))
      .orderBy(desc(reservation.startedAt)),
  ),
});

function isUniqueViolation(err: unknown): boolean {
  return typeof err === "object" && err !== null && "code" in err && err.code === "23505";
}
