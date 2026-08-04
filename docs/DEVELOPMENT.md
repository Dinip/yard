# Development

## Prerequisites

- [Bun](https://bun.sh) ≥ 1.3
- Docker (for Postgres; also for the full stack)
- Rust toolchain — phases 3+ only

## First run

```bash
cp .env.example .env
# AUTH_SECRET must be ≥32 chars:  openssl rand -base64 32

bun install
docker compose -f docker-compose.dev.yml up -d     # Postgres on :5432
bun run db:migrate

# two terminals, or `bun run dev` for both
bun run dev:coordinator                            # :3000
bun run dev:web                                    # :5173
```

Open http://localhost:5173, create an account (email/password is enabled by
default for exactly this), then make yourself an admin:

```bash
bun --env-file=.env packages/coordinator/src/cli/grant-admin.ts you@example.com
```

Sign out and back in — role changes are masked by the 60s session cookie cache.

## Running against synthetic devices

No hardware is needed to develop or test anything above the provider. Sign in as
an admin, go to **Manage → Add provider**, create one (id `lab-1`, any HTTPS base
URL), issue it a token, then:

```bash
bun packages/protocol/test/fake-provider.ts --token pft_… --id lab-1 --devices 4
```

Four synthetic devices appear in the UI immediately, with realistic identifiers,
geometry and codecs. They reserve, answer commands, and vanish when you stop the
process. This is also what `packages/coordinator/test/gateway.test.ts` drives.

### …or the real Rust provider, with mock devices

The TS fake provider exercises the coordinator. To exercise the **provider**
itself — session plane, video fan-out, uploads, JWT verification — run the real
binary with `backend: mock` devices:

```bash
cargo build --release -p farm-provider
cp packages/provider/provider.example.yaml /tmp/provider.yaml   # then edit
FARM_PROVIDER_TOKEN=pft_… ./target/release/farm-provider --config /tmp/provider.yaml
```

`--check` validates the config and exits. Still no hardware required: mock
devices stream synthetic video and accept input, uploads and screenshots.

## Everyday commands

```bash
bun run check          # biome lint + format check
bun run check:fix      # …and fix
bun run typecheck      # every package
bun test --env-file=.env packages/protocol packages/coordinator/test

bun run db:generate    # after editing packages/db/src/schema/ — writes SQL
bun run db:migrate
bun run db:studio      # drizzle studio
```

Rust and the wire contract:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
bun run protocol:check    # tests + regen + diff, as CI runs it
```

`protocol:check` is the drift guard: `generated.rs` is committed, so a zod schema
edit that wasn't regenerated fails here rather than shipping to a provider as a
silent no-op.

## Full stack in Docker

```bash
cp .env.example .env    # set PUBLIC_URL, AUTH_SECRET, SITE_ADDRESS
docker compose up --build
```

Migrations run automatically from the coordinator's entrypoint, so this is a
one-command deploy on a single node.

The `provider` service is behind a profile because it needs host device access:

```bash
docker compose --profile provider up
```

## Microsoft / Entra ID sign-in

1. Register an app in Entra ID.
2. Redirect URI: `<PUBLIC_URL>/api/auth/callback/microsoft`
3. Set `MICROSOFT_CLIENT_ID`, `MICROSOFT_CLIENT_SECRET`, and
   `MICROSOFT_TENANT_ID` (your tenant GUID, or `common`).
4. Set `ENABLE_EMAIL_PASSWORD=false` once it works.

The sign-in page renders only the methods that are actually configured — it
reads `user.capabilities` — so a missing client ID shows as a missing button
rather than a broken one.

## Conventions

- **Formatting and linting**: biome. 2-space indent, double quotes, 100 cols,
  trailing commas. `bun run check:fix` before committing.
- **Imports**: `.ts`/`.tsx` extensions on relative imports (bun resolves them
  natively); `@/` alias inside `packages/web`.
- **Migrations are generated, never hand-written.** Edit the schema, run
  `db:generate`, commit the SQL.
- **`packages/web` imports coordinator *types* only.** If you find yourself
  importing a value, the shared thing belongs in `packages/protocol`.
- **Comments explain why, not what.** The ported Rust in particular carries
  comments describing field failures — preserve them verbatim.

## Where to look

| Question | File |
|---|---|
| What's built, what's next | [PROGRESS.md](./PROGRESS.md) |
| Why the system is shaped this way | [ARCHITECTURE.md](./ARCHITECTURE.md) |
| Tables and their invariants | [DATA-MODEL.md](./DATA-MODEL.md) |
| Backend internals | [COORDINATOR.md](./COORDINATOR.md) |
| Frontend internals | [WEB.md](./WEB.md) |
| The Rust provider design | [PROVIDER.md](./PROVIDER.md) |
| Wire contract | [PROTOCOL.md](./PROTOCOL.md) |
| What to read in the old STF sources | [REFERENCES.md](./REFERENCES.md) |
| Renaming the project | [RENAMING.md](./RENAMING.md) |
