# YARD

**Y**our **A**ccess (to) **R**eal **D**evices.

A device farm for real Android and iOS hardware — live video, touch and keyboard
input, app install, clipboard, screenshots, reservations, and adb remote
debugging, all from a browser. Plug phones into a machine anywhere, and they
show up as something a whole team can book and use over the network.

## How it fits together

It takes most of its inspiration from [STF](https://github.com/DeviceFarmer/stf),
which proved the idea. The iOS device layer comes from
[stf-ios-provider](https://github.com/Dinip/stf-ios-provider), an earlier
attempt at bolting iOS support onto STF, which is what this project grew out of.

The system is two pieces. The **coordinator** owns identity, inventory,
reservations and policy; the **providers** own devices and all high-bandwidth
traffic.

```
  browser ---- tRPC ----> coordinator <---- WSS ---- provider ---- USB ---> devices
     \_________________ WSS + HTTPS _________________/
```

- A **provider** is a small Rust daemon on whatever machine the phones are
  plugged into. It talks to iOS over CoreDevice and to Android over adb, and
  dials *out* to the coordinator, so it needs no inbound reachability and works
  fine behind NAT.
- The **coordinator** is the thing everyone logs into. It holds users, the
  device list, reservations and the audit log in Postgres, and signs the
  short-lived tokens that let a browser open a session.
- Video and input go **browser to provider directly**, over a WebSocket the
  provider serves and authorises itself against the coordinator's public keys.
  The coordinator is never on the data path.
- Uploads take that same direct path and are never stored: an APK or IPA
  streams to the provider, gets installed, and is deleted. The only trace is an
  audit-log row.

[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) has the detail.

## Status

Verified against real hardware: an iPhone 13 on iOS 17.4+ and a Galaxy S22 on
Android both reserve, stream to a browser, and take touch and keyboard input —
through the containerised stack, with the coordinator nowhere near the video.

It also runs with **no hardware at all**: the Rust provider ships a mock backend
that registers, reserves, streams synthetic video and accepts input and
uploads, and `packages/protocol/test/fake-provider.ts` does the same one layer
up.

## Quick start

```bash
cp .env.example .env      # set AUTH_SECRET (openssl rand -base64 32)
bun install
docker compose -f docker-compose.dev.yml up -d
bun run db:migrate
bun run dev
```

Then http://localhost:5173. Full instructions, including the first-admin
bootstrap, are in [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md).

## Layout

```
packages/
├── db/             drizzle schema, migrations, client
├── coordinator/    Hono + tRPC + better-auth + provider gateway
├── web/            TanStack Router SPA + shadcn
├── protocol/       zod schemas + Rust codegen + fake provider
└── provider/       cargo workspace: yard-protocol, provider-core,
                     backend-mock, yard-provider (ios, android)
```

## Documentation

| | |
|---|---|
| [ARCHITECTURE.md](docs/ARCHITECTURE.md) | The four planes and why they're separate |
| [DATA-MODEL.md](docs/DATA-MODEL.md) | Tables and their invariants |
| [COORDINATOR.md](docs/COORDINATOR.md) | Backend package |
| [WEB.md](docs/WEB.md) | Frontend package |
| [PROVIDER.md](docs/PROVIDER.md) | Rust provider design |
| [PROTOCOL.md](docs/PROTOCOL.md) | Wire contract |
| [CLEANUP.md](docs/CLEANUP.md) | Resetting a device between users |
| [DEVELOPMENT.md](docs/DEVELOPMENT.md) | Setup, commands, conventions |

`docs/REFERENCES.local.md` covers the STF sources kept on disk outside this
repo, and what to read in them. It's local-only and not committed.

## Stack

Bun · Hono · tRPC v11 · better-auth · Drizzle + Postgres · TanStack Router +
Query · shadcn/ui + Tailwind v4 · Rust (tokio) for providers · Caddy · Docker.
