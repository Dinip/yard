import type { Database } from "@yard/db";
import { device, reservation, reservationObserver, userAdbKey } from "@yard/db";
import type { AdbKey } from "@yard/protocol";
import { and, eq, inArray, isNull, or } from "drizzle-orm";
import { providers } from "../gateway/registry.ts";

/**
 * Who may `adb connect` to a device, and telling its provider about it.
 *
 * The provider decides locally, against a set pushed here — not by asking per
 * connection. That is what lets it keep serving an authorised session across a
 * coordinator restart, the same stance the session plane takes.
 *
 * Every push carries the **whole set**, never a delta, so a dropped message can
 * leave the provider with a stale set but never with a revoked key.
 */

/**
 * The keys entitled to one reservation: the holder's, plus those of everyone
 * still present as an observer.
 *
 * An observer counts because they are already in the session — the holder let
 * them in, and refusing them `adb` while granting them the screen would be an
 * odd place to draw the line.
 */
export async function entitledKeys(db: Database, reservationId: string): Promise<AdbKey[]> {
  const rows = await db
    .select({
      userId: userAdbKey.userId,
      fingerprint: userAdbKey.fingerprint,
      publicKey: userAdbKey.publicKey,
      comment: userAdbKey.comment,
    })
    .from(userAdbKey)
    .where(
      inArray(
        userAdbKey.userId,
        db
          .select({ id: reservation.userId })
          .from(reservation)
          .where(and(eq(reservation.id, reservationId), eq(reservation.state, "active")))
          .union(
            db
              .select({ id: reservationObserver.userId })
              .from(reservationObserver)
              .where(
                and(
                  eq(reservationObserver.reservationId, reservationId),
                  isNull(reservationObserver.leftAt),
                ),
              ),
          ),
      ),
    );

  return rows.map((row) => ({
    userId: row.userId,
    fingerprint: row.fingerprint,
    publicKey: row.publicKey,
    ...(row.comment ? { comment: row.comment } : {}),
  }));
}

/**
 * Re-push the entitled set for one reservation.
 *
 * A no-op when the reservation is over or its provider is offline: the provider
 * refuses every connection without a set anyway, and one that just reconnected
 * gets the current set with its next `session.authorize`.
 */
export async function pushAdbKeysForReservation(db: Database, reservationId: string) {
  const [held] = await db
    .select({ deviceId: reservation.deviceId, providerId: device.providerId })
    .from(reservation)
    .innerJoin(device, eq(device.id, reservation.deviceId))
    .where(and(eq(reservation.id, reservationId), eq(reservation.state, "active")))
    .limit(1);
  if (!held) return;

  const conn = providers.get(held.providerId);
  if (!conn) return;

  conn.commandNoWait({
    kind: "device.adb.keys",
    deviceId: held.deviceId,
    keys: await entitledKeys(db, reservationId),
  });
}

/** The same, addressed by device rather than by reservation. */
export async function pushAdbKeys(db: Database, deviceId: string) {
  const [held] = await db
    .select({ id: reservation.id })
    .from(reservation)
    .where(and(eq(reservation.deviceId, deviceId), eq(reservation.state, "active")))
    .limit(1);
  if (held) await pushAdbKeysForReservation(db, held.id);
}

/**
 * Re-push everywhere one user's keys are in play.
 *
 * Called when a key is added or removed, and when somebody joins or leaves a
 * session. Scoped to the reservations that user is actually in, rather than
 * broadcast: on a farm of any size most providers have nothing to do with them.
 */
export async function pushAdbKeysForUser(db: Database, userId: string) {
  const affected = await db
    .selectDistinct({ deviceId: reservation.deviceId })
    .from(reservation)
    .leftJoin(
      reservationObserver,
      and(
        eq(reservationObserver.reservationId, reservation.id),
        isNull(reservationObserver.leftAt),
      ),
    )
    .where(
      and(
        eq(reservation.state, "active"),
        or(eq(reservation.userId, userId), eq(reservationObserver.userId, userId)),
      ),
    );

  for (const row of affected) await pushAdbKeys(db, row.deviceId);
}
