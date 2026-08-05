/**
 * Pointer mapping under a rotated picture.
 *
 * Only iOS produces a non-zero render rotation — it captures a fixed portrait
 * buffer and draws the rotated UI inside it — and when the viewer turns that
 * picture to make it readable, every tap has to travel back the other way. Get
 * the direction wrong and taps land on the mirror image of where they were
 * aimed, which looks like broken input rather than a rotation bug.
 */

import { describe, expect, test } from "bun:test";
import { normalizeRotation, toDevicePoint } from "../src/lib/screen/rotation.ts";

describe("normalizeRotation", () => {
  test("snaps to quarter turns and never returns a negative", () => {
    expect(normalizeRotation(0)).toBe(0);
    expect(normalizeRotation(90)).toBe(90);
    expect(normalizeRotation(360)).toBe(0);
    expect(normalizeRotation(450)).toBe(90);
    expect(normalizeRotation(-90)).toBe(270);
    expect(normalizeRotation(undefined)).toBe(0);
    expect(normalizeRotation(null)).toBe(0);
  });
});

describe("toDevicePoint", () => {
  test("is the identity when nothing was rotated", () => {
    expect(toDevicePoint(0.25, 0.75, 0)).toEqual({ x: 0.25, y: 0.75 });
    expect(toDevicePoint(0.25, 0.75, undefined)).toEqual({ x: 0.25, y: 0.75 });
  });

  test("undoes a quarter turn clockwise", () => {
    // The device's top-left is drawn at the viewer's top-right, so a tap there
    // must come back as the device's origin.
    expect(toDevicePoint(1, 0, 90)).toEqual({ x: 0, y: 0 });
    expect(toDevicePoint(0, 0, 90)).toEqual({ x: 0, y: 1 });
    expect(toDevicePoint(0.5, 0.5, 90)).toEqual({ x: 0.5, y: 0.5 });
  });

  test("undoes a half and a three-quarter turn", () => {
    expect(toDevicePoint(1, 1, 180)).toEqual({ x: 0, y: 0 });
    expect(toDevicePoint(0, 1, 270)).toEqual({ x: 0, y: 0 });
  });

  test("every quarter turn round-trips through its own inverse", () => {
    // Rotating a point forward and mapping it back must land where it started,
    // which is the property the input path actually depends on.
    const forward = (x: number, y: number, rotation: number) => {
      switch (rotation) {
        case 90:
          return { x: 1 - y, y: x };
        case 180:
          return { x: 1 - x, y: 1 - y };
        case 270:
          return { x: y, y: 1 - x };
        default:
          return { x, y };
      }
    };

    for (const rotation of [0, 90, 180, 270]) {
      for (const [x, y] of [
        [0.1, 0.2],
        [0.9, 0.35],
        [0.5, 0.5],
      ]) {
        const drawn = forward(x!, y!, rotation);
        const back = toDevicePoint(drawn.x, drawn.y, rotation);
        expect(back.x).toBeCloseTo(x!, 10);
        expect(back.y).toBeCloseTo(y!, 10);
      }
    }
  });
});
