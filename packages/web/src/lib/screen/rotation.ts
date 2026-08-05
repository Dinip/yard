/**
 * Rotation of the *picture*, not of the device.
 *
 * Android's encoder follows the phone, so a rotated device streams swapped
 * dimensions and the frames arrive upright. iOS does not: CoreDevice captures
 * the native portrait buffer whatever the orientation and draws the rotated UI
 * inside it, so the frames stay 9:16 with the content sideways and the viewer is
 * the only place left to put it right. `display.renderRotation` says which case
 * this is; see packages/protocol/src/common.ts.
 *
 * Rotating what is drawn means the pointer has to travel the other way: the
 * canvas is in viewer space and the device's HID surface is in device space.
 */

/** Snap anything to 0, 90, 180 or 270. */
export function normalizeRotation(degrees: number | null | undefined): number {
  if (!degrees) return 0;
  return (((Math.round(degrees / 90) * 90) % 360) + 360) % 360;
}

/**
 * Viewer-space normalised point → device-space normalised point.
 *
 * The inverse of a clockwise rotation of the drawn image. With no rotation this
 * is the identity, which is every Android device and every unrotated iPhone.
 */
export function toDevicePoint(
  x: number,
  y: number,
  renderRotation: number | null | undefined,
): { x: number; y: number } {
  switch (normalizeRotation(renderRotation)) {
    case 90:
      return { x: y, y: 1 - x };
    case 180:
      return { x: 1 - x, y: 1 - y };
    case 270:
      return { x: 1 - y, y: x };
    default:
      return { x, y };
  }
}
