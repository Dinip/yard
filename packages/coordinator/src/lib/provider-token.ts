/**
 * Provider credentials.
 *
 * Deliberately outside better-auth: providers are machines, never carry a
 * session, and are rotated by admins. Only the sha256 of the secret is stored,
 * so a database leak does not hand out working provider credentials.
 */
import { createHash, randomBytes } from "node:crypto";

const PREFIX = "pft";

export interface GeneratedProviderToken {
  /** Shown to the admin exactly once, at creation. */
  plaintext: string;
  hash: string;
}

export function generateProviderToken(): GeneratedProviderToken {
  const plaintext = `${PREFIX}_${randomBytes(32).toString("base64url")}`;
  return { plaintext, hash: hashProviderToken(plaintext) };
}

// node:crypto rather than Bun's global: `packages/web` typechecks the router's
// type graph, which reaches this file, and the SPA has no Bun types.
export function hashProviderToken(plaintext: string): string {
  return createHash("sha256").update(plaintext).digest("hex");
}

/**
 * Renders a token for display after creation. The secret is never recoverable,
 * so lists show only this.
 */
export function maskProviderToken(plaintext: string): string {
  return `${plaintext.slice(0, PREFIX.length + 5)}…${plaintext.slice(-4)}`;
}

/** Pulls the bearer credential off an upgrade request. */
export function bearerFrom(headers: Headers): string | null {
  const auth = headers.get("authorization");
  if (!auth?.toLowerCase().startsWith("bearer ")) return null;
  const value = auth.slice(7).trim();
  return value.length > 0 ? value : null;
}
