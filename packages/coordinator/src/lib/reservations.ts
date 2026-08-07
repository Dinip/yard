import type { Database } from "@farm/db";
import { device, joinRequest, reservation, reservationObserver } from "@farm/db";
import type { AuditAction } from "@farm/protocol";
import { and, eq, inArray, isNull, lt } from "drizzle-orm";
import { providers } from "../gateway/registry.ts";
import { audit } from "./audit.ts";
import { deviceEvents } from "./events.ts";
import { getSettings } from "./settings.ts";

/**
 * Releasing a reservation, in one place.
 *
 * There are four ways a device comes free — the holder releases it, an admin
 * takes it back, the reaper sweeps a lapsed one, or its provider disconnects —
 * and they were drifting apart. The one that mattered: force-release updated
 * the row but never pushed `session.revoke`, so the device the admin had "taken
 * back" carried on streaming to the person it was taken from.
 */
export interface ReleaseOptions {
  /** Who did it. `null` for the reaper, which is nobody. */
  actorUserId: string | null;
  reason: string;
  /**
   * Whether to push `session.revoke`. False only when the provider is already
   * gone, since it has lost every session with the socket.
   */
  revoke?: boolean;
  /** Written to the audit log; omit to skip the entry. */
  auditAction?: AuditAction;
}

/**
 * Release every active reservation matching `deviceIds`, or all of them when it
 * is omitted.
 *
 * Returns the released rows, so a caller can tell "nothing to do" from "done"
 * without a second query.
 */
export async function releaseActive(
  db: Database,
  deviceIds: string[] | undefined,
  options: ReleaseOptions,
) {
  const conditions = [eq(reservation.state, "active")];
  if (deviceIds) {
    if (deviceIds.length === 0) return [];
    conditions.push(inArray(reservation.deviceId, deviceIds));
  }

  const released = await db
    .update(reservation)
    .set({
      state: "released",
      releasedAt: new Date(),
      releasedBy: options.actorUserId,
      reason: options.reason,
    })
    .where(and(...conditions))
    .returning();

  if (released.length === 0) return [];

  const reservationIds = released.map((row) => row.id);

  // An observer's presence is scoped to the session they joined; the session
  // is over.
  await db
    .update(reservationObserver)
    .set({ leftAt: new Date() })
    .where(
      and(
        inArray(reservationObserver.reservationId, reservationIds),
        isNull(reservationObserver.leftAt),
      ),
    );

  // And so is a request to join it. Leaving one pending would let the holder
  // answer, after the fact, a question about a session that no longer exists.
  await expirePendingRequests(db, reservationIds);

  const ids = released.map((row) => row.deviceId);
  await db
    .update(device)
    .set({ status: "ready" })
    .where(and(inArray(device.id, ids), eq(device.status, "busy")));

  if (options.revoke !== false) {
    // Revocation is a push, not a token-expiry side effect: live viewers must
    // drop now, not up to SESSION_TOKEN_TTL seconds from now.
    const rows = await db
      .select({ id: device.id, providerId: device.providerId })
      .from(device)
      .where(inArray(device.id, ids));

    for (const row of rows) {
      providers.get(row.providerId)?.commandNoWait({
        kind: "session.revoke",
        deviceId: row.id,
        reason: options.reason,
      });
    }
  }

  if (options.auditAction) {
    for (const row of released) {
      await audit(db, options.actorUserId, options.auditAction, "device", row.deviceId, {
        reason: options.reason,
        takenFrom: row.userId,
      });
    }
  }

  deviceEvents.publish();
  return released;
}

/**
 * Put somebody into a session, with full control.
 *
 * The one way presence is created, whether an admin joined by themselves or the
 * holder approved a request. Both must produce exactly the same row, because
 * that row is what `device.sessionToken` treats as the grant.
 *
 * Rejoining after a reload is not an error, and must not leave two open rows
 * for one person — hence the conflict clause, which the partial unique index
 * on `(reservation_id, user_id) where left_at is null` backs.
 */
export async function addObserver(db: Database, reservationId: string, userId: string) {
  await db
    .insert(reservationObserver)
    .values({ id: crypto.randomUUID(), reservationId, userId })
    .onConflictDoNothing();
}

/**
 * How long an unanswered request to join stays answerable.
 *
 * A constant rather than a `SETTINGS` key: this is the span in which a holder
 * plausibly notices a dialog, not policy anyone needs to tune, and a stale
 * request on screen is worse than a lapsed one.
 */
export const JOIN_REQUEST_TTL = 120_000;

/** Retire every pending request against these reservations. */
async function expirePendingRequests(db: Database, reservationIds: string[]) {
  if (reservationIds.length === 0) return;
  await db
    .update(joinRequest)
    .set({ state: "expired", decidedAt: new Date() })
    .where(
      and(inArray(joinRequest.reservationId, reservationIds), eq(joinRequest.state, "pending")),
    );
}

/** How often the reaper looks. */
const SWEEP_INTERVAL = 30_000;

/** Minutes, or hours and minutes — this ends up in a message a user reads. */
function describeDuration(seconds: number): string {
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes} minute${minutes === 1 ? "" : "s"}`;
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  return rest === 0 ? `${hours} hour${hours === 1 ? "" : "s"}` : `${hours}h ${rest}m`;
}

/**
 * Release every active reservation matching one sweep condition.
 *
 * Selecting first and releasing by device id keeps every path through
 * `releaseActive`, which is what pushes `session.revoke` and writes the audit
 * row. The select's predicate is the same one the release runs under, so a
 * reservation renewed in between is simply not found by the update.
 */
async function sweepCondition(
  db: Database,
  condition: ReturnType<typeof lt>,
  options: { reason: string; auditAction: AuditAction },
) {
  const matched = await db
    .select({ deviceId: reservation.deviceId })
    .from(reservation)
    .where(and(eq(reservation.state, "active"), condition));

  if (matched.length === 0) return;

  const released = await releaseActive(
    db,
    matched.map((row) => row.deviceId),
    { actorUserId: null, ...options },
  );

  if (released.length) {
    console.log(`[reaper] ${options.reason}: released ${released.length} reservation(s)`);
  }
}

/**
 * Reclaim reservations nobody is using, on three conditions.
 *
 * - **Lapsed** (`expiresAt`): the client renews while a session is open, so a
 *   lapsed reservation means the holder is gone — closed the tab, lost the
 *   network, went home.
 * - **Idle** (`lastActivityAt`, when configured): the tab is open and renewing
 *   but nobody has driven the device. This is what reclaims the device someone
 *   left reserved over a long weekend.
 * - **Too long** (`startedAt`, when configured): a hard cap however busy the
 *   session is.
 *
 * Each sweep is a select and a single UPDATE under the same predicate, so two
 * coordinators running it would not double-release: the second finds no rows.
 * That is worth keeping true even though only one coordinator is supported
 * today.
 *
 * It also retires unanswered requests to join, which release nothing and so sit
 * outside the three conditions above.
 */
export function startReservationReaper(db: Database) {
  const sweep = async () => {
    try {
      const settings = await getSettings(db);
      const now = new Date();

      await sweepCondition(db, lt(reservation.expiresAt, now), {
        reason: "reservation expired",
        auditAction: "device.reservation_expired",
      });

      // The two policies below are off unless an admin turned them on, so the
      // condition is built only when there is one to build.
      const idle = settings["reservation.idleTimeoutSeconds"];
      if (idle !== null) {
        await sweepCondition(
          db,
          lt(reservation.lastActivityAt, new Date(now.getTime() - idle * 1000)),
          {
            reason: `released after ${describeDuration(idle)} without interaction`,
            auditAction: "device.reservation_idle",
          },
        );
      }

      const max = settings["reservation.maxDurationSeconds"];
      if (max !== null) {
        await sweepCondition(db, lt(reservation.startedAt, new Date(now.getTime() - max * 1000)), {
          reason: `released after the ${describeDuration(max)} session limit`,
          auditAction: "device.reservation_max_duration",
        });
      }

      // Unanswered requests to join. A plain UPDATE rather than a
      // `sweepCondition`: nothing is being released, so there is no session to
      // revoke and nothing worth an audit row — the ask is already logged.
      const lapsed = await db
        .update(joinRequest)
        .set({ state: "expired", decidedAt: now })
        .where(and(eq(joinRequest.state, "pending"), lt(joinRequest.expiresAt, now)))
        .returning({ id: joinRequest.id });
      // Both ends are showing a request that is no longer answerable.
      if (lapsed.length) deviceEvents.publish();
    } catch (error) {
      // A failed sweep must not kill the timer: the next one will find the
      // same rows, and a coordinator that silently stopped reaping is exactly
      // the failure this exists to prevent.
      console.error("[reaper] sweep failed:", error);
    }
  };

  const timer = setInterval(sweep, SWEEP_INTERVAL);
  // Do not hold the process open for a timer that only tidies up.
  timer.unref?.();
  void sweep();

  return () => clearInterval(timer);
}
