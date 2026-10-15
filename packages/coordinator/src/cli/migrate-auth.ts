import { createDb } from "@yard/db";
import { createMicrosoftMigrationReader, migrateMicrosoftAccounts } from "../lib/auth-migration.ts";

let pool: ReturnType<typeof createDb>["pool"] | undefined;
try {
  const args = process.argv.slice(2);
  if (args.length > 1 || (args.length === 1 && args[0] !== "--apply"))
    throw new Error("usage: migrate-auth [--apply]");
  const { DATABASE_URL, MICROSOFT_CLIENT_ID, MICROSOFT_TENANT_ID } = process.env;
  if (!DATABASE_URL || !MICROSOFT_CLIENT_ID)
    throw new Error("DATABASE_URL and MICROSOFT_CLIENT_ID are required");
  const connection = createDb(DATABASE_URL, { max: 1 });
  pool = connection.pool;
  const readIdentity = createMicrosoftMigrationReader({
    clientId: MICROSOFT_CLIENT_ID,
    tenantId: MICROSOFT_TENANT_ID || "common",
  });
  const apply = args[0] === "--apply";
  const result = await migrateMicrosoftAccounts(connection.db, readIdentity, apply);
  console.table(
    result.accounts.map((row) => ({
      account: row.id,
      from: row.from,
      to: row.to ?? "",
      status: row.error ?? (row.from === row.to ? "already migrated" : "ready"),
    })),
  );
  if (result.accounts.some((row) => row.error)) {
    console.error(
      "[auth] No accounts changed. Resolve the reported token or identity errors before applying.",
    );
    process.exitCode = 1;
  } else {
    console.log(
      apply
        ? `[auth] updated ${result.updated} Microsoft accounts`
        : "[auth] Preview only; no accounts changed. Stop the coordinator and rerun with --apply to migrate.",
    );
  }
} catch (error) {
  console.error(`[auth] ${error instanceof Error ? error.message : "Migration failed"}`);
  process.exitCode = 1;
} finally {
  await pool?.end();
}
