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
| First-admin bootstrap (first sign-up + CLI) | ✅ | `.../src/auth.ts`, `.../src/cli/grant-admin.ts` |

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

**Bundling adb is not the same as running it.** The first Linux deployment hit
`Connection refused (os error 111)` on `127.0.0.1:5037` in the scrcpy retry
loop: the image had the binary, the entrypoint was the provider binary, and the
backend never shells out, so no server ever existed. `docker-entrypoint.sh` now
runs `adb start-server` first, and the runtime stage runs as root — `privileged`
grants access to the `/dev/bus/usb` nodes, not permission on them, and the adb
server writes to them.

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
| CI: lint, typecheck, tests, drift guard, image build | ✅ | `.github/workflows/ci.yml` |
| Publishing: `edge` from main | ✅ | `.github/workflows/publish.yml`, `docker.yml` |
| Releases: release-please PR → tag → versioned images | ✅ | `.github/workflows/release.yml`, `release-please-config.json` |
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

Fronting a provider at `/p` needs `uri strip_prefix /p` — the provider serves
`/s/:deviceId` at the root, so an unstripped prefix 404s and the device page
sits on "Session closed". Caddy's healthcheck probes the admin API now; the old
one probed `127.0.0.1`, which matches no site once `SITE_ADDRESS` is a hostname.

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
lands; the pointer mapping round-trips through every quarter turn.

**Verified on a Galaxy S22**: the device rotates and the picture follows.

### iOS rotates the UI, not the stream

Android was the easy half. **iOS never changes its capture geometry at all**:
CoreDevice hands over the native portrait buffer whatever the phone is doing and
draws the rotated UI *inside* it, so the frames stay 9:16 with the content
sideways. Measured, not assumed — a rotation produced no new SPS, and a brand
new capture session started while the phone was already rotated published
`1184×2576` again. No amount of geometry plumbing can fix that in the provider,
and rotating pixels there would mean a transcode, which this project does
nowhere.

So the viewer does it, which is what `stf-ios-provider` did too — its
integration notes set `screen.rotation = device.display.rotation` on the element.

- **`Display.renderRotation`** is new on the wire: *how far the viewer must turn
  the decoded picture*, which is a different question from `rotation`, the
  device's own orientation. Android sets it to 0 because its encoder already did
  the work; iOS sets it to the orientation. Inferring it by comparing the
  reported rotation against the frame's aspect would be guessing at which
  backend is on the other end.
- **iOS reports orientation for the first time.** There is no orientation query
  in CoreDevice — but the rotate *answers* with the state it landed in, and that
  reply was being thrown away (`format!("{:?}")` of a struct, discarded by the
  caller). `hid.rs` now maps the typed enum to degrees, and takes
  `non_flat_orientation`: a phone lying on a desk answers `faceUp`, which is the
  farm's normal resting state and says nothing about how the UI is drawn.
- **So iOS's `rotate` becomes a delta too**, exactly like Android's. Until the
  first rotation the orientation is genuinely unknown, and an unknown one is
  walked as a single step rather than against a zero we invented.
- **The renderer turns the picture** in `drawFrame` rather than via a CSS
  transform, so the canvas stays the shape of what is on it and the box sizing
  follows from one number. **The pointer travels back the other way**
  (`lib/screen/rotation.ts`): the canvas is viewer space, the HID surface is
  device space, and a wrong sign there lands taps on the mirror image of where
  they were aimed.

**Still to confirm on the iPhone**: which way `landscapeLeft` reads. The mapping
follows Apple's convention — the names say where the device's left edge went,
not how the picture turned — but only the device settles it.

---

## Phase 8 — Device identity and remote debugging ✅

| Item | State | Where |
|---|---|---|
| `serial`, `brand`, `buildId`, `securityPatch`, `abiList` on the wire | ✅ | `protocol/src/common.ts` |
| Fixtures for `device.upsert`, `device.display`, `device.battery` | ✅ | `protocol/test/fixtures.ts` |
| Matching nullable columns + generated migration | ✅ | `db/src/schema/farm.ts`, `drizzle/0001_*.sql` |
| Gateway maps them on upsert | ✅ | `coordinator/src/gateway/handler.ts` |
| Android reads them from the existing `getprop` | ✅ | `backend-android/src/lib.rs` |
| `stream_codec` actually populated | ✅ | `provider-core/src/supervisor.rs` |
| Device page: battery, patch, rotation, and no repeated rows | ✅ | `web/.../devices.$deviceId.tsx` |
| `adb connect` block, holder + Android only | ✅ | `web/.../devices.$deviceId.tsx` |

**A second bug that had been quietly wrong since phase 3a:** `snapshot_from`
hardcoded `stream_codec: None` with a comment saying the session server fills it
in. It never did, so the device page's "Codec" row was blank on every device in
every deployment. It now reads the live codec off the video handle without
waiting — a device that is not streaming simply reports none.

**The metadata costs nothing extra.** All five fields come out of the single
`getprop` round-trip `info()` already made; iOS reports `serial` (its UDID) and
`buildId` and nothing else, and the `<Detail>` helper hides falsy values, so an
iPhone shows fewer rows with no branching anywhere.

**What the card actually shows is less than what is collected**, after looking
at it on a real device: `ro.serialno` *is* the adb serial and `ro.product.brand`
is usually the manufacturer again, so Serial and Brand appeared as rows
repeating the two above them — they now render only when a device disagrees.
Build id and the full ABI list are collected but not shown: a Samsung build id
is 32 characters and pushed the card open. They stay on the wire and in the DB,
where they cost nothing, and the row that broke the layout is gone rather than
the data behind it. `<Detail>` also gained `min-w-0`, without which `truncate`
does nothing inside a flex row and a long value widens the card instead of being
cut short.

Remote debugging was fully implemented in phase 4 and **unreachable from the
UI** — `device.adbExpose`/`adbUnexpose` existed and nothing called them. The
Details card now offers it to the holder of an Android device, prints
`adb connect host:port` with click-to-copy, and turns it off again. The host
comes from `provider.publicBaseUrl`, which `listDevices` already returned.

One generator change came with this: a wire union's variants are inherently
uneven — `hello` carries a whole inventory — so `emitTaggedUnion` now emits
`#[allow(clippy::large_enum_variant)]`. Fixing it in the generator rather than
in `generated.rs`, which is never hand-edited.

**`adb forward` could not serve this.** Exercising it on a containerised
provider showed the forward binding the adb server's *loopback*, on an ephemeral
port — unreachable from any other machine, and unpublishable besides. The
provider runs its own listener now and splices each connection to the device's
`adbd` over the USB transport, on a port claimed from the `remote_debug.ports`
pool and returned on release.

---

## Phase 9 — Popout ergonomics ✅

| Item | State | Where |
|---|---|---|
| `controls="overlay"` — corner handle expanding to an icon bar | ✅ | `web/src/components/device-console.tsx` |
| Popout: video fills the window, no column padding | ✅ | `web/src/routes/_session/devices.$deviceId.popout.tsx` |
| `BroadcastChannel` presence — one stream at a time | ✅ | `web/src/hooks/use-popout-presence.ts` |
| Parent suspends and offers "Bring it back here" | ✅ | `web/.../devices.$deviceId.tsx` |
| Renewal follows the visible window | ✅ | both routes |

The popout and its parent each ran a full `useDeviceSession` — two sockets, two
decoders, two backlogs on one device — because **no coordination existed at
all** (no `BroadcastChannel`, `postMessage` or `beforeunload` anywhere in
`packages/web`). The popout now heartbeats every 2s on
`farm-device-<id>`; the parent treats "alive within 5s" as popped-out and
passes `active={false}`, which already meant "open no session".

A **heartbeat rather than a single announcement**, because a popout that
crashes never gets to say goodbye and a parent stuck suspended forever is worse
than a redundant decoder. The parent posts `who` on mount, so reloading it
re-discovers a popout that is already open.

**One phase-6 decision reversed.** Phase 6 says "the popout deliberately does
*not* renew — a popout left open should not keep a device nobody is watching."
That guard cost a user who closed the parent tab their device mid-session, and
with phase 10's idle timeout as the real backstop it is no longer load-bearing.
Both windows renew now; whichever is streaming keeps the reservation.

The overlay renders the *same* action list as the toolbar — one array, two
renderings — so the popout cannot drift into being the lesser window.

**Where it sits was decided on a real device.** The first version was a bar
across the bottom that faded after a pause, and on any phone with a home
indicator it covered exactly the wrong strip: the gesture area. Auto-hiding did
not save it, since the bar has to be visible to be used and is then sitting on
the swipe-up. It is now a single small handle, inset from a corner. Hover or
focus expands the full bar; clicking pins it open, because a pointer that cannot
hover still has to reach the controls.

**And the corner is the user's to pick**, because there is no right answer:
the bottom edge is the home indicator, the top corners are where iOS pulls
control centre and notifications from, and which of those matters depends on the
device and the task. The handle drags to any of the four and the choice is
remembered in `localStorage` — one key for the whole farm, since the reason to
move it is the shape of your own screen, not the device's. A drag under four
pixels is still a click, so pinning did not become fiddly. The line that holds
it is fixed across its own axis: the expanded pill is the larger of the two, and
a centred line re-centred both the moment it appeared, so the handle jumped as
you reached it.

**The bar follows the device's orientation.** It was a column whatever the
device was doing, which is right for a portrait popout — a window shaped like a
phone has height to spend and no width — and exactly wrong once the screen
turns: the column ran off the bottom of a short wide window and started
scrolling, with the width beside it empty. `controlsAxis`
(`web/src/lib/controls-corner.ts`) now picks the axis from the shape of the
picture, so a landscape device gets a row along the edge from the same handle.
The handle sits *on* that line rather than beside it — above the column, left of
the row — so the two read as one object however the device is held. It reads the renderer's frame size, which is reported after `renderRotation` is
applied and so is already correct on both backends; `display` stands in until
the first frame. Unlike the corner it is not remembered — the axis belongs to
the device, not the user.

**Verification is manual and outstanding:** pop out and watch the parent's
WebSocket close, close the popout and watch the parent resume within ~5s,
reload the parent while the popout is open and confirm it comes back suspended,
and rotate the device with the bar open from each of the four corners.

---

## Phase 10 — Session governance ✅

The largest of the post-launch phases: schema, policy, and two new interaction
flows. Built in four parts, in this order, each its own commit.

| Item | State | Where |
|---|---|---|
| 10.1 `setting` table + typed registry, admin page | ✅ | `coordinator/src/lib/settings.ts`, `web/.../admin.settings.tsx` |
| 10.2 Idle timeout — provider activity, reaper, warning | ✅ | `provider-core/src/supervisor.rs`, `coordinator/src/lib/reservations.ts` |
| 10.3 "You were kicked" dialog | ✅ | `web/src/components/session-ended-dialog.tsx` |
| 10.4 Admin joins a session | ✅ | `db/src/schema/farm.ts`, `coordinator/.../admin.ts` |

### 10.1 Settings ✅

The first DB-backed configuration in the project, so it had to establish the
pattern rather than just add a knob: a typed registry declares every key with a
zod schema and a default, reads are cached for five seconds because reserve,
renew and every reaper sweep consult them, and **defaults are the env vars they
replace** — `RESERVATION_TTL` is now a seed, so an existing deployment behaves
exactly as it did until an admin changes something.

Two decisions worth keeping:

- **An absent row means "use the default".** Nothing is seeded, a fresh database
  needs no migration data, and resetting a setting is deleting a row.
- **`value` is nullable.** `null` is a real value for the two timeouts — it
  means the policy is off — and jsonb `NOT NULL` cannot express it. Absence of
  the *row* is what means unset; the two are different questions.

A bad row (hand-edited, or left from an older shape) is logged and ignored in
favour of the default rather than propagating a wrong type into policy. So is a
failed read: reserve and renew both go through here, and settings must not be
able to take the coordinator down.

`settings.get`/`set` are admin-only; `settings.public` is the smaller subset the
browser needs to render the idle countdown, a separate procedure rather than a
branch inside `get`.

**Two bugs found by using it, both fixed:**

- **The detail page needed a reload to see an observer arrive.** `useDeviceStream`
  invalidated `device.list` and `provider.list` but not `device.get`, so the page
  10.3 had just subscribed to still refetched nothing — a join, a release from
  another tab, and its own status all went unseen. It invalidates
  `device.get.pathKey()` now.
- **`ObserverDisclosure` looped, once an observer was actually present.** It
  tracked who had been announced in *state* with `observers` in the dependency
  array: the effect set the state, the state was a dependency, round it went.
  It survived at all only because react-query's structural sharing happened to
  keep the array's identity stable — far too subtle a thing to rest a render
  loop on. It now keys the effect on a sorted id **string** and keeps the seen
  set in a ref, so the effect re-runs when the people change and never merely
  because a new array arrived saying the same thing.

- **Letting go of a device announced itself as being kicked.** Release revokes
  the session like every other path, so the console reported it exactly as it
  reports an admin taking the device — "Session ended", naming the user to
  themselves. The device page now flags a release it initiated *before* the
  request goes out, since the revoke can arrive over the socket first, and
  navigates back to the device list: there is nothing left on the page to look
  at.

**Five new audit actions** land with this phase and the admin UI's hand-written
`ACTIONS` list does not know about them yet: `device.reservation_idle`,
`device.reservation_max_duration`, `device.session_join`, `device.session_leave`
and `settings.update`. Phase 11 replaces that list with one exported constant,
which is the actual fix.

**Verification still manual and outstanding:** two browsers (one admin, one
normal user) — reserve as the user, join as the admin, confirm both paint frames
and both can drive the device and that the user gets the disclosure; force
release and confirm the "Session ended" dialog names the admin; set the idle
timeout to two minutes and leave a session alone, confirming the warning appears
at ~12s remaining and the reaper's own reason explains the release afterwards.

**That two-minute run was done, and found three bugs — all fixed:**

- **`last_activity_at` was written in the coordinator's local timezone.**
  `renew` clamps with `greatest(last_activity_at, $interacted)`, and a raw
  ``sql`` `` parameter skips drizzle's column serializer: the driver renders a
  `Date` with the server's UTC offset, which `timestamp without time zone`
  then *drops*, storing local wall clock in a column every other writer —
  including the provider's own activity reports — fills with UTC. On a UTC+1
  host the countdown read "releases in 60:14" and the reaper could never
  reclaim the device. It binds `toISOString()::timestamp` now. The test
  ("the activity clock is UTC whatever timezone the coordinator runs in")
  pins `TZ` to Asia/Tokyo, because a UTC test host cannot see this class of
  bug at all.
- **The details panel and the warning dialog were two different clocks.** The
  panel derived its own deadline from `lastActivityAt`; the hook uses
  `max(lastActivityAt, local interaction)`. The panel therefore hit 0:00 while
  the dialog beside it still had ~15s. `useReservationRenewal` exposes
  `idleDeadline` and everything renders that one value — the route calls
  `useReservationKeeper` (one instance per window, since it drives the
  heartbeat) and passes the result to `<ReservationKeeper>`.
- **The countdown only moved when something else re-rendered the page.** It was
  `Date.now()` evaluated inline, so it sat still between query refetches. A
  `<Countdown>` component owns the one-second tick, so only the number
  re-renders. It appears only inside the last 15% of the idle budget
  (`COUNTDOWN_FRACTION`, just above the 10% that opens the dialog): a number
  ticking for a whole session, resetting at every touch, is noise, and it is
  not actionable until the end. Above that the row states the policy alone.
- **The popout never learned its session had ended.** It had no
  `useDeviceStream` of its own — the opener's does not reach another window —
  and no `refetchInterval`, so the only thing that could tell it was a
  `session.revoke` over the session socket. An idle release therefore left the
  window streaming a device the holder no longer had, with the warning dialog
  still up and nothing to click. It subscribes now.

  Both windows share `useSessionEnded`, which watches *both* signals: the
  provider's revoke carries a reason but needs a live socket, and a session
  gone from `device.get` has ended whatever the socket did. The device page
  had the second signal all along and never acted on it, so an idle release
  there turned the page quietly back into an unreserved device and explained
  nothing.
- **The warning dialog counted past zero.** The reaper sweeps every 30s, so the
  deadline always passes before the release does, and the dialog sat at 0:00
  still offering "Keep it" — a button that races a sweep that may already have
  taken the device. Past zero it says the device is being released and offers
  only to close.

### 10.4 Admin joins a session ✅

Force-release was the only thing an admin could do to a session in progress, and
it is a blunt instrument: the holder loses the device mid-task. Joining is the
gentler option — full control, and the holder is told.

**The provider needed no change at all.** `SessionRegistry::check` matches on
`reservationId`, and `authorize` deliberately does not disturb existing viewers
when re-authorizing the same reservation — behaviour written for the popout,
with a test. So an admin holding a token minted against *the holder's*
reservation is accepted exactly like the holder's second tab. The only
coordinator change is which callers `device.sessionToken` will mint one for:
an admin with an open observer row now gets a token carrying the holder's
`reservationId` and their own `userId`. Same TTL, same claims, same provider
check.

`reservation_observer` is a table rather than a jsonb column because join and
leave are events worth querying, and because the holder's UI names who is
present. A partial unique index on `(reservationId, userId) where left_at is
null` means rejoining after a reload cannot leave two open rows for one person.
`releaseActive` closes every open row with the session it belongs to.

**The holder's disclosure is non-blocking on purpose.** A modal they must clear
would be theatre: by the time it renders, the admin is already in the session
and already has control. So it is a one-shot dialog on the transition from
nobody to somebody, plus a persistent badge in the header for as long as an
observer is present — a refresh does not re-announce an observer who has been
there for an hour.

### 10.3 "You were kicked" ✅

A force-released user could not tell an administrative action from a dropped
network: both showed a spinner over a frozen last frame. Two bugs in
`use-device-session.ts` made that unavoidable —

- **`session.closed` set the reason but not the state.** The closed state
  arrived later from `onclose`, whose handler then **overwrote the reason with
  `event.reason || undefined`**, turning "taken back by Ana Silva" back into
  "Session closed". The revocation reason now lives in a ref that `onState`
  prefers over the socket's, and the state is set at revoke time.
- **Nothing invalidated `device.get` on revoke**, so the header carried on
  offering "Release" for a device the user no longer had.

The canvas is cleared with the renderer, because a frozen frame under a dialog
still reads as a live session.

**The actor's name is not on the wire and should not be** — the provider has no
notion of users, and `session.closed` is a string. `device.reservationOutcome`
reads it back from the reservation row, every column of which `releaseActive`
already wrote. A reaper release has `releasedBy = null` and is phrased without
an actor, because saying a person did it would be false and "system" says
nothing.

The device page also subscribes to `useDeviceStream` for the first time. It
never did, so the detail page got no live updates at all.

### 10.2 Idle timeout ✅

Nothing bounded a reservation but a browser tab staying open. `expiresAt` only
ever caught a user who *closed* the tab; a tab left open on a device nobody was
touching held it indefinitely, which is how the farm bleeds capacity.

**The provider is authoritative about use, the browser is a floor under it.**
Two sources, because neither is sufficient alone:

- The provider sees every `ClientMessage` that reaches the device and every
  install — including a session driven entirely through an exposed adb
  transport, which the browser cannot see at all. New `device.activity` wire
  message, rate-limited to one per device per 30s by an `ActivityThrottle`,
  because a drag is hundreds of pointer events a second and the coordinator only
  needs to know the reservation is not idle. `keyframe` and `pong` are
  deliberately **not** interaction: they are the stream keeping itself alive,
  and counting them would make the idle timeout unreachable for as long as a tab
  is open.
- The browser reports `interactedAt` on renewal, which covers the case the
  renewal hook was always about — reading a crash log on a reserved device is
  still using it, and nothing reaches the device while that happens.

**Neither may move the clock backwards, and neither may move it forwards past
now.** Both writes clamp to `now` and are guarded (`lt` on the gateway,
`greatest()` on renew), so a provider host with a fast clock cannot buy an extra
hour and a backgrounded tab replaying a stale timestamp cannot undo a real
touch. There are tests for both directions.

The reaper now sweeps three conditions in the same select-then-`releaseActive`
shape: lapsed, idle (when configured), and a hard `maxDurationSeconds` cap.
Every path still goes through `releaseActive`, which is what pushes
`session.revoke` and writes the audit row — the reason string it stores is what
the released user is told, so it is written for a person to read ("released
after 30 minutes without interaction").

**The warning is a dialog at 10% remaining**, in a `ReservationKeeper`
component that both the device page and the popout render — renewal and the
warning are one feature, and splitting them across routes is how they drift.
Interacting anywhere in the tab dismisses it, and an interaction inside the
warning band pushes a renewal **immediately** rather than waiting for the
scheduled one: the reaper reads the database, and on a long TTL with a short
idle timeout the next scheduled renewal can be minutes too late.

---

## Phase 11 — Audit filtering ✅

The audit log had one filter, on action, over a hand-maintained list of options
that had already drifted twice — and it paginated by "was the page full?", which
can neither show a count nor recognise the last page.

| Item | State | Where |
|---|---|---|
| `AUDIT_ACTIONS` — one list, in the protocol package | ✅ | `protocol/src/audit.ts` |
| `admin.audit` filters + `{ items, total }` | ✅ | `coordinator/.../admin.ts` |
| Index on `audit_log(target_id)` | ✅ | `db/drizzle/0005_*.sql` |
| Filter row in search params, count, target links | ✅ | `web/.../admin.audit.tsx` |

**The action list lives in `packages/protocol`**, not the coordinator, because
the web app needs the *values* and the working agreement is that `packages/web`
imports coordinator **types** only. It is deliberately a plain const rather than
a `named()` zod schema, so the Rust generator does not emit it: a provider has
no notion of audit actions. `audit()` and `ReleaseOptions.auditAction` now take
the union, so an action written any other way does not compile — which is what
stops the list drifting a third time. Typing them turned up no unknown actions,
confirming the list is complete at 20; the UI's copy had known about 8.

`auditActionLabel` falls back to the raw value rather than rendering nothing for
an action retired from the list. The audit log is a historical record, and an
action nobody writes any more has not stopped having happened.

**Filters live in the URL.** "Look at what happened to this device" should be a
link, which is most of the reason to have filters at all — so they are route
search params via `validateSearch`, `page` moved there too, and any filter
change resets to page 0. The target filter is a prefix match so a partially
pasted id still finds it, and debounced, because a request per keystroke would
also mean a history entry per keystroke.

`total` is a second `count()` over the same predicate, built once and passed to
both queries so the two cannot disagree about what they are counting.

### A latent bug this surfaced

**The `23505` → `CONFLICT` translation had stopped working.** `isUniqueViolation`
read `err.code` directly, and drizzle now wraps query failures in an error
carrying the driver's as its `cause` — so the loser of a concurrent reserve got
a 500 with the whole INSERT in the message instead of "Device is in use".

Invisible because losing that race is rare enough that a 500 there reads as a
flake. The phase-1 concurrency test only caught it once this phase's settings
read shifted the timing enough for both callers to reach the insert reliably. It
now walks the cause chain, lives in `lib/pg-errors.ts` rather than inside a
router, and has a test per error shape. It is the one function standing between
the database's exclusivity guarantee and what a user sees.

---

## Phase 12 — Asking to join, and two iOS corrections ✅

A device somebody else held was a dead end for everyone but an admin: the page
named the holder and offered nothing. The co-driving machinery already existed —
`admin.joinSession` builds an observer row, discloses it, audits it, and mints a
token the provider accepts as one more viewer — so what was missing was only a
way to *ask*.

| Item | State | Where |
|---|---|---|
| `join_request` table + partial unique index | ✅ | `db/src/schema/farm.ts`, `drizzle/0006_*.sql` |
| `requestJoin` / `cancelJoinRequest` / `answerJoinRequest` / `myJoinRequest` | ✅ | `coordinator/.../device.ts` |
| Observer row is the authorization, not the admin role | ✅ | `device.sessionToken`, `requireOwnedDevice` |
| Request expiry: reaper sweep + death with the reservation | ✅ | `coordinator/lib/reservations.ts` |
| Ask to join / waiting / `JoinRequestPrompt` / badge | ✅ | `web/.../devices.$deviceId.tsx` |
| Popout opens for anyone in the session | ✅ | `web/.../devices.$deviceId.popout.tsx` |
| `platformLabel` — "iOS", not "Ios" | ✅ | `web/src/lib/utils.ts` |
| `pick_usb` — USB only, network refused | ✅ | `provider/crates/backend-ios/src/device.rs` |

**Approval creates a `reservationObserver` row and nothing else.** That is the
whole design: an invited user arrives by the exact path an admin's self-join
takes, so the session token, the provider, and the disclosure the holder already
sees need no notion of how the person got there. `addObserver` is now the single
writer of that row, so the two paths cannot drift.

**The authorization change is the part worth reviewing.** Two call sites
conflated "admin" with "present in the session", in opposite directions:
`sessionToken` required `role === "admin" && isObserver(...)`, and
`requireOwnedDevice` let an admin through with *no* observer row while refusing
a non-admin who had one. So an approved asker would have been able to stream but
not launch an app. Both now read the observer row; admins keep their pass on
device commands.

**Requests cannot outlive what they asked about.** They are keyed on
`reservation_id`, `releaseActive` expires the pending ones in the same place it
closes observer rows, and the reaper retires those nobody answered within
`JOIN_REQUEST_TTL`. `state` keeps `denied`, `cancelled` and `expired` apart —
the asker is told which, since an approval announces itself by the console
simply opening and the other outcomes announce nothing.

### The two iOS corrections

**"Ios".** Tailwind's `capitalize` on the wire value `ios`. `platformLabel`
replaces it at three render sites; the enum, the wire value and the generated
Rust variant are untouched, because those are protocol and not prose.

**usbmuxd list order decided the transport.** A Wi-Fi-synced iPhone is listed
twice, `USB` and `Network`, and `idevice`'s `get_device` is a `find` — so which
one a session was built over was a coin toss. The 17.4+ root-free tunnel is
built and tested over USB, and a session that silently came up over Wi-Fi fails
later in the media path where the cause is invisible. `pick_usb` takes the USB
entry and refuses a network-only device with an error that says what to do. This
is stricter than "prefer USB": a network-attached device that happened to work
stops, loudly. Deliberate.

The 17.4 floor error also told operators to *"Route this device to the legacy
WDA provider"*. There is no WDA provider in this project — it was left behind in
the port from `stf-ios-provider` — so the sentence sent people looking for
something that does not exist. It now states the floor and stops.

---

## Phase 13 — Getting things off a device ✅

Everything the farm could do pushed *into* a device — install, type, tap, paste.
The only thing that came back out was a single PNG, so a tester who reproduced a
bug had no way to hand over the evidence. Two features close that, deliberately
built on opposite sides of the system.

| Item | State | Where |
|---|---|---|
| `FileEntry` / `FileListing` — the artifact plane's first schemas | ✅ | `protocol/src/files.ts` |
| `file.pulled` control message + `device.file_pull` audit action | ✅ | `protocol/src/{control,audit}.ts` |
| `files_root` / `list_files` / `pull_file` on the backend trait | ✅ | `provider-core/src/backend.rs` |
| `GET /s/:id/files` and `GET /s/:id/file` | ✅ | `provider-core/src/server.rs` |
| adb sync `LIST` + `RECV` | ✅ | `backend-android/src/adb.rs` |
| AFC listing and chunked read | ✅ | `backend-ios/src/lib.rs` |
| A synthetic tree, so the whole path runs with no hardware | ✅ | `backend-mock/src/lib.rs` |
| Files dialog; `saveBlob` shared by three callers | ✅ | `web/src/components/device-files-dialog.tsx` |
| `ScreenRecorder` — canvas → MP4, capped at 2 minutes | ✅ | `web/src/lib/screen/recorder.ts` |

### The file browser

**Read-only, and that is a decision.** Writing to a device's filesystem is a
separate question with its own audit weight, and `install` already covers the one
file anybody needs to put on a device.

It rides the artifact plane, so the coordinator mints a token and is then out of
the way — and it is **the only operation in the system that carries data out of a
device**, which makes it the one that most needs auditing. The provider therefore
reports `file.pulled` up the control plane exactly as it reports an install, with
the sha256, and the coordinator writes the row. Two things about that row:

- **It is written before the body is sent.** A download the client aborts half
  way still took the bytes off the device; an egress record that only lands on a
  clean finish is the wrong direction to be wrong in.
- **Directory listings are not audited.** One browse would write a row per click,
  and the row worth keeping is the one with a digest on it.

The pulled file is staged in the scratch directory and deleted when the response
body drops — the install path's arrangement in reverse, and for the same reason:
a video off a phone must never sit in the provider's memory. There is still no
artifact storage anywhere.

**Android uses the sync subprotocol, not `adb shell ls`.** `ls` output differs
between toybox and toolbox builds and has to be re-guessed per device; `LIST`
answers fixed-width `DENT` records carrying the mode and size, so a listing costs
no extra `stat` calls and a directory is tellable from a file without one.
`RECV` is the mirror of the `SEND` that was already there for the scrcpy server.
Both are tested against a **fake adb server that writes one byte at a time** —
the phase-4 scrcpy bug came from assuming a single `read()` returns a whole
header, and framing readers in this crate now get tested against the hostile case
by default.

**Navigation is not fenced to `/sdcard`.** adb runs unrooted, so the device's own
permissions are the real boundary and a second one in the provider would only be
decoration — it would hide `/sdcard/Android/data` while `/data/data` still
answered "Permission denied" on its own. So a refused directory answers 502 with
**the device's own words**, which is also the only thing that tells an unreadable
directory from an empty one.

**iOS has no single filesystem, so its root is synthetic.** The first cut served
only `com.apple.afc` — the media directory — and that turned out to be the wrong
half: **the first thing tried on a real iPhone was a file saved through "Save to
Files", which lands in an app container and was not there.** So the root now
lists the media domain *and* every app that shares files, each reached by its own
`house_arrest` vend. They are genuinely disjoint trees with no common parent on
the device, so the path carries which one it means (`media:/DCIM`,
`app:<bundle>:/Documents`) rather than inventing a root above them.

Three things that only a real device would have said:

- **A `VendDocuments` session denies everything above `/Documents`.** Listing the
  container root answers `Afc(PermDenied)`, which reads exactly like a broken
  feature, so the app tree starts at `/Documents`.
- **`UIFileSharingEnabled` is the right filter**, because it is the same flag
  that decides whether an app appears under "On My iPhone" — so the browser shows
  the set the phone shows. It rides in the `installation_proxy` answer already,
  so this costs one round-trip rather than one per app.
- **An app suite ships several bundles under one display name.** The test phone
  listed "My BMW" three times, which is not a choice anyone can make; the
  ambiguous rows now carry their bundle id and unique ones stay clean.

Still out of reach: files saved to the *root* of "On My iPhone" rather than into
an app's folder, and `crashreportcopymobile` for crash logs — the latter is a
third tree and the obvious next addition.

One shape worth noting: `files_root` is a **backend** answer rather than a
platform check in the web app, because what a device exposes is a property of how
the provider reaches it, not of the logo on the front. A backend that answers
`None` makes `/files` a 501, which the browser renders as "this device does not
offer file access" — a different thing from an empty directory.

### Recording

**No provider, protocol or coordinator change at all**, and that is the whole
point: the frames are already decoded and already painted, so this is the browser
recording its own canvas. There is nothing here for the audit log to say — the
coordinator cannot observe it, and a row claiming otherwise would be a fiction.

- **MP4 where the browser will give one.** `MediaRecorder` grew MP4 output only
  recently and Firefox still has WebM only, so the container is a preference list
  resolved by `isTypeSupported` and the extension follows from what was actually
  chosen. A file named `.mp4` by a recorder that produced WebM is worse than a
  `.webm`.
- **It captures a fixed-size mirror of the canvas, not the canvas.** The renderer
  reassigns `canvas.width`/`height` on every geometry change, and an MP4 track
  has one geometry for its whole length — so the size is fixed at record-start
  and each frame is drawn in aspect-fitted. Rotating mid-recording letterboxes
  rather than ending the file, which is what someone recording a rotation bug
  needs. It also keeps the capture off the live context, which is created
  `desynchronized` for latency.
- **The 2-minute cap finalises and saves rather than discarding** — an accidental
  two minutes is still evidence. So do a revoked session, unmount, and `active`
  going false, which is the popout handover: every one of those funnels through
  one `finishRecording`, because a recording that vanished when its window went
  quiet would be the worst way for this to fail.
- **Frames are pushed explicitly**, with `captureStream(0)` + `requestFrame()`.
  The mirror is not in the document, so nothing composites it, and Chrome emits
  from an uncomposited canvas only occasionally whatever rate it is given.
  Measured on the Galaxy S22, `captureStream(30)` produced **three frames in
  forty-one seconds of continuous motion** — a correctly timed slideshow, which
  is worse than an error because the file looks fine until it is played. With
  `requestFrame` the same test gives 360 frames in 18s.
- **A recording running behind a collapsed overlay had nothing to show for
  itself.** The popout is exactly where a window gets left alone, so the badge —
  a pulsing dot and the elapsed time — sits outside the collapsing bar, in the
  opposite corner from the handle, and stops the recording when clicked.

Files are named `<device>_20260807_194105.<ext>`: a unix millisecond count sorts
just as well and tells a person nothing, and these exist to be matched against a
bug report.

The raw alternative — teeing the H.264/HEVC access units into an MP4 muxer, no
re-encode — was considered and rejected. The wire carries no presentation
timestamps (`decodeChunk` synthesises a nominal 16 666 µs step), so pacing would
drift; HEVC in MP4 is a narrower playback story than AVC; and recording what the
viewer actually saw is the honest artifact for a bug report anyway.

### Verified

`cargo test --workspace` and `bun test` (124) pass, clippy clean at `-D warnings`.
The provider-core suite runs the real axum router against the real mock backend
over a real socket: a listing opens at the backend root with a null `parent`, a
subdirectory reports sizes and walks back up, a refused directory answers 502
carrying the path, a download serves the right bytes with a `Content-Disposition`
filename **and the staged copy is gone afterwards** — which asserts the guard
fires rather than that it was never created — and both new routes refuse a
garbage token (401), a token for another device (403), and a revoked reservation
(403). The adb `LIST`/`RECV` readers are tested against a fake adb server that
**writes one byte at a time**.

**Verified on hardware**, which is where three of this phase's bugs came from:

- **Galaxy S22.** Browsed `/sdcard` → `Pictures`, downloaded a 1.1 MB PNG, and
  its sha256 matched `adb shell sha256sum` exactly. The scratch dir was empty
  afterwards and `/admin/audit` had the `device.file_pull` row.
- **iPhone 13.** The synthetic root listed Media and five file-sharing apps;
  browsing into one showed its real `Documents` and pulling `Approov.log` off it
  gave 9184 bytes of the app's own log.
- **Recording**, in the popout: 360 frames over 18.3s of H.264 at the stream's
  own 590×1280, and a 44.9s file whose duration matched the badge's `0:44`.

**Still to confirm by hand:** the 2-minute cap firing on its own, a rotation
mid-recording producing one letterboxed file, and a recording started in the main
tab surviving the popout handover.

---

## Phase 14 — Device metrics ✅

*Done when: a Prometheus server scrapes CPU, memory, temperature and per-app
usage off every device a provider owns, and a stale device's series disappear
rather than going flat.*

The provider supervised every device on a host but reported almost nothing about
their *condition* — `DeviceSnapshot` carried battery level and charging state and
that was all. A phone that is thermally throttling, swapping, or pinned by a
leaked process streams badly and fails tests in ways that look like flaky
software, and the only way to notice was for a tester to say a device "feels
slow". The metric set is taken from `Dinip/adb_metrics`.

**Nothing crosses the coordinator wire.** No zod schema, no `generated.rs`, no DB
column, no UI — `bun run protocol:check` is unaffected. Don't go looking; this is
provider-local, in the spirit of Phase 13's recorder.

| Item | State | Where |
|---|---|---|
| `metrics:` config section | ✅ | `provider-core/src/config.rs` |
| `DeviceMetrics` + `AppFilter` on the backend trait | ✅ | `provider-core/src/backend.rs` |
| Synthetic metrics for the mock | ✅ | `backend-mock/src/lib.rs` |
| Per-device in-process counters | ✅ | `provider-core/src/video.rs`, `supervisor.rs` |
| Sampler + cache | ✅ | `provider-core/src/metrics.rs` |
| Exporter + listener | ✅ | `provider-core/src/metrics.rs` |
| Android CPU / memory / thermal | ✅ | `backend-android/src/lib.rs` |
| Android per-app CPU + PSS | ✅ | `backend-android/src/lib.rs` |
| iOS diagnostics-relay probe | ✅ | `backend-ios/examples/` |
| iOS battery | ✅ | `backend-ios/src/lib.rs` |
| Dev observability stack | ✅ | `docker-compose.dev.yml` |

### Verified with no hardware

`--profile observability` up, the real binary running two mock devices: Prometheus
reports the target `up`, `farm_device_cpu_seconds_total` advances between scrapes,
`sum by (device) (rate(...{mode!="idle"}[2m])) / sum by (device) (rate(...[2m]))`
reads ~0.18 against the mock's ~0.15 ± wobble, `farm_app_memory_pss_bytes` exists
for `com.example.demo.player` and not for `com.example.mock.app`, the iOS mock has
no CPU series at all, and Grafana provisions the dashboard. With a deliberately
bad provider token, `farm_provider_control_connected` reads 0 while every device
metric keeps being served — the exporter does not depend on the coordinator.

**A real bug the tests caught.** The encoder wrote each sample as it went, which
is wrong: samples are gathered device-major, so families arrived interleaved and
each was re-declared per device. Prometheus requires a family's samples contiguous
under one `# HELP`/`# TYPE`. The encoder now buffers per family, and there is a
unit test asserting the exact regrouped bytes.

### Verified on hardware

**Galaxy S22 (Android 15)**, via `cargo run -p backend-android --example
metrics_probe`: the batched read parses `/proc/stat` (all eight modes), 7.10 GiB
total memory with 2.85 GiB available, battery 100% "full" at **33.7 °C**, and 209
processes out of the PSS section with comma-grouped values and `:remote`/`:search`
sub-processes staying distinct from their parents. `*.google.*` matched 12 of them
and every one got CPU seconds back — so the `/proc/<pid>/stat` last-`)` split works
on real process names.

**Thermal zones came back empty**, as predicted: this device exposes only
`cooling_device*` under `/sys/class/thermal`, no `thermal_zone*` at all. It
degrades to no series rather than an error, which is the intended behaviour. 209
processes against a cap of 32 is also a good demonstration of why the cap exists.

**iPhone 13**, via `cargo run -p backend-ios --example diagnostics_probe`: level
and charging state read correctly. Two things the hardware corrected — see
PROVIDER.md: `gasguage` is *not* the battery source despite the name, and there is
**no temperature** available on iOS at all.

### A real bug the hardware caught

`read_battery` chose its source by whether the dictionary came back non-empty.
`gasguage` answers a non-empty and completely useless dictionary — `CycleCount`,
`FullChargeCapacity`, `Status`, nested under a `GasGauge` key, no charge level —
so the fallback to `ioregistry` never fired and iOS reported no battery at all.
It now tries the registry first and judges each source by whether a *level* came
out of it. Both real shapes are now test fixtures.

### A second bug the hardware caught

A reserved device reported `farm_device_status{status="ready"}`. The provider's
own status can never be `busy` — that is the coordinator's word — so the raw
export made the `busy` series permanently dead *and* made every in-use device
advertise itself as available. The exporter now combines the provider status with
the session registry; health still wins over occupancy.

### The dashboard needed a browser to verify

Every check through Grafana's API passed while the dashboard was, in fact,
blank. Three separate faults, none visible without opening it:

- Prometheus targets default to **instant** queries. Hand-written panel JSON has
  to say `range: true` — Grafana's query editor writes it for you. Without it a
  panel draws a legend and an empty plot, with no error anywhere.
- The status panel was a time series, which is the wrong shape for a categorical
  signal. It is a **state timeline** now, with the statuses encoded numerically in
  PromQL and mapped back to names and colours.
- Value mappings and a `thresholds` block on the same field fight, and thresholds
  win: every band rendered as a uniform grey bar labelled `-∞+`.

### Still to confirm

A device that is *actually* warm or busy — everything above was measured on an
idle, fully charged phone, so the numbers are right but the range is narrow. And
a device with readable `thermal_zone*` entries, to exercise the unit heuristic on
something other than a fixture.

### Decisions worth not relitigating

- **A listener of its own, not a route on 7100.** That port is browser-facing,
  carries a CORS layer and session tokens, and is publicly TLS-terminated. There
  is deliberately **no auth** on the metrics port; the operator binds it to an
  interface only their monitoring reaches. A `/metrics` alias on 7100 "for
  convenience" would inherit the CORS layer and defeat the whole point.
- **It is not a fifth plane.** A plane in this codebase carries authenticated user
  traffic; this has neither property. See ARCHITECTURE.md.
- **A background sampler with a cache**, not sample-on-scrape: a scrape never
  waits on a phone, and a second scraper cannot double the load on the devices.
- **CPU is a counter, not a percent.** `/proc/stat` is already a monotonic jiffy
  counter, so exporting `_total` means nothing here holds a previous sample,
  `rate()` gives the operator any window they want, and a rebooted device's
  counter reset is handled for free.
- **A stale or failed device emits *no* device-sourced series at all.** Absence is
  how Prometheus spells "no data". Re-emitting the last known value would be a
  lie — an unplugged phone would show a flat, healthy 40 °C forever. The
  operational metrics are read live and are never suppressed, which is what keeps
  "device gone" distinguishable from "exporter broken".
- **No metrics crate.** The values arrive as a snapshot and the exporter
  transcribes them on GET. Both `prometheus-client` and
  `metrics-exporter-prometheus` *retain* label sets they have seen, and making a
  series disappear through a retaining registry is more code than emitting the
  text format directly.

---

## Phase 15 — UI rework: hardware buttons and a page that is mostly device ✅

*Done when: the device page gives its whole height to the screen, Back and Home
are one click away in both the page and the popout, a long audit metadata blob
cannot widen the table, and there is one providers page instead of two.*

Four unrelated complaints, one pass.

| Item | State | Where |
|---|---|---|
| Navigation moved to a 56px left icon rail; `AppShell` bounded (`h-svh`) | ✅ | `.../components/app-shell.tsx` |
| Viewport-locked device page: one-line header, full-height screen | ✅ | `.../routes/_app/devices.$deviceId.tsx` |
| Both flanks are one collapsible `SidePanel`, open by default, persisted | ✅ | `.../components/side-panel.tsx`, `.../lib/side-panels.ts` |
| Console actions grouped into Device / Screen / Files sections | ✅ | `.../components/device-console.tsx` |
| Back / Home / Recents, platform-gated, sent as a down/up pair | ✅ | same |
| Rail is one labelled button per row; popout overlay a floating icon column | ✅ | same |
| `openPopout` lifted out of the console | ✅ | `.../lib/popout.ts` |
| Keyboard Home/End split off as `MoveHome`/`MoveEnd` | ✅ | web + both backends |
| Audit table `table-fixed`, one-line Detail, clickable row → dialog | ✅ | `.../routes/_app/admin.audit.tsx` |
| `/providers` reduced to a redirect; one nav item | ✅ | `.../routes/_app/providers.tsx` |

### The buttons needed no protocol work at all

`{ type: "key", key, down }` has existed since phase 2, `provider-core` already
routes it to `InputEvent::Key`, and both real backends already mapped `Back`
(`KEYCODE_BACK`), `Home` and `AppSwitch`. `bun run protocol:check` is unaffected.
The whole feature was a UI that never offered them.

### The bug that fell out of adding a Home button

The browser sent the *keyboard's* Home key as `key: "Home"`, and Android maps
that to `KEYCODE_HOME` — so pressing Home while typing in a text field threw the
device to the launcher. Adding a deliberate hardware Home button forced the two
apart: the keyboard keys now go as `MoveHome`/`MoveEnd`
(`KEYCODE_MOVE_HOME`/`_END`, HID `0x4A`/`0x4D`), and `End` was never mapped on
Android at all, so it started working in the same change. Regression tests on
both backends and a `key` case in `session_plane.rs`.

### The top bar was the last row worth reclaiming

Once the page header was one line, the row *above* it was the remaining waste: a
top bar carrying, for a non-admin, a product name and a single link — and the
device page stacked its own header under it either way. Navigation is now a 56px
icon rail down the left edge, labels in tooltips, account menu at the bottom. It
costs width, which is the axis a portrait device leaves spare, and it took the
canvas from 648px tall to 721px on the same screen.

`APP_NAME` still never appears as a literal: the rail's brand mark carries it as
a tooltip, from `user.capabilities`. See RENAMING.md.

### `min-h-full` is not a height

The first cut of the device page overflowed vertically. Flex only bounds a child
when the container's height is **definite**, and `min-height: 100%` is not that —
`flex-1` fell back to content height and `DeviceScreen`'s aspect-ratio box sized
itself from the available *width*, producing a box taller than the viewport. The
fix is `h-full` on the shell's content column with the padding and the scroll
moved onto `main`. Worth remembering before adding another full-height page.

### Verified in the browser, against a real Galaxy S22

Not just typechecked: page scroll is zero on both axes, the rail carries all ten
actions with the right labels and platform gating, Details collapses and the
state survives a reload, the popout's column is 46px wide and stays inside the
window, a 600-character metadata blob truncates with an ellipsis and opens in
full in the dialog, and `/providers` redirects.

### Decisions worth not relitigating

- **The rail sits beside the screen, not under it.** A portrait device leaves the
  horizontal space empty anyway, and every row of controls below the screen is a
  row of screen nobody gets. This is the whole point of the phase.
- **Both renderings are columns.** Every horizontal cut of this failed: two
  icons across in the rail wasted the height it had and read as scattered, and
  the popout's controls wrapped onto two rows and filled the window's width —
  the one direction a phone-shaped window has none of. A column is also why the
  rail can afford to *label* its buttons, which the overlay cannot: it floats
  over the picture, so it stays icon-only with the label in the tooltip. That is
  why every action's label is a phrase — "Copy from device", not "Copy".
- **No divider lines between the groups**, in either rendering. A vertical
  separator was left dangling at the end of a row the moment the pill wrapped,
  and once both were columns, proximity and the rail's headings did the job
  without drawing anything.
- **The audit table needed `table-fixed`, not a `max-w-*`.** Under auto layout
  the widest cell sets the column, so a cap on the cell never binds.
- **`/providers` is a redirect, not a deletion.** It was in the nav long enough
  to be bookmarked, and a bare "Not found" is a poor answer to a link that used
  to work.

---

## Phase 16 — cleaning a device between users ✅

*Done when: a device released by any path is reset by its provider before it
becomes reservable, an admin chooses which steps run, and no failure anywhere
can leave a device parked out of the pool.*

Until now a released device went straight back to `ready` carrying whatever the
last user left on it. STF had a mechanism for this; the design ports its intent
and deliberately not its implementation — see [CLEANUP.md](./CLEANUP.md) for
what `cleanup.js` does and the four bugs not to inherit.

| Item | State | Where |
|---|---|---|
| STF analysis + design written up | ✅ | `docs/CLEANUP.md`, `docs/REFERENCES.md` |
| `cleaning` device status, `device.cleanup`, `cleanup.finished` | ✅ | `packages/protocol`, `packages/db` |
| Backend primitives + cleanup orchestrator, under a deadline | ✅ | `provider-core/src/cleanup.rs`, all three backends |
| Per-reservation app baseline, taken on `session.authorize` | ✅ | `.../supervisor.rs` |
| `releaseActive` holds the device; reaper unsticks a stale one | ✅ | `.../lib/reservations.ts` |
| Eight settings, all defaulting to today's behaviour | ✅ | `.../lib/settings.ts` |
| Allow/deny app id globs scoping `clearAppData` | ✅ | `.../cleanup.rs`, `.../lib/settings.ts` |
| iOS 26+ app listing, which idevice cannot do | ✅ | `backend-ios/src/app_list.rs` |
| `cleanup_paths` per device, guarded against `/system` at load | ✅ | `.../config.rs` |
| Settings card + `cleaning` treatment in the UI | ✅ | `packages/web` |

### The device always comes back

The one property STF never had, and the reason most of the tests exist. A
failing step, an `Unsupported` one, a backend that hangs forever, a run past its
deadline, a provider that dies mid-clean — each has a test, and each ends with
the device reservable again. A device stuck in `cleaning` is invisible
inventory, which is worse than the dirty device the feature exists to prevent.

### What is not built

The app baseline lives in provider memory, so a provider restarted mid-session
loses it and the uninstall step declines to act (logged, and reported in the
audit row). Persisting it is STF's abandoned `fcd0d150` and the obvious
follow-up if that turns out to matter. iOS has no `pm clear` equivalent, so
`clearAppData` is Android-only.

---

## Phase 17 — mounting the Developer Disk Image ✅

*Done when: an iPhone that has just rebooted comes back on its own, with nobody
running `devicectl` at the host.*

The last hands-on step in the iOS path. A device loses its DDI mount on every
reboot and offers no `com.apple.coredevice.*` service without one — no screen,
no input, no app list — so until now `docs/PROVIDER.md` told the operator to run
a `devicectl` command and the device sat `unhealthy` in a 5s retry loop until
they did.

| Item | State | Where |
|---|---|---|
| `ddi:` config block: enabled, cache_dir, base_url | ✅ | `provider-core/src/config.rs` |
| Mirror fetch + on-disk cache, one download per host | ✅ | `backend-ios/src/ddi.rs` |
| `ensure_mounted` over lockdown, before the tunnel | ✅ | `.../ddi.rs`, `.../device.rs` |
| `auto_mount_ddi` per-device opt-out | ✅ | `backend-ios/src/lib.rs` |
| Cache + backoff tests against a throwaway mirror | ✅ | `.../ddi.rs` tests |

### Three cheap guards, because this touches Apple's servers

`LookupImage` runs first, so the steady state is one plist round trip and a
device rebooted mid-shift is remounted by the next retry. The payload is fetched
once per process and shared by every device on the host, since iOS 17's image is
the same bytes for all of them. And a mount that genuinely fails is left alone
for five minutes rather than retried every five seconds — the failure path ends
in a personalization request to Apple's TSS server, and the 5s loop would
otherwise turn a device that can never mount into a permanent load on it.

### What it still cannot fix for you

Developer Mode being off, and an iOS build newer than the mirror's image. Both
are named in the log, and both are an operator's job. Mount failure is a warning
and never fatal, so a farm whose devices are mounted by hand is unaffected.

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
