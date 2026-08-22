import { SESSION_TOKEN_AUDIENCE, type SessionTokenClaims } from "@yard/protocol";
import { calculateJwkThumbprint, exportJWK, generateKeyPair, importPKCS8, SignJWT } from "jose";
import { env } from "../env.ts";

const ALG = "EdDSA";

/**
 * Ed25519 signer for session tokens (browser → provider).
 *
 * Providers verify against the JWKS published at `/.well-known/yard-jwks.json`,
 * so no secret is ever distributed to a provider and a provider keeps serving
 * an already-authorized session across a coordinator restart.
 *
 * If `SESSION_TOKEN_PRIVATE_KEY` is unset a keypair is generated in memory.
 * That is fine for development and actively wrong in production: every restart
 * mints a new `kid`, and providers holding the old JWKS will reject freshly
 * issued tokens until they refetch.
 */
async function loadKeys() {
  if (env.SESSION_TOKEN_PRIVATE_KEY) {
    // `extractable` is not optional: jose imports keys non-extractable by
    // default, and the JWK below is derived by exporting this one. Without it
    // the coordinator throws at boot — but only when the key is *configured*,
    // which is to say only in production.
    const privateKey = await importPKCS8(env.SESSION_TOKEN_PRIVATE_KEY, ALG, {
      extractable: true,
    });
    // jose cannot derive a public key from a private one, so the JWK is built
    // from the private JWK with the private component dropped.
    const jwk = await exportJWK(privateKey);
    const { d: _discard, ...publicJwk } = jwk;
    const kid = await calculateJwkThumbprint(publicJwk);
    return { privateKey, publicJwk: { ...publicJwk, kid, alg: ALG, use: "sig" as const } };
  }

  console.warn(
    "[session-token] SESSION_TOKEN_PRIVATE_KEY is unset — generating an ephemeral keypair. " +
      "Live device sessions will break on every restart. Set it in production.",
  );
  const { privateKey, publicKey } = await generateKeyPair(ALG, { extractable: true });
  const publicJwk = await exportJWK(publicKey);
  const kid = await calculateJwkThumbprint(publicJwk);
  return { privateKey, publicJwk: { ...publicJwk, kid, alg: ALG, use: "sig" as const } };
}

const keys = await loadKeys();

/** Served at `/.well-known/yard-jwks.json` and cached by every provider. */
export const jwks = { keys: [keys.publicJwk] };

export interface SessionTokenInput {
  deviceId: string;
  userId: string;
  reservationId: string;
  providerId: string;
}

export async function signSessionToken(input: SessionTokenInput): Promise<{
  token: string;
  expiresAt: Date;
}> {
  const now = Math.floor(Date.now() / 1000);
  const exp = now + env.SESSION_TOKEN_TTL;

  const token = await new SignJWT({
    deviceId: input.deviceId,
    userId: input.userId,
    reservationId: input.reservationId,
    providerId: input.providerId,
  } satisfies Omit<SessionTokenClaims, "iss" | "sub" | "aud" | "exp" | "iat">)
    .setProtectedHeader({ alg: ALG, kid: keys.publicJwk.kid })
    .setIssuer(env.PUBLIC_URL)
    .setSubject(input.userId)
    .setAudience(SESSION_TOKEN_AUDIENCE)
    .setIssuedAt(now)
    .setExpirationTime(exp)
    .sign(keys.privateKey);

  return { token, expiresAt: new Date(exp * 1000) };
}
