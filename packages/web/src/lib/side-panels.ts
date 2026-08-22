/**
 * Whether the device page's two side panels — the control rail and the Details
 * panel — are open.
 *
 * One key per panel for every device, like the popout handle's corner: the
 * reason to close one is that you want the picture bigger, which is about your
 * screen and your habits, not about which phone you happen to be holding.
 *
 * Both default to open. A control surface that hid itself until you found the
 * toggle would be a worse first impression than a slightly narrower screen.
 */

export type SidePanel = "controls" | "details";

const STORAGE_KEY = "yard.device.panels";

export function loadPanelOpen(panel: SidePanel): boolean {
  try {
    return window.localStorage.getItem(`${STORAGE_KEY}.${panel}`) !== "closed";
  } catch {
    // Private mode, or storage disabled. A default is a fine answer.
    return true;
  }
}

export function savePanelOpen(panel: SidePanel, open: boolean): void {
  try {
    window.localStorage.setItem(`${STORAGE_KEY}.${panel}`, open ? "open" : "closed");
  } catch {
    /* not worth telling anyone about */
  }
}
