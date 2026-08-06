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

**Not yet exercised on real hardware:** the `adb connect` round-trip and the
Android metadata read.

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
pixels is still a click, so pinning did not become fiddly. The row that holds it
has a fixed height: the expanded pill is taller than the handle, and a centred
row re-centred both the moment it appeared, so the handle jumped down as you
reached it.

**Verification is manual and outstanding:** pop out and watch the parent's
WebSocket close, close the popout and watch the parent resume within ~5s,
reload the parent while the popout is open and confirm it comes back suspended.

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
