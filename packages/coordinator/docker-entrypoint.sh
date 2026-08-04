#!/bin/sh
set -e

# Migrations run on every boot and are idempotent. Running them here rather than
# in a separate job keeps a single-node `docker compose up` a one-command deploy.
echo "[entrypoint] applying migrations"
MIGRATIONS_DIR=/app/drizzle bun /app/migrate.js

exec "$@"
