/**
 * The build stamp is the first thing asked for in a bug report, so it has to
 * degrade rather than disappear: a build made without a sha still names its
 * version, and never links to a commit that cannot exist.
 */

import { describe, expect, test } from "bun:test";
import { buildLabel, commitUrl, REPO_URL, shortCommit } from "../src/lib/build-info.ts";

describe("buildLabel", () => {
  test("pairs the version with a short sha", () => {
    expect(buildLabel("1.2.3", "a1b2c3d4e5f6")).toBe("v1.2.3 · a1b2c3d");
  });

  test("is version-only when the sha is unknown", () => {
    expect(buildLabel("1.2.3", null)).toBe("v1.2.3");
  });
});

describe("shortCommit", () => {
  test("leaves an already-short sha alone", () => {
    expect(shortCommit("a1b2c3d")).toBe("a1b2c3d");
  });
});

describe("commitUrl", () => {
  test("points at the full sha, not the abbreviated one", () => {
    expect(commitUrl("a1b2c3d4e5f6")).toBe(`${REPO_URL}/commit/a1b2c3d4e5f6`);
  });

  test("is null when there is nothing to link to", () => {
    expect(commitUrl(null)).toBeNull();
  });
});
