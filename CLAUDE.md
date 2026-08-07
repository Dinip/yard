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
packages/protocol/     zod schemas + Rust codegen + fake provider
packages/provider/     cargo workspace: farm-protocol, provider-core,
                       backend-mock, farm-provider (ios: 3b, android: 4)
```

Reference sources live **outside** this repo as siblings: `../stf`,
`../stf-ios-provider`, `../idevice`. They are read-only reference material — port
from them, never import from them. See [REFERENCES.md](docs/REFERENCES.md).

## Commands

```bash
bun run check:fix                                    # biome lint + format
bun run typecheck                                    # all packages
bun test --env-file=.env packages/protocol packages/coordinator/test
bun run db:generate && bun run db:migrate            # after schema edits
bun run protocol:check                               # wire-contract drift guard
cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings
```

Run `check:fix` and `typecheck` before committing. Tests need
`docker compose -f docker-compose.dev.yml up -d`.

**No hardware needed**, at either layer:
- `packages/protocol/test/fake-provider.ts` — a TS provider, for coordinator/UI work.
- `backend: mock` in provider.yaml — a synthetic device inside the real Rust
  provider, for provider work.

See docs/DEVELOPMENT.md.

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
- **Never hand-edit `generated.rs`.** Edit the zod schema and run
  `protocol:gen`. Every nested object needs `named("Foo", …)` or the generator
  errors rather than guessing a type name.
- **The product name lives in `APP_NAME`.** Never hardcode "Device Farm" in a
  user-visible string. See [RENAMING.md](docs/RENAMING.md).
- **Keep the docs in step with the code.** Before finishing a change, check
  whether the docs above describe what you just changed — an invariant, a wire
  message, a schema, a command — and update them in the same commit. PROGRESS.md
  always; the rest when relevant.

## Invariants worth knowing

- **Reservation exclusivity is a database property**, not application logic: a
  partial unique index on `reservation(device_id) where state = 'active'`. Don't
  add a lock; do keep the `23505` → `CONFLICT` translation.
- **A second reserve by the same user renews**, it does not conflict. This is
  what makes the popout window work.
- **Providers dial out.** The coordinator never connects to a provider. This is
  why providers work behind NAT.
- **An open `reservationObserver` row is what authorizes a non-holder** on the
  session plane — the admin role is one way to get one (`admin.joinSession`),
  the holder approving a `joinRequest` is the other, and neither is a substitute
  for the row itself.
- **Session tokens are Ed25519 JWTs verified by the provider against a cached
  JWKS.** A provider keeps serving an authorized session across a coordinator
  restart; revocation is a control-plane push, not a token-expiry side effect.
- **TLS is a hard requirement for streaming**, because WebCodecs needs a secure
  context from any non-loopback origin.
- **There is no artifact storage anywhere.** Uploads stream to the provider, get
  installed, and are deleted. The only trace is an `auditLog` row.
- **Reconcile, don't patch.** `hello` carries a provider's whole inventory and
  anything missing becomes `absent`; `stream.devices` sends a revision counter,
  not device data, and clients refetch. Both exist so a dropped or reordered
  message can never leave two sides permanently disagreeing.
- **`.optional()` and `.nullable()` are different on the wire** — absent vs.
  present-and-null. The Rust emitter maps them differently on purpose.
- **The provider registry and event broadcast are in-memory**, so exactly one
  coordinator instance is supported today.
- **A session-plane request must pass two checks**: the JWT verifies against the
  cached JWKS *and* its `reservationId` is the one the coordinator last
  authorized. A signed, unexpired token for a revoked reservation is refused.
- **JWT leeway is 5s on purpose.** The library default of 60 doubled a ~60s
  token's lifetime; there is a regression test.
- **Pointer coordinates are normalised 0..1, never pixels**, and down/move/up
  are never collapsed into a `tap` — the backend needs them to tell a drag from
  a tap.

## Git

Commit per meaningful unit of work with PROGRESS.md updated alongside. The
default branch is `main`.

Commit messages and PR titles follow
[Conventional Commits](https://www.conventionalcommits.org): `type(scope): summary`
— e.g. `feat(sessions): ask to join a session`, `fix(provider): drop stale RTP
keyframes`. Scope is the package or feature area; use `!` after the type/scope
for a breaking change.

This includes PRs: merges are squashed, so the PR title becomes the commit on
`main` and must be a valid Conventional Commit — not a bare description of the
branch.

**Ask before opening a PR.** Finishing a unit of work is not permission to
publish it; propose the title and body and wait for a yes.
