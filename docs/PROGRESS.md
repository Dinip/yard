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

## Phase 2 — Protocol + gateway ⬜

| Item | State |
|---|---|
| `packages/protocol` zod schemas for every control/session message | ⬜ |
| `bun run protocol:gen` → `crates/farm-protocol/src/generated.rs` | ⬜ |
| Provider token issuance + revocation (tRPC + admin UI) | ⬜ |
| `/api/providers/connect` gateway WS + control-plane state machine | ⬜ |
| Device inventory reconciliation on `hello`; devices → `absent` on drop | ⬜ |
| Ed25519 session-token signer + `/.well-known/farm-jwks.json` | ⬜ |
| `stream` subscription for live device-list updates | ⬜ |
| TS fake provider harness (no hardware needed) | ⬜ |

*Done when: the fake provider registers synthetic devices and the UI shows them
appear and disappear live.*

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
