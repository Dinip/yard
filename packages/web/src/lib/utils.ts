import { type ClassValue, clsx } from "clsx";
import { twMerge } from "tailwind-merge";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

/**
 * A platform's name as it is written.
 *
 * The wire value is `ios`, and CSS `capitalize` renders that "Ios" — which is
 * not how anyone spells it. The fallback keeps a platform added to the enum
 * before this map from rendering as a bare lowercase token.
 */
export function platformLabel(platform: string): string {
  if (platform === "ios") return "iOS";
  return platform.charAt(0).toUpperCase() + platform.slice(1);
}

export function relativeTime(date: Date | string | null | undefined): string {
  if (!date) return "never";
  const then = typeof date === "string" ? new Date(date) : date;
  const seconds = Math.round((Date.now() - then.getTime()) / 1000);
  const abs = Math.abs(seconds);
  const fmt = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });
  if (abs < 60) return fmt.format(-seconds, "second");
  if (abs < 3600) return fmt.format(-Math.round(seconds / 60), "minute");
  if (abs < 86400) return fmt.format(-Math.round(seconds / 3600), "hour");
  return fmt.format(-Math.round(seconds / 86400), "day");
}
