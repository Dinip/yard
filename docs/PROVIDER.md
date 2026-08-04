# `packages/provider` (Rust)

One binary, one process per host, supervising every device configured for it.

`stf-ios-provider` ran one *container* per device. Consolidating to one process
keeps crash isolation — each device is an independently restartable task tree —
while collapsing N containers, N ZMQ connections and N config files into one.

## Status

| Crate | State |
|---|---|
| `farm-protocol` | ✅ generated wire types + framing helpers |
| `provider-core` | ✅ config, control plane, session plane, supervision, JWT auth |
| `backend-mock` | ✅ synthetic device — the whole provider runs with no hardware |
| `backend-ios` | ⬜ phase 3b |
| `backend-android` | ⬜ phase 4 |

```
packages/provider/
├── Dockerfile
├── provider.example.yaml
└── crates/
    ├── farm-protocol/    generated mirror of packages/protocol
    ├── provider-core/    everything that is not device-specific
    ├── backend-mock/     synthetic device
    └── farm-provider/    the binary
```

## Running it

```bash
cargo build --release -p farm-provider

# Register a provider and issue a token under /admin/providers first.
FARM_PROVIDER_TOKEN=pft_… \
  ./target/release/farm-provider --config packages/provider/provider.example.yaml
```

`--check` validates the config and exits, which is also the container's
healthcheck.

The example config ships two **mock** devices. They register with the
coordinator, appear in the UI, reserve, stream synthetic video, accept input and
uploads — no hardware, no iPhone, no adb. This is the fastest way to work on
anything above the backend trait.

## `provider-core`

```
src/
├── config.rs      one YAML file (replaces provider.sh's 13.5k of awk)
├── backend.rs     the DeviceBackend trait — the seam
├── video.rs       codec-agnostic access-unit fan-out
├── auth.rs        JWKS fetch + session-token verification
├── session.rs     which reservation may use each device
├── control.rs     the outbound WSS to the coordinator
├── supervisor.rs  owns every device, routes commands
└── server.rs      session plane (WS) + artifact plane (HTTP)
```

### Config

One real YAML file. Token precedence is `FARM_PROVIDER_TOKEN` > `token_file` >
inline `token`, so a secret need never sit in the file. Unknown keys are
rejected rather than ignored — a typo in a device stanza should not silently
mean "no devices".

Validation happens at load: a missing token is a startup error, not a 401 an
hour later.

### Control plane (`control.rs`)

Replaces `bus.rs` (ZMQ PUSH/SUB) and deletes the `zeromq`, `prost` and `protox`
dependencies with it.

One outbound WSS, reconnecting with exponential backoff (1s → 30s). `hello`
carries the **whole** inventory so the coordinator reconciles rather than
patching. Events are best-effort: when the socket is down a message is dropped
rather than queued, because the next `hello` carries current state anyway and a
queue would only deliver stale device states after a reconnect.

**A coordinator outage does not interrupt streaming.** Authorization lives in
`session.rs` and tokens verify against the cached JWKS, so live viewers keep
working throughout. Verified by drill: kill the coordinator, the session plane
keeps serving 200s and the provider backs off and re-registers on its own.

### Session authorization (`session.rs`, `auth.rs`)

Replaces `group.rs`, which made the *device* the arbiter of ownership and
therefore needed idle-group expiry timers on every device. Here the coordinator
tells the provider the answer, and the provider only enforces it.

A session-plane connection must pass **both** checks:

1. the Ed25519 JWT verifies against the cached JWKS (EdDSA only — accepting any
   other algorithm is the classic JWT confusion bug), and its `providerId`
   matches this provider;
2. its `reservationId` equals what the coordinator last authorized for that
   device.

So a token that is signed, unexpired, and for the right device is still refused
once its reservation is revoked. Revocation is a push, and drops live sockets
immediately rather than waiting out the token.

> **Clock skew leeway is 5s, deliberately.** `jsonwebtoken` defaults to 60,
> which silently doubles the lifetime of a ~60s session token — an expired token
> kept working for another full minute. There is a regression test.

`TokenVerifier::self_test()` runs at boot: verification only happens when a user
opens a session, so without it a broken crypto backend surfaces hours after
deploy. That is not hypothetical — it is exactly what happened when
`jsonwebtoken` 11's crypto-provider feature was missing.

### Session + artifact planes (`server.rs`)

axum. `GET /s/:id` (WebSocket), `GET /s/:id/screenshot.png`,
`POST /s/:id/install`, `GET /health`.

Per-viewer fanout with backlog shedding, ported in spirit from `screen_ws.rs`:
when a viewer lags, everything is dropped until the next keyframe, which is then
promoted to `key-with-reset`. Feeding deltas that reference a frame the viewer
never decoded produces torn output with **no error callback** — the decoder does
not tell you it is wrong.

Uploads stream to the scratch dir as they arrive, so a 200 MB APK never sits in
memory, and a `Drop` guard deletes the staged file however the install turns
out. The filename comes from a client header and is reduced to a single path
component — there are tests asserting `../../etc/passwd` cannot escape.

### Supervisor (`supervisor.rs`)

Replaces `provider.rs`. Owns every device, executes control-plane commands, and
polls each device every 15s, pushing an update only when something actually
changed. `busy` is the coordinator's word, not the provider's — a reserved
device stays reserved across a poll.

## Backend trait

```rust
#[async_trait]
pub trait DeviceBackend: Send + Sync + 'static {
    async fn info(&self) -> Result<DeviceInfo>;
    fn video(&self) -> VideoHandle;
    async fn input(&self, event: InputEvent) -> Result<()>;
    async fn screenshot(&self) -> Result<Vec<u8>>;
    async fn clipboard_get(&self) -> Result<Option<String>>;
    async fn clipboard_set(&self, text: &str) -> Result<()>;
    async fn apps(&self) -> Result<Vec<AppInfo>>;
    async fn install(&self, staged: &Path, progress: &dyn ProgressSink) -> Result<()>;
    async fn uninstall(&self, app_id: &str) -> Result<()>;
    async fn launch(&self, app_id: &str, args: &[String]) -> Result<()>;
    async fn rotate(&self, degrees: i64) -> Result<()>;
    async fn reboot(&self) -> Result<()>;
    async fn remote_debug(&self) -> Result<RemoteDebug>;      // android only
    async fn restart(&self) -> Result<()>;
    async fn is_healthy(&self) -> bool;
}
```

`install` takes a **path**, not bytes, because the upload has already been
streamed to disk. Pointer coordinates in `InputEvent` are **normalised 0..1**,
so the browser needs no knowledge of the true resolution and a mid-gesture
rotation cannot desynchronise an in-flight event.

`PointerDown`/`Move`/`Up` are deliberately three variants rather than one `tap`.
The backend's pointer state machine needs them to tell a drag from a tap — the
regression `stf-ios-provider/src/control.rs` documents at length.

## `backend-ios` — phase 3b

Ports, essentially unchanged, from `../../stf-ios-provider/src/`:

| File | What it carries |
|---|---|
| `device/mod.rs` | `DeviceHost` supervisor, RSD/CoreDeviceProxy session, `DeviceInfo` |
| `device/media.rs` | RTP/HEVC depacketization, RTCP receiver reports, PLI/FIR keyframe recovery, motion-IDR loop, stall watchdog |
| `device/hid.rs` | HID report encoding, `to_hid` normalization, key tables |
| `control.rs` | Every device operation, and the `Pointer` state machine |

> **`device/media.rs` carries hard-won behaviour. Do not touch it.** Every line
> in the keyframe-recovery and stall-watchdog paths exists because something
> failed in the field.

The work is to implement `DeviceBackend` by delegating to those, and to delete
`bus.rs`, `wire.rs`, `group.rs`, `provider.rs` and `provider.sh` — all of which
`provider-core` now replaces. `MediaHandle` becomes `video::VideoPublisher`.

## `backend-android` — phase 4

- **adb** — bundle `adb` from platform-tools, run a provider-local adb server,
  and talk the adb host protocol (4-hex-length-prefixed commands) for
  `host:track-devices` hotplug, `sync:` push, `shell:`, `localabstract:`.
- **Video** — push a pinned `scrcpy-server` jar, launch via `app_process`, read
  the codec-metadata header, split H.264 config packets into an `avcC`
  description and frames into `AccessUnit`. **Zero transcode.**
- **Input/clipboard** — scrcpy's control socket covers touch, keycodes, text,
  `GET_CLIPBOARD`/`SET_CLIPBOARD`, back/home/power and rotation.
- **Apps** — `pm install`/`list`/`uninstall`, `am start`.
- **Screenshot** — `screencap -p` over `exec:`.
- **Remote debug** — bind a per-device port proxying into
  `host:transport:<serial>`, so a developer runs `adb connect <host>:<port>`.
  Same trick as STF's `connect.js` + `remotedebug.js`, minus the RethinkDB
  round-trip.

### ⚠️ Risk

The adb host-protocol client and the scrcpy handshake are the **two
highest-uncertainty items in the project**, and the scrcpy framing is
version-sensitive. Pin an exact `scrcpy-server`, vendor the jar, write the
handshake against *that version's* `DeviceMessage`/`ControlMessage` layout, and
validate against a real device before building any UI on top.

## Docker

`debian:bookworm-slim` rather than `distroless/cc`, because phase 4 needs the
`adb` binary and the scrcpy jar. `ca-certificates` is **required**, not
optional: the control plane and JWKS fetch both use the OS trust store, which is
what lets an on-prem farm terminate TLS with a private CA.

The builder uses the stub-source dependency-cache trick from
`stf-ios-provider/Dockerfile`; dependency compilation dominates the build.

## Testing

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

`crates/provider-core/tests/session_plane.rs` runs the real axum server against
the real mock backend, with a locally generated Ed25519 key served as a JWKS by
a throwaway server — the whole browser-facing surface, in-process, with no
coordinator, database or device. Most of what it asserts is a security boundary:
forged tokens, expired tokens, tokens for another device, tokens whose
reservation was revoked, and upload filenames trying to escape the scratch dir.
