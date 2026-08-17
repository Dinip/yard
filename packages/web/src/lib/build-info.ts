/**
 * Which build the browser is actually running.
 *
 * Substituted as literals by vite's `define` (see vite.config.ts) rather than
 * fetched: the sign-in page shows the stamp too, and that page renders before
 * there is a session — and before a misconfigured coordinator would answer at
 * all, which is exactly when someone wants to know what they are looking at.
 *
 * The `typeof` guards are what let a test import this module outside a vite
 * build, where the defines do not exist.
 */

declare const __APP_VERSION__: string;
declare const __GIT_SHA__: string | null;

/**
 * A rename site — see docs/RENAMING.md. Not APP_NAME-style configuration: the
 * source of a deployment is the source it was built from, not something an
 * operator sets.
 */
export const REPO_URL = "https://github.com/Dinip/device-farm";

export const VERSION = typeof __APP_VERSION__ === "string" ? __APP_VERSION__ : "0.0.0";

export const COMMIT = typeof __GIT_SHA__ === "string" ? __GIT_SHA__ : null;

/** Long enough to be unambiguous in this repo, short enough for a nav rail. */
export function shortCommit(commit: string): string {
  return commit.slice(0, 7);
}

export function buildLabel(version: string, commit: string | null): string {
  return commit ? `v${version} · ${shortCommit(commit)}` : `v${version}`;
}

/** Null when the sha is unknown, so the caller renders text instead of a link. */
export function commitUrl(commit: string | null): string | null {
  return commit ? `${REPO_URL}/commit/${commit}` : null;
}
