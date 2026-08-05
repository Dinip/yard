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

## Phase 3a — provider-core ✅

Everything a provider does that is not device-specific. Verifiable with no
hardware, which is why it was split from the iOS port.

*Done when: a provider registers with the coordinator, appears in the UI,
reserves, and serves a session-plane connection with a valid token.* — **met**

| Item | State | Where |
|---|---|---|
| Cargo workspace + `farm-provider` binary | ✅ | `packages/provider/crates/` |
| One YAML config, multi-device, token precedence | ✅ | `provider-core/src/config.rs` |
| `DeviceBackend` trait — the seam for iOS/Android | ✅ | `provider-core/src/backend.rs` |
| Codec-agnostic access-unit fan-out | ✅ | `provider-core/src/video.rs` |
| Control-plane client, reconnect with backoff | ✅ | `provider-core/src/control.rs` |
| Session registry (replaces `group.rs`) | ✅ | `provider-core/src/session.rs` |
| JWKS fetch + session-token verification | ✅ | `provider-core/src/auth.rs` |
| Session plane (WS) + artifact plane (HTTP upload/screenshot) | ✅ | `provider-core/src/server.rs` |
| Device supervision + command routing | ✅ | `provider-core/src/supervisor.rs` |
| Mock backend — the whole provider with no hardware | ✅ | `backend-mock/` |
| Dockerfile, compose wiring | ✅ | `packages/provider/Dockerfile` |
| 49 Rust tests incl. in-process session-plane suite | ✅ | `provider-core/tests/` |

**Verified end-to-end** against the real coordinator: the Rust provider
registers and reconciles inventory (stale TS fake-provider devices correctly
went `absent`); reserve → session token → WS handshake delivers `hev1.1.6.L93.B0`
+ display geometry, then key and delta frames; input, clipboard round-trip,
screenshot and a 3 MB upload all work; the staged file is deleted; the install
lands in the coordinator's audit log with its sha256; a token for the wrong
device gets 403, garbage gets 401, and after release the same valid token gets
403 while the live socket receives `session.closed`.

**Failure drill:** killing the coordinator left the provider running and the
session plane serving 200s; it backed off 1s → 2s → 4s and re-registered
automatically when the coordinator returned.

**Two real bugs the tests caught**, both now regression-tested:
- `jsonwebtoken`'s default 60s leeway silently doubled the lifetime of a ~60s
  session token. Leeway is now 5s.
- `.optional()` vs `.nullable()` in the Rust emitter (see phase 2a).

---

## Phase 3b — iOS backend ✅

*Done when: an iPhone appears, reserves, streams and takes touch input
end-to-end.* — **met**

| Item | State | Where |
|---|---|---|
| `device/mod.rs` → tunnel + session supervision | ✅ | `backend-ios/src/device.rs` |
| `device/media.rs` → RTP in, RTCP out, access units | ✅ | `backend-ios/src/media.rs` |
| `device/hevc.rs` → depacketisation, hvcC, codec string | ✅ | `backend-ios/src/hevc.rs` |
| `device/hid.rs` → touch, keyboard, buttons, rotation | ✅ | `backend-ios/src/hid.rs` |
| `control.rs` → the pointer state machine + service ops | ✅ | `backend-ios/src/lib.rs` |
| `DeviceBackend` implemented over them | ✅ | `backend-ios/src/lib.rs` |
| Frame geometry from the SPS | ✅ | `hevc.rs::dimensions_from_sps` |

`media.rs` is carried over verbatim — every constant and branch in it marks a
field failure, and the comments say which. The only structural change is that
the fan-out now belongs to `provider-core::video`, which had already
generalised this file's `MediaHandle` so Android can share it.

**Verified end-to-end on an iPhone 13, iOS 27.0:** the tunnel comes up, HID
surfaces connect, audio and video streams start under one `clientSessionID`,
and the SPS parses to `hev1.1.6.L150.B0`. In the browser the device reserves and
**paints a live frame** — the first real frame the project has rendered — and a
mouse drag on the canvas scrolls the app on the physical phone. That is the
whole path: device → RSD tunnel → RTP → depacketiser → hvcC → provider fan-out →
WebSocket → `VideoDecoder` → canvas, and back the other way for input.

**One thing the reference implementation did not have to handle:** `mobilegestalt`
answers `MobileGestaltDeprecated` on iOS 26/27 and returns no screen dimensions
at all, so the display geometry it used is simply unavailable. Geometry now
comes from the stream's own SPS, which is better anyway — it reports what is
actually encoded, which is what a viewer sees. The lockdown path stays as a
fallback for older devices, since it is the only one that knows the scale.

**Deliberately not ported:** STF's app-shortcut buttons (`settings`, `store`,
`camera` → `appActivate`) and `open_url`, which existed for STF's toolbar and
have no equivalent in this protocol; `swipe`/`tap` helpers, since the session
plane always streams down/move/up. `zeromq`, `prost` and `protox` never entered
this workspace at all — they were STF's transport, and the control plane
replaced them in phase 2.

**Known gaps:** battery level and state are not reported (they need a
diagnostics round-trip per poll and nothing consumes them yet), and `sdk` is
null because iOS has no API-level equivalent worth inventing.

---

## Phase 4 — Android backend ✅

*Done when: a phone appears, reserves, streams and takes touch input
end-to-end.* — **met**

| Item | State | Where |
|---|---|---|
| adb host-protocol client (transport, `shell:`, `sync:`, forward) | ✅ | `backend-android/src/adb.rs` |
| Pinned `scrcpy-server` embedded; handshake + video socket | ✅ | `backend-android/src/scrcpy.rs` |
| Annex-B → `avcC` rewrite | ✅ | `backend-android/src/h264.rs` |
| Control socket: touch, keycodes, text, clipboard, rotation | ✅ | `backend-android/src/scrcpy.rs` |
| Apps: `pm list/install/uninstall`, `monkey` launch | ✅ | `backend-android/src/lib.rs` |
| `screencap -p` screenshot | ✅ | `backend-android/src/lib.rs` |
| adb remote-debug transport proxy on a per-device port | ✅ | `backend-android/src/lib.rs` |

The advice to validate before building paid for itself. The handshake was read
off a real device by `examples/scrcpy_probe.rs` rather than inferred from
scrcpy's source, and the first version of that probe *misparsed it* by calling
`read()` once — a socket is free to hand over one byte at a time. Building the
video reader on that reading would have produced a decoder fed with garbage and
no obvious cause.

What the probe found, on v4.1: a dummy byte, a 64-byte device name, the codec
id `"h264"`, then flags/width/height, then `pts+flags`/`size` framed packets
carrying **Annex-B**. The browser needs `avcC` and length prefixes — the
Annex-B path is what tears in Chrome under motion, the same finding that drove
the iOS side to hvcC — so the config packet's SPS/PPS become an
`AVCDecoderConfigurationRecord` sent once out of band, and every frame is
rewritten. `h264.rs` is the H.264 mirror of `backend-ios/src/hevc.rs`; they
share no code because the bitstreams differ exactly where it matters.

**Verified end-to-end on a Galaxy S25+, Android 15 (SDK 35):** the server is
pushed and started, both sockets connect, the codec announces as
`avc1.640020`, and the browser **paints the live home screen**. A drag on the
canvas pages between home screens on the physical phone, and a clipboard read
round-trips. Battery, SDK, ABI and geometry all reach the device list.

**Two host dependencies became one.** The scrcpy server is embedded in the
binary with `include_bytes!` and pushed to the phone at session start, so a
provider host installs nothing for it. Only the adb server remains, because it
owns the USB transport — and its address is config (`adb_server`), so a Linux
host bundles adb in the provider image with the USB bus passed through, while
macOS, where Docker cannot pass USB through at all, points at the host's server.

**Two bugs found by leaving it running**, both fixed and both invisible to a
short test:

- **The stream desynchronised on every viewer connect.** A viewer connecting
  makes provider-core request a keyframe, which maps to scrcpy's `RESET_VIDEO`,
  and a reset makes the server re-send the *bare* 12-byte session block rather
  than a framed packet. Read as a packet header it looks entirely valid — its
  "size" is the height — so it swallowed 1024 bytes of the next packet and the
  stream never recovered. The symptom was a black screen; the error only came
  later, when a garbage length asked for a 3.9 GB allocation. The first four
  bytes disambiguate (bit 31 = bit 63 of `pts_and_flags`, which no timestamp
  reaches), and there are two regression tests, one of which serves the bytes
  a byte at a time.
- **The device dozed.** A farm phone left alone hits its screen timeout, and
  scrcpy then faithfully streams a black display — `adb screencap` showed the
  same black, which is what proved it was not a decode bug. `stay_awake=true`
  and `power_on=true` are now passed; scrcpy restores both on cleanup, so the
  device is left as it was found.

**Since exercised on hardware** (`examples/device_ops.rs`), and it was worth it:
app listing and launch work, a bad APK is refused with the device's own error
and leaves nothing staged, and remote debugging now genuinely works — but the
first implementation of it did not.

`setprop service.adb.tcp.port 5555` plus an adbd restart is the recipe that
circulates for adb-over-network. **It needs root, and without it fails
silently**: the property does not stick, the daemon never listens, and the
forward points at nothing. The device reported an empty port and the test
passed anyway because nothing checked the end state. It now uses the `tcpip:`
transport service — what `adb tcpip` itself uses, no root needed — and the
probe asserts the forwarded port actually accepts a connection.

Two more found in the same pass:

- **Stopping remote debug left the phone listening on the network.** Removing
  the host-side forward does nothing to `adbd`; the device stayed reachable by
  anyone who could route to it, indefinitely. Stop now issues `usb:` as well,
  and does it even when no forward was recorded — a provider restart forgets
  the port while the device keeps listening.
- **Waiting for the transport to reappear is a race.** `adbd` has not dropped
  yet when the request returns, so a presence check passes against the
  connection that is about to die. It now polls for the *end state* — the port
  the device reports — which is also the only thing that proves the restart
  landed. "Not listening" is spelled `0` on a Galaxy S25+, `-1` in the docs and
  `""` on a fresh device, so that is normalised rather than string-matched.

**Still not exercised:** a real APK install (the failure path is covered; the
success path needs an APK, and installing onto a personal device was not
something to do unasked).

**Worth knowing for a real deployment:** a woken device shows its lock screen.
A managed farm device should have its screen lock disabled, or every session
starts with a lock the user cannot get past.

---

## Phase 5 — Web control surface ✅

The browser half of the session plane. Everything here talks to the provider's
own origin; nothing new touches the coordinator except `device.sessionToken`.

| Item | State | Where |
|---|---|---|
| Codec-agnostic `VideoDecoder` canvas renderer | ✅ | `web/src/lib/screen/renderer.ts` |
| Session-plane client (WS + artifact plane, backoff reconnect) | ✅ | `web/src/lib/screen/session.ts` |
| Input capture (pointer down/move/up), keyboard, text, paste | ✅ | `web/src/components/device-screen.tsx` |
| Clipboard get/set, screenshot download, rotate | ✅ | `web/src/components/device-console.tsx` |
| Drag-and-drop install with upload progress | ✅ | `.../device-console.tsx` |
| `/devices/:id/popout` chrome-free window | ✅ | `web/src/routes/_session/` |
| Renderer state-machine tests (stub `VideoDecoder`) | ✅ | `packages/web/test/renderer.test.ts` |
| Provider CORS for the artifact plane, origins via `hello.ack` | ✅ | `provider-core/src/origins.rs` |
| Paint a real frame | ✅ | verified on an iPhone 13, iOS 27 — see phase 3b |

**Verified in a browser** against the coordinator and the Rust provider with
mock devices: reserve opens a session and the codec handshake sizes the canvas
box; pointer down/move/up arrive at the backend as separate events with exact
normalised coordinates (0.5/0.25 …); `Enter` arrives as key down+up and a
printable character as `text`; rotate reaches the device and the new geometry
comes back and re-shapes the canvas live; `clipboard.get` round-trips; a 3 MB
drag-and-drop install uploads with progress, installs, writes an audit row with
its sha256, and leaves nothing in the scratch dir; the popout joins the same
reservation as a second concurrent viewer.

**One real bug this found, in the provider rather than the web app:** the
artifact plane had no CORS at all, so uploads and screenshots were unreachable
from a browser in *every* deployment while looking healthy to `curl`. The
allowed origins now ride in `hello.ack`; two regression tests cover the grant
and the refusal.

**Still unverified:** the screenshot download (a file download, left for the
user to trigger) and a genuine mouse drag-and-drop — a synthetic `DragEvent`
does not reach React's delegated handler, so the handler was driven directly
instead.

The renderer is the `stf-ios-provider` HEVC one generalised: codec and
parameter sets come from the `{type:"codec"}` handshake, so `hev1.*`+hvcC and
`avc1.*`+avcC both work without the browser knowing which backend it has. The
iOS motion-collapse compensation is kept but defaults on for HEVC only — it is
an iOS encoder behaviour and its per-frame readback is waste elsewhere.

**On the mock backend** the video is deliberately undecodable filler, so the
browser reports the stream as undecodable and shows the fallback message —
which is the right behaviour to have seen. The decode state machine itself —
keyframe gating, the type-2 rebuild, error resync — is covered by unit tests
against a stub `VideoDecoder`. Real HEVC from an iPhone paints correctly; see
phase 3b.

**Deferred to phase 6:** the MJPEG fallback. The renderer already reports
`unsupported` when `VideoDecoder.isConfigSupported` fails or the origin is not
secure, and the UI says so plainly — that is the hook the fallback plugs into.

---

## Phase 6 — Operations ✅

| Item | State | Where |
|---|---|---|
| Reservation reaper + client-side renewal | ✅ | `coordinator/src/lib/reservations.ts`, `web/src/hooks/` |
| Admin force-release UI | ✅ | `web/src/routes/_app/devices.$deviceId.tsx` |
| Audit log UI | ✅ | `web/src/routes/_app/admin.audit.tsx` |
| Degraded fallback stream | ✅ | `provider-core/src/server.rs`, `web/src/components/device-screen.tsx` |
| Healthchecks on every service | ✅ | `docker-compose.yml`, `packages/*/Dockerfile` |
| CI: lint, typecheck, tests, drift guard, multi-arch images | ✅ | `.github/workflows/ci.yml` |
| Docs finalised | ✅ | `docs/` |

Releasing a reservation now lives in one place. There were four ways a device
came free — the holder releases it, an admin takes it back, the reaper sweeps a
lapsed one, a provider disconnects — and they had drifted. The one that
mattered: **force-release never pushed `session.revoke`**, so a device an admin
had "taken back" carried on streaming to the person it was taken from. It does
now, which is also why the UI asks for a reason: the holder's session ends the
moment it lands, and that reason is what they and the audit log get.

The reaper sweeps every 30s; the browser renews every third of the reservation's
lifetime, derived from the actual `expiresAt` rather than assuming the server's
TTL so the two cannot drift. The popout deliberately does *not* renew — it
shares the parent tab's reservation, and a popout left open should not keep a
device nobody is watching.

Also fixed: the app shell hardcoded "Device Farm" in its header, which
[RENAMING.md](./RENAMING.md) exists to prevent. It reads `APP_NAME` through
`user.capabilities` now, like the sign-in page always did.

**The fallback stream** is `multipart/x-mixed-replace` at ~3fps, served from the
same screenshot primitive both backends already have, for browsers with no
usable hardware decoder or no secure context. Its parts are **PNG, not JPEG**,
despite the `/mjpeg` path the protocol documents: both backends capture PNG and
converting would mean an image codec and a transcode in the provider, which is
the one thing this project does nowhere. The provider re-checks the reservation
every frame, because this request outlives its token by design. The UI labels it
rather than letting a jerky picture look like a slow farm, and `?fallback=1`
forces it — an untestable fallback rots until the day it is needed.

---

## Phase 7 — Rotation, end to end ✅

The one real bug in the post-launch set: the device rotated and the picture did
not. `ServerMessage::display` and `AuKind::KeyWithReset` were both designed for
this case and **neither was ever emitted**, so the browser kept decoding
new-geometry frames against a stale `avcC`/`hvcC`. It recovered only by
accident — decoder-error resync, and the iOS crop detector.

| Item | State | Where |
|---|---|---|
| Geometry watch channel alongside the codec one | ✅ | `provider-core/src/video.rs` |
| `codec_watch()` — observe changes, not just the first | ✅ | `provider-core/src/video.rs` |
| Session arms: re-announce codec + reset; push display | ✅ | `provider-core/src/server.rs` |
| Android publishes geometry, with real rotation | ✅ | `backend-android/src/{lib,scrcpy}.rs` |
| iOS publishes geometry from the SPS (rotation `None`) | ✅ | `backend-ios/src/media.rs` |
| Mock publishes it on rotate — the whole path, no hardware | ✅ | `backend-mock/src/lib.rs` |
| `ScreenRenderer.reconfigure` — swap config in place | ✅ | `web/src/lib/screen/renderer.ts` |
| `case "codec"` reconfigures rather than rebuilding | ✅ | `web/src/hooks/use-device-session.ts` |

**Two latent bugs fixed here, both invisible until rotation was reported:**

- **`backend-android::rotate` took an absolute angle and walked it as relative
  steps.** It worked only because `display.rotation` was always `None`, so the
  UI always sent `90` → exactly one step. The moment rotation is reported,
  rotating from 270 asks for `0` → zero steps → nothing happens. It now walks
  `((target - current) / 90).rem_euclid(4)` against a `dumpsys window displays`
  read, with a regression test. **iOS is deliberately left absolute** and
  documented: it reports no rotation at all — the SPS gives dimensions, not
  orientation — so the browser always asks for 90 and always means one step.
- **A keyframe reset re-published unchanged geometry**, which would have made
  every reset look like a rotation to a viewer. `set_geometry` and Android's
  `Geometry` both use `send_if_modified`, so an unchanged value wakes nobody.

Rotation costs a shell round-trip on Android, so announcing it lives in its own
task rather than inside `pump_video` — the video loop must never block on the
device to read the next packet.

`reconfigure` exists rather than rebuilding the renderer because a rebuild drops
the canvas and re-runs `isStreamSupported` for a codec that is by definition
still supported: the picture would blank and the loader would flash back over a
frame that is already painted.

**Tests:** a mid-session `set_codec` produces a second `codec` message *and* an
`AU_KEY_RESET` frame; `rotate(90)` on the mock swaps 1179×2556 → 2556×1179 on a
live socket; the renderer holds deltas after `reconfigure` until the reset
lands. **Not yet exercised on real hardware** — the mock path is verified, the
Android and iOS confirmation is outstanding.

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
