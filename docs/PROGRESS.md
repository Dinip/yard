# Progress

The resumable state of the build. **Update this file in the same commit as the
work it describes** — it is the first thing to read when picking the project back
up after a break.

Legend: ✅ done · 🚧 in progress · ⬜ not started

---

## Phase 1 — Foundation ✅

Monorepo scaffolding, data model, authentication, the app plane, and a
single-command local stack.

*Done when: sign in, see an empty device list, admin can list users.* — **met**

| Item | State | Where |
|---|---|---|
| bun workspaces, biome, tsconfig, bunfig | ✅ | root |
| Drizzle schema (auth + farm tables) | ✅ | `packages/db/src/schema/` |
| Initial migration incl. partial unique index | ✅ | `packages/db/drizzle/0000_*.sql` |
| better-auth: Microsoft social + admin plugin + email/password bootstrap | ✅ | `packages/coordinator/src/auth.ts` |
| t3-env boot validation | ✅ | `packages/coordinator/src/env.ts` |
| tRPC v11 skeleton: `user`, `device`, `provider`, `admin` | ✅ | `packages/coordinator/src/trpc/` |
| Reservation reserve/renew/release with DB-enforced exclusivity | ✅ | `.../routers/device.ts` |
| Reservation exclusivity tests (concurrent race) | ✅ | `packages/coordinator/test/` |
| TanStack Router SPA + shadcn shell, devices/providers/admin routes | ✅ | `packages/web/` |
| Dockerfiles (coordinator, web), compose with postgres + caddy | ✅ | root, `packages/*/Dockerfile` |
| First-admin bootstrap CLI | ✅ | `.../src/cli/grant-admin.ts` |

**Deferred out of phase 1 deliberately:** live device-list updates (still polling
at 5s until the `stream` subscription lands with the gateway), provider token
issuance UI, session-token signing/JWKS.

---

## Phase 2 — Protocol + gateway ✅

The wire contract, the provider control plane, and everything needed to run the
whole system against synthetic devices.

*Done when: the fake provider registers synthetic devices and the UI shows them
appear and disappear live.* — **met**

| Item | State | Where |
|---|---|---|
| zod schemas for control, session and artifact planes | ✅ | `packages/protocol/src/` |
| `bun run protocol:gen` → `crates/farm-protocol/src/generated.rs` | ✅ | `packages/protocol/scripts/` |
| Cross-language fixture round-trip (zod ↔ serde) | ✅ | `test/fixtures.ts`, `crates/farm-protocol/tests/` |
| Provider CRUD + token issuance/revocation | ✅ | `.../routers/provider.ts` |
| `/api/providers/connect` gateway + control-plane state machine | ✅ | `.../gateway/` |
| Inventory reconciliation; devices → `absent` on drop | ✅ | `.../gateway/handler.ts` |
| Command correlation with per-command timeouts | ✅ | `.../gateway/registry.ts` |
| Ed25519 session tokens + `/.well-known/farm-jwks.json` | ✅ | `.../lib/session-token.ts` |
| `stream.devices` SSE subscription + live device list | ✅ | `.../routers/stream.ts`, `web/src/hooks/` |
| Device commands (apps, launch, uninstall, reboot, rotate, adb) | ✅ | `.../routers/device.ts` |
| Admin providers + tokens UI | ✅ | `web/src/routes/_app/admin.providers.tsx` |
| TS fake provider (library + CLI) | ✅ | `packages/protocol/test/fake-provider.ts` |
| Gateway integration tests over a real socket | ✅ | `packages/coordinator/test/gateway.test.ts` |

**Verified end-to-end:** four synthetic devices register and appear live; reserve
pushes `session.authorize`; release pushes `session.revoke`; `device.apps` and
`device.adbExpose` round-trip real data back from the provider; a session token
carries the right claims and a `kid` matching the published JWKS; killing the
provider flips its devices to `absent`, releases its reservations, and makes
further reserves fail `PRECONDITION_FAILED`.

**Deferred deliberately:** the reservation reaper (phase 6) — reservations have
an `expiresAt` and the client renews, but nothing sweeps lapsed ones yet.

---

## Phase 3 — Provider core + iOS ⬜

| Item | State |
|---|---|
| Cargo workspace under `packages/provider` | ⬜ |
| Port `device/`, `control.rs`, `screen_ws.rs`, `health.rs` from `stf-ios-provider` | ⬜ |
| Delete `bus.rs`, `wire.rs`, `group.rs`, `provider.sh`, zeromq + prost deps | ⬜ |
| `provider-core`: control client, session server (JWT), device actor, supervisor | ⬜ |
| One YAML config, multi-device in one process | ⬜ |

*Done when: an iPhone appears, reserves, streams and takes touch input end-to-end.*

---

## Phase 4 — Android backend ⬜

| Item | State |
|---|---|
| adb host-protocol client (`host:track-devices`, `sync:`, `shell:`) | ⬜ |
| Pinned `scrcpy-server` vendored; video socket + `avcC` extraction | ⬜ |
| scrcpy control socket: touch, keycodes, text, clipboard, rotation | ⬜ |
| Apps: `pm install/uninstall/list`, `am start` | ⬜ |
| `screencap -p` screenshot + MJPEG fallback source | ⬜ |
| adb remote-debug transport proxy on a per-device port | ⬜ |

> Highest-uncertainty phase. Validate the adb client and scrcpy handshake against
> a real device *before* building anything on top of them.

---

## Phase 5 — Web control surface ⬜

| Item | State |
|---|---|
| Codec-agnostic `VideoDecoder` canvas renderer | ⬜ |
| Input capture (pointer → tap/swipe), keyboard, text | ⬜ |
| Clipboard get/set, screenshot download, rotate | ⬜ |
| Drag-and-drop install with upload + install progress | ⬜ |
| `/devices/:id/popout` chrome-free window | ⬜ |

---

## Phase 6 — Operations ⬜

| Item | State |
|---|---|
| Reservation reaper + client-side renewal | ⬜ |
| Admin force-release UI (procedure exists, UI does not) | ⬜ |
| Audit log UI | ⬜ |
| MJPEG fallback path | ⬜ |
| Healthchecks, multi-arch CI images | ⬜ |
| Docs finalised | ⬜ |

---

## Open decisions

- **Name.** The project is "Device Farm" as a placeholder. See
  [RENAMING.md](./RENAMING.md) for the exact rename procedure — everything
  user-visible already routes through the `APP_NAME` env var.
- **Entra ID app registration.** Not yet created. Until `MICROSOFT_CLIENT_ID`
  and `MICROSOFT_CLIENT_SECRET` are set, sign-in falls back to email/password,
  which `ENABLE_EMAIL_PASSWORD` gates. Turn it off once Microsoft works.
- **Single coordinator instance.** The provider registry and the device-event
  broadcast are in-memory, so running two coordinators would leave each blind to
  the other's providers. Providers dial out, so this is not a transparent
  scale-out — it needs Postgres `LISTEN`/`NOTIFY` or a bus first. Not a problem
  at the scale this replaces STF at, but it is a real ceiling.
- **`SESSION_TOKEN_PRIVATE_KEY` must be set in production.** Unset, a keypair is
  generated per boot, so every restart invalidates live sessions until providers
  refetch the JWKS. The coordinator warns loudly at startup.
