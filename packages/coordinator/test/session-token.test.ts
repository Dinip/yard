/**
 * The configured-key path, which only production takes.
 *
 * Development leaves `SESSION_TOKEN_PRIVATE_KEY` unset and gets an ephemeral
 * keypair, so every test and every dev run exercised the *other* branch. The
 * configured one threw at boot — jose imports keys non-extractable by default
 * and the JWKS is derived by exporting the private key — and the first thing to
 * find out was a container restart loop.
 */
import { describe, expect, test } from "bun:test";
import { calculateJwkThumbprint, exportJWK, exportPKCS8, generateKeyPair, importPKCS8 } from "jose";

const ALG = "EdDSA";

/** What `loadKeys` does with a configured key. */
async function keysFromPem(pem: string) {
  const privateKey = await importPKCS8(pem, ALG, { extractable: true });
  const jwk = await exportJWK(privateKey);
  const { d: _private, ...publicJwk } = jwk;
  const kid = await calculateJwkThumbprint(publicJwk);
  return { privateKey, publicJwk: { ...publicJwk, kid, alg: ALG, use: "sig" as const } };
}

describe("session token keys", () => {
  test("a configured PKCS8 key yields a publishable JWKS", async () => {
    const { privateKey } = await generateKeyPair(ALG, { extractable: true });
    const pem = await exportPKCS8(privateKey);

    const keys = await keysFromPem(pem);

    expect(keys.publicJwk.kty).toBe("OKP");
    expect(keys.publicJwk.crv).toBe("Ed25519");
    expect(keys.publicJwk.kid).toBeTruthy();
    // The private component must never reach the JWKS.
    expect(keys.publicJwk).not.toHaveProperty("d");
  });

  test("the same key always produces the same kid", async () => {
    const { privateKey } = await generateKeyPair(ALG, { extractable: true });
    const pem = await exportPKCS8(privateKey);

    const first = await keysFromPem(pem);
    const second = await keysFromPem(pem);

    // Providers cache the JWKS by kid. A kid that moved across a restart would
    // make every provider reject freshly issued tokens until it refetched —
    // which is the whole reason for configuring a key rather than generating
    // one per boot.
    expect(second.publicJwk.kid).toBe(first.publicJwk.kid);
  });

  test("importing without extractable is what broke production", async () => {
    const { privateKey } = await generateKeyPair(ALG, { extractable: true });
    const pem = await exportPKCS8(privateKey);

    const nonExtractable = await importPKCS8(pem, ALG);
    expect(exportJWK(nonExtractable)).rejects.toThrow(/non-extractable/);
  });
});
