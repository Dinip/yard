/**
 * Reservation exclusivity is a database property, and this function is the
 * whole of its translation into something a user sees. It had already stopped
 * working once — silently, because the losing side of a race is rare enough
 * that a 500 there looks like a flake.
 */
import { describe, expect, test } from "bun:test";
import { isUniqueViolation } from "../src/lib/pg-errors.ts";

describe("isUniqueViolation", () => {
  test("the driver's own error", () => {
    expect(isUniqueViolation(Object.assign(new Error("duplicate key"), { code: "23505" }))).toBe(
      true,
    );
  });

  test("wrapped by drizzle, which is how it actually arrives", () => {
    const driver = Object.assign(new Error("duplicate key"), { code: "23505" });
    const wrapped = Object.assign(new Error("Failed query: insert into …"), { cause: driver });
    expect(isUniqueViolation(wrapped)).toBe(true);
  });

  test("wrapped twice, because one more layer must not break it again", () => {
    const driver = Object.assign(new Error("duplicate key"), { code: "23505" });
    const once = Object.assign(new Error("Failed query"), { cause: driver });
    expect(isUniqueViolation(new Error("boom", { cause: once }))).toBe(true);
  });

  test("some other database error is not a conflict", () => {
    const driver = Object.assign(new Error("null value in column"), { code: "23502" });
    expect(isUniqueViolation(Object.assign(new Error("Failed query"), { cause: driver }))).toBe(
      false,
    );
  });

  test("anything else, including a cycle", () => {
    expect(isUniqueViolation(undefined)).toBe(false);
    expect(isUniqueViolation(null)).toBe(false);
    expect(isUniqueViolation("23505")).toBe(false);

    const looping: { cause?: unknown } = {};
    looping.cause = looping;
    expect(isUniqueViolation(looping)).toBe(false);
  });
});
