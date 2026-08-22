/**
 * The chrome-free single-device window.
 *
 * It joins the *same* reservation as the tab that opened it — reservations are
 * per user+device — and the provider fans video out per viewer, so this needs
 * no protocol support beyond a second session.
 */

/** Only the window's own chrome: the popout's controls float over the video. */
const CHROME = 40;

/**
 * Open, or focus, the popout for a device, sized to the device's own shape.
 *
 * The display comes from the device record rather than a live session, so the
 * page can offer this without holding the stream itself.
 */
export function openPopout(
  deviceId: string,
  display: { width?: number | null; height?: number | null; scale?: number | null },
): void {
  const scale = display.scale ?? 1;
  const logicalWidth = display.width ? display.width / scale : 400;
  const logicalHeight = display.height ? display.height / scale : 800;
  const maxHeight = Math.round(window.screen.availHeight * 0.9) - CHROME;
  const factor = Math.min(1, maxHeight / logicalHeight);
  const width = Math.round(logicalWidth * factor);
  const height = Math.round(logicalHeight * factor) + CHROME;
  window.open(
    `/devices/${deviceId}/popout`,
    `yard-${deviceId}`,
    `width=${width},height=${height},menubar=no,toolbar=no,location=no,status=no`,
  );
}
