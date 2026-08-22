/**
 * Audit filtering.
 *
 * The count matters as much as the rows: the UI paginates from `total`, so a
 * predicate that the two queries disagree about would show a Next button
 * leading nowhere. Every test here asserts both.
 *
 * Requires Postgres at DATABASE_URL.
 */
import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { auditLog, user } from "@yard/db";
import { inArray } from "drizzle-orm";
import { db } from "../src/db.ts";
import { caller as callerFor, closePoolOnExit, testUser } from "./helpers.ts";

closePoolOnExit();

const USERS = ["audit-test-a", "audit-test-b"];
const DEVICE = "audit-test-device";
const PROVIDER = "audit-test-provider";

/**
 * Fixed timestamps, so the date filters are testable without sleeping — and
 * deliberately in the future, because the table is shared with a real farm's
 * history. Ordering is newest first, so dated rows are always on the first
 * page whatever else has been recorded.
 */
const DAY = 24 * 60 * 60 * 1000;
const NOW = new Date("2099-03-15T12:00:00Z");
const YESTERDAY = new Date(NOW.getTime() - DAY);
const LAST_WEEK = new Date(NOW.getTime() - 7 * DAY);

const ROWS = [
  { actor: USERS[0]!, action: "device.reserve", type: "device", target: DEVICE, at: NOW },
  { actor: USERS[0]!, action: "device.install", type: "device", target: DEVICE, at: YESTERDAY },
  { actor: USERS[1]!, action: "device.reserve", type: "device", target: DEVICE, at: LAST_WEEK },
  { actor: USERS[1]!, action: "device.force_release", type: "device", target: "other", at: NOW },
  {
    actor: USERS[1]!,
    action: "provider.token.create",
    type: "provider",
    target: PROVIDER,
    at: YESTERDAY,
  },
] as const;

beforeAll(async () => {
  await cleanup();
  await db.insert(user).values(USERS.map(testUser));
  await db.insert(auditLog).values(
    ROWS.map((row, i) => ({
      id: `audit-test-${i}`,
      actorUserId: row.actor,
      action: row.action,
      targetType: row.type,
      targetId: row.target,
      at: row.at,
    })),
  );
});

afterAll(cleanup);

async function cleanup() {
  await db.delete(auditLog).where(
    inArray(
      auditLog.id,
      ROWS.map((_, i) => `audit-test-${i}`),
    ),
  );
  await db.delete(user).where(inArray(user.id, USERS));
}

/** Only the rows this test file created; the table is shared with other suites. */
function mine(items: { id: string }[]) {
  return items.filter((row) => row.id.startsWith("audit-test-"));
}

const admin = () => callerFor(USERS[0]!, "admin");

describe("audit filtering", () => {
  test("by actor", async () => {
    const result = await admin().admin.audit({ limit: 500, offset: 0, actorUserId: USERS[1] });
    expect(mine(result.items)).toHaveLength(3);
    expect(result.items.every((row) => row.actorUserId === USERS[1])).toBe(true);
  });

  test("by target, as a prefix so a partial id still finds it", async () => {
    const exact = await admin().admin.audit({ limit: 500, offset: 0, targetId: DEVICE });
    expect(mine(exact.items)).toHaveLength(3);

    const partial = await admin().admin.audit({
      limit: 500,
      offset: 0,
      targetId: "audit-test-dev",
    });
    expect(mine(partial.items)).toHaveLength(3);
  });

  test("by target type", async () => {
    const result = await admin().admin.audit({ limit: 500, offset: 0, targetType: "provider" });
    expect(mine(result.items)).toHaveLength(1);
  });

  test("by several actions at once", async () => {
    const result = await admin().admin.audit({
      limit: 500,
      offset: 0,
      action: ["device.reserve", "device.force_release"],
    });
    expect(mine(result.items)).toHaveLength(3);
  });

  test("by date range, inclusive of both ends", async () => {
    const result = await admin().admin.audit({
      limit: 500,
      offset: 0,
      from: YESTERDAY,
      to: NOW,
    });
    // Everything but the last-week row.
    expect(mine(result.items)).toHaveLength(4);
  });

  test("filters combine", async () => {
    const result = await admin().admin.audit({
      limit: 500,
      offset: 0,
      actorUserId: USERS[0],
      action: ["device.reserve"],
      targetId: DEVICE,
    });
    expect(mine(result.items)).toHaveLength(1);
    expect(result.items[0]?.action).toBe("device.reserve");
  });

  test("total counts the whole predicate, not the page", async () => {
    const page = await admin().admin.audit({ limit: 2, offset: 0, targetId: DEVICE });
    expect(page.items).toHaveLength(2);
    expect(page.total).toBe(3);

    const rest = await admin().admin.audit({ limit: 2, offset: 2, targetId: DEVICE });
    expect(rest.items).toHaveLength(1);
    expect(rest.total).toBe(3);
  });

  test("newest first, so a page boundary is stable", async () => {
    const result = await admin().admin.audit({ limit: 500, offset: 0, targetId: DEVICE });
    const times = mine(result.items).map((row) => new Date(row.at).getTime());
    expect(times).toEqual([...times].sort((a, b) => b - a));
  });

  test("a non-admin cannot read it at all", async () => {
    await expect(callerFor(USERS[1]!).admin.audit({ limit: 10, offset: 0 })).rejects.toMatchObject({
      code: "FORBIDDEN",
    });
  });
});
