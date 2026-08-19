import { device, joinRequest, provider, reservation, reservationObserver, user } from "@farm/db";
import { TRPCError } from "@trpc/server";
import { and, desc, eq, inArray, isNull, sql } from "drizzle-orm";
import { z } from "zod";
import { providers } from "../../gateway/registry.ts";
import { audit } from "../../lib/audit.ts";
import { deviceEvents } from "../../lib/events.ts";
import { isUniqueViolation } from "../../lib/pg-errors.ts";
import { addObserver, JOIN_REQUEST_TTL, releaseActive } from "../../lib/reservations.ts";
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

  // Two extra queries rather than two more joins: observers and pending join
  // requests are both rare one-to-manys, and joining either would multiply
  // every device row.
  const reservationIds = rows
    .map((r) => r.reservation?.id)
    .filter((id): id is string => id != null);
  const [observers, requests] = reservationIds.length
    ? await Promise.all([
        db
          .select({
            reservationId: reservationObserver.reservationId,
            userId: reservationObserver.userId,
            joinedAt: reservationObserver.joinedAt,
            name: user.name,
          })
          .from(reservationObserver)
          .innerJoin(user, eq(reservationObserver.userId, user.id))
          .where(
            and(
              inArray(reservationObserver.reservationId, reservationIds),
              isNull(reservationObserver.leftAt),
            ),
          ),
        db
          .select({
            id: joinRequest.id,
            reservationId: joinRequest.reservationId,
            userId: joinRequest.userId,
            note: joinRequest.note,
            requestedAt: joinRequest.createdAt,
            name: user.name,
          })
          .from(joinRequest)
          .innerJoin(user, eq(joinRequest.userId, user.id))
          .where(
            and(
              inArray(joinRequest.reservationId, reservationIds),
              eq(joinRequest.state, "pending"),
            ),
          )
          .orderBy(joinRequest.createdAt),
      ])
    : [[], []];

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
          lastActivityAt: r.reservation.lastActivityAt,
          /** Everyone who has openly joined. The holder's UI names them. */
          observers: observers
            .filter((o) => o.reservationId === r.reservation?.id)
            .map(({ userId, name, joinedAt }) => ({ userId, name, joinedAt })),
          /** Unanswered asks to join. Only the holder's UI renders these. */
          joinRequests: requests
            .filter((q) => q.reservationId === r.reservation?.id)
            .map(({ id, userId, name, note, requestedAt }) => ({
              id,
              userId,
              name,
              note,
              requestedAt,
            })),
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
        adbKeys: [],
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
        // ...and so does anyone who has openly joined this session. The open
        // observer row *is* the grant — not the admin role, which is merely one
        // way to get one (the holder approving a request is the other). The
        // token carries *the holder's* reservationId and the joiner's own
        // userId: nothing else about it changes, and the provider — which
        // matches on reservationId — treats it as one more viewer.
        if (!(await isObserver(ctx.db, row.reservationId, ctx.user.id))) {
          throw new TRPCError({ code: "FORBIDDEN", message: "Someone else holds this device" });
        }
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

  /**
   * Keep a reservation alive, and optionally say when the tab was last used.
   *
   * `interactedAt` is the browser's floor under the provider's authoritative
   * reporting: reading a crash log on a reserved device is still using it, and
   * nothing reaches the device while that happens. It is clamped both ways —
   * never into the future, never backwards — because two sources write this
   * column and neither may be able to move the other's clock.
   */
  renew: protectedProcedure
    .input(z.object({ reservationId: z.string(), interactedAt: z.number().int().optional() }))
    .mutation(async ({ ctx, input }) => {
      const interacted =
        input.interactedAt === undefined
          ? undefined
          : new Date(Math.min(input.interactedAt, Date.now()));

      const [updated] = await ctx.db
        .update(reservation)
        .set({
          expiresAt: await expiryFromNow(ctx.db),
          ...(interacted
            ? {
                // ISO, not the Date: a raw `sql` parameter skips drizzle's
                // column serializer, and the driver's rendering of a Date
                // stores local wall clock in a UTC column.
                lastActivityAt: sql`greatest(${reservation.lastActivityAt}, ${interacted.toISOString()}::timestamp)`,
              }
            : {}),
        })
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

  /**
   * How a reservation ended, for the person it was taken from.
   *
   * `session.closed` carries a reason string and nothing else — the actor's
   * name is not on the wire and does not belong there, since the provider has
   * no notion of users. Every column this needs is already on the row, written
   * by `releaseActive`; this is the read that turns them into a sentence.
   */
  reservationOutcome: protectedProcedure
    .input(z.object({ reservationId: z.string() }))
    .query(async ({ ctx, input }) => {
      const [row] = await ctx.db
        .select({
          state: reservation.state,
          reason: reservation.reason,
          releasedAt: reservation.releasedAt,
          userId: reservation.userId,
          releasedByName: user.name,
        })
        .from(reservation)
        .leftJoin(user, eq(reservation.releasedBy, user.id))
        .where(eq(reservation.id, input.reservationId))
        .limit(1);

      if (!row) throw new TRPCError({ code: "NOT_FOUND" });
      if (row.userId !== ctx.user.id && ctx.user.role !== "admin") {
        throw new TRPCError({ code: "FORBIDDEN" });
      }

      return {
        state: row.state,
        reason: row.reason,
        releasedAt: row.releasedAt,
        // Null for the reaper, which is nobody — an idle expiry must not read
        // as a person having taken the device.
        releasedByName: row.releasedByName,
      };
    }),

  // ── asking to join somebody else's session ───────────────────────────────
  // An admin can already let themselves in (`admin.joinSession`). This is the
  // same destination for everyone else, with the holder as the gate: approval
  // creates the identical observer row, so nothing downstream — the session
  // token, the provider, the disclosure the holder sees — knows the difference.

  requestJoin: protectedProcedure
    .input(z.object({ deviceId: z.string(), note: z.string().max(200).optional() }))
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
      if (await isObserver(ctx.db, held.id, ctx.user.id)) {
        throw new TRPCError({ code: "BAD_REQUEST", message: "You are already in this session" });
      }

      // Asking twice is asking once: a double-clicked button, or a second tab,
      // must not put two rows in front of the holder. The partial unique index
      // is what makes that true under a race, and this returns the row that
      // won rather than an error the user cannot act on.
      const [created] = await ctx.db
        .insert(joinRequest)
        .values({
          id: crypto.randomUUID(),
          reservationId: held.id,
          userId: ctx.user.id,
          note: input.note,
          expiresAt: new Date(Date.now() + JOIN_REQUEST_TTL),
        })
        .onConflictDoNothing()
        .returning();

      if (!created) return pendingRequest(ctx.db, held.id, ctx.user.id);

      await audit(ctx.db, ctx.user.id, "device.session_request", "device", input.deviceId, {
        reservationId: held.id,
        holder: held.userId,
      });
      deviceEvents.publish();
      return created;
    }),

  /** Withdraw your own pending request. */
  cancelJoinRequest: protectedProcedure
    .input(z.object({ deviceId: z.string() }))
    .mutation(async ({ ctx, input }) => {
      const [cancelled] = await ctx.db
        .update(joinRequest)
        .set({ state: "cancelled", decidedAt: new Date(), decidedBy: ctx.user.id })
        .where(
          and(
            eq(joinRequest.state, "pending"),
            eq(joinRequest.userId, ctx.user.id),
            inArray(
              joinRequest.reservationId,
              ctx.db
                .select({ id: reservation.id })
                .from(reservation)
                .where(
                  and(eq(reservation.deviceId, input.deviceId), eq(reservation.state, "active")),
                ),
            ),
          ),
        )
        .returning();

      if (!cancelled) throw new TRPCError({ code: "NOT_FOUND" });
      deviceEvents.publish();
      return cancelled;
    }),

  /**
   * The holder's answer.
   *
   * Only the person whose session it is may give it — an admin does not need to
   * ask in the first place, but may answer, since they can already join and
   * take the device outright.
   */
  answerJoinRequest: protectedProcedure
    .input(z.object({ requestId: z.string(), approve: z.boolean() }))
    .mutation(async ({ ctx, input }) => {
      const [found] = await ctx.db
        .select({
          id: joinRequest.id,
          state: joinRequest.state,
          requesterId: joinRequest.userId,
          reservationId: reservation.id,
          deviceId: reservation.deviceId,
          holderId: reservation.userId,
          reservationState: reservation.state,
        })
        .from(joinRequest)
        .innerJoin(reservation, eq(joinRequest.reservationId, reservation.id))
        .where(eq(joinRequest.id, input.requestId))
        .limit(1);

      if (!found) throw new TRPCError({ code: "NOT_FOUND" });
      if (found.holderId !== ctx.user.id && ctx.user.role !== "admin") {
        throw new TRPCError({ code: "FORBIDDEN", message: "This is not your session" });
      }
      if (found.state !== "pending" || found.reservationState !== "active") {
        throw new TRPCError({
          code: "PRECONDITION_FAILED",
          message: "That request is no longer open",
        });
      }

      // Presence first: a row marked approved with nobody actually in the
      // session is the one inconsistency worth ruling out, and `addObserver`
      // is safe to repeat.
      if (input.approve) await addObserver(ctx.db, found.reservationId, found.requesterId);

      const [answered] = await ctx.db
        .update(joinRequest)
        .set({
          state: input.approve ? "approved" : "denied",
          decidedAt: new Date(),
          decidedBy: ctx.user.id,
        })
        .where(and(eq(joinRequest.id, found.id), eq(joinRequest.state, "pending")))
        .returning();
      if (!answered) throw new TRPCError({ code: "PRECONDITION_FAILED" });

      await audit(
        ctx.db,
        ctx.user.id,
        input.approve ? "device.session_request_approved" : "device.session_request_denied",
        "device",
        found.deviceId,
        { reservationId: found.reservationId, requester: found.requesterId },
      );
      deviceEvents.publish();
      return answered;
    }),

  /**
   * Step out of a session you are in.
   *
   * Not an admin power, which is what it was when only admins could be in one:
   * however somebody got in — letting themselves in, or being let in — leaving
   * is closing your own row, and nobody else's.
   */
  leaveSession: protectedProcedure
    .input(z.object({ deviceId: z.string() }))
    .mutation(async ({ ctx, input }) => {
      const [held] = await ctx.db
        .select({ id: reservation.id })
        .from(reservation)
        .where(and(eq(reservation.deviceId, input.deviceId), eq(reservation.state, "active")))
        .limit(1);
      if (!held) throw new TRPCError({ code: "NOT_FOUND" });

      const [left] = await ctx.db
        .update(reservationObserver)
        .set({ leftAt: new Date() })
        .where(
          and(
            eq(reservationObserver.reservationId, held.id),
            eq(reservationObserver.userId, ctx.user.id),
            isNull(reservationObserver.leftAt),
          ),
        )
        .returning({ id: reservationObserver.id });
      // Leaving a session you are not in is a no-op worth reporting: the
      // button should not have been there.
      if (!left) throw new TRPCError({ code: "NOT_FOUND", message: "You are not in this session" });

      await audit(ctx.db, ctx.user.id, "device.session_leave", "device", input.deviceId);
      deviceEvents.publish();
      return { ok: true };
    }),

  /**
   * The caller's own request against this device's current session.
   *
   * Any state, not just pending: this is how a requester learns they were
   * turned down. An approval needs no telling — the observer row arrives on the
   * next device fetch and the console opens by itself.
   */
  myJoinRequest: protectedProcedure
    .input(z.object({ deviceId: z.string() }))
    .query(async ({ ctx, input }) => {
      const [held] = await ctx.db
        .select({ id: reservation.id })
        .from(reservation)
        .where(and(eq(reservation.deviceId, input.deviceId), eq(reservation.state, "active")))
        .limit(1);
      if (!held) return null;

      const [row] = await ctx.db
        .select({
          id: joinRequest.id,
          state: joinRequest.state,
          note: joinRequest.note,
          createdAt: joinRequest.createdAt,
          expiresAt: joinRequest.expiresAt,
          decidedAt: joinRequest.decidedAt,
        })
        .from(joinRequest)
        .where(and(eq(joinRequest.reservationId, held.id), eq(joinRequest.userId, ctx.user.id)))
        .orderBy(desc(joinRequest.createdAt))
        .limit(1);

      return row ?? null;
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
      reservationId: reservation.id,
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

  if (!row.reservationId || !row.reservationUserId) {
    throw new TRPCError({ code: "PRECONDITION_FAILED", message: "Device is not reserved" });
  }
  if (row.reservationUserId !== ctx.user.id) {
    // Someone who joined the session gets the device commands too, not only the
    // stream: the holder is told "they can control the device, the same as
    // you", and an approved joiner who could not launch an app would make that
    // a lie. Admins keep their unconditional pass.
    const present =
      ctx.user.role === "admin" || (await isObserver(ctx.db, row.reservationId, ctx.user.id));
    if (!present) {
      throw new TRPCError({ code: "FORBIDDEN", message: "Someone else holds this device" });
    }
  }

  return { row, conn: providers.require(row.providerId) };
}

/** The request this user already has open on a reservation. */
async function pendingRequest(
  db: import("@farm/db").Database,
  reservationId: string,
  userId: string,
) {
  const [row] = await db
    .select()
    .from(joinRequest)
    .where(
      and(
        eq(joinRequest.reservationId, reservationId),
        eq(joinRequest.userId, userId),
        eq(joinRequest.state, "pending"),
      ),
    )
    .limit(1);
  // The insert conflicted, so the row it conflicted with exists — unless the
  // reaper retired it in between, which is a request that is simply gone.
  if (!row) throw new TRPCError({ code: "CONFLICT", message: "That request just lapsed" });
  return row;
}

/** An open observer row on this reservation — somebody who joined, and stayed. */
async function isObserver(db: import("@farm/db").Database, reservationId: string, userId: string) {
  const [row] = await db
    .select({ id: reservationObserver.id })
    .from(reservationObserver)
    .where(
      and(
        eq(reservationObserver.reservationId, reservationId),
        eq(reservationObserver.userId, userId),
        isNull(reservationObserver.leftAt),
      ),
    )
    .limit(1);
  return Boolean(row);
}
