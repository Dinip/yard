import type { Database } from "@yard/db";
import { account, setting } from "@yard/db";
import { microsoft } from "better-auth/social-providers";
import { and, eq } from "drizzle-orm";
import { decodeJwt, errors } from "jose";
import { z } from "zod";

const identitySchema = z.object({ sub: z.string().min(1), oid: z.guid(), tid: z.guid() });
type ReadIdentity = (token: string) => z.infer<typeof identitySchema>;
export const MICROSOFT_ACCOUNT_MIGRATION = "migration.auth.microsoft-oid-v1";

function failureReason(error: unknown) {
  if (error instanceof errors.JOSEError) {
    return error.code;
  }
  if (error instanceof z.ZodError) return "missing or invalid Microsoft identity claims";
  if (
    error instanceof Error &&
    [
      "Stored ID token is missing",
      "Stored account does not match the token identity",
      "Stored Microsoft ID token does not match the configured client ID",
      "Stored Microsoft ID token does not match the configured issuer or tenant",
    ].includes(error.message)
  )
    return error.message;
  return "Stored Microsoft ID token could not be read";
}

/** Trust only tokens already stored by successful logins; never use this for signing in. */
export function createMicrosoftMigrationReader(options: {
  clientId: string;
  tenantId: string;
}): ReadIdentity {
  const provider = microsoft(options);
  return (token) => {
    const payload = decodeJwt(token);
    const identity = identitySchema.parse(payload);
    if (
      payload.aud !== options.clientId &&
      !(Array.isArray(payload.aud) && payload.aud.includes(options.clientId))
    ) {
      throw new Error("Stored Microsoft ID token does not match the configured client ID");
    }
    if (
      (provider.idToken.issuer && payload.iss !== provider.idToken.issuer) ||
      !provider.idToken.verifyClaims(payload)
    ) {
      throw new Error("Stored Microsoft ID token does not match the configured issuer or tenant");
    }
    return identity;
  };
}

/** Explicit offline migration. Without apply, this only reads accounts. */
export async function migrateMicrosoftAccounts(
  db: Database,
  readIdentity: ReadIdentity,
  apply = false,
) {
  return db.transaction(async (tx) => {
    const rows = await tx.select().from(account).where(eq(account.providerId, "microsoft"));
    const accounts: { id: string; from: string; to: string | null; error: string | null }[] = [];
    const objectIds = new Map<string, (typeof accounts)[number]>();
    for (const row of rows) {
      const result: (typeof accounts)[number] = {
        id: row.id,
        from: row.accountId,
        to: null,
        error: null,
      };
      accounts.push(result);
      try {
        if (!row.idToken) throw new Error("Stored ID token is missing");
        const identity = readIdentity(row.idToken);
        if (row.accountId !== identity.sub && row.accountId !== identity.oid) {
          throw new Error("Stored account does not match the token identity");
        }
        result.to = identity.oid;
        const previous = objectIds.get(identity.oid.toLowerCase());
        if (previous)
          previous.error = result.error =
            "Multiple accounts resolve to the same Microsoft object ID";
        objectIds.set(identity.oid.toLowerCase(), result);
      } catch (error) {
        result.error = failureReason(error);
      }
    }

    let updated = 0;
    if (apply && accounts.every((row) => !row.error)) {
      for (const row of accounts) {
        if (row.from === row.to) continue;
        const changed = await tx
          .update(account)
          .set({ accountId: row.to!, updatedAt: new Date() })
          .where(
            and(
              eq(account.id, row.id),
              eq(account.providerId, "microsoft"),
              eq(account.accountId, row.from),
            ),
          )
          .returning({ id: account.id });
        if (changed.length !== 1)
          throw new Error("Account changed during migration; retry with the coordinator stopped");
        updated++;
      }
      await tx
        .insert(setting)
        .values({ key: MICROSOFT_ACCOUNT_MIGRATION, value: true })
        .onConflictDoUpdate({
          target: setting.key,
          set: { value: true, updatedAt: new Date() },
        });
    }
    return { accounts, updated };
  });
}

export async function runMicrosoftAccountMigration(
  db: Database,
  options?: { clientId: string; tenantId: string },
) {
  if (!options) return 0;
  const [completed] = await db
    .select({ value: setting.value })
    .from(setting)
    .where(eq(setting.key, MICROSOFT_ACCOUNT_MIGRATION));
  if (completed?.value === true) return 0;

  const result = await migrateMicrosoftAccounts(db, createMicrosoftMigrationReader(options), true);
  const failures = result.accounts.filter((row) => row.error);
  if (failures.length) {
    throw new Error(
      `Microsoft account migration failed: ${failures.map((row) => `${row.id}: ${row.error}`).join("; ")}. See docs/UPGRADING.md#if-migration-fails.`,
    );
  }
  return result.updated;
}
