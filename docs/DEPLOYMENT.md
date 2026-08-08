# Deployment

Running this in production. For working on it, see
[DEVELOPMENT.md](./DEVELOPMENT.md).

Two shapes are documented, and they are the same system:

- **[Single machine](#single-machine)** — coordinator, Postgres, SPA, Caddy and a
  provider on one host. The devices are plugged into that host.
- **[Multiple machines](#multiple-machines)** — one coordinator host, and a
  provider on each machine the devices are plugged into. This is the shape the
  architecture exists for.

Start with the single machine even if you are heading for the second: the
coordinator half is identical, and splitting providers out later changes nothing
about it.

## Before you start

```
                    coordinator host                provider hosts
              ┌───────────────────────────┐   ┌───────────────────────┐
  browser ──► │ caddy ─┬─► coordinator ──►│   │ caddy ──► provider ──►│──► 📱
      │       │        └─► web (SPA)   postgres                       │
      │       └───────────────────────────┘   └───────────────────────┘
      │                       ▲                          │
      │                       └──── control plane ───────┘
      │                            (provider dials out)
      └───────────── video, input, uploads ─────────────►
```

**The coordinator is never on the data path.** Video, input, screenshots and APK
uploads go browser↔provider directly. This is what sizes the machines: the
coordinator host carries tRPC calls and one WebSocket per provider, and nothing
that scales with device count or resolution. Bandwidth planning belongs on the
provider hosts.

Nothing is built at deploy time. Three multi-arch images — `coordinator`, `web`
and `provider` under `ghcr.io/dinip/device-farm/` — are published by the
**Release** workflow, and the compose files pull them. `IMAGE_TAG` selects which:

| Tag | Published when | Use it for |
|---|---|---|
| `v1.2.3` | a release is published | production. This is the one to pin. |
| `latest` | the same, unless the release is a prerelease | tracking releases without editing `.env` |
| `<commit sha>` | every build | pinning to an exact commit, or rolling back |
| `pr-42` | a PR is labelled `build` | trying a branch on real hardware before it merges. Never reaches `latest`. |

```dotenv
IMAGE_TAG=v1.2.3      # pin a release
IMAGE_TAG=latest      # default: newest non-prerelease release
IMAGE_TAG=pr-42       # a branch build, for a test host
```

Prefer a version in production. `latest` moves on every release, so a
`docker compose pull` takes whatever was released last — usually fine, but it is
a decision made for you at pull time rather than at deploy time.

Nothing publishes on a merge to `main` any more. A commit reaches a deployment
only through a release, or through a PR build you asked for by label.

GHCR packages start private. While these are, a deploy host has to authenticate
before it can pull — a classic PAT with `read:packages` is enough, and it is the
only credential the images need:

```bash
echo "$GHCR_TOKEN" | docker login ghcr.io -u <github-user> --password-stdin
```

Making the three packages public (**Package settings → Change visibility**, once
each) removes that step; the repo can stay private either way.

To run an unpublished local change, build the Dockerfile over the same name —
compose prefers an image it already has:

```bash
docker build -f packages/coordinator/Dockerfile \
  -t ghcr.io/dinip/device-farm/coordinator:latest .
```

You need:

- Docker with the Compose plugin. Linux for any host with devices attached —
  Docker on macOS cannot pass USB through, and there is no workaround that
  belongs in production.
- A DNS name for the coordinator, and one per provider host, each resolving to
  that machine with ports 80 and 443 reachable. Caddy provisions certificates
  itself; port 80 must be open for the ACME challenge even though everything
  serves on 443.
- **TLS on every host.** `WebCodecs` refuses to decode outside a secure context,
  so a provider reached over plain HTTP produces a blank screen with no error in
  the console. This is not a hardening step to do later; nothing streams without
  it. See [ARCHITECTURE.md](./ARCHITECTURE.md#why-tls-is-not-cosmetic).

## Single machine

### 1. Configure

```bash
git clone <this repo> && cd device-farm
cp .env.example .env
```

`.env.example` is grouped by deploy unit and its header block lists the values
that appear in more than one of them. On a single host they all live in one file,
so this is just a list of what to set:

| Value | Set it to |
|---|---|
| `PUBLIC_URL` | `https://farm.example.com` — the coordinator's public origin. Also the OAuth redirect host. |
| `WEB_ORIGIN` | The same origin. Caddy serves the SPA and the API from one site, so they match. |
| `SITE_ADDRESS` | `farm.example.com` — the hostname alone, no scheme. This is what makes Caddy provision TLS. |
| `AUTH_SECRET` | `openssl rand -base64 32`. At least 32 characters; the coordinator refuses to start otherwise. |
| `POSTGRES_PASSWORD` | Something generated. The default `farm` is a development value. |
| `SESSION_TOKEN_PRIVATE_KEY` | `openssl genpkey -algorithm ed25519` — see below. |
| `ENABLE_EMAIL_PASSWORD` | `true` for now. You need it to create the first admin; turn it off afterwards. |

**Set `SESSION_TOKEN_PRIVATE_KEY`.** Left empty, the coordinator generates a
keypair in memory at boot, which is fine in development and wrong here: every
restart invalidates every live session token, and viewers stay broken until each
provider refetches the JWKS. It is a PEM, so it spans lines — quote it:

```bash
openssl genpkey -algorithm ed25519
```

```dotenv
SESSION_TOKEN_PRIVATE_KEY="-----BEGIN PRIVATE KEY-----
MC4CAQAwBQYDK2VwBCIEIE5X…
-----END PRIVATE KEY-----"
```

Everything in `.env` is validated at import by
`packages/coordinator/src/env.ts` — a bad config is a refusal to start, not a
failure on the first request.

### 2. Bring it up

```bash
docker compose up -d
```

Migrations are applied by the coordinator's entrypoint on every boot and are
idempotent, so there is no separate migration step and no ordering to get right.

Check it:

```bash
docker compose ps    # every service healthy
docker compose exec coordinator \
  bun -e "console.log(await (await fetch('http://127.0.0.1:3000/health')).text())"
```

The coordinator's `/health` is not routed publicly — Caddy proxies `/api/*` and
the JWKS, and everything else is the SPA — so from outside, `docker compose ps`
is the answer. That is deliberate: the healthcheck is for the orchestrator.

### 3. The first admin

There is no seeded account. Sign up through the UI at your `PUBLIC_URL` with
email and password — this is what `ENABLE_EMAIL_PASSWORD` is for — then promote
yourself:

```bash
docker compose exec coordinator bun /app/grant-admin.js you@example.com
```

Sign out and back in. Role changes are masked by a 60-second session cookie
cache, so the UI will not show admin pages until the cookie is reissued.

Once Microsoft sign-in works (see below), set `ENABLE_EMAIL_PASSWORD=false` and
`docker compose up -d` to apply it.

### 4. Register the provider

Providers authenticate with a token the coordinator issues, so the row must exist
before the provider starts:

1. **Providers → Add provider** in the nav rail (admins only). The `id` here is the `id`
   in `provider.yaml` — they must match.
2. Issue a token. It is shown once. Put it in `.env` as `FARM_PROVIDER_TOKEN`.
3. Edit `packages/provider/provider.yaml` (start from
   `packages/provider/provider.example.yaml`) with the devices attached to this
   host. On a single-machine deployment, `coordinator_url` is
   `http://coordinator:3000` — the compose network — and `public_base_url` is
   still the *browser's* route to the provider, so it must be a public HTTPS URL
   or `http://localhost:7100` if only local browsers use it.
4. Start it:

```bash
docker compose --profile provider up -d
```

The provider service is behind a profile because it needs host device access, so
`docker compose up` never starts it accidentally.

**On Linux, uncomment the USB mounts** in `docker-compose.yml` — the
`/var/run/usbmuxd` socket for iOS, and the whole `/dev/bus/usb` *directory* for
Android, plus `privileged: true`. Mount the directory, not individual `--device`
nodes: a phone that reboots re-enumerates under a new node and a static binding
silently loses it. The host must not run an adb server of its own; only one
process may own a device, and it needs to be the one in the container.

Devices appear in the UI within a few seconds of the provider registering.

## Multiple machines

The coordinator host is exactly the single-machine setup with step 4 left out —
no `--profile provider`, no `FARM_PROVIDER_TOKEN` in its `.env`. Everything below
is per provider host.

### 1. Register the provider first

Same as above: **Providers → Add provider** on the coordinator, one row
per host, and issue each its own token. A provider with no row cannot register,
and a provider whose `id` does not match its token's row is refused.

### 2. Configure the host

```bash
git clone <this repo> && cd device-farm
cp packages/provider/provider.example.yaml packages/provider/provider.yaml
```

```yaml
id: lab-1                                   # the row you just registered
name: Lab 1

# The coordinator's PUBLIC_URL. The provider derives its control-plane socket
# (wss://…/api/providers/connect) and the JWKS URL from this one value.
coordinator_url: https://farm.example.com

# What the BROWSER uses to reach this host. The coordinator never connects to
# it — it only hands the URL to the browser. Must be HTTPS.
public_base_url: https://lab-1.example.com

bind: 0.0.0.0:7100
scratch_dir: /var/lib/farm/scratch
max_upload_mb: 2048

devices:
  - udid: R5CT10ABCDE
    backend: android
    name: Pixel 8
```

Then an `.env` beside it with the three values this shape needs:

```dotenv
FARM_PROVIDER_TOKEN=pft_…                    # issued in step 1, shown once
PROVIDER_SITE_ADDRESS=lab-1.example.com      # hostname only; Caddy gets TLS for it
FARM_LOG=info
```

`PROVIDER_SITE_ADDRESS` and `public_base_url` are the same name written twice —
one for the terminator, one for what the coordinator advertises. They must agree.

### 3. Bring it up

```bash
docker compose -f docker-compose.provider.yml up -d
```

That file is the provider and its own Caddy, and nothing else. It builds
nothing, so a provider host does not need a clone of this repo — three files are
enough: `docker-compose.provider.yml`, your `provider.yaml`, and
`packages/provider/Caddyfile`. Put them where you like and point
`PROVIDER_CONFIG` and `PROVIDER_CADDYFILE` at them; the defaults are the repo's
own paths. Uncomment the same
USB mounts and `privileged` as in the single-machine case.

```bash
docker compose -f docker-compose.provider.yml logs -f provider
```

A successful start logs the JWKS fetch and the registration; the devices appear
in the coordinator's UI. `--check` validates a config without starting anything,
and is also the container's healthcheck:

```bash
docker compose -f docker-compose.provider.yml run --rm provider --check
```

### What has to agree across hosts

| This | must match | Symptom when it does not |
|---|---|---|
| provider `coordinator_url` | coordinator `PUBLIC_URL` | Provider never registers; it retries with backoff forever. |
| provider `public_base_url` | this host's `PROVIDER_SITE_ADDRESS` | Device page loads, video never starts — the browser is dialling a name that is not this host. |
| provider `id` | the registered row the token belongs to | Registration refused. |
| coordinator `WEB_ORIGIN` | the origin the SPA is served from | Uploads and screenshots fail CORS on every provider. See below. |
| `public_base_url` scheme | `https://` | Blank video, no error. WebCodecs will not run insecurely. |

**A provider's CORS allowlist is not in its config.** The coordinator sends it in
`hello.ack`, so it is whatever `WEB_ORIGIN` says, and it is *empty* until the
provider has registered at least once — a provider that cannot reach the
coordinator refuses browser requests rather than guessing. If uploads fail with a
CORS error, check the control plane before you check Caddy.

### Firewall

| Host | Inbound | Outbound |
|---|---|---|
| coordinator | 80, 443 from browsers | — |
| provider | 80, 443 from browsers; 9100 from Prometheus only | 443 to the coordinator |

The coordinator needs no route to a provider at all. Providers dial out, which is
why they work behind NAT and why nothing here requires a VPN between sites.

## TLS

Caddy handles it on both sides: give it a hostname in `SITE_ADDRESS` (coordinator)
or `PROVIDER_SITE_ADDRESS` (provider) and it provisions and renews certificates
from Let's Encrypt automatically. Certificates live in the `caddy-data` volume;
keep it across upgrades or you re-issue on every deploy and will hit rate limits.

On a private network with no public DNS, point Caddy at your own CA or supply
certificates directly — `tls /path/cert.pem /path/key.pem` in the relevant
Caddyfile, with the files mounted in. The requirement is a certificate the
*browser* trusts; nothing in the system cares who signed it.

## Metrics

A provider can export per-device CPU, memory and temperature for Prometheus. Add
to `provider.yaml`:

```yaml
metrics:
  enabled: true
  bind: 0.0.0.0:9100
  interval_secs: 30
  app_patterns:
    - "com.example.*"    # per-app CPU/memory, Android only
```

An absent `metrics:` block means off; writing the block and omitting `enabled`
means on, because writing it at all is the intent.

This is a separate listener from the session port on purpose, and it carries **no
authentication**. `docker-compose.provider.yml` publishes it on `127.0.0.1` by
default; set `METRICS_BIND_ADDRESS` to a private interface your monitoring can
reach, and never to `0.0.0.0` on an internet-facing host.

`docs/observability/prometheus.yml` is a working scrape config and
`docs/observability/grafana/dashboards/devices.json` a dashboard for what it
produces. Point the scrape targets at your provider hosts.

## Microsoft / Entra ID sign-in

Register an application in Entra ID with redirect URI
`<PUBLIC_URL>/api/auth/callback/microsoft`, then set `MICROSOFT_CLIENT_ID`,
`MICROSOFT_CLIENT_SECRET` and `MICROSOFT_TENANT_ID` in the coordinator's `.env`.
Social sign-in is configured only when both the ID and secret are present.

Once it works, set `ENABLE_EMAIL_PASSWORD=false`. Existing admin roles survive —
the flag gates the sign-in method, not the accounts.

## Operating it

### Upgrades

An upgrade is a new `IMAGE_TAG` and a pull — there is nothing to compile and the
repo does not need to be current on the host:

```bash
# coordinator host
docker compose pull && docker compose up -d

# each provider host
docker compose -f docker-compose.provider.yml pull
docker compose -f docker-compose.provider.yml up -d
```

On `IMAGE_TAG=latest` the `pull` is what moves you; on a pinned sha, edit `.env`
first and the `pull` fetches that tag. Rolling back is the same edit in reverse —
the old images are still in GHCR — with one caveat: migrations are forward-only,
so rolling the coordinator back across a schema change needs the database
restored to match. Providers carry no state and roll back freely.

Migrations run at coordinator boot and are idempotent. Upgrade the coordinator
first: a provider speaks the wire contract it was built against, and the
coordinator reconciles a provider's whole inventory on every `hello`, so a
version skew of one deploy is survivable in that direction.

Restarting a provider drops its devices to `absent` and releases their
reservations. Restarting the coordinator does **not** interrupt live streams —
providers hold the authorized reservation and verify tokens against a cached
JWKS — but the device list goes stale until they reconnect.

### Backups

Postgres is the only state in the system. There is no artifact storage anywhere:
uploads stream to a provider, get installed, and are deleted, leaving an
`auditLog` row as the only trace.

```bash
docker compose exec -T postgres pg_dump -U farm farm | gzip > farm-$(date +%F).sql.gz

# restore
gunzip -c farm-2026-01-01.sql.gz | docker compose exec -T postgres psql -U farm farm
```

The `caddy-data` volume is worth keeping too — it holds the certificates.

### Health and logs

Both the coordinator and the provider serve `/health`. The coordinator's is
internal only — Caddy does not route it, so reach it from inside the container.
The provider's is behind its terminator and answers publicly, which is what its
own healthcheck and any external monitor should use:

```bash
curl https://lab-1.example.com/health
```

Every service has a Docker healthcheck, so `docker compose ps` is the quick
answer. The web container's health is "the bundle is being served", not "the
process is up" — a Caddy that started with an empty `/srv` looks fine to a port
check and 404s everything.

```bash
docker compose logs -f coordinator
docker compose -f docker-compose.provider.yml logs -f provider
```

`FARM_LOG` takes an env-filter string, so `FARM_LOG=debug` or
`FARM_LOG=provider_core::control=debug,info` for one subsystem.

### Reservation policy

`RESERVATION_TTL` in the coordinator's `.env` only **seeds** the setting; it is
read when no row exists yet. After that, reservation lifetime and idle timeout
are edited under **Settings** in the UI, and changing the env var does nothing to a
deployment that already has a value. This is deliberate — an existing deployment
keeps what an admin configured.

## Troubleshooting

| Symptom | Cause |
|---|---|
| Device page loads, video never starts, console is clean | `public_base_url` is not HTTPS, or names a host the browser cannot resolve. WebCodecs fails silently outside a secure context. |
| Upload or screenshot fails with a CORS error | `WEB_ORIGIN` does not match the origin serving the SPA, **or** the provider has never registered — its origin list arrives in `hello.ack` and starts empty. |
| Devices show `absent` and never recover | The provider's control socket is down. Check outbound 443 from the provider host and `coordinator_url`. |
| Every live session breaks on a coordinator restart | `SESSION_TOKEN_PRIVATE_KEY` is unset, so a new keypair was generated at boot. |
| Provider logs a JWKS fetch failure | The coordinator's certificate is not trusted by the provider container, or `/.well-known/farm-jwks.json` is not proxied. Both are in the shipped Caddyfiles. |
| Reserving returns `CONFLICT` | Someone else holds it. Exclusivity is a partial unique index in Postgres, so this is the system working. A second reserve by the *same* user renews instead. |
| Provider starts, then exits immediately | Config error. Run it with `--check` for the specific message. |
| Android device present but never streams | An adb server on the host is holding the USB transport. Only one process may own a device. |

## Related documents

- [ARCHITECTURE.md](./ARCHITECTURE.md) — the four planes, and why the coordinator
  is off the data path
- [PROVIDER.md](./PROVIDER.md) — provider internals and backend requirements
- [COORDINATOR.md](./COORDINATOR.md) — the environment table with sharp edges
  called out
- [DEVELOPMENT.md](./DEVELOPMENT.md) — running it locally, with or without hardware
