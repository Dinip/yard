# `packages/provider` (Rust) — phases 3–4

> **Status: not yet built.** This document is the design contract, written up
> front so the port has a target. Update it as the code lands.

One binary, one process per host, supervising every device configured for that
host.

`stf-ios-provider` runs one *container per device*. Consolidating to one process
keeps crash isolation — each device gets a restartable supervisor task tree,
which `DeviceHost::spawn` in `src/device/mod.rs` already is — while collapsing N
containers, N ZMQ connections and N config files into one.

## Planned layout

```
packages/provider/
├── src/main.rs
└── crates/
    ├── farm-protocol/    generated Rust mirror of packages/protocol
    ├── provider-core/    supervisor, config, control client, session server, storage
    ├── backend-ios/      ported from stf-ios-provider
    └── backend-android/  adb + scrcpy
```

## What ports over essentially unchanged

From `../../stf-ios-provider/src/` (kept on disk, see [REFERENCES.md](./REFERENCES.md)):

| File | What it carries |
|---|---|
| `device/mod.rs` | `DeviceHost` supervisor, RSD/CoreDeviceProxy session, `DeviceInfo` |
| `device/media.rs` | RTP/HEVC depacketization, RTCP receiver reports, PLI/FIR keyframe recovery, motion-IDR loop, stall watchdog |
| `device/hid.rs` | HID report encoding, `to_hid` normalization, key tables |
| `control.rs` | tap/swipe/type/rotate/clipboard/launch/list/uninstall/install/screenshot/reboot/display, and the `Pointer` state machine |
| `screen_ws.rs` | per-viewer fanout, codec handshake, backlog shedding, ping keepalive |
| `health.rs`, `config.rs` | reshaped for multi-device |

> **`device/media.rs` carries hard-won behaviour. Do not touch it.** Every line
> in the keyframe-recovery and stall-watchdog paths exists because something
> failed in the field.

## What is replaced

| Old | New | Effect |
|---|---|---|
| `bus.rs` (ZMQ PUSH/SUB) | `provider-core/src/control.rs` — one outbound WSS, reconnect with backoff, bearer auth, JSON envelopes | Deletes `zeromq` and `prost`/`protox` deps |
| `wire.rs`, `proto/wire.proto` | `farm-protocol` (generated) | Deletes ~700 lines of STF protobuf mapping |
| `group.rs` | `provider-core/src/session.rs` | No ownership arbitration, no idle-group timers. The provider holds the coordinator-pushed `reservationId` per device and validates each session JWT against it |
| `provider.rs` | `provider-core/src/device_actor.rs` | One actor per device, multiplexing control-plane commands and session-plane input onto the backend trait |
| `provider.sh` (13.5k of awk-parsed YAML + docker orchestration) | one YAML file parsed by `serde_yaml`, one container | Deleted |

## Backend trait

```rust
#[async_trait]
trait DeviceBackend: Send + Sync {
    async fn info(&self) -> Result<DeviceInfo>;
    fn video(&self) -> VideoHandle;              // codec description + AU broadcast
    async fn input(&self, event: InputEvent) -> Result<()>;   // touch/key/text
    async fn screenshot(&self) -> Result<Vec<u8>>;
    async fn clipboard_get(&self) -> Result<Option<String>>;
    async fn clipboard_set(&self, text: &str) -> Result<()>;
    async fn apps(&self) -> Result<Vec<AppInfo>>;
    async fn install(&self, staged: &Path, progress: ProgressSink) -> Result<()>;
    async fn uninstall(&self, id: &str) -> Result<()>;
    async fn launch(&self, id: &str, args: &[&str]) -> Result<()>;
    async fn rotate(&self, degrees: i32) -> Result<()>;
    async fn reboot(&self) -> Result<()>;
    async fn remote_debug(&self) -> Result<Option<RemoteDebug>>; // android only
}
```

`install` takes a **path** because the upload has already been streamed to the
provider's scratch dir by the artifact plane. The iOS path then reuses
`control.rs`'s existing AFC push to `PublicStaging` + `installation_proxy` flow
unchanged. The staged file is removed in a `Drop` guard, so a failed install
cannot leak disk.

`VideoHandle` is `media::MediaHandle` generalised over codec.

## `backend-android` (phase 4)

- **adb** — bundle `adb` from platform-tools in the image, run a provider-local
  adb server, and talk to it over the adb host protocol (4-hex-length-prefixed
  commands) for `host:track-devices` hotplug, `sync:` push, `shell:`, and
  `localabstract:` forwards.
- **Video** — push a pinned `scrcpy-server` jar, launch it via `app_process`,
  connect to its video socket, read the codec-metadata header, split H.264
  config packets into an `avcC` description and frames into the same
  `[type byte][AU]` framing the iOS path uses. **Zero transcode**; the browser's
  `VideoDecoder` does the work.
- **Input/clipboard** — scrcpy's control socket covers injected touch, keycodes,
  text injection, `GET_CLIPBOARD`/`SET_CLIPBOARD`, back/home/power and rotation:
  the entire input requirement in one protocol. (STF used minitouch + `input`
  shell; scrcpy is strictly better and is what upstream tooling standardised on.)
- **Apps** — `pm install` via adb sync push + shell, `pm list packages` +
  dumpsys for the list, `pm uninstall`, `am start`.
- **Screenshot / MJPEG source** — `screencap -p` over `exec:`.
- **Remote debugging** — on `device.adb.expose`, bind a per-device TCP port on
  the provider host and proxy each accepted connection into an
  `host:transport:<serial>` transport on the local adb server, so a developer
  runs `adb connect <providerHost>:<port>`. Same trick as STF's
  `lib/units/device/plugins/connect.js` + `remotedebug.js`, minus the RethinkDB
  round-trip. The port is reported to the coordinator and surfaced in the UI.

### ⚠️ Risk

The adb host-protocol client and the scrcpy handshake are the **two
highest-uncertainty items in the entire project**, and the scrcpy framing is
version-sensitive.

Pin an exact `scrcpy-server` version, vendor the jar, and write the handshake
against *that version's* `DeviceMessage`/`ControlMessage` layout. Validate both
against a real device before building any UI on top of them.

## Docker

Adapt the existing two-stage build (`../../stf-ios-provider/Dockerfile`),
including its stub-binary dependency-cache trick and the `cmake`/`clang` needed
by `aws-lc-sys`.

The runtime image changes from `distroless/cc` to `debian:bookworm-slim`,
because the Android backend needs the `adb` binary and the vendored
`scrcpy-server` jar. Keep the `--health` self-probe healthcheck.

The compose service is behind a `provider` profile — it needs host device access
(usbmuxd socket for iOS, `/dev/bus/usb` for Android) and a tmpfs scratch volume
for staged uploads. Nothing else in the system is stateful except Postgres.
