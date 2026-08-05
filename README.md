# Device Farm

> The name is a placeholder — see [docs/RENAMING.md](docs/RENAMING.md).

A modular device farm for remotely controlling iOS and Android devices from a
browser: live video, touch and keyboard input, app install, clipboard,
screenshots, reservations, and adb remote debugging.

It replaces [STF](https://github.com/DeviceFarmer/stf), whose architecture
proxied every device's video stream through the app server. Here **the
coordinator owns identity, inventory, reservations and policy; providers own
devices and all high-bandwidth traffic.** Video and input go browser↔provider
directly. The coordinator is never on the data path.

## Status

All six phases are complete and verified against real hardware: an iPhone 13 on
iOS 27 and a Galaxy S22 on Android 16 both reserve, stream to a browser, and
take touch and keyboard input — through the containerised stack, with the
coordinator nowhere near the video.

It also runs with **no hardware at all**: the Rust provider ships a mock backend
that registers, reserves, streams synthetic video and accepts input and uploads,
and `packages/protocol/test/fake-provider.ts` does the same one layer up.

**[docs/PROGRESS.md](docs/PROGRESS.md) is the live status board** — read it
first. It records what each phase cost, including the bugs that only appeared
once the thing was run the way it deploys.

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
├── protocol/       zod schemas + Rust codegen
└── provider/       cargo workspace: iOS + Android
```

## Documentation

| | |
|---|---|
| [PROGRESS.md](docs/PROGRESS.md) | What's built, what's next, open decisions |
| [ARCHITECTURE.md](docs/ARCHITECTURE.md) | The four planes and why they're separate |
| [DATA-MODEL.md](docs/DATA-MODEL.md) | Tables and their invariants |
| [COORDINATOR.md](docs/COORDINATOR.md) | Backend package |
| [WEB.md](docs/WEB.md) | Frontend package |
| [PROVIDER.md](docs/PROVIDER.md) | Rust provider design |
| [PROTOCOL.md](docs/PROTOCOL.md) | Wire contract |
| [REFERENCES.md](docs/REFERENCES.md) | The STF sources kept on disk, and what to read in them |
| [DEVELOPMENT.md](docs/DEVELOPMENT.md) | Setup, commands, conventions |
| [RENAMING.md](docs/RENAMING.md) | How to rename the project |

## Stack

Bun · Hono · tRPC v11 · better-auth · Drizzle + Postgres · TanStack Router +
Query · shadcn/ui + Tailwind v4 · Rust (tokio) for providers · Caddy · Docker.
