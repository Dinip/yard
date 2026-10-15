# Upgrading YARD - Device Farm

## Upgrading to 0.6.5

Upgrade normally. YARD applies the auth schema changes and migrates existing
Microsoft accounts automatically on the first coordinator boot. Users, roles
and farm history are preserved. No Microsoft configuration changes or manual
migration commands are needed.

Back up PostgreSQL as usual, set `VERSION_TAG=0.6.5` in `.env`, then run:

```bash
docker compose pull coordinator web
docker compose up -d coordinator web caddy
```

Update provider hosts using your usual deployment procedure. For source
deployments, install the locked dependencies and run `bun run db:migrate`
before starting the new coordinator. Confirm that an existing user can sign in.

### If migration fails

The coordinator logs the affected account and reason, leaving account identifiers
unchanged. Check that the configured Microsoft client ID and tenant match the
original app. Missing tokens or conflicting account records need repairing.

With the coordinator stopped, this optional command previews the mappings:

```bash
docker compose run --rm --no-deps --entrypoint bun coordinator /app/migrate-auth.js
```

Correct the reported issue and restart the coordinator to retry automatically.
The CLI also accepts `--apply` as a manual recovery fallback.

To roll back, restore the pre-migration database backup and previous YARD version
together, because older versions expect the original Microsoft identifiers.
