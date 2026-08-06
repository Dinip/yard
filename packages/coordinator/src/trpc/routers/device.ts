import { device, provider, reservation, user } from "@farm/db";
import { TRPCError } from "@trpc/server";
import { and, desc, eq } from "drizzle-orm";
import { z } from "zod";
import { providers } from "../../gateway/registry.ts";
import { audit } from "../../lib/audit.ts";
import { deviceEvents } from "../../lib/events.ts";
import { releaseActive } from "../../lib/reservations.ts";
import { signSessionToken } from "../../lib/session-token.ts";
import { getSetting } from "../../lib/settings.ts";
import { protectedProcedure, router } from "../init.ts";

const RESERVABLE: ReadonlyArray<"ready" | "present"> = ["ready", "present"];

/**
 * When a reservation taken (or renewed) now should lapse.
 *
 * The TTL is admin-configurable policy rather than an env var, so it is read
 * per call — the settings cache makes that a memory lookup almost every time.
 */
async function expiryFromNow(db: import("@farm/db").Database) {
  const ttl = await getSetting(db, "reservation.ttlSeconds");
  return new Date(Date.now() + ttl * 1000);
}

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
          .set({ expiresAt: await expiryFromNow(ctx.db) })
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

      // Fail before taking the reservation rather than handing out a device
      // nobody can reach: `require` throws PRECONDITION_FAILED when the owning
      // provider has no live control-plane socket.
      const conn = providers.require(target.providerId);

      let created: typeof reservation.$inferSelect;
      try {
        const [row] = await ctx.db
          .insert(reservation)
          .values({
            id: crypto.randomUUID(),
            deviceId: input.deviceId,
            userId: ctx.user.id,
            state: "active",
            expiresAt: await expiryFromNow(ctx.db),
          })
          .returning();
        created = row!;
      } catch (err) {
        if (isUniqueViolation(err)) {
          throw new TRPCError({ code: "CONFLICT", message: "Device is in use" });
        }
        throw err;
      }

      await ctx.db.update(device).set({ status: "busy" }).where(eq(device.id, input.deviceId));

      // Tell the provider which reservation may now open a session. Sent after
      // the row exists so the provider can never authorize a reservation the
      // database does not have.
      conn.commandNoWait({
        kind: "session.authorize",
        deviceId: input.deviceId,
        reservationId: created.id,
        userId: ctx.user.id,
      });

      await audit(ctx.db, ctx.user.id, "device.reserve", "device", input.deviceId);
      deviceEvents.publish();
      return created;
    }),

  /**
   * Mints a short-lived Ed25519 token for the session and artifact planes, and
   * returns the provider URL the browser should talk to **directly**.
   *
   * Called on connect and refreshed well before `expiresAt`. The coordinator is
   * not on the data path — this is the last it hears of the session.
   */
  sessionToken: protectedProcedure
    .input(z.object({ deviceId: z.string() }))
    .mutation(async ({ ctx, input }) => {
      const [row] = await ctx.db
        .select({
          providerId: device.providerId,
          publicBaseUrl: provider.publicBaseUrl,
          reservationId: reservation.id,
          reservationUserId: reservation.userId,
        })
        .from(device)
        .innerJoin(provider, eq(device.providerId, provider.id))
        .leftJoin(
          reservation,
          and(eq(reservation.deviceId, device.id), eq(reservation.state, "active")),
        )
        .where(eq(device.id, input.deviceId))
        .limit(1);

      if (!row) throw new TRPCError({ code: "NOT_FOUND" });
      if (!row.reservationId) {
        throw new TRPCError({ code: "PRECONDITION_FAILED", message: "Device is not reserved" });
      }
      // Reservation is per user+device, so every tab and the popout window get
      // their own token against the same reservation.
      if (row.reservationUserId !== ctx.user.id) {
        throw new TRPCError({ code: "FORBIDDEN", message: "Someone else holds this device" });
      }

      const { token, expiresAt } = await signSessionToken({
        deviceId: input.deviceId,
        userId: ctx.user.id,
        reservationId: row.reservationId,
        providerId: row.providerId,
      });

      return {
        token,
        expiresAt,
        deviceId: input.deviceId,
        providerBaseUrl: row.publicBaseUrl,
        sessionUrl: `${row.publicBaseUrl.replace(/^http/, "ws")}/s/${input.deviceId}`,
      };
    }),

  renew: protectedProcedure
    .input(z.object({ reservationId: z.string() }))
    .mutation(async ({ ctx, input }) => {
      const [updated] = await ctx.db
        .update(reservation)
        .set({ expiresAt: await expiryFromNow(ctx.db) })
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
      // An admin may release anyone's device; everyone else only their own.
      // The ownership check is here rather than in `releaseActive`, which has
      // no notion of who is asking.
      const conditions = [
        eq(reservation.deviceId, input.deviceId),
        eq(reservation.state, "active"),
      ];
      if (ctx.user.role !== "admin") conditions.push(eq(reservation.userId, ctx.user.id));

      const [held] = await ctx.db
        .select({ id: reservation.id })
        .from(reservation)
        .where(and(...conditions))
        .limit(1);
      if (!held) throw new TRPCError({ code: "NOT_FOUND" });

      const [released] = await releaseActive(ctx.db, [input.deviceId], {
        actorUserId: ctx.user.id,
        reason: "reservation released",
        auditAction: "device.release",
      });
      if (!released) throw new TRPCError({ code: "NOT_FOUND" });
      return released;
    }),

  myReservations: protectedProcedure.query(({ ctx }) =>
    ctx.db
      .select()
      .from(reservation)
      .where(and(eq(reservation.userId, ctx.user.id), eq(reservation.state, "active")))
      .orderBy(desc(reservation.startedAt)),
  ),

  // ── device commands ──────────────────────────────────────────────────────
  // All of these require the caller to actually hold the device, and all route
  // through the provider's control-plane socket.

  apps: protectedProcedure
    .input(z.object({ deviceId: z.string() }))
    .query(async ({ ctx, input }) => {
      const { conn } = await requireOwnedDevice(ctx, input.deviceId);
      const data = await conn.command({ kind: "device.apps", deviceId: input.deviceId });
      return data?.apps ?? [];
    }),

  launch: protectedProcedure
    .input(
      z.object({ deviceId: z.string(), appId: z.string(), args: z.array(z.string()).optional() }),
    )
    .mutation(async ({ ctx, input }) => {
      const { conn } = await requireOwnedDevice(ctx, input.deviceId);
      await conn.command({
        kind: "device.launch",
        deviceId: input.deviceId,
        appId: input.appId,
        args: input.args,
      });
      await audit(ctx.db, ctx.user.id, "device.launch", "device", input.deviceId, {
        appId: input.appId,
      });
      return { ok: true };
    }),

  uninstall: protectedProcedure
    .input(z.object({ deviceId: z.string(), appId: z.string() }))
    .mutation(async ({ ctx, input }) => {
      const { conn } = await requireOwnedDevice(ctx, input.deviceId);
      await conn.command({
        kind: "device.uninstall",
        deviceId: input.deviceId,
        appId: input.appId,
      });
      await audit(ctx.db, ctx.user.id, "device.uninstall", "device", input.deviceId, {
        appId: input.appId,
      });
      return { ok: true };
    }),

  reboot: protectedProcedure
    .input(z.object({ deviceId: z.string() }))
    .mutation(async ({ ctx, input }) => {
      const { conn } = await requireOwnedDevice(ctx, input.deviceId);
      await conn.command({ kind: "device.reboot", deviceId: input.deviceId });
      await audit(ctx.db, ctx.user.id, "device.reboot", "device", input.deviceId);
      return { ok: true };
    }),

  rotate: protectedProcedure
    .input(z.object({ deviceId: z.string(), degrees: z.number().int() }))
    .mutation(async ({ ctx, input }) => {
      const { conn } = await requireOwnedDevice(ctx, input.deviceId);
      await conn.command({
        kind: "device.rotate",
        deviceId: input.deviceId,
        degrees: input.degrees,
      });
      return { ok: true };
    }),

  /**
   * Android only. Binds a port on the provider host that proxies into an adb
   * transport, so a developer runs `adb connect <providerHost>:<port>`.
   */
  adbExpose: protectedProcedure
    .input(z.object({ deviceId: z.string() }))
    .mutation(async ({ ctx, input }) => {
      const { conn, row } = await requireOwnedDevice(ctx, input.deviceId);
      if (row.platform !== "android") {
        throw new TRPCError({ code: "BAD_REQUEST", message: "adb is Android-only" });
      }

      const data = await conn.command({ kind: "device.adb.expose", deviceId: input.deviceId });
      const port = data?.adbPort;
      if (!port) {
        throw new TRPCError({
          code: "INTERNAL_SERVER_ERROR",
          message: "Provider exposed adb but reported no port",
        });
      }

      await ctx.db.update(device).set({ adbPort: port }).where(eq(device.id, input.deviceId));
      await audit(ctx.db, ctx.user.id, "device.adb.expose", "device", input.deviceId, { port });
      deviceEvents.publish();

      // Host, not full URL: the provider's public base may sit behind a proxy
      // that does not forward raw adb, so the developer needs the bare host.
      const host = new URL(conn.publicBaseUrl).hostname;
      return { port, host, connectString: `${host}:${port}` };
    }),

  adbUnexpose: protectedProcedure
    .input(z.object({ deviceId: z.string() }))
    .mutation(async ({ ctx, input }) => {
      const { conn } = await requireOwnedDevice(ctx, input.deviceId);
      await conn.command({ kind: "device.adb.unexpose", deviceId: input.deviceId });
      await ctx.db.update(device).set({ adbPort: null }).where(eq(device.id, input.deviceId));
      deviceEvents.publish();
      return { ok: true };
    }),
});

/**
 * Every device command requires an active reservation held by the caller (or
 * admin), and a live provider socket. Returning the connection alongside keeps
 * callers from looking it up a second time and racing a disconnect.
 */
async function requireOwnedDevice(
  ctx: { db: import("@farm/db").Database; user: { id: string; role?: string | null } },
  deviceId: string,
) {
  const [row] = await ctx.db
    .select({
      providerId: device.providerId,
      platform: device.platform,
      reservationUserId: reservation.userId,
    })
    .from(device)
    .leftJoin(
      reservation,
      and(eq(reservation.deviceId, device.id), eq(reservation.state, "active")),
    )
    .where(eq(device.id, deviceId))
    .limit(1);

  if (!row) throw new TRPCError({ code: "NOT_FOUND" });

  const isAdmin = ctx.user.role === "admin";
  if (!row.reservationUserId) {
    throw new TRPCError({ code: "PRECONDITION_FAILED", message: "Device is not reserved" });
  }
  if (row.reservationUserId !== ctx.user.id && !isAdmin) {
    throw new TRPCError({ code: "FORBIDDEN", message: "Someone else holds this device" });
  }

  return { row, conn: providers.require(row.providerId) };
}

function isUniqueViolation(err: unknown): boolean {
  return typeof err === "object" && err !== null && "code" in err && err.code === "23505";
}
