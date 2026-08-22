/**
 * Settings are the first DB-backed configuration in the project, and the TTL
 * they now own is read on every reserve and renew — so what matters here is
 * that an unset key is indistinguishable from the old env-var behaviour, and
 * that a bad row cannot break policy.
 *
 * Requires Postgres at DATABASE_URL.
 */
import { afterAll, beforeEach, describe, expect, test } from "bun:test";
import { setting, user } from "@yard/db";
import { eq, inArray } from "drizzle-orm";
import { db } from "../src/db.ts";
import { env } from "../src/env.ts";
import { getSettings, invalidateSettings, setSetting } from "../src/lib/settings.ts";
import { caller as callerFor, closePoolOnExit, testUser } from "./helpers.ts";

closePoolOnExit();

const ADMIN = "settings-test-admin";
const MEMBER = "settings-test-member";

beforeEach(async () => {
  await cleanup();
  await db.insert(user).values([ADMIN, MEMBER].map(testUser));
});

afterAll(cleanup);

async function cleanup() {
  await db.delete(setting);
  await db.delete(user).where(inArray(user.id, [ADMIN, MEMBER]));
  invalidateSettings();
}

describe("settings", () => {
  test("an empty table reads as the env-var defaults", async () => {
    const values = await getSettings(db);
    expect(values["reservation.ttlSeconds"]).toBe(env.RESERVATION_TTL);
    expect(values["reservation.idleTimeoutSeconds"]).toBeNull();
    expect(values["reservation.maxDurationSeconds"]).toBeNull();
  });

  test("a stored value wins, and null is a value rather than unset", async () => {
    await setSetting(db, "reservation.idleTimeoutSeconds", 600, ADMIN);
    expect((await getSettings(db))["reservation.idleTimeoutSeconds"]).toBe(600);

    await setSetting(db, "reservation.idleTimeoutSeconds", null, ADMIN);
    expect((await getSettings(db))["reservation.idleTimeoutSeconds"]).toBeNull();
  });

  test("a row that does not match its schema falls back rather than propagating", async () => {
    await db.insert(setting).values({ key: "reservation.ttlSeconds", value: "half an hour" });
    invalidateSettings();
    expect((await getSettings(db))["reservation.ttlSeconds"]).toBe(env.RESERVATION_TTL);
  });

  test("set rejects a value the key's schema refuses", async () => {
    const admin = callerFor(ADMIN, "admin");
    await expect(
      admin.settings.set({ key: "reservation.ttlSeconds", value: -1 }),
    ).rejects.toThrow();
    await expect(
      admin.settings.set({ key: "reservation.idleTimeoutSeconds", value: "20m" }),
    ).rejects.toThrow();
  });

  test("only an admin may read or write the whole set", async () => {
    const member = callerFor(MEMBER);
    await expect(member.settings.get()).rejects.toThrow();
    await expect(
      member.settings.set({ key: "reservation.ttlSeconds", value: 300 }),
    ).rejects.toThrow();

    // But everyone needs the policy the countdown is rendered from.
    await expect(member.settings.public()).resolves.toMatchObject({
      idleTimeoutSeconds: null,
    });
  });

  test("a write is visible to the next read despite the cache", async () => {
    const admin = callerFor(ADMIN, "admin");
    await getSettings(db);
    await admin.settings.set({ key: "reservation.ttlSeconds", value: 300 });
    expect((await getSettings(db))["reservation.ttlSeconds"]).toBe(300);
    const [row] = await db.select().from(setting).where(eq(setting.key, "reservation.ttlSeconds"));
    expect(row?.updatedBy).toBe(ADMIN);
  });
});
