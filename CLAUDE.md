# CLAUDE.md

Guidance for Claude Code when working in this repository.

## Read first

**[docs/PROGRESS.md](docs/PROGRESS.md)** — the live status board. The project is
built in six phases and work is expected to stop and resume; PROGRESS.md is the
resumption point. **Update it in the same commit as the work it describes.**

Then, depending on what you're touching:
[ARCHITECTURE.md](docs/ARCHITECTURE.md) · [DATA-MODEL.md](docs/DATA-MODEL.md) ·
[COORDINATOR.md](docs/COORDINATOR.md) · [WEB.md](docs/WEB.md) ·
[PROVIDER.md](docs/PROVIDER.md) · [PROTOCOL.md](docs/PROTOCOL.md) ·
[REFERENCES.md](docs/REFERENCES.md) · [DEVELOPMENT.md](docs/DEVELOPMENT.md)

## What this is

A device farm replacing STF. The load-bearing architectural idea:

> The coordinator owns identity, inventory, reservations and policy. Providers
> own devices and every byte of high-bandwidth traffic. Video and input go
> browser↔provider directly; **the coordinator is never on the data path.**

If a change would route video, input, or an APK/IPA through the coordinator, it
is wrong — that is the specific failure of STF this project exists to fix.

## Repository

```
packages/db/           drizzle schema, migrations, client
packages/coordinator/  Hono + tRPC + better-auth + provider gateway
packages/web/          TanStack Router SPA + shadcn
packages/protocol/     zod schemas + Rust codegen        (phase 2, not built)
packages/provider/     cargo workspace: iOS + Android    (phase 3–4, not built)
```

Reference sources live **outside** this repo as siblings: `../stf`,
`../stf-ios-provider`, `../idevice`. They are read-only reference material — port
from them, never import from them. See [REFERENCES.md](docs/REFERENCES.md).

## Commands

```bash
bun run check:fix                                  # biome lint + format
bun run typecheck                                  # all packages
bun test --env-file=.env packages/coordinator/test # needs dev Postgres
bun run db:generate && bun run db:migrate          # after schema edits
```

Run `check:fix` and `typecheck` before committing. Tests need
`docker compose -f docker-compose.dev.yml up -d`.

Phases 3+: `cargo clippy --workspace -- -D warnings`, `cargo fmt --check`, and
`bun run protocol:gen && git diff --exit-code`.

## Working agreements

- **Dependency versions**: check for the current release before adding anything
  (`bun pm view <pkg> version`); do not copy versions from memory.
- **Migrations are generated, never hand-written.** Edit
  `packages/db/src/schema/`, run `db:generate`, commit the SQL.
- **`packages/web` imports coordinator *types* only.** Importing a value means
  the shared thing belongs in `packages/protocol`.
- **Don't touch `device/media.rs` when it's ported.** Its RTP/keyframe-recovery
  behaviour is hard-won; every comment in it marks a field failure.
- **Comments explain why, not what**, and match the density of surrounding code.
- **The product name lives in `APP_NAME`.** Never hardcode "Device Farm" in a
  user-visible string. See [RENAMING.md](docs/RENAMING.md).

## Invariants worth knowing

- **Reservation exclusivity is a database property**, not application logic: a
  partial unique index on `reservation(device_id) where state = 'active'`. Don't
  add a lock; do keep the `23505` → `CONFLICT` translation.
- **A second reserve by the same user renews**, it does not conflict. This is
  what makes the popout window work.
- **Providers dial out.** The coordinator never connects to a provider. This is
  why providers work behind NAT.
- **Session tokens are Ed25519 JWTs verified by the provider against a cached
  JWKS.** A provider keeps serving an authorized session across a coordinator
  restart; revocation is a control-plane push, not a token-expiry side effect.
- **TLS is a hard requirement for streaming**, because WebCodecs needs a secure
  context from any non-loopback origin.
- **There is no artifact storage anywhere.** Uploads stream to the provider, get
  installed, and are deleted. The only trace is an `auditLog` row.

## Git

Commit per meaningful unit of work with PROGRESS.md updated alongside. The
default branch is `main`.
