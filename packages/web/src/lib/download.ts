/**
 * Hand a blob to the browser's downloads.
 *
 * The `createObjectURL` → `<a download>` dance, in one place because there are
 * now three callers — a screenshot, a file pulled off a device, and a screen
 * recording — and the third is the reason it stopped being fine to inline.
 */

/**
 * The URL is revoked on the next task rather than immediately after `click()`.
 *
 * Revoking synchronously happens to work for a small PNG, but the browser has
 * only *queued* the download at that point: for a 60 MB recording the source
 * can be pulled out from under it. Yielding once is all it takes.
 */
export function saveBlob(blob: Blob, filename: string) {
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = filename;
  link.rel = "noopener";
  link.click();
  setTimeout(() => URL.revokeObjectURL(url), 0);
}

/**
 * `20260807_234959` — local time, sortable, and readable at a glance.
 *
 * A unix millisecond count sorts just as well and tells a person nothing: the
 * whole reason these files are named after the device and the moment is so a
 * folder of them can be matched against a bug report.
 */
export function fileStamp(at: Date = new Date()): string {
  const pad = (value: number) => String(value).padStart(2, "0");
  return (
    `${at.getFullYear()}${pad(at.getMonth() + 1)}${pad(at.getDate())}` +
    `_${pad(at.getHours())}${pad(at.getMinutes())}${pad(at.getSeconds())}`
  );
}

/** Bytes as a person reads them. Undefined size renders as an em dash. */
export function formatBytes(bytes: number | undefined): string {
  if (bytes === undefined) return "—";
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  // One decimal below 10, none above: "1.4 MB" is useful, "847.3 MB" is noise.
  return `${value < 10 ? value.toFixed(1) : Math.round(value)} ${units[unit]}`;
}
