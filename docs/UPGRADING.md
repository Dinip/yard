# Upgrading YARD - Device Farm

## Upgrading to 0.6.5

YARD 0.6.5 updates Better Auth to 1.7.10 and automatically migrates existing
Microsoft / Entra ID account identifiers on the first coordinator boot. Users,
roles, sessions and farm history are preserved. No new Entra app registration,
directory permissions or manual account migration is required for intact account
records from previous successful logins.

### What runs automatically

The container entrypoint applies the generated schema migration, which adds
lookup indexes on `account.user_id`, `session.user_id` and
`verification.identifier`. YARD's configured auth plugins need no additional
tables or columns. Source deployments must still run `bun run db:migrate`.

Before opening its HTTP listener, the coordinator changes Microsoft account
identifiers from the old app-specific `sub` to the directory `oid`. This fixes
`account_not_linked` for older accounts, including those affected by YARD 0.6.0
through 0.6.3. YARD 0.6.4 returned to Better Auth 1.6, so accounts from that
release are migrated too.

The migration trusts ID tokens already saved by previous successful Microsoft
logins. It decodes them locally and checks their account, client ID, issuer,
tenant and target `oid`, without re-verifying signatures or contacting Microsoft.
Expired tokens and retired signing keys do not prevent migration. This relies
on the integrity of the stored account records and differs from the
[upstream guide's recommendation to re-verify each token](https://better-auth.com/docs/guides/1-7-upgrade-guide#migrate-microsoft-account-identifiers).
Normal login verification is unchanged.

Account updates and the internal `migration.auth.microsoft-oid-v1` completion
marker are committed in one transaction. Later boots skip the migration.
Accounts already using `oid` are retained, including those migrated with the
earlier CLI. When Microsoft login is disabled, migration is deferred until it
is enabled. Email/password accounts are unaffected.

### Docker Compose

Run these commands from your deployment directory using your deployment's
Compose files. Compose loads `.env` automatically.

1. Record your current image tag for rollback. Stop the coordinator and back up
   PostgreSQL, keeping the database service running. Confirm the backup succeeds:

   ```bash
   docker compose stop coordinator
   docker compose exec -T postgres \
     sh -c 'pg_dump -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Fc' > yard-before-0.6.5.dump
   ```

2. Set `VERSION_TAG=0.6.5` in `.env`, then pull and start the upgraded services:

   ```bash
   docker compose pull coordinator web
   docker compose up -d coordinator web caddy
   docker compose logs coordinator
   ```

   Schema and account migrations run before the coordinator becomes healthy.
   Update provider hosts to the same YARD version with your usual deployment
   procedure.

3. Sign in with an existing account and confirm that its role and farm history
   are intact. If startup reports a migration error, follow the recovery steps
   below. Failed validation leaves all account identifiers unchanged and the
   completion marker absent, so the next boot retries after the records are fixed.

### From source

Stop the coordinator, record the current revision and back up PostgreSQL.
Check out `v0.6.5`, retain the deployment's `.env`, and run:

```bash
bun install --frozen-lockfile
bun run db:migrate
```

Build and start YARD using your usual deployment commands. Microsoft account
migration runs before the HTTP listener opens. Verify sign-in as described above.

### If migration fails

Startup reports the affected local account row and validation failure without
printing tokens. Stop the coordinator while inspecting or repairing records.
Check that `MICROSOFT_CLIENT_ID` and `MICROSOFT_TENANT_ID` match the original app.
The migration itself needs no Microsoft network connection or client secret.

The bundled CLI is a fallback for inspection and recovery. It is optional and
can preview account mappings without modifying the database:

```bash
docker compose run --rm --no-deps --entrypoint bun coordinator /app/migrate-auth.js
```

Missing or malformed ID tokens, client/tenant mismatches and conflicting
identities must be resolved before startup can complete. A missing token needs
a trusted replacement or a trusted mapping from your company. The CLI does not
accept a mapping file or infer ownership from email. Do not delete users or
enable automatic email linking to get past an error.

For duplicate account keys, inspect:

```sql
SELECT provider_id, account_id, count(*)
FROM account GROUP BY provider_id, account_id HAVING count(*) > 1;
```

Keep different users separate when resolving duplicates. After repairing the
records, restart the coordinator to retry. Alternatively, review the CLI preview
and apply manually while the coordinator is stopped:

```bash
docker compose run --rm --no-deps --entrypoint bun coordinator /app/migrate-auth.js --apply
```

From source, use `bun --env-file=.env packages/coordinator/src/cli/migrate-auth.ts`
and repeat with `--apply`. A successful apply also records completion, so the
next coordinator boot skips the migration.

### Rolling back

Stop the coordinator and restore the database backup taken before this upgrade:

```bash
docker compose stop coordinator
docker compose exec -T postgres \
  sh -c 'pg_restore -U "$POSTGRES_USER" -d "$POSTGRES_DB" --clean --if-exists --exit-on-error' \
  < yard-before-0.6.5.dump
```

Restore the previous `VERSION_TAG`, pull those images and start the services
again. Source deployments should restore the previous revision and its locked
dependencies after restoring the database. Restoring a backup discards changes
made since that backup.

Returning to Better Auth 1.6 requires the pre-migration account identifiers as
well as the old application version. If your backup was taken after an earlier
successful account migration, use a backup from before that migration to return
to Better Auth 1.6.
