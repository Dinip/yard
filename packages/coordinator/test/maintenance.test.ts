import { afterAll, beforeAll, beforeEach, describe, expect, test } from "bun:test";
import { auditLog, device, provider, reservation, user } from "@yard/db";
import { eq, inArray } from "drizzle-orm";
import { db } from "../src/db.ts";
import { providers } from "../src/gateway/registry.ts";
import { caller, closePoolOnExit, stubProviderConnection, testUser } from "./helpers.ts";

closePoolOnExit();

const PROVIDER = "maintenance-provider";
const OTHER_PROVIDER = "maintenance-other-provider";
const DEVICES = ["maintenance-device", "maintenance-sibling", "maintenance-other"];
const ADMIN = "maintenance-admin";
const HOLDER = "maintenance-holder";
const admin = caller(ADMIN, "admin");
const holder = caller(HOLDER);

async function cleanup() {
  await db
    .delete(auditLog)
    .where(inArray(auditLog.targetId, [...DEVICES, PROVIDER, OTHER_PROVIDER]));
  await db.delete(provider).where(inArray(provider.id, [PROVIDER, OTHER_PROVIDER]));
  await db.delete(user).where(inArray(user.id, [ADMIN, HOLDER]));
}

beforeAll(async () => {
  await cleanup();
  await db.insert(user).values([ADMIN, HOLDER].map(testUser));
  await db.insert(provider).values(
    [PROVIDER, OTHER_PROVIDER].map((id) => ({
      id,
      name: id,
      publicBaseUrl: "https://maintenance.test",
    })),
  );
  await db.insert(device).values(
    DEVICES.map((id, index) => ({
      id,
      providerId: index === 2 ? OTHER_PROVIDER : PROVIDER,
      platform: "ios" as const,
      status: "ready" as const,
    })),
  );
  stubProviderConnection(PROVIDER);
  stubProviderConnection(OTHER_PROVIDER);
});

beforeEach(async () => {
  await db.delete(reservation).where(inArray(reservation.deviceId, DEVICES));
  await db
    .update(device)
    .set({ status: "ready", maintenanceReason: null })
    .where(inArray(device.id, DEVICES));
  await db
    .update(provider)
    .set({ draining: false })
    .where(inArray(provider.id, [PROVIDER, OTHER_PROVIDER]));
});

afterAll(async () => {
  for (const id of [PROVIDER, OTHER_PROVIDER]) {
    const conn = providers.get(id);
    if (conn) providers.remove(conn);
  }
  await cleanup();
});

describe("maintenance policy", () => {
  test("only admins can change policy, and maintenance requires a reason or null", async () => {
    await expect(
      holder.device.setMaintenance({ deviceId: DEVICES[0]!, reason: "repair" }),
    ).rejects.toMatchObject({ code: "FORBIDDEN" });
    await expect(
      holder.provider.setDraining({ id: PROVIDER, draining: true }),
    ).rejects.toMatchObject({ code: "FORBIDDEN" });
    for (const reason of ["   ", "x".repeat(1001)]) {
      await expect(
        admin.device.setMaintenance({ deviceId: DEVICES[0]!, reason }),
      ).rejects.toMatchObject({ code: "BAD_REQUEST" });
    }
    await expect(
      admin.device.setMaintenance({ deviceId: "missing", reason: "repair" }),
    ).rejects.toMatchObject({ code: "NOT_FOUND" });
    await expect(
      admin.provider.setDraining({ id: "missing", draining: true }),
    ).rejects.toMatchObject({ code: "NOT_FOUND" });
  });

  test("maintenance is visible, audited, and blocks reservations until cleared", async () => {
    const deviceId = DEVICES[0]!;
    await admin.device.setMaintenance({ deviceId, reason: "  Replace USB cable  " });
    expect(await holder.device.get({ id: deviceId })).toMatchObject({
      maintenanceReason: "Replace USB cable",
      status: "ready",
    });
    expect((await holder.device.list()).find((d) => d.id === deviceId)?.maintenanceReason).toBe(
      "Replace USB cable",
    );
    await expect(holder.device.reserve({ deviceId })).rejects.toMatchObject({
      code: "PRECONDITION_FAILED",
      message: "Device is in maintenance: Replace USB cable",
    });
    const records = await db.select().from(auditLog).where(eq(auditLog.targetId, deviceId));
    expect(records).toContainEqual(
      expect.objectContaining({
        actorUserId: ADMIN,
        action: "device.maintenance",
        metadata: { reason: "Replace USB cable" },
      }),
    );
    await expect(holder.device.reserve({ deviceId: DEVICES[1]! })).resolves.toBeDefined();
    await admin.device.setMaintenance({ deviceId, reason: null });
    await expect(holder.device.reserve({ deviceId })).resolves.toBeDefined();
  });

  test("admins can use a maintained device without ending maintenance", async () => {
    const deviceId = DEVICES[0]!;
    await admin.device.setMaintenance({ deviceId, reason: "Repair" });
    const held = await admin.device.reserve({ deviceId });
    await expect(admin.device.sessionToken({ deviceId })).resolves.toBeDefined();
    expect((await admin.device.reserve({ deviceId })).id).toBe(held.id);
    await expect(holder.device.reserve({ deviceId })).rejects.toMatchObject({ code: "CONFLICT" });
    expect(await admin.device.get({ id: deviceId })).toMatchObject({
      maintenanceReason: "Repair",
      reservation: { userId: ADMIN },
    });
    await admin.device.release({ deviceId });
    await db.update(device).set({ status: "ready" }).where(eq(device.id, deviceId));
    expect((await holder.device.get({ id: deviceId })).maintenanceReason).toBe("Repair");
    await expect(holder.device.reserve({ deviceId })).rejects.toMatchObject({
      code: "PRECONDITION_FAILED",
    });
    await expect(admin.device.reserve({ deviceId })).resolves.toBeDefined();
  });

  test("admin maintenance access still requires a usable device and a provider accepting reservations", async () => {
    const deviceId = DEVICES[0]!;
    await admin.device.setMaintenance({ deviceId, reason: "Repair" });
    await admin.provider.setDraining({ id: PROVIDER, draining: true });
    await expect(admin.device.reserve({ deviceId })).rejects.toMatchObject({
      code: "PRECONDITION_FAILED",
      message: "Provider is draining",
    });
    await admin.provider.setDraining({ id: PROVIDER, draining: false });
    for (const status of ["cleaning", "unhealthy", "absent"] as const) {
      await db.update(device).set({ status }).where(eq(device.id, deviceId));
      await expect(admin.device.reserve({ deviceId })).rejects.toMatchObject({
        code: "PRECONDITION_FAILED",
        message: `Device is ${status}`,
      });
    }
  });

  test("maintenance preserves existing sessions and remains after release and cleanup", async () => {
    const deviceId = DEVICES[0]!;
    const held = await holder.device.reserve({ deviceId });
    await admin.device.setMaintenance({ deviceId, reason: "Flaky USB" });
    expect((await holder.device.reserve({ deviceId })).id).toBe(held.id);
    await expect(holder.device.renew({ reservationId: held.id })).resolves.toBeDefined();
    await expect(holder.device.sessionToken({ deviceId })).resolves.toBeDefined();
    await holder.device.release({ deviceId });
    await db.update(device).set({ status: "ready" }).where(eq(device.id, deviceId));
    await expect(holder.device.reserve({ deviceId })).rejects.toMatchObject({
      code: "PRECONDITION_FAILED",
    });
  });

  test("draining blocks the provider's new reservations while current sessions finish", async () => {
    const deviceId = DEVICES[0]!;
    const held = await holder.device.reserve({ deviceId });
    await admin.provider.setDraining({ id: PROVIDER, draining: true });
    expect((await holder.provider.list()).find((p) => p.id === PROVIDER)).toMatchObject({
      draining: true,
      activeReservationCount: 1,
      deviceCount: 2,
    });
    expect((await holder.device.get({ id: deviceId })).provider.draining).toBe(true);
    await expect(holder.device.reserve({ deviceId: DEVICES[1]! })).rejects.toMatchObject({
      code: "PRECONDITION_FAILED",
      message: "Provider is draining",
    });
    await expect(admin.device.reserve({ deviceId: DEVICES[1]! })).rejects.toMatchObject({
      code: "PRECONDITION_FAILED",
      message: "Provider is draining",
    });
    await expect(holder.device.reserve({ deviceId: DEVICES[2]! })).resolves.toBeDefined();
    expect((await holder.device.reserve({ deviceId })).id).toBe(held.id);
    await expect(holder.device.renew({ reservationId: held.id })).resolves.toBeDefined();
    await expect(holder.device.sessionToken({ deviceId })).resolves.toBeDefined();
    await holder.device.release({ deviceId });
    await db.update(device).set({ status: "ready" }).where(eq(device.id, deviceId));
    expect(
      (await holder.provider.list()).find((p) => p.id === PROVIDER)?.activeReservationCount,
    ).toBe(0);
    await expect(holder.device.reserve({ deviceId })).rejects.toMatchObject({
      code: "PRECONDITION_FAILED",
    });
    await admin.provider.setDraining({ id: PROVIDER, draining: false });
    await expect(holder.device.reserve({ deviceId })).resolves.toBeDefined();
    const records = await db.select().from(auditLog).where(eq(auditLog.targetId, PROVIDER));
    expect(records).toContainEqual(
      expect.objectContaining({ action: "provider.draining", metadata: { draining: true } }),
    );
    expect(records).toContainEqual(
      expect.objectContaining({ action: "provider.draining", metadata: { draining: false } }),
    );
  });

  test("clearing one policy leaves the other in force", async () => {
    const deviceId = DEVICES[0]!;
    await admin.device.setMaintenance({ deviceId, reason: "Repair" });
    await admin.provider.setDraining({ id: PROVIDER, draining: true });
    await admin.device.setMaintenance({ deviceId, reason: null });
    await expect(holder.device.reserve({ deviceId })).rejects.toMatchObject({
      code: "PRECONDITION_FAILED",
    });
    await admin.device.setMaintenance({ deviceId, reason: "Repair" });
    await admin.provider.setDraining({ id: PROVIDER, draining: false });
    await expect(holder.device.reserve({ deviceId })).rejects.toMatchObject({
      code: "PRECONDITION_FAILED",
    });
    await admin.device.setMaintenance({ deviceId, reason: null });
    await expect(holder.device.reserve({ deviceId })).resolves.toBeDefined();
  });
});
