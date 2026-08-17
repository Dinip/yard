# Renaming the project

The name "Device Farm" is a placeholder. This document keeps the eventual rename
from becoming an archaeology project.

## What users actually see

**One environment variable: `APP_NAME`.** It is surfaced through
`user.capabilities` and rendered by the sign-in page and the app shell. Changing
it in `.env` renames the product everywhere a human looks, with no code change
and no rebuild.

That is the only thing that *must* change. Everything below is cosmetic
consistency, safe to do at leisure or all at once.

## Identifiers, in ascending order of pain

### 1. The `@farm/*` package scope — easy

```bash
grep -rl '@farm/' --include='*.ts' --include='*.tsx' --include='*.json' \
  --exclude-dir=node_modules . | xargs sed -i '' 's|@farm/|@newname/|g'
bun install
```

Touches `package.json` names and dependency entries, plus every import. Fully
mechanical; `bun run typecheck` will catch anything missed.

### 2. The repo directory and GitHub repo — easy

`git remote set-url origin …` after renaming on GitHub. GitHub redirects the old
URL, so nothing breaks in the interim.

Also update `REPO_URL` in `packages/web/src/lib/build-info.ts`, which the
account menu and sign-in page link to. Deliberately a constant rather than an
env var: the source a build came from is not something an operator configures.

### 3. The compose project name — easy, but note the volumes

`name: device-farm` in `docker-compose.yml`. **Renaming it orphans the existing
`postgres-data` volume** — Docker namespaces volumes by project name. Either
dump and restore, or rename the volume explicitly. On a fresh deploy this is
free; on a live one it is a migration.

### 4. Rust crate names — moderate

`farm-protocol`, `provider-core`, `backend-ios`, `backend-android` and the
workspace `Cargo.toml`. Once phase 3 exists this is a `sed` plus a
`cargo build` to confirm.

### 5. Wire-protocol constants — check before changing

`/.well-known/farm-jwks.json` is a public URL that **providers fetch and cache**.
Changing it requires deploying coordinator and providers together, or serving
both paths for one release. Not hard, but it is the one rename with a
coordination cost.

## What deliberately does *not* encode the name

- Database table and column names — generic (`device`, `provider`,
  `reservation`), so no migration is ever needed for a rename.
- Environment variable names other than `APP_NAME` — `DATABASE_URL`,
  `AUTH_SECRET`, `PUBLIC_URL` etc. are all generic.
- The session-token JWT claims.

This was intentional. The rename should never require a database migration.
