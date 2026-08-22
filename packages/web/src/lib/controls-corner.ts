/**
 * Where the popout's floating control handle lives, and which way its bar
 * unfolds.
 *
 * Any corner is in *something's* way — the bottom edge is the home indicator,
 * the top corners are where iOS pulls control centre and notifications down —
 * so which one is least bad depends on the device and on what the user is
 * doing. Rather than pick for them, the handle moves and remembers.
 */

import { normalizeRotation } from "@/lib/screen/rotation";

export type Corner = "tl" | "tr" | "bl" | "br";

/** Which way the bar unfolds from the handle. */
export type Axis = "vertical" | "horizontal";

/** Top-right by default: the bottom edge belongs to the home indicator. */
export const DEFAULT_CORNER: Corner = "tr";

const STORAGE_KEY = "yard.console.controlsCorner";

const CORNERS: readonly Corner[] = ["tl", "tr", "bl", "br"];

function isCorner(value: unknown): value is Corner {
  return typeof value === "string" && (CORNERS as readonly string[]).includes(value);
}

/**
 * Which corner a point belongs to, by quadrant.
 *
 * The drop point decides, not the drag distance: someone dragging the handle
 * to the bottom-left means the bottom-left, however they got there.
 */
export function nearestCorner(
  point: { x: number; y: number },
  size: { width: number; height: number },
): Corner {
  const left = point.x < size.width / 2;
  const top = point.y < size.height / 2;
  if (top) return left ? "tl" : "tr";
  return left ? "bl" : "br";
}

/**
 * Which way the bar should unfold, from the shape of the picture.
 *
 * The popout is a window shaped like the device, so the axis with room is
 * whichever one the device is *not* long in: a column beside a portrait
 * picture, a row along the edge of a landscape one.
 *
 * `frameSize` decides where it is known, because the renderer reports it after
 * `renderRotation` is applied — it is the shape actually on screen, on both
 * backends, where Android rotates in the encoder and iOS rotates in the viewer.
 * `display` only stands in for the gap before the first frame, and it needs the
 * same rotation applied by hand for the same reason: iOS reports a portrait
 * `width`/`height` however the device is held, and `renderRotation` is the only
 * thing that says so. Nothing known at all means portrait — the shape of nearly
 * every device here, and the same assumption the screen box already makes.
 */
export function controlsAxis(input: {
  frameSize: { width: number; height: number } | null;
  display: { renderRotation?: number; width?: number; height?: number } | null;
}): Axis {
  const { frameSize, display } = input;
  if (frameSize) return frameSize.width > frameSize.height ? "horizontal" : "vertical";
  if (!display?.width || !display?.height) return "vertical";
  const swap = normalizeRotation(display.renderRotation) % 180 !== 0;
  const width = swap ? display.height : display.width;
  const height = swap ? display.width : display.height;
  return width > height ? "horizontal" : "vertical";
}

/**
 * Tailwind placement for a corner, and which way the bar unfolds from it.
 *
 * The handle and the bar are one line, along the bar's own axis: the handle
 * stays nearest its corner and the bar unfolds inwards, away from the edge it
 * would otherwise open off. The fixed extent across that line — `w-11` for a
 * column, `h-11` for a row — is what stops the handle moving when the bar
 * appears beside it, since the bar is the larger of the two.
 */
export function cornerClasses(corner: Corner, axis: Axis): string {
  const position = `${corner.startsWith("t") ? "top-3" : "bottom-3"} ${
    corner.endsWith("l") ? "left-3" : "right-3"
  }`;
  if (axis === "horizontal") {
    return `${position} h-11 items-center ${corner.endsWith("l") ? "flex-row-reverse" : "flex-row"}`;
  }
  return `${position} w-11 items-center ${corner.startsWith("t") ? "flex-col-reverse" : "flex-col"}`;
}

/**
 * Read the remembered corner.
 *
 * A single key rather than one per device: the reason to move the handle is
 * the shape of your own screen and hands, which does not change between
 * devices in the farm.
 */
export function loadCorner(): Corner {
  try {
    const stored = window.localStorage.getItem(STORAGE_KEY);
    return isCorner(stored) ? stored : DEFAULT_CORNER;
  } catch {
    // Private mode, or storage disabled. A default is a fine answer.
    return DEFAULT_CORNER;
  }
}

export function saveCorner(corner: Corner): void {
  try {
    window.localStorage.setItem(STORAGE_KEY, corner);
  } catch {
    /* not worth telling anyone about */
  }
}
