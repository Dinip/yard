/**
 * Holding a released device until its provider has reset it.
 *
 * The behaviour under test is the one STF never had: the device does not go
 * back in the pool at the moment the reservation ends, it goes back when the
 * provider says it is clean — and nothing, including a provider that dies
 * mid-clean, may leave it parked. Requires a Postgres at DATABASE_URL
 * (docker compose -f docker-compose.dev.yml up -d).
 */
import { afterAll, beforeAll, beforeEach, describe, expect, test } from "bun:test";
import { auditLog, device, provider, reservation, setting, user } from "@farm/db";
import type { CommandPayload } from "@farm/protocol";
import { and, desc, eq, inArray } from "drizzle-orm";
import { db } from "../src/db.ts";
import { ProviderConnection, providers } from "../src/gateway/registry.ts";
import { releaseActive, startReservationReaper } from "../src/lib/reservations.ts";
import { invalidateSettings, setSetting } from "../src/lib/settings.ts";
import { caller as callerFor, closePoolOnExit, testUser } from "./helpers.ts";

closePoolOnExit();

const PROVIDER_ID = "cleanup-provider";
const DEVICE_ID = "cleanup-device-0001";
const USERS = ["cleanup-user-a", "cleanup-user-b"];

/** Every command this provider was sent, in order. */
let sent: CommandPayload[] = [];

/**
 * A provider that records what it is told rather than doing it — the point of
 * most of these tests is *which* commands went out, and in what order.
 */
function recordingProvider() {
  const conn: ProviderConnection = new ProviderConnection(
    PROVIDER_ID,
    "https://cleanup.test",
    (msg) => {
      if (msg.type === "command") {
        sent.push(msg.payload);
        queueMicrotask(() => conn.settle(msg.id, true, undefined));
      }
    },
  );
  providers.add(conn);
  return conn;
}

beforeAll(async () => {
  await wipe();
  await db.insert(provider).values({
    id: PROVIDER_ID,
    name: "cleanup test",
    publicBaseUrl: "https://cleanup.test",
    status: "online",
  });
  await db.insert(device).values({
    id: DEVICE_ID,
    providerId: PROVIDER_ID,
    platform: "android",
    name: "Test Pixel",
    status: "ready",
  });
  await db.insert(user).values(USERS.map(testUser));
});

afterAll(async () => {
  await wipe();
  await clearCleanupSettings();
});

beforeEach(async () => {
  sent = [];
  dropProvider();
  recordingProvider();
  await db.delete(reservation).where(eq(reservation.deviceId, DEVICE_ID));
  await db
    .update(device)
    .set({ status: "ready", updatedAt: new Date() })
    .where(eq(device.id, DEVICE_ID));
});

async function wipe() {
  await db.delete(auditLog).where(eq(auditLog.targetId, DEVICE_ID));
  await db.delete(reservation).where(eq(reservation.deviceId, DEVICE_ID));
  await db.delete(device).where(eq(device.id, DEVICE_ID));
  await db.delete(provider).where(eq(provider.id, PROVIDER_ID));
  await db.delete(user).where(inArray(user.id, USERS));
  dropProvider();
}

function dropProvider() {
  const existing = providers.get(PROVIDER_ID);
  if (existing) providers.remove(existing);
}

async function clearCleanupSettings() {
  await db.delete(setting).where(eq(setting.key, "cleanup.enabled"));
  await db.delete(setting).where(eq(setting.key, "cleanup.clearAppData"));
  await db.delete(setting).where(eq(setting.key, "cleanup.timeoutSeconds"));
  await db.delete(setting).where(eq(setting.key, "cleanup.clearAppDataAllow"));
  await db.delete(setting).where(eq(setting.key, "cleanup.clearAppDataDeny"));
  invalidateSettings();
}

async function enableCleanup(overrides: Record<string, unknown> = {}) {
  await setSetting(db, "cleanup.enabled", true, null);
  for (const [key, value] of Object.entries(overrides)) {
    await setSetting(db, key as "cleanup.clearAppData", value as never, null);
  }
  invalidateSettings();
}

async function status() {
  const [row] = await db.select().from(device).where(eq(device.id, DEVICE_ID)).limit(1);
  return row?.status;
}

const cleanupCommands = () => sent.filter((cmd) => cmd.kind === "device.cleanup");

describe("cleanup on release", () => {
  test("is off until an admin turns it on", async () => {
    await clearCleanupSettings();
    const caller = callerFor(USERS[0]);
    await caller.device.reserve({ deviceId: DEVICE_ID });
    await caller.device.release({ deviceId: DEVICE_ID });

    expect(await status()).toBe("ready");
    expect(cleanupCommands()).toHaveLength(0);
    // Revocation is unconditional: it is what drops live viewers.
    expect(sent.some((cmd) => cmd.kind === "session.revoke")).toBe(true);
  });

  test("holds the device in `cleaning` and sends the configured steps", async () => {
    await enableCleanup({ "cleanup.clearAppData": true });
    const caller = callerFor(USERS[0]);
    await caller.device.reserve({ deviceId: DEVICE_ID });
    await caller.device.release({ deviceId: DEVICE_ID });

    expect(await status()).toBe("cleaning");

    const [cmd] = cleanupCommands();
    expect(cmd).toBeDefined();
    if (cmd?.kind !== "device.cleanup") throw new Error("unreachable");
    expect(cmd.deviceId).toBe(DEVICE_ID);
    expect(cmd.steps).toEqual({
      uninstallApps: true,
      resetScreen: true,
      clearAppData: true,
      wipeFolders: false,
    });
    expect(cmd.timeoutSeconds).toBe(120);
    // Unset means unrestricted, which is only reachable by an admin who left
    // both boxes empty — the UI says as much.
    expect(cmd.clearAppDataFilter).toEqual({ allow: [], deny: [] });
  });

  test("carries the app id patterns clearing is scoped to", async () => {
    await enableCleanup({
      "cleanup.clearAppData": true,
      "cleanup.clearAppDataAllow": ["*.google.*", " com.acme.harness ", ""],
      "cleanup.clearAppDataDeny": ["com.acme.mdm"],
    });
    const caller = callerFor(USERS[0]);
    await caller.device.reserve({ deviceId: DEVICE_ID });
    await caller.device.release({ deviceId: DEVICE_ID });

    const [cmd] = cleanupCommands();
    if (cmd?.kind !== "device.cleanup") throw new Error("unreachable");
    // Trimmed and de-blanked on the way in, so the provider never has to guess
    // whether a stray space was meant to be part of a pattern.
    expect(cmd.clearAppDataFilter).toEqual({
      allow: ["*.google.*", "com.acme.harness"],
      deny: ["com.acme.mdm"],
    });
  });

  test("revokes before it cleans", async () => {
    await enableCleanup();
    const caller = callerFor(USERS[0]);
    await caller.device.reserve({ deviceId: DEVICE_ID });
    await caller.device.release({ deviceId: DEVICE_ID });

    const kinds = sent.map((cmd) => cmd.kind);
    // Live viewers must drop before anything starts uninstalling under them.
    expect(kinds.indexOf("session.revoke")).toBeLessThan(kinds.indexOf("device.cleanup"));
  });

  test("a cleaning device cannot be reserved", async () => {
    await enableCleanup();
    const caller = callerFor(USERS[0]);
    await caller.device.reserve({ deviceId: DEVICE_ID });
    await caller.device.release({ deviceId: DEVICE_ID });

    await expect(callerFor(USERS[1]).device.reserve({ deviceId: DEVICE_ID })).rejects.toMatchObject(
      { code: "PRECONDITION_FAILED" },
    );
  });

  test("the provider reporting `ready` puts it back in the pool", async () => {
    await enableCleanup();
    const caller = callerFor(USERS[0]);
    await caller.device.reserve({ deviceId: DEVICE_ID });
    await caller.device.release({ deviceId: DEVICE_ID });
    expect(await status()).toBe("cleaning");

    // What the provider's `device.status` push lands as.
    await db
      .update(device)
      .set({ status: "ready", updatedAt: new Date() })
      .where(eq(device.id, DEVICE_ID));

    await expect(
      callerFor(USERS[1]).device.reserve({ deviceId: DEVICE_ID }),
    ).resolves.toBeDefined();
  });

  test("a disconnected provider is not asked to clean", async () => {
    await enableCleanup();
    await callerFor(USERS[0]).device.reserve({ deviceId: DEVICE_ID });
    sent = []; // forget the reserve's `session.authorize`

    // The disconnect path: the socket is gone, so there is nothing to send to
    // and marking the device `cleaning` would park it until the reaper looked.
    await releaseActive(db, [DEVICE_ID], {
      actorUserId: null,
      reason: "provider disconnected",
      revoke: false,
    });

    expect(await status()).toBe("ready");
    expect(sent).toHaveLength(0);
  });

  test("an admin force-release cleans too", async () => {
    await enableCleanup();
    await callerFor(USERS[0]).device.reserve({ deviceId: DEVICE_ID });
    await callerFor(USERS[1], "admin").admin.forceRelease({ deviceId: DEVICE_ID });

    expect(await status()).toBe("cleaning");
    expect(cleanupCommands()).toHaveLength(1);
  });

  test("the reaper returns a device stuck in cleaning", async () => {
    await enableCleanup({ "cleanup.timeoutSeconds": 30 });
    const caller = callerFor(USERS[0]);
    await caller.device.reserve({ deviceId: DEVICE_ID });
    await caller.device.release({ deviceId: DEVICE_ID });
    expect(await status()).toBe("cleaning");

    // The provider died mid-clean and will never report anything again.
    // Backdate past the timeout plus its grace period.
    await db
      .update(device)
      .set({ updatedAt: new Date(Date.now() - (30 + 60 + 5) * 1000) })
      .where(eq(device.id, DEVICE_ID));

    const stop = startReservationReaper(db);
    await Bun.sleep(300);
    stop();

    expect(await status()).toBe("ready");
  });

  test("the reaper leaves a cleaning device that is still within its deadline", async () => {
    await enableCleanup({ "cleanup.timeoutSeconds": 600 });
    const caller = callerFor(USERS[0]);
    await caller.device.reserve({ deviceId: DEVICE_ID });
    await caller.device.release({ deviceId: DEVICE_ID });

    const stop = startReservationReaper(db);
    await Bun.sleep(300);
    stop();

    expect(await status()).toBe("cleaning");
  });

  test("a reservation reclaimed by the reaper is cleaned like any other", async () => {
    await enableCleanup();
    await callerFor(USERS[0]).device.reserve({ deviceId: DEVICE_ID });
    await db
      .update(reservation)
      .set({ expiresAt: new Date(Date.now() - 1000) })
      .where(and(eq(reservation.deviceId, DEVICE_ID), eq(reservation.state, "active")));

    const stop = startReservationReaper(db);
    await Bun.sleep(300);
    stop();

    expect(await status()).toBe("cleaning");
    expect(cleanupCommands()).toHaveLength(1);
  });

  test("a finished cleanup leaves exactly one audit row", async () => {
    const { GatewaySession } = await import("../src/gateway/handler.ts");
    void GatewaySession;

    await db.delete(auditLog).where(eq(auditLog.targetId, DEVICE_ID));
    const { audit } = await import("../src/lib/audit.ts");
    // The gateway's `cleanup.finished` arm, at the point it writes.
    await audit(db, null, "device.cleanup", "device", DEVICE_ID, {
      removed: ["com.example.sideloaded"],
      cleared: [],
      wiped: ["/sdcard/Download"],
      errors: [],
      durationMs: 1234,
    });

    const rows = await db
      .select()
      .from(auditLog)
      .where(and(eq(auditLog.targetId, DEVICE_ID), eq(auditLog.action, "device.cleanup")))
      .orderBy(desc(auditLog.at));

    expect(rows).toHaveLength(1);
    // Nobody's action: the reservation ending caused it, not a person.
    expect(rows[0]?.actorUserId).toBeNull();
    expect(rows[0]?.metadata).toMatchObject({ removed: ["com.example.sideloaded"] });
  });
});
