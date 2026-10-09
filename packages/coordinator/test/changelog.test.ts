import { afterAll, beforeAll, expect, test } from "bun:test";
import { user } from "@yard/db";
import { inArray } from "drizzle-orm";
import { db } from "../src/db.ts";
import { appRouter } from "../src/trpc/router.ts";
import { caller, closePoolOnExit, testUser } from "./helpers.ts";

closePoolOnExit();
const USERS = ["changelog-reader", "changelog-other"];
const reader = caller(USERS[0]!);

beforeAll(async () => {
  await db.delete(user).where(inArray(user.id, USERS));
  await db.insert(user).values(USERS.map(testUser));
});
afterAll(async () => {
  await db.delete(user).where(inArray(user.id, USERS));
});

test("viewing a version persists per account and only dismisses that version", async () => {
  expect(await reader.user.changelog.viewed({ version: "1.0.0" })).toEqual({ viewed: false });
  await Promise.all([
    reader.user.changelog.markViewed({ version: "1.0.0" }),
    reader.user.changelog.markViewed({ version: "1.0.0" }),
  ]);
  expect(await caller(USERS[0]!).user.changelog.viewed({ version: "1.0.0" })).toEqual({
    viewed: true,
  });
  expect(await caller(USERS[1]!).user.changelog.viewed({ version: "1.0.0" })).toEqual({
    viewed: false,
  });
  expect(await reader.user.changelog.viewed({ version: "1.1.0" })).toEqual({ viewed: false });

  await reader.user.changelog.markViewed({ version: "1.1.0" });
  await reader.user.changelog.markViewed({ version: "1.0.0" });
  expect(await reader.user.changelog.viewed({ version: "1.1.0" })).toEqual({ viewed: true });
});

test("view status requires authentication and bounded version input", async () => {
  const anonymous = appRouter.createCaller({
    db,
    req: new Request("http://test.local"),
    user: null,
    session: null,
  });
  await expect(anonymous.user.changelog.viewed({ version: "1.0.0" })).rejects.toThrow(
    "UNAUTHORIZED",
  );
  await expect(anonymous.user.changelog.markViewed({ version: "1.0.0" })).rejects.toThrow(
    "UNAUTHORIZED",
  );
  await expect(reader.user.changelog.markViewed({ version: "" })).rejects.toThrow();
  await expect(reader.user.changelog.markViewed({ version: "x".repeat(101) })).rejects.toThrow();
});
