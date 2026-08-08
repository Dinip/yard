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
an admin, go to **Providers → Add provider**, create one (id `lab-1`, any HTTPS base
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

### Device metrics

The example config ships `metrics:` enabled on **:9100**, and the mock devices
report synthetic CPU, memory, battery and temperature — so the fastest end-to-end
check of the whole feature is:

```bash
curl -s localhost:9100/metrics | grep farm_
```

The Android mock reports two processes and the example's `*.demo.*` pattern
matches exactly one of them, so the app filter is visible without hardware. The
iOS mock reports battery and no CPU, which is what a real iPhone does.

For the full picture, Prometheus and Grafana are behind a compose profile:

```bash
docker compose -f docker-compose.dev.yml --profile observability up -d
```

Grafana on **:3001** (anonymous, no login), Prometheus on **:9090**, scraping a
provider running on the host. The default `up -d` still brings up Postgres alone
— nothing in the test suite needs either. Provisioning lives in
`docs/observability/`.

## Running the whole thing in Docker

The production shape — Caddy in front, coordinator and web in containers,
migrations applied by the entrypoint, a real signing key:

```bash
cp .env.example .env.docker      # then fill in, or see below
docker compose --env-file .env.docker up -d
docker compose --env-file .env.docker exec coordinator \
  bun /app/grant-admin.js you@example.com
```

The compose files pull published images from
`ghcr.io/dinip/device-farm/{coordinator,web,provider}` rather than building, so
this brings up the last *release*, not your working tree. To run a local change,
build over the image name first — compose uses what it already has:

```bash
docker build -f packages/coordinator/Dockerfile \
  -t ghcr.io/dinip/device-farm/coordinator:latest .
```

`PUBLIC_URL=http://farm.localhost` is worth knowing about: `*.localhost` is a
*potentially trustworthy origin* in every browser, so WebCodecs gets its secure
context with no certificate to trust. A real deployment sets `SITE_ADDRESS` to
a hostname and Caddy provisions TLS automatically.

**Set `SESSION_TOKEN_PRIVATE_KEY`** (`openssl genpkey -algorithm ed25519`).
Unset means a new keypair per restart, and every live session breaks until
providers refetch the JWKS.

### The provider container

```bash
docker compose --env-file .env.docker --profile provider up -d
```

On **Linux** uncomment the USB mounts in `docker-compose.yml` — the whole
`/dev/bus/usb` directory, not individual `--device` nodes, because a phone that
reboots re-enumerates under a new node — and make sure the host runs no adb
server of its own, since only one may own a device.

On **macOS** Docker cannot pass USB through at all, so the container talks to
the host's daemons instead:

```bash
bun packages/provider/scripts/usbmuxd-bridge.ts   # iOS only; leave it running
docker compose --env-file .env.docker \
  -f docker-compose.yml -f docker-compose.macos.yml --profile provider up -d
```

Android needs no bridge — the adb server already speaks TCP, and the device's
`adb_server` option points at it. This is a development shape; a provider host
in production is Linux with the bus passed in.

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

## CI, and how an image gets published

Two workflows, split so that testing a change and shipping one are separate
decisions:

- **`ci.yml`** — tests, on every PR and every push to `main`. Nothing else. It
  publishes nothing, so a merge to `main` produces no image.
- **`release.yml`** — builds and pushes the three images to GHCR, multi-arch, on
  two events and no others:
  - **a release is published** → tagged with the version (`1.2.3`, the `v` is
    stripped) and the commit sha, plus `latest` unless it is a prerelease;
  - **a PR is labelled `build`** → tagged `pr-<number>`, for trying a branch on
    real hardware before it merges. It never touches `latest`.

Publishing does not wait on CI. The two events are the whole of the policy: a
release is cut from a commit you have already seen tested, and a `build` label is
an explicit "I want this image" — checking the PR is green before labelling it is
yours to do. A fork's PR is refused, since its token cannot push a package.

**The label is consumed.** Published or failed, the run removes it at the end.
That is not tidiness: the
`labeled` event does not fire for a label that is already on the PR, so a label
left in place would make a second build of that PR impossible to ask for. Adding
it again is the rebuild gesture, and `pr-<number>` is overwritten, so a test host
on that tag picks up the newer image after a `docker compose pull`.

Tags come from `docker/metadata-action`, which is where the `latest`-on-release,
not-on-prerelease rule actually lives (`flavor: latest=auto`). Layer cache is a
registry tag per image and arch (`:buildcache-amd64`), not GitHub's cache:
publishing is rare now, and GHA's cache is scoped per branch and evicted by age —
so a release would reliably find nothing.

## Full stack in Docker

```bash
cp .env.example .env    # set PUBLIC_URL, AUTH_SECRET, SITE_ADDRESS
docker compose up -d    # pulls the published images; nothing is built here
```

Migrations run automatically from the coordinator's entrypoint, so this is a
one-command deploy on a single node.

`.env.example` is grouped by deploy unit — postgres, migrations, coordinator,
web+caddy, provider — because the units can be deployed separately. Its
**Shared across deploy units** block at the top is the list of values that then
have to agree in more than one place (`DATABASE_URL`, the `PUBLIC_URL` host
against Caddy's `SITE_ADDRESS` and the provider's `coordinator_url`, and so on).
On a single node every section goes in one file and the question does not arise.

This section is the local, one-host shape. Deploying it for real — TLS,
the first admin, provider hosts of their own — is
**[DEPLOYMENT.md](./DEPLOYMENT.md)**.

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
