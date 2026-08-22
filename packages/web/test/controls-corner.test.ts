/**
 * Where the popout's control handle lands when it is dragged, whether that
 * survives a reload, and which way its bar unfolds from there.
 *
 * The point of moving it at all is that every corner is in something's way on
 * some device — the home indicator, control centre, the notification pull — so
 * the one thing that must not happen is the handle quietly returning to a
 * corner the user already rejected. The axis is the other half: a column is
 * right beside a portrait picture and unreachable beside a landscape one.
 */

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import {
  type Corner,
  controlsAxis,
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
  test("a row unfolds away from its own left or right edge", () => {
    // The handle must stay outermost, or the bar would open off the screen.
    expect(cornerClasses("tl", "horizontal")).toContain("flex-row-reverse");
    expect(cornerClasses("bl", "horizontal")).toContain("flex-row-reverse");
    expect(cornerClasses("tr", "horizontal")).toContain("flex-row");
    expect(cornerClasses("tr", "horizontal")).not.toContain("flex-row-reverse");
    expect(cornerClasses("br", "horizontal")).not.toContain("flex-row-reverse");
  });

  test("a column unfolds away from its own top or bottom edge", () => {
    // The bar comes first in the DOM, so the reversed direction is the one that
    // puts the handle *first* — nearest a top edge, with the bar below it.
    expect(cornerClasses("tl", "vertical")).toContain("flex-col-reverse");
    expect(cornerClasses("tr", "vertical")).toContain("flex-col-reverse");
    expect(cornerClasses("bl", "vertical")).not.toContain("flex-col-reverse");
    expect(cornerClasses("br", "vertical")).not.toContain("flex-col-reverse");
    expect(cornerClasses("br", "vertical")).toContain("flex-col");
  });

  test("the handle and the bar share one line", () => {
    // Fixed across the line, so the wider bar appearing cannot move the handle.
    for (const corner of ["tl", "tr", "bl", "br"] as Corner[]) {
      expect(cornerClasses(corner, "vertical")).toContain("w-11");
      expect(cornerClasses(corner, "horizontal")).toContain("h-11");
      expect(cornerClasses(corner, "vertical")).toContain("items-center");
      expect(cornerClasses(corner, "horizontal")).toContain("items-center");
    }
  });

  test("every corner is inset from the edge", () => {
    for (const corner of ["tl", "tr", "bl", "br"] as Corner[]) {
      for (const axis of ["vertical", "horizontal"] as const) {
        const classes = cornerClasses(corner, axis);
        expect(classes).toMatch(/(top|bottom)-3/);
        expect(classes).toMatch(/(left|right)-3/);
      }
    }
  });
});

describe("controlsAxis", () => {
  test("the bar takes the axis the picture leaves room in", () => {
    expect(controlsAxis({ frameSize: { width: 1080, height: 2400 }, display: null })).toBe(
      "vertical",
    );
    expect(controlsAxis({ frameSize: { width: 2400, height: 1080 }, display: null })).toBe(
      "horizontal",
    );
  });

  test("the frame wins over the display, which lags a rotation", () => {
    // `display` can still describe the old orientation for a frame or two, and
    // the bar must not flip twice on one rotation.
    expect(
      controlsAxis({
        frameSize: { width: 2400, height: 1080 },
        display: { width: 1080, height: 2400, renderRotation: 0 },
      }),
    ).toBe("horizontal");
  });

  test("before the first frame, the display decides", () => {
    // Android: the encoder followed the device, so the reported size is already
    // the rotated one and there is nothing left for the viewer to turn.
    expect(controlsAxis({ frameSize: null, display: { width: 2400, height: 1080 } })).toBe(
      "horizontal",
    );
    // iOS: portrait dimensions whatever the orientation, and `renderRotation`
    // is the only thing that says the picture is on its side.
    expect(
      controlsAxis({ frameSize: null, display: { width: 1170, height: 2532, renderRotation: 90 } }),
    ).toBe("horizontal");
    expect(
      controlsAxis({
        frameSize: null,
        display: { width: 1170, height: 2532, renderRotation: 180 },
      }),
    ).toBe("vertical");
  });

  test("knowing nothing yet means portrait, not no bar at all", () => {
    expect(controlsAxis({ frameSize: null, display: null })).toBe("vertical");
    expect(controlsAxis({ frameSize: null, display: { renderRotation: 90 } })).toBe("vertical");
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
    store.set("yard.console.controlsCorner", "middle");
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
