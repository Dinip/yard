/**
 * Where the popout's control handle lands when it is dragged, and whether that
 * survives a reload.
 *
 * The point of moving it at all is that every corner is in something's way on
 * some device — the home indicator, control centre, the notification pull — so
 * the one thing that must not happen is the handle quietly returning to a
 * corner the user already rejected.
 */

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import {
  type Corner,
  cornerClasses,
  DEFAULT_CORNER,
  loadCorner,
  nearestCorner,
  saveCorner,
} from "../src/lib/controls-corner.ts";

const SCREEN = { width: 400, height: 800 };

describe("nearestCorner", () => {
  test("picks the quadrant the handle was dropped in", () => {
    expect(nearestCorner({ x: 10, y: 10 }, SCREEN)).toBe("tl");
    expect(nearestCorner({ x: 390, y: 10 }, SCREEN)).toBe("tr");
    expect(nearestCorner({ x: 10, y: 790 }, SCREEN)).toBe("bl");
    expect(nearestCorner({ x: 390, y: 790 }, SCREEN)).toBe("br");
  });

  test("a drop just past the middle commits to the far corner", () => {
    // Halfway is a boundary, not a dead zone: a drag that crosses it means it.
    expect(nearestCorner({ x: 199, y: 399 }, SCREEN)).toBe("tl");
    expect(nearestCorner({ x: 201, y: 401 }, SCREEN)).toBe("br");
  });

  test("a drop outside the screen still resolves to a corner", () => {
    // Pointer capture keeps events coming after the pointer leaves the video.
    expect(nearestCorner({ x: -50, y: -50 }, SCREEN)).toBe("tl");
    expect(nearestCorner({ x: 900, y: 900 }, SCREEN)).toBe("br");
  });
});

describe("cornerClasses", () => {
  test("left corners unfold the bar towards the middle", () => {
    // On the left the handle must stay outermost, or the bar would open off
    // the edge of the screen.
    expect(cornerClasses("tl")).toContain("flex-row-reverse");
    expect(cornerClasses("bl")).toContain("flex-row-reverse");
    expect(cornerClasses("tr")).not.toContain("flex-row-reverse");
    expect(cornerClasses("br")).not.toContain("flex-row-reverse");
  });

  test("every corner is inset from the edge", () => {
    for (const corner of ["tl", "tr", "bl", "br"] as Corner[]) {
      const classes = cornerClasses(corner);
      expect(classes).toMatch(/(top|bottom)-3/);
      expect(classes).toMatch(/(left|right)-3/);
    }
  });
});

describe("persistence", () => {
  const store = new Map<string, string>();

  beforeEach(() => {
    store.clear();
    Object.assign(globalThis, {
      window: {
        localStorage: {
          getItem: (key: string) => store.get(key) ?? null,
          setItem: (key: string, value: string) => void store.set(key, value),
        },
      },
    });
  });

  afterEach(() => {
    delete (globalThis as Record<string, unknown>).window;
  });

  test("a moved handle stays moved", () => {
    saveCorner("bl");
    expect(loadCorner()).toBe("bl");
  });

  test("nothing stored means the default corner", () => {
    expect(loadCorner()).toBe(DEFAULT_CORNER);
  });

  test("a corrupt value is ignored rather than placing the handle nowhere", () => {
    store.set("farm.console.controlsCorner", "middle");
    expect(loadCorner()).toBe(DEFAULT_CORNER);
  });

  test("storage that throws is not an error worth surfacing", () => {
    Object.assign(globalThis, {
      window: {
        localStorage: {
          getItem() {
            throw new Error("storage disabled");
          },
          setItem() {
            throw new Error("storage disabled");
          },
        },
      },
    });
    expect(() => saveCorner("tl")).not.toThrow();
    expect(loadCorner()).toBe(DEFAULT_CORNER);
  });
});
