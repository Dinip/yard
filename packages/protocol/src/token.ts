import { z } from "zod";

/**
 * Claims in the Ed25519 session token the coordinator signs and the provider
 * verifies against the cached JWKS.
 *
 * Deliberately short-lived (~60s) and refreshed by the client. The lifetime is a
 * backstop, not the revocation mechanism — that is a `session.revoke` push over
 * the control plane, which works instantly and does not depend on the token
 * expiring.
 */
export const SessionTokenClaims = z.object({
  iss: z.string(),
  sub: z.string(),
  aud: z.string(),
  exp: z.number().int(),
  iat: z.number().int(),
  deviceId: z.string(),
  userId: z.string(),
  reservationId: z.string(),
  providerId: z.string(),
  /** Present only on a grant for the admin preload endpoint. */
  scope: z.literal("preload").optional(),
  /** The target platform carried by a preload grant. */
  platform: z.enum(["ios", "android"]).optional(),
});

export type SessionTokenClaims = z.infer<typeof SessionTokenClaims>;

export const JWKS_PATH = "/.well-known/yard-jwks.json";
export const SESSION_TOKEN_AUDIENCE = "yard-provider";
