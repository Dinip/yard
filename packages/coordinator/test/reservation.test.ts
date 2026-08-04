/**
 * Exercises the claim the whole ownership model rests on: two users cannot hold
 * the same device at once, even when they race. Requires a Postgres at
 * DATABASE_URL (docker compose -f docker-compose.dev.yml up -d).
 */
import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { device, provider, reservation, user } from "@farm/db";
import { eq, inArray } from "drizzle-orm";
import { db } from "../src/db.ts";
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
