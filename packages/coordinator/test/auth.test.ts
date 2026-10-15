import { afterAll, expect, test } from "bun:test";
import { account, setting, user } from "@yard/db";
import { betterAuth } from "better-auth";
import { eq } from "drizzle-orm";
import { createLocalJWKSet, exportJWK, generateKeyPair, jwtVerify, SignJWT } from "jose";
import { auth } from "../src/auth.ts";
import { db } from "../src/db.ts";
import { env } from "../src/env.ts";
import {
  createMicrosoftMigrationReader,
  MICROSOFT_ACCOUNT_MIGRATION,
  migrateMicrosoftAccounts,
} from "../src/lib/auth-migration.ts";
import { closePoolOnExit, testUser } from "./helpers.ts";

closePoolOnExit();
const CLIENT_ID = "auth-upgrade-test-client";
const TENANT = "45ee1896-6fa6-4998-ad67-979e6443fef3";
const OID = "44975757-b99e-4ecb-a895-527308afebca";
const ISSUER = `https://login.microsoftonline.com/${TENANT}/v2.0`;
const ADMIN = "auth-upgrade-admin";
const { privateKey, publicKey } = await generateKeyPair("RS256");
const keys = createLocalJWKSet({
  keys: [{ ...(await exportJWK(publicKey)), kid: "auth-upgrade" }],
});

const upgradedAuth = betterAuth({
  ...auth.options,
  logger: { disabled: true },
  socialProviders: {
    microsoft: {
      clientId: CLIENT_ID,
      clientSecret: "test-client-secret",
      tenantId: TENANT,
      disableProfilePhoto: true,
      verifyIdToken: async (token) => {
        try {
          await jwtVerify(token, keys, {
            audience: CLIENT_ID,
            issuer: ISSUER,
            algorithms: ["RS256"],
          });
          return true;
        } catch {
          return false;
        }
      },
    },
  },
});

function request(path: string, body?: unknown, cookie?: string) {
  return upgradedAuth.handler(
    new Request(`http://coordinator.internal:3000/api/auth${path}`, {
      method: body === undefined ? "GET" : "POST",
      headers: {
        origin: env.PUBLIC_URL,
        "content-type": "application/json",
        ...(cookie ? { cookie } : {}),
      },
      body: body === undefined ? undefined : JSON.stringify(body),
    }),
  );
}

function cookies(response: Response) {
  return response.headers
    .getSetCookie()
    .map((value) => value.split(";")[0])
    .join("; ");
}

afterAll(async () => {
  await db.delete(user).where(eq(user.id, ADMIN));
  await db.delete(setting).where(eq(setting.key, MICROSOFT_ACCOUNT_MIGRATION));
});

test("pre-1.7 Microsoft admin can sign in after migration without linking by email", async () => {
  await db.delete(user).where(eq(user.id, ADMIN));
  await db.insert(user).values({ ...testUser(ADMIN), emailVerified: false, role: "admin" });
  const now = Math.floor(Date.now() / 1000);
  const signToken = (issued: number) =>
    new SignJWT({
      sub: "auth-upgrade-old-sub",
      oid: OID,
      tid: TENANT,
      name: "Existing admin",
      email: `${ADMIN}@test.local`,
    })
      .setProtectedHeader({ alg: "RS256", kid: "auth-upgrade" })
      .setIssuer(ISSUER)
      .setAudience(CLIENT_ID)
      .setIssuedAt(issued)
      .setExpirationTime(issued + 3600)
      .sign(privateKey);
  await db.insert(account).values({
    id: "auth-upgrade-existing-account",
    accountId: "auth-upgrade-old-sub",
    providerId: "microsoft",
    userId: ADMIN,
    idToken: await signToken(now - 86_400),
    createdAt: new Date(),
    updatedAt: new Date(),
  });
  const provider = (await upgradedAuth.$context).socialProviders.find(
    (entry) => entry.id === "microsoft",
  )!;
  const exchange = provider.validateAuthorizationCode;
  provider.validateAuthorizationCode = async ({ code, codeVerifier }) => {
    expect(code).toBe("auth-upgrade-code");
    expect(codeVerifier).toEqual(expect.any(String));
    return { idToken: await signToken(now), scopes: ["openid", "profile", "email"] };
  };
  try {
    const login = async () => {
      const start = await request("/sign-in/social", {
        provider: "microsoft",
        callbackURL: "/devices",
      });
      const { url } = await start.json();
      const state = new URL(url).searchParams.get("state")!;
      return request(
        `/callback/microsoft?code=auth-upgrade-code&state=${encodeURIComponent(state)}`,
        undefined,
        cookies(start),
      );
    };
    const before = await login();
    expect(before.status).toBe(302);
    expect(new URL(before.headers.get("location")!).searchParams.get("error")).toBe(
      "account_not_linked",
    );

    const readIdentity = createMicrosoftMigrationReader({ clientId: CLIENT_ID, tenantId: TENANT });
    expect((await migrateMicrosoftAccounts(db, readIdentity, true)).updated).toBe(1);
    const after = await login();
    expect(after.status).toBe(302);
    expect(after.headers.get("location")).toBe("/devices");
    expect(await db.select().from(account).where(eq(account.userId, ADMIN))).toHaveLength(1);
    const signedIn = await request("/get-session", undefined, cookies(after));
    expect(await signedIn.json()).toMatchObject({ user: { id: ADMIN, role: "admin" } });
  } finally {
    provider.validateAuthorizationCode = exchange;
  }
});
