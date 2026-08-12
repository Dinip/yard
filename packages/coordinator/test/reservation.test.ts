/**
 * Exercises the claim the whole ownership model rests on: two users cannot hold
 * the same device at once, even when they race. Requires a Postgres at
 * DATABASE_URL (docker compose -f docker-compose.dev.yml up -d).
 */
import { afterAll, beforeAll, beforeEach, describe, expect, test } from "bun:test";
import {
  device,
  joinRequest,
  provider,
  reservation,
  reservationObserver,
  setting,
  user,
} from "@farm/db";
import { and, desc, eq, inArray } from "drizzle-orm";
import { db } from "../src/db.ts";
import { startReservationReaper } from "../src/lib/reservations.ts";
import { invalidateSettings, setSetting } from "../src/lib/settings.ts";
import {
  caller as callerFor,
  closePoolOnExit,
  stubProviderConnection,
  testUser,
} from "./helpers.ts";

closePoolOnExit();

const PROVIDER_ID = "test-provider";
const DEVICE_ID = "test-device-0001";
const USERS = ["test-user-a", "test-user-b", "test-user-c"];

beforeAll(async () => {
  await cleanup();
  await db.insert(provider).values({
    id: PROVIDER_ID,
    name: "test",
    publicBaseUrl: "https://provider.test",
    status: "online",
  });
  await db.insert(device).values({
    id: DEVICE_ID,
    providerId: PROVIDER_ID,
    platform: "ios",
    name: "Test iPhone",
    status: "ready",
  });
  await db.insert(user).values(USERS.map(testUser));

  // reserve() refuses a device whose provider is offline, and these tests are
  // about the database's exclusivity guarantee, not the control plane.
  stubProviderConnection(PROVIDER_ID);
});

afterAll(async () => {
  await cleanup();
});

async function cleanup() {
  await db.delete(reservation).where(eq(reservation.deviceId, DEVICE_ID));
  await db.delete(device).where(eq(device.id, DEVICE_ID));
  await db.delete(provider).where(eq(provider.id, PROVIDER_ID));
  await db.delete(user).where(inArray(user.id, USERS));
}

async function resetDevice() {
  await db.delete(reservation).where(eq(reservation.deviceId, DEVICE_ID));
  await db.update(device).set({ status: "ready" }).where(eq(device.id, DEVICE_ID));
}

describe("reservations", () => {
  test("concurrent reserves produce exactly one winner", async () => {
    await resetDevice();

    const results = await Promise.allSettled(
      USERS.map((id) => callerFor(id).device.reserve({ deviceId: DEVICE_ID })),
    );

    const fulfilled = results.filter((r) => r.status === "fulfilled");
    expect(fulfilled).toHaveLength(1);

    for (const r of results) {
      if (r.status === "rejected") expect(r.reason.code).toBe("CONFLICT");
    }

    const active = await db.select().from(reservation).where(eq(reservation.deviceId, DEVICE_ID));
    expect(active.filter((r) => r.state === "active")).toHaveLength(1);
  });

  test("a second reserve by the same user renews rather than conflicts", async () => {
    await resetDevice();
    const caller = callerFor(USERS[0]!);

    const first = await caller.device.reserve({ deviceId: DEVICE_ID });
    const second = await caller.device.reserve({ deviceId: DEVICE_ID });

    expect(second.id).toBe(first.id);
    expect(second.expiresAt.getTime()).toBeGreaterThanOrEqual(first.expiresAt.getTime());
  });

  test("release frees the device for the next user", async () => {
    await resetDevice();
    await callerFor(USERS[0]!).device.reserve({ deviceId: DEVICE_ID });

    await expect(
      callerFor(USERS[1]!).device.reserve({ deviceId: DEVICE_ID }),
    ).rejects.toMatchObject({ code: "CONFLICT" });

    await callerFor(USERS[0]!).device.release({ deviceId: DEVICE_ID });
    const taken = await callerFor(USERS[1]!).device.reserve({ deviceId: DEVICE_ID });
    expect(taken.userId).toBe(USERS[1]);
  });

  test("a non-owner cannot release, an admin can", async () => {
    await resetDevice();
    await callerFor(USERS[0]!).device.reserve({ deviceId: DEVICE_ID });

    await expect(
      callerFor(USERS[1]!).device.release({ deviceId: DEVICE_ID }),
    ).rejects.toMatchObject({ code: "NOT_FOUND" });

    const released = await callerFor(USERS[2]!, "admin").device.release({ deviceId: DEVICE_ID });
    expect(released.state).toBe("released");
    expect(released.releasedBy).toBe(USERS[2]);
  });

  test("an unhealthy device cannot be reserved", async () => {
    await resetDevice();
    await db.update(device).set({ status: "unhealthy" }).where(eq(device.id, DEVICE_ID));

    await expect(
      callerFor(USERS[0]!).device.reserve({ deviceId: DEVICE_ID }),
    ).rejects.toMatchObject({ code: "PRECONDITION_FAILED" });
  });
});

describe("the reaper", () => {
  test("sweeps a lapsed reservation and frees the device", async () => {
    await resetDevice();
    const caller = callerFor(USERS[0]);
    await caller.device.reserve({ deviceId: DEVICE_ID });

    // Backdate it rather than waiting out a 15-minute TTL.
    await db
      .update(reservation)
      .set({ expiresAt: new Date(Date.now() - 1000) })
      .where(and(eq(reservation.deviceId, DEVICE_ID), eq(reservation.state, "active")));

    const stop = startReservationReaper(db);
    // The reaper sweeps once immediately on start.
    await Bun.sleep(200);
    stop();

    const [row] = await db
      .select()
      .from(reservation)
      .where(eq(reservation.deviceId, DEVICE_ID))
      .orderBy(desc(reservation.startedAt))
      .limit(1);
    expect(row?.state).toBe("released");
    expect(row?.reason).toBe("reservation expired");
    // Nobody released it, so nobody is recorded as having done so.
    expect(row?.releasedBy).toBeNull();

    const [freed] = await db.select().from(device).where(eq(device.id, DEVICE_ID)).limit(1);
    expect(freed?.status).toBe("ready");

    // And the device can be taken by someone else, which is the point.
    await expect(
      callerFor(USERS[1]).device.reserve({ deviceId: DEVICE_ID }),
    ).resolves.toBeDefined();
  });

  test("leaves a renewed reservation alone", async () => {
    await resetDevice();
    const caller = callerFor(USERS[0]);
    const held = await caller.device.reserve({ deviceId: DEVICE_ID });

    await db
      .update(reservation)
      .set({ expiresAt: new Date(Date.now() - 1000) })
      .where(eq(reservation.id, held.id));

    // What the browser does every third of the lifetime.
    await caller.device.renew({ reservationId: held.id });

    const stop = startReservationReaper(db);
    await Bun.sleep(200);
    stop();

    const [row] = await db.select().from(reservation).where(eq(reservation.id, held.id)).limit(1);
    expect(row?.state).toBe("active");
  });
});

/**
 * An admin joining a session is the gentler alternative to taking the device
 * away. The provider needs no change for it — it matches on `reservationId` —
 * so what has to hold is that the coordinator mints a token carrying *the
 * holder's* reservation, and only for an admin who openly joined.
 */
describe("joining someone else's session", () => {
  test("an admin who joins gets a token against the holder's reservation", async () => {
    await resetDevice();
    const holder = callerFor(USERS[0]);
    const held = await holder.device.reserve({ deviceId: DEVICE_ID });

    const admin = callerFor(USERS[1], "admin");
    // Before joining, an admin is just somebody else.
    await expect(admin.device.sessionToken({ deviceId: DEVICE_ID })).rejects.toMatchObject({
      code: "FORBIDDEN",
    });

    const joined = await admin.admin.joinSession({ deviceId: DEVICE_ID });
    expect(joined.reservationId).toBe(held.id);

    const token = await admin.device.sessionToken({ deviceId: DEVICE_ID });
    const claims = JSON.parse(atob(token.token.split(".")[1]!.replace(/-/g, "+")));
    expect(claims.reservationId).toBe(held.id);
    // The admin's own identity, against the holder's reservation.
    expect(claims.userId).toBe(USERS[1]);

    // The holder's own session is untouched — this is a join, not a takeover.
    await expect(holder.device.sessionToken({ deviceId: DEVICE_ID })).resolves.toBeDefined();
  });

  test("a non-admin cannot join, and neither can the holder", async () => {
    await resetDevice();
    await callerFor(USERS[0]).device.reserve({ deviceId: DEVICE_ID });

    await expect(
      callerFor(USERS[1]).admin.joinSession({ deviceId: DEVICE_ID }),
    ).rejects.toMatchObject({ code: "FORBIDDEN" });

    await expect(
      callerFor(USERS[0], "admin").admin.joinSession({ deviceId: DEVICE_ID }),
    ).rejects.toMatchObject({ code: "BAD_REQUEST" });
  });

  test("leaving withdraws the token, and the holder sees who is present", async () => {
    await resetDevice();
    const holder = callerFor(USERS[0]);
    await holder.device.reserve({ deviceId: DEVICE_ID });

    const admin = callerFor(USERS[1], "admin");
    await admin.admin.joinSession({ deviceId: DEVICE_ID });

    const seen = await holder.device.get({ id: DEVICE_ID });
    expect(seen.reservation?.observers.map((o) => o.userId)).toEqual([USERS[1]]);

    await admin.device.leaveSession({ deviceId: DEVICE_ID });
    await expect(admin.device.sessionToken({ deviceId: DEVICE_ID })).rejects.toMatchObject({
      code: "FORBIDDEN",
    });

    const after = await holder.device.get({ id: DEVICE_ID });
    expect(after.reservation?.observers).toEqual([]);
  });

  test("releasing the device closes every observer row with it", async () => {
    await resetDevice();
    const holder = callerFor(USERS[0]);
    const held = await holder.device.reserve({ deviceId: DEVICE_ID });
    const admin = callerFor(USERS[1], "admin");
    await admin.admin.joinSession({ deviceId: DEVICE_ID });

    await holder.device.release({ deviceId: DEVICE_ID });

    const rows = await db
      .select()
      .from(reservationObserver)
      .where(eq(reservationObserver.reservationId, held.id));
    expect(rows).toHaveLength(1);
    expect(rows[0]?.leftAt).not.toBeNull();
  });
});

/**
 * Asking to join, for everyone who is not an admin.
 *
 * The whole point is that approval routes through the *same* observer row the
 * admin path creates, so what has to hold is that a plain user ends up with a
 * session token — and does not, on any other answer.
 */
describe("asking to join a session", () => {
  test("an approved request gets a non-admin a token against the holder's reservation", async () => {
    await resetDevice();
    const holder = callerFor(USERS[0]);
    const held = await holder.device.reserve({ deviceId: DEVICE_ID });
    const asker = callerFor(USERS[1]);

    await expect(asker.device.sessionToken({ deviceId: DEVICE_ID })).rejects.toMatchObject({
      code: "FORBIDDEN",
    });

    const request = await asker.device.requestJoin({ deviceId: DEVICE_ID, note: "need it" });
    expect(request.state).toBe("pending");

    // The holder is the one who sees it, and the one who answers.
    const seen = await holder.device.get({ id: DEVICE_ID });
    expect(seen.reservation?.joinRequests.map((r) => r.userId)).toEqual([USERS[1]]);

    await holder.device.answerJoinRequest({ requestId: request.id, approve: true });

    const token = await asker.device.sessionToken({ deviceId: DEVICE_ID });
    const claims = JSON.parse(atob(token.token.split(".")[1]!.replace(/-/g, "+")));
    expect(claims.reservationId).toBe(held.id);
    expect(claims.userId).toBe(USERS[1]);

    // The holder keeps the device, and now names who is in with them.
    const after = await holder.device.get({ id: DEVICE_ID });
    expect(after.reservation?.userId).toBe(USERS[0]);
    expect(after.reservation?.observers.map((o) => o.userId)).toEqual([USERS[1]]);
    expect(after.reservation?.joinRequests).toEqual([]);
  });

  test("somebody who was let in can leave again", async () => {
    await resetDevice();
    const holder = callerFor(USERS[0]);
    await holder.device.reserve({ deviceId: DEVICE_ID });
    const asker = callerFor(USERS[1]);

    const request = await asker.device.requestJoin({ deviceId: DEVICE_ID });
    await holder.device.answerJoinRequest({ requestId: request.id, approve: true });
    await expect(asker.device.sessionToken({ deviceId: DEVICE_ID })).resolves.toBeDefined();

    // Leaving used to live on the admin router, from when only admins could be
    // in a session at all — so the first non-admin ever let into one got a 403
    // from the button that was right there.
    await asker.device.leaveSession({ deviceId: DEVICE_ID });

    await expect(asker.device.sessionToken({ deviceId: DEVICE_ID })).rejects.toMatchObject({
      code: "FORBIDDEN",
    });
    const after = await holder.device.get({ id: DEVICE_ID });
    expect(after.reservation?.observers).toEqual([]);
  });

  test("a declined request lets nobody in", async () => {
    await resetDevice();
    const holder = callerFor(USERS[0]);
    await holder.device.reserve({ deviceId: DEVICE_ID });
    const asker = callerFor(USERS[1]);

    const request = await asker.device.requestJoin({ deviceId: DEVICE_ID });
    await holder.device.answerJoinRequest({ requestId: request.id, approve: false });

    await expect(asker.device.sessionToken({ deviceId: DEVICE_ID })).rejects.toMatchObject({
      code: "FORBIDDEN",
    });
    // And the asker can tell why, which is the only way they would know.
    expect(await asker.device.myJoinRequest({ deviceId: DEVICE_ID })).toMatchObject({
      state: "denied",
    });
    // Answering twice is not a second chance.
    await expect(
      holder.device.answerJoinRequest({ requestId: request.id, approve: true }),
    ).rejects.toMatchObject({ code: "PRECONDITION_FAILED" });
  });

  test("asking twice is asking once", async () => {
    await resetDevice();
    await callerFor(USERS[0]).device.reserve({ deviceId: DEVICE_ID });
    const asker = callerFor(USERS[1]);

    // Two tabs, or one impatient double-click.
    const [first, second] = await Promise.all([
      asker.device.requestJoin({ deviceId: DEVICE_ID }),
      asker.device.requestJoin({ deviceId: DEVICE_ID }),
    ]);
    expect(second.id).toBe(first.id);

    const seen = await callerFor(USERS[0]).device.get({ id: DEVICE_ID });
    expect(seen.reservation?.joinRequests).toHaveLength(1);
  });

  test("only the holder answers, and the holder cannot ask", async () => {
    await resetDevice();
    await callerFor(USERS[0]).device.reserve({ deviceId: DEVICE_ID });

    const request = await callerFor(USERS[1]).device.requestJoin({ deviceId: DEVICE_ID });

    await expect(
      callerFor(USERS[2]).device.answerJoinRequest({ requestId: request.id, approve: true }),
    ).rejects.toMatchObject({ code: "FORBIDDEN" });
    // Not even the person who asked.
    await expect(
      callerFor(USERS[1]).device.answerJoinRequest({ requestId: request.id, approve: true }),
    ).rejects.toMatchObject({ code: "FORBIDDEN" });

    await expect(
      callerFor(USERS[0]).device.requestJoin({ deviceId: DEVICE_ID }),
    ).rejects.toMatchObject({ code: "BAD_REQUEST" });
  });

  test("releasing the device leaves no request to answer", async () => {
    await resetDevice();
    const holder = callerFor(USERS[0]);
    const held = await holder.device.reserve({ deviceId: DEVICE_ID });
    const request = await callerFor(USERS[1]).device.requestJoin({ deviceId: DEVICE_ID });

    await holder.device.release({ deviceId: DEVICE_ID });

    const [row] = await db.select().from(joinRequest).where(eq(joinRequest.id, request.id));
    expect(row?.state).toBe("expired");
    expect(row?.reservationId).toBe(held.id);
  });

  test("the reaper retires a request nobody answered", async () => {
    await resetDevice();
    await callerFor(USERS[0]).device.reserve({ deviceId: DEVICE_ID });
    const request = await callerFor(USERS[1]).device.requestJoin({ deviceId: DEVICE_ID });

    // Backdate rather than waiting out the TTL.
    await db
      .update(joinRequest)
      .set({ expiresAt: new Date(Date.now() - 1000) })
      .where(eq(joinRequest.id, request.id));

    const stop = startReservationReaper(db);
    await Bun.sleep(200);
    stop();

    const [row] = await db.select().from(joinRequest).where(eq(joinRequest.id, request.id));
    expect(row?.state).toBe("expired");
    // The reservation itself was never at risk.
    const [held] = await db
      .select()
      .from(reservation)
      .where(eq(reservation.deviceId, DEVICE_ID))
      .orderBy(desc(reservation.startedAt))
      .limit(1);
    expect(held?.state).toBe("active");
  });
});

/**
 * A force-released user could not tell they had been kicked: the reason string
 * was clobbered and rendered under a spinner. The wire carries only a string,
 * so the name comes from here.
 */
describe("reservation outcomes", () => {
  test("name and reason survive a force release, for the person it happened to", async () => {
    await resetDevice();
    const holder = callerFor(USERS[0]);
    const held = await holder.device.reserve({ deviceId: DEVICE_ID });

    await callerFor(USERS[1], "admin").admin.forceRelease({
      deviceId: DEVICE_ID,
      reason: "needed for a release build",
    });

    const outcome = await holder.device.reservationOutcome({ reservationId: held.id });
    expect(outcome.state).toBe("released");
    expect(outcome.reason).toBe("needed for a release build");
    expect(outcome.releasedByName).toBe(USERS[1]);
    expect(outcome.releasedAt).not.toBeNull();
  });

  test("an expiry has no actor, because nobody did it", async () => {
    await resetDevice();
    const holder = callerFor(USERS[0]);
    const held = await holder.device.reserve({ deviceId: DEVICE_ID });

    await db
      .update(reservation)
      .set({ expiresAt: new Date(Date.now() - 1000) })
      .where(eq(reservation.id, held.id));

    const stop = startReservationReaper(db);
    await Bun.sleep(200);
    stop();

    const outcome = await holder.device.reservationOutcome({ reservationId: held.id });
    expect(outcome.releasedByName).toBeNull();
    expect(outcome.reason).toBe("reservation expired");
  });

  test("only the holder and an admin may read one", async () => {
    await resetDevice();
    const held = await callerFor(USERS[0]).device.reserve({ deviceId: DEVICE_ID });

    await expect(
      callerFor(USERS[2]).device.reservationOutcome({ reservationId: held.id }),
    ).rejects.toMatchObject({ code: "FORBIDDEN" });

    await expect(
      callerFor(USERS[2], "admin").device.reservationOutcome({ reservationId: held.id }),
    ).resolves.toBeDefined();
  });
});

/**
 * The idle policy is the one that reclaims a device from someone who left a tab
 * open over a weekend, so what matters is that a *renewing* reservation is
 * still released when nobody is driving the device — the whole reason expiry
 * alone was not enough.
 */
describe("the idle timeout", () => {
  beforeEach(async () => {
    await db.delete(setting);
    invalidateSettings();
  });

  afterAll(async () => {
    await db.delete(setting);
    invalidateSettings();
  });

  test("releases a live-but-untouched reservation, and says why", async () => {
    await resetDevice();
    await setSetting(db, "reservation.idleTimeoutSeconds", 60, USERS[0]);

    const caller = callerFor(USERS[0]);
    const held = await caller.device.reserve({ deviceId: DEVICE_ID });

    // Renewed, so `expiresAt` is far away: only the idle sweep can take this.
    await db
      .update(reservation)
      .set({ lastActivityAt: new Date(Date.now() - 120_000) })
      .where(eq(reservation.id, held.id));

    const stop = startReservationReaper(db);
    await Bun.sleep(200);
    stop();

    const [row] = await db.select().from(reservation).where(eq(reservation.id, held.id)).limit(1);
    expect(row?.state).toBe("released");
    expect(row?.reason).toBe("released after 1 minute without interaction");
    expect(row?.releasedBy).toBeNull();
  });

  test("an interaction the browser reports holds it off", async () => {
    await resetDevice();
    await setSetting(db, "reservation.idleTimeoutSeconds", 60, USERS[0]);

    const caller = callerFor(USERS[0]);
    const held = await caller.device.reserve({ deviceId: DEVICE_ID });
    await db
      .update(reservation)
      .set({ lastActivityAt: new Date(Date.now() - 120_000) })
      .where(eq(reservation.id, held.id));

    await caller.device.renew({ reservationId: held.id, interactedAt: Date.now() });

    const stop = startReservationReaper(db);
    await Bun.sleep(200);
    stop();

    const [row] = await db.select().from(reservation).where(eq(reservation.id, held.id)).limit(1);
    expect(row?.state).toBe("active");
  });

  test("neither source can wind the activity clock backwards", async () => {
    await resetDevice();
    const caller = callerFor(USERS[0]);
    const held = await caller.device.reserve({ deviceId: DEVICE_ID });

    const recent = new Date(Date.now() - 5_000);
    await db.update(reservation).set({ lastActivityAt: recent }).where(eq(reservation.id, held.id));

    // A stale renewal — a tab that was backgrounded, or a clock behind ours.
    await caller.device.renew({ reservationId: held.id, interactedAt: Date.now() - 600_000 });

    const [row] = await db.select().from(reservation).where(eq(reservation.id, held.id)).limit(1);
    expect(row?.lastActivityAt.getTime()).toBe(recent.getTime());
  });

  test("a browser clock running fast cannot buy extra time", async () => {
    await resetDevice();
    const caller = callerFor(USERS[0]);
    const held = await caller.device.reserve({ deviceId: DEVICE_ID });

    const before = Date.now();
    await caller.device.renew({ reservationId: held.id, interactedAt: before + 3_600_000 });

    const [row] = await db.select().from(reservation).where(eq(reservation.id, held.id)).limit(1);
    expect(row?.lastActivityAt.getTime()).toBeLessThanOrEqual(Date.now());
  });

  // Pinned away from UTC: a UTC test host cannot see this class of bug at all,
  // and the column is written by two sources, so a disagreement is silent.
  test("the activity clock is UTC whatever timezone the coordinator runs in", async () => {
    await resetDevice();
    const tz = process.env.TZ;
    process.env.TZ = "Asia/Tokyo";
    try {
      const caller = callerFor(USERS[0]);
      const held = await caller.device.reserve({ deviceId: DEVICE_ID });
      await caller.device.renew({ reservationId: held.id, interactedAt: Date.now() });

      const [row] = await db.select().from(reservation).where(eq(reservation.id, held.id)).limit(1);
      expect(Math.abs(row!.lastActivityAt.getTime() - Date.now())).toBeLessThan(5_000);
    } finally {
      process.env.TZ = tz;
    }
  });

  test("the maximum session length releases a device however busy it is", async () => {
    await resetDevice();
    await setSetting(db, "reservation.maxDurationSeconds", 60, USERS[0]);

    const caller = callerFor(USERS[0]);
    const held = await caller.device.reserve({ deviceId: DEVICE_ID });
    await db
      .update(reservation)
      .set({ startedAt: new Date(Date.now() - 120_000), lastActivityAt: new Date() })
      .where(eq(reservation.id, held.id));

    const stop = startReservationReaper(db);
    await Bun.sleep(200);
    stop();

    const [row] = await db.select().from(reservation).where(eq(reservation.id, held.id)).limit(1);
    expect(row?.state).toBe("released");
    expect(row?.reason).toBe("released after the 1 minute session limit");
  });
});
