import { useEffect, useState } from "react";

/**
 * The colour a user's own reservations are drawn in.
 *
 * Purely a display preference, so it lives in localStorage rather than on the
 * account: it needs no round trip, and — the same reason the theme is stored
 * there — a popout window picks up the opener's choice for free.
 */
export const ACCENTS = [
  { value: "default", label: "Default" },
  { value: "blue", label: "Blue" },
  { value: "violet", label: "Violet" },
  { value: "emerald", label: "Emerald" },
  { value: "amber", label: "Amber" },
  { value: "rose", label: "Rose" },
] as const;

export type Accent = (typeof ACCENTS)[number]["value"];

/** Shared with the pre-paint script in index.html. */
export const ACCENT_STORAGE_KEY = "mine-accent";

function read(): Accent {
  const stored = localStorage.getItem(ACCENT_STORAGE_KEY);
  return ACCENTS.some((a) => a.value === stored) ? (stored as Accent) : "default";
}

export function useAccent() {
  const [accent, setAccentState] = useState<Accent>(read);

  // `storage` only fires in *other* documents, which is exactly what makes it
  // the popout sync: this window already knows what it just set.
  useEffect(() => {
    const onStorage = (e: StorageEvent) => {
      if (e.key === ACCENT_STORAGE_KEY) setAccentState(read());
    };
    window.addEventListener("storage", onStorage);
    return () => window.removeEventListener("storage", onStorage);
  }, []);

  useEffect(() => {
    document.documentElement.dataset.accent = accent;
  }, [accent]);

  return {
    accent,
    setAccent: (next: Accent) => {
      localStorage.setItem(ACCENT_STORAGE_KEY, next);
      setAccentState(next);
    },
  };
}
