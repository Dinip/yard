# Reference sources

Three read-only repositories live **outside** this one, as siblings on disk:

```
~/dev/farm/
├── device-farm/         ← this repo
├── stf/                 DeviceFarmer STF 3.7.1 — the system being replaced
├── stf-ios-provider/    Rust iOS provider — the device layer being kept
└── idevice/             Rust CoreDevice/lockdown library used by the above
```

They are **not vendored and not submodules**. They are reference material: read
them, port from them, do not depend on them. Nothing in this repo may `import`
or `include!` from those paths.

## `stf-ios-provider/` — port from this

~4.8k LOC of well-documented Rust driving iOS 17.4+ over a root-free CoreDevice
RSD tunnel. HEVC/RTP straight to the browser's WebCodecs decoder with no
transcode, HID touch/keyboard, AppService app install/launch/uninstall,
ScreenCapture, Pasteboard, Diagnostics reboot.

It exists only to impersonate an STF device unit — `bus.rs` speaks STF's ZMQ
PUSH/SUB and `wire.rs`/`provider.rs`/`group.rs` reimplement STF's protobuf
contract and reservation semantics. **Those four files are exactly what the iOS
throws away.** Everything below them is kept.

| Read this | For |
|---|---|
| `src/device/media.rs` | RTP/HEVC depacketization, keyframe recovery, stall watchdog. The highest-value file in the repo |
| `src/device/mod.rs` | `DeviceHost` supervisor, RSD session lifecycle |
| `src/device/hid.rs` | HID encoding, key tables |
| `src/control.rs` | Every device operation, and the `Pointer` state machine (see its docs on the drag-vs-tap regression) |
| `src/screen_ws.rs` | Multi-viewer fanout with per-viewer backlog shedding |
| `src/frontend/hevc-screen.js` | The renderer to port into `packages/web/src/lib/screen/` |
| `src/frontend/INTEGRATION.md` | Precise wire framing for video AUs |
| `Dockerfile` | Two-stage build, stub-binary dep cache, `cmake`/`clang` for `aws-lc-sys` |

## `stf/` — learn from, do not port

~26k LOC of CommonJS across 212 files, AngularJS 1.x built with gulp + bower +
webpack 3, RethinkDB, ZeroMQ `triproxy` fanout. The build chain is unbuildable
on a current toolchain without archaeology, which is a large part of why this
project exists.

Worth reading anyway, for the parts that encode real operational knowledge:

| Read this | For |
|---|---|
| `lib/units/device/plugins/connect.js`, `remotedebug.js` | The adb transport-proxy trick reimplemented in `backend-android` |
| `lib/units/ios-device/plugins/group.js` | The ownership model being deliberately abandoned — read it to understand why device-owned arbitration needs idle timers |
| `lib/units/poorxy` | The MJPEG proxy that made the app server the bottleneck. The thing this architecture exists to avoid |

## `idevice/`

The Rust library `stf-ios-provider` uses for lockdown/CoreDevice/RSD. Consult it
when the iOS backend's device layer misbehaves; it is a normal cargo dependency
of the ported code, not something to copy.

## Version pinning note

`stf-ios-provider` targets iOS 17.4+ specifically because that is when
CoreDevice's root-free RSD tunnel became workable. Do not "simplify" the tunnel
setup without a device on an older OS to prove it still works.
