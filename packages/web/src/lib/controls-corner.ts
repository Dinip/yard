/**
 * Where the popout's floating control handle lives.
 *
 * Any corner is in *something's* way — the bottom edge is the home indicator,
 * the top corners are where iOS pulls control centre and notifications down —
 * so which one is least bad depends on the device and on what the user is
 * doing. Rather than pick for them, the handle moves and remembers.
 */

export type Corner = "tl" | "tr" | "bl" | "br";

/** Top-right by default: the bottom edge belongs to the home indicator. */
export const DEFAULT_CORNER: Corner = "tr";

const STORAGE_KEY = "farm.console.controlsCorner";

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

/** Tailwind placement for a corner, and which side the bar unfolds towards. */
export function cornerClasses(corner: Corner): string {
  switch (corner) {
    case "tl":
      return "top-3 left-3 flex-row-reverse";
    case "bl":
      return "bottom-3 left-3 flex-row-reverse";
    case "br":
      return "right-3 bottom-3";
    default:
      return "top-3 right-3";
  }
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
