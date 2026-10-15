import { afterAll, beforeEach, describe, expect, spyOn, test } from "bun:test";
import { account, session, setting, user } from "@yard/db";
import { eq, inArray } from "drizzle-orm";
import { generateKeyPair, SignJWT } from "jose";
import { db } from "../src/db.ts";
import {
  createMicrosoftMigrationReader,
  MICROSOFT_ACCOUNT_MIGRATION,
  migrateMicrosoftAccounts,
  runMicrosoftAccountMigration,
} from "../src/lib/auth-migration.ts";
import { closePoolOnExit, testUser } from "./helpers.ts";

closePoolOnExit();
const CLIENT_ID = "migration-test-client";
const TENANT = "1f200f3e-8af6-4f99-9f92-12d376a00465";
const OID = "af40ff6c-d46c-4eb6-a46c-43fc41f8a953";
const OID_OTHER = "07710924-f4ad-4b0f-9222-9f18e95561ab";
const CONSUMER_TENANT = "9188040d-6c67-4c5b-b112-36a304b66dad";
const USERS = ["auth-migration-admin", "auth-migration-other"];
const { privateKey } = await generateKeyPair("RS256");
const readIdentity = createMicrosoftMigrationReader({ clientId: CLIENT_ID, tenantId: TENANT });

function token(claims: Record<string, unknown> = {}) {
  const issued = Math.floor(Date.now() / 1000) - 86_400;
  return new SignJWT({
    sub: "legacy-sub",
    oid: OID,
    tid: TENANT,
    iss: `https://login.microsoftonline.com/${TENANT}/v2.0`,
    aud: CLIENT_ID,
    iat: issued,
    exp: issued + 3600,
    ...claims,
  })
    .setProtectedHeader({ alg: "RS256", kid: "retired-signing-key" })
    .sign(privateKey);
}

async function insertAccount(
  id: string,
  accountId: string,
  idToken: string | null,
  userId = USERS[0]!,
  providerId = "microsoft",
) {
  const now = new Date();
  await db.insert(account).values({
    id,
    accountId,
    userId,
    providerId,
    idToken,
    accessToken: "keep-access-token",
    refreshToken: "keep-refresh-token",
    createdAt: now,
    updatedAt: now,
  });
}

async function cleanup() {
  await db.delete(user).where(inArray(user.id, USERS));
  await db.delete(setting).where(eq(setting.key, MICROSOFT_ACCOUNT_MIGRATION));
}

beforeEach(async () => {
  await cleanup();
  await db
    .insert(user)
    .values(USERS.map((id) => ({ ...testUser(id), emailVerified: false, role: "admin" })));
});
afterAll(cleanup);

describe("trusted stored Microsoft identities", () => {
  test("reads expired tokens with retired keys without contacting Microsoft", async () => {
    const fetch = spyOn(globalThis, "fetch").mockImplementation(() => {
      throw new Error("Migration must work without Microsoft's signing keys");
    });
    try {
      expect(readIdentity(await token())).toEqual({ sub: "legacy-sub", oid: OID, tid: TENANT });
      expect(fetch).not.toHaveBeenCalled();
    } finally {
      fetch.mockRestore();
    }
  });

  test("accepts audience lists and ignores historical token lifetimes", async () => {
    expect(
      readIdentity(await token({ aud: ["another-app", CLIENT_ID], iat: undefined, exp: 0 })),
    ).toEqual({ sub: "legacy-sub", oid: OID, tid: TENANT });
  });

  test.each([
    ["wrong audience", { aud: "another-app" }],
    ["missing audience", { aud: undefined }],
    ["wrong audience list", { aud: ["another-app"] }],
    ["wrong issuer", { iss: "https://attacker.example.test" }],
    ["missing issuer", { iss: undefined }],
    ["wrong tenant", { tid: OID_OTHER }],
    ["invalid tenant", { tid: "not-a-tenant-id" }],
    ["missing subject", { sub: undefined }],
    ["empty subject", { sub: "" }],
    ["missing object ID", { oid: undefined }],
    ["invalid object ID", { oid: "not-an-object-id" }],
  ])("rejects %s", async (_label, claims) => {
    const stored = await token(claims);
    expect(() => readIdentity(stored)).toThrow();
  });

  test("enforces common, organizations and consumers tenant restrictions", async () => {
    const personal = await token({
      tid: CONSUMER_TENANT,
      iss: `https://login.microsoftonline.com/${CONSUMER_TENANT}/v2.0`,
    });
    const forTenant = (tenantId: string) =>
      createMicrosoftMigrationReader({ clientId: CLIENT_ID, tenantId });
    expect(forTenant("common")(personal).oid).toBe(OID);
    expect(forTenant("consumers")(personal).oid).toBe(OID);
    expect(() => forTenant("organizations")(personal)).toThrow();
    const organizational = await token();
    expect(() => forTenant("consumers")(organizational)).toThrow();
    const wrongIssuer = await token({ iss: "https://attacker.example.test" });
    expect(() => forTenant("common")(wrongIssuer)).toThrow();
  });
});

test("preview is read-only; apply preserves users, roles, tokens and sessions and can be repeated", async () => {
  await insertAccount("auth-migration-legacy", "legacy-sub", await token());
  await insertAccount("auth-migration-credentials", USERS[0]!, null, USERS[0]!, "credential");
  const [original] = await db.select().from(account).where(eq(account.id, "auth-migration-legacy"));
  const [originalUser] = await db.select().from(user).where(eq(user.id, USERS[0]!));
  const now = new Date();
  await db.insert(session).values({
    id: "auth-migration-session",
    userId: USERS[0]!,
    token: "auth-migration-session-token",
    createdAt: now,
    updatedAt: now,
    expiresAt: new Date(Date.now() + 86_400_000),
  });

  const preview = await migrateMicrosoftAccounts(db, readIdentity);
  expect(preview).toEqual({
    accounts: [{ id: original!.id, from: "legacy-sub", to: OID, error: null }],
    updated: 0,
  });
  expect(await db.select().from(account).where(eq(account.id, original!.id))).toEqual([original]);
  expect(
    await db.select().from(setting).where(eq(setting.key, MICROSOFT_ACCOUNT_MIGRATION)),
  ).toHaveLength(0);
  expect((await migrateMicrosoftAccounts(db, readIdentity, true)).updated).toBe(1);
  expect(
    await db.select().from(setting).where(eq(setting.key, MICROSOFT_ACCOUNT_MIGRATION)),
  ).toMatchObject([{ value: true }]);
  expect(await db.select().from(account).where(eq(account.id, original!.id))).toEqual([
    { ...original, accountId: OID, updatedAt: expect.any(Date) },
  ]);
  expect(await db.select().from(user).where(eq(user.id, USERS[0]!))).toEqual([originalUser]);
  expect(
    await db.select().from(session).where(eq(session.id, "auth-migration-session")),
  ).toHaveLength(1);
  expect(
    await db.select().from(account).where(eq(account.id, "auth-migration-credentials")),
  ).toHaveLength(1);
  const repeated = await migrateMicrosoftAccounts(db, readIdentity, true);
  expect(repeated).toEqual({
    accounts: [{ id: original!.id, from: OID, to: OID, error: null }],
    updated: 0,
  });
});

test("fresh databases and already oid-based accounts complete without rewriting accounts", async () => {
  expect(await migrateMicrosoftAccounts(db, readIdentity, true)).toEqual({
    accounts: [],
    updated: 0,
  });
  await insertAccount("auth-migration-current", OID, await token());
  expect((await migrateMicrosoftAccounts(db, readIdentity, true)).updated).toBe(0);
});

test.each([null, "invalid-token"])(
  "missing or malformed token %s is reported and prevents all updates",
  async (badToken) => {
    await insertAccount("auth-migration-valid", "legacy-sub", await token());
    await insertAccount("auth-migration-invalid", "another-sub", badToken, USERS[1]!);
    const report = await migrateMicrosoftAccounts(db, readIdentity, true);
    expect(report.updated).toBe(0);
    expect(report.accounts.find((row) => row.id === "auth-migration-invalid")!.error).toBe(
      badToken === null ? "Stored ID token is missing" : "ERR_JWT_INVALID",
    );
    const rows = await db.select().from(account).where(eq(account.id, "auth-migration-valid"));
    expect(rows[0]!.accountId).toBe("legacy-sub");
  },
);

test("a stored token for another account cannot supply the mapping", async () => {
  await insertAccount("auth-migration-mismatch", "unrelated-sub", await token());
  const report = await migrateMicrosoftAccounts(db, readIdentity, true);
  expect(report.updated).toBe(0);
  expect(report.accounts[0]!.error).toBe("Stored account does not match the token identity");
});

test("a target oid already owned by another row fails without merging users", async () => {
  await insertAccount("auth-migration-old", "legacy-sub", await token());
  await insertAccount("auth-migration-target", OID, await token(), USERS[1]!);
  const report = await migrateMicrosoftAccounts(db, readIdentity, true);
  expect(report.updated).toBe(0);
  expect(report.accounts.every((row) => row.error?.includes("Multiple accounts"))).toBe(true);
  expect(await db.select().from(account).where(eq(account.id, "auth-migration-old"))).toMatchObject(
    [{ accountId: "legacy-sub", userId: USERS[0] }],
  );
});

test("diagnostics identify the failing claim without printing the stored token", async () => {
  const stored = await token({ aud: "another-app" });
  await insertAccount("auth-migration-wrong-app", "legacy-sub", stored);
  const report = await migrateMicrosoftAccounts(db, readIdentity);
  expect(report.accounts[0]!.error).toBe(
    "Stored Microsoft ID token does not match the configured client ID",
  );
  expect(JSON.stringify(report)).not.toContain(stored);
});

test("startup migrates once, then skips stored token parsing on subsequent boots", async () => {
  await insertAccount("auth-migration-startup", "legacy-sub", await token());
  const options = { clientId: CLIENT_ID, tenantId: TENANT };
  expect(await runMicrosoftAccountMigration(db, options)).toBe(1);
  expect(
    await db.select().from(account).where(eq(account.id, "auth-migration-startup")),
  ).toMatchObject([{ accountId: OID }]);
  await db.update(account).set({ idToken: null }).where(eq(account.id, "auth-migration-startup"));
  expect(await runMicrosoftAccountMigration(db, options)).toBe(0);
});

test("failed startup leaves all accounts and the completion marker unchanged, then retries", async () => {
  await insertAccount("auth-migration-valid", "legacy-sub", await token());
  await insertAccount("auth-migration-invalid", "another-sub", null, USERS[1]!);
  const options = { clientId: CLIENT_ID, tenantId: TENANT };
  await expect(runMicrosoftAccountMigration(db, options)).rejects.toThrow(
    "auth-migration-invalid: Stored ID token is missing",
  );
  expect(
    await db.select().from(account).where(eq(account.id, "auth-migration-valid")),
  ).toMatchObject([{ accountId: "legacy-sub" }]);
  expect(
    await db.select().from(setting).where(eq(setting.key, MICROSOFT_ACCOUNT_MIGRATION)),
  ).toHaveLength(0);
  await db
    .update(account)
    .set({ idToken: await token({ sub: "another-sub", oid: OID_OTHER }) })
    .where(eq(account.id, "auth-migration-invalid"));
  expect(await runMicrosoftAccountMigration(db, options)).toBe(2);
});

test("disabled Microsoft login defers migration until it is enabled", async () => {
  await insertAccount("auth-migration-disabled", "legacy-sub", await token());
  expect(await runMicrosoftAccountMigration(db)).toBe(0);
  expect(
    await db.select().from(setting).where(eq(setting.key, MICROSOFT_ACCOUNT_MIGRATION)),
  ).toHaveLength(0);
  expect(await runMicrosoftAccountMigration(db, { clientId: CLIENT_ID, tenantId: TENANT })).toBe(1);
});
