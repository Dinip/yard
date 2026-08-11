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
| `backend-ios` | ✅ CoreDevice over a root-free RSD tunnel, iOS 17.4+ |
| `backend-android` | ✅ adb host protocol + scrcpy, no transcode |

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
├── metrics.rs     device sampling + the Prometheus exposition
├── origins.rs     browser origins allowed on the browser-facing planes
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
`GET /s/:id/mjpeg`, `GET /s/:id/files`, `GET /s/:id/file`,
`POST /s/:id/install`, `GET /health`. The Prometheus exposition is deliberately
**not** here — it gets a listener of its own; see [Metrics](#metrics).

Per-viewer fanout with backlog shedding, ported in spirit from `screen_ws.rs`:
when a viewer lags, everything is dropped until the next keyframe, which is then
promoted to `key-with-reset`. Feeding deltas that reference a frame the viewer
never decoded produces torn output with **no error callback** — the decoder does
not tell you it is wrong.

The artifact plane is **always** cross-origin — the browser loads the app from
the coordinator and connects straight here — so it carries a CORS layer whose
allowed origins arrive in `hello.ack` (`origins.rs`). They come from the
coordinator because that is where policy lives; a provider configured separately
would drift the first time the web app moved. Before registration the list is
empty and browser requests are refused, so it fails closed. Credentials are
never allowed: the token is in the query string, never a cookie.

Uploads stream to the scratch dir as they arrive, so a 200 MB APK never sits in
memory, and a `Drop` guard deletes the staged file however the install turns
out. The filename comes from a client header and is reduced to a single path
component — there are tests asserting `../../etc/passwd` cannot escape.

### Supervisor (`supervisor.rs`)

Replaces `provider.rs`. Owns every device, executes control-plane commands, and
polls each device every 15s, pushing an update only when something actually
changed. `busy` is the coordinator's word, not the provider's — a reserved
device stays reserved across a poll.

### Metrics

Device CPU, memory, temperature and per-app usage, for Prometheus. Off unless
`provider.yaml` has a `metrics:` block — writing the block at all turns it on,
so an omitted `enabled` means yes.

```yaml
metrics:
  bind: 0.0.0.0:9100
  interval_secs: 30
  app_patterns: ["*.demo.*"]   # Android only, globbed on the process name
```

```yaml
# prometheus.yml
scrape_configs:
  - job_name: farm-provider
    static_configs:
      - targets: ["lab-1.example.com:9100"]
```

**A listener of its own, not a route on 7100.** The session port is
browser-facing, carries a CORS layer and session tokens, and is publicly
TLS-terminated; a scraper has none of those, and a `/metrics` added there would
inherit the CORS layer. There is deliberately **no auth** on the metrics port —
bind it to an interface only your monitoring reaches. It is not a fifth plane;
see [ARCHITECTURE.md](./ARCHITECTURE.md).

**`farm_device_status` is reconstructed, not reported.** A provider's own status
is only ever `preparing`, `ready` or `unhealthy` — `busy` is the coordinator's
word, set when a reservation goes active, and the supervisor deliberately never
writes it. Exporting the raw value made `busy` a permanently dead series and made
every *reserved* device report `ready`, which on a dashboard reads as "free, take
it" about a device someone is using. So the exporter combines the status with the
session registry, which is the same fact the coordinator sets `busy` for and the
same one that admits a viewer to the session plane. Health still wins over
occupancy: a broken phone that happens to be reserved reports `unhealthy`. None of
this changes what goes upstream — the control plane still never says `busy`.

**The scrape reads a cache.** A background task samples every device on
`interval_secs`, eight at a time, each under a timeout of half the interval. The
supervisor's own poll loop is sequential but this one cannot be: a sample is
several adb round trips plus a `dumpsys` that walks every process, so devices in
series would spend a third of the interval in one pass and one wedged device
would starve the rest. Sampling on scrape instead would put that on the request
path and let a second scraper double the load on the phones.

**CPU is a counter, not a percent.** `/proc/stat` is already monotonic, so
exporting `_total` means nothing here holds a previous sample, `rate()` gives the
operator any window they want rather than the one we picked, and a rebooted
device's counter reset is handled for free. `memory_pct` and a `mode="total"`
series are likewise absent: both are derivable, and a `total` mode would
double-count under `sum without(mode)`.

**A stale or failed device emits none of its device-sourced series.** Absence is
how Prometheus spells "no data" — `rate()` and `avg_over_time` handle a gap, and
`absent()` alerts on it. Re-serving the last known value would be a lie: an
unplugged phone would show a flat, healthy 40 °C forever. The operational
metrics (`farm_device_status`, viewers, install counts, error counts) are read
live and are never suppressed, which is what keeps *the device is gone* — status
still reported, no CPU series — distinguishable from *the exporter is broken*,
where there is nothing at all. A backend that answers `Unsupported` is a success
holding nothing, not an error; otherwise every iOS device, which has no CPU or
memory to give, would look permanently broken.

**Per-app metrics are the cardinality risk.** A bare `*` pattern is refused at
config load, and a sample is capped at 32 processes per device — the cap is the
real defence, since `com.*` is just as expensive and impossible to reject on
sight. With no patterns configured the backend skips the expensive read entirely.

Android costs **two adb round trips per device per interval**, three with app
patterns: `/proc/stat`, `/proc/meminfo`, `dumpsys battery` and the thermal zones
are batched into one `sh -c`, because each read is ~5-40 ms of device work
against ~15-30 ms of transport setup. None of it goes near `info()`, which
already makes 3-5 round trips every 15s on the supervisor's cadence.

The exposition format is written by hand rather than through a registry crate.
Both `prometheus-client` and `metrics-exporter-prometheus` *retain* every label
set they have seen, and making a device's series disappear through one of those
is more code than emitting the bytes. The encoder buffers per family, because
samples are gathered device-major and Prometheus requires a family's samples
contiguous under one `# HELP`/`# TYPE`.

`metrics: enabled: false` does not skip spawning anything — the listener and the
sampler both park. That keeps `main.rs` to one code path, where every long-lived
task is spawned, aborted on shutdown, and fatal if it returns.

## `backend-ios`

```
crates/backend-ios/src/
├── device.rs   tunnel + session supervision (one rebuild loop)
├── media.rs    RTP in, RTCP out, access units — carried over verbatim
├── hevc.rs     RFC 7798 depacketisation, hvcC, codec string, frame size
├── hid.rs      touch, keyboard, hardware buttons, rotation
└── lib.rs      the pointer state machine and the DeviceBackend impl
```

**Battery level and charging state, and nothing else.** Not CPU, not memory, and
— on the hardware checked — not temperature either. `examples/diagnostics_probe.rs`
is how that was established, against a real iPhone 13, and the negative result is
recorded here so the question stops being re-asked:

- `all()` returns **752 bytes**. There is nothing resembling CPU, memory or
  thermal in it. `host_statistics`/`vm_statistics` are available to on-device
  code only, and this relay is a lockdown service with a fixed dictionary and no
  `sysctl` surface.
- **`gasguage` is not the battery source**, despite the name. It answers
  `CycleCount`, `FullChargeCapacity` and `Status`, nested under a `GasGauge` key,
  with no charge level in it at all.
- **`ioregistry` on `AppleSmartBattery` is.** `CurrentCapacity` and `MaxCapacity`
  are both percentages there, alongside `IsCharging` and `ExternalConnected`.
- **No `Temperature` key exists on either.** The names under `IOReportLegend`
  (`BatteryMaxTemp` and friends) are channel labels, not readings.

So the registry is tried first and the gas gauge is the fallback, which is the
opposite of what the service names suggest. Each source is judged by whether a
*level* came out of it rather than by whether the dictionary was non-empty —
the gas gauge's reply is non-empty and useless, so an emptiness check takes it and
never falls through. That was a real bug, caught only on hardware.

The temperature parsing is kept anyway, since it costs nothing and another iOS
version may populate it; `farm_device_battery_temperature_celsius` is simply
absent for an iPhone, which is what an absent series is for. `metrics()` answers
`Ok` with mostly `None` rather than `Unsupported`, so a healthy iPhone does not
advance the exporter's error counter.

Battery was previously not reported at all, because a relay round trip is far too
expensive on `info()`'s 15s cadence. It is now cached with a 60s TTL and shared:
the metrics sampler refreshes it on its own cadence and `info()` rides on that,
so the worst case is one round trip a minute instead of four.

iOS 17.4+ only: below that the root-free `CoreDeviceProxy` tunnel does not
exist, and bring-up fails loudly rather than half-working. There is no other
iOS backend to hand an older device to.

**USB only.** usbmuxd lists a Wi-Fi-synced device *twice* — once `USB`, once
`Network` — in no guaranteed order, and `idevice`'s `get_device` is a `find`, so
the transport the whole session was built over came down to list order.
`pick_usb` takes the USB entry and refuses a device offered only over the
network, saying so: the tunnel is built and tested over USB, and a session that
silently came up over Wi-Fi fails later, in the media path, where the cause is
invisible.

One supervisor task rebuilds tunnel, media and HID **together**, because they
are not independent — the HID surfaces authenticate against the live media
stream, so a media restart invalidates them and a tunnel drop invalidates
everything. That is also why `restart` just closes the adapter: there is no
finer-grained restart to offer.

`media.rs` is the file to leave alone. Receiver reports are not optional (the
encoder stalls in ~20 s without them), audio is started and then ignored on
purpose (iOS throttles a lone video client), corrupt access units are dropped
whole, and keyframe requests are rate-limited because the iOS 26/27 encoder
wedges under a PLI barrage. Every constant marks a field failure.

Display geometry comes from the SPS. `mobilegestalt` — what `stf-ios-provider`
read — answers `MobileGestaltDeprecated` on iOS 26/27, so it is a fallback for
older devices only.

## `backend-android`

```
crates/backend-android/src/
├── adb.rs      the adb server's host protocol — transport, shell, sync, forward
├── scrcpy.rs   pushing/starting the server, its sockets, its control protocol
├── h264.rs     Annex-B → avcC, so the browser gets the framing it wants
└── lib.rs      session supervision and the DeviceBackend impl
```

The provider speaks adb's TCP protocol directly instead of shelling out: one
dependency instead of a subprocess per operation. Two framings live in that one
protocol and mixing them **hangs rather than errors** — the host protocol uses a
4-digit hex length prefix, the sync subprotocol a 4-byte little-endian one.

`adb_server` is per-device config, defaulting to `127.0.0.1:5037`. A Linux
provider bundles adb in its image with `/dev/bus/usb` mounted (the directory,
not a `--device` node: a phone that re-enumerates after a reboot gets a new node
and a static binding would silently lose it) and leaves the default alone.
macOS cannot pass USB into Docker at all, so there the provider points at the
host's adb server.

Because the backend never shells out, *something else* has to start the server
that owns the USB transport: `docker-entrypoint.sh` runs `adb start-server`
before exec'ing the provider. Without it the container has the `adb` binary and
no server, and every session retries against a refused connection.

**Remote debugging is the provider's own listener**, not `adb forward`, whose
socket binds the adb server's loopback and so is unreachable from a container.
Each connection is spliced to the device's `adbd` over the USB transport. Ports
come from the `remote_debug.ports` pool, claimed while exposed and returned on
release, because they have to be published by whoever runs the provider.

The scrcpy server is **embedded in the binary** and pushed to the phone at
session start, so nothing is installed on a provider host for it. It is started
with `tunnel_forward=true`, which makes it listen on a device-side abstract
socket that the adb transport can open directly — no `adb reverse`, no host port
per device.

Its wire format was read off a real device by `examples/scrcpy_probe.rs`, not
inferred from scrcpy's source, because that source changes between releases.
Re-run the probe when bumping the pin: the version string in `scrcpy.rs` must
match the jar, and the server refuses a mismatch rather than half-working.

Video arrives as Annex-B and is rewritten to length-prefixed NALUs, with the
config packet's SPS/PPS lifted into an `avcC` sent once out of band. Both halves
matter: the Annex-B path tears in Chrome under motion.

**Not everything on that socket is a packet.** A reset or a resize re-sends the
session block bare — four bytes of flags, then width and height — and reading it
as a packet header desynchronises the stream permanently, with a black screen as
the only symptom until a garbage length eventually fails an allocation. Bit 31
of the first word says which it is. A viewer connecting requests a keyframe,
which is a reset, so this is the common path rather than an edge case.

Sessions run with `stay_awake` and `power_on`. A device left to its own screen
timeout goes black, and a black stream from a dozing phone is indistinguishable
from a broken one until you run `adb screencap` and see the same black.

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
    fn files_root(&self) -> Option<&'static str>;              // None = no file access
    async fn list_files(&self, path: &str) -> Result<FileListing>;
    async fn pull_file(&self, path: &str, dest: &Path) -> Result<u64>;
    async fn rotate(&self, degrees: i64) -> Result<()>;
    async fn reboot(&self) -> Result<()>;
    async fn remote_debug(&self) -> Result<RemoteDebug>;      // android only
    async fn restart(&self) -> Result<()>;
    async fn is_healthy(&self) -> bool;
    async fn metrics(&self, apps: &AppFilter) -> Result<DeviceMetrics>;
}
```

`install` takes a **path**, not bytes, because the upload has already been
streamed to disk. `pull_file` takes one for the mirror-image reason: it writes
into the scratch directory rather than answering `Vec<u8>`, so a video coming
off a phone never sits in the provider's memory. The handler serves that file
and deletes it with the response body.

`files_root` is what a backend says about its own reach, rather than the web app
branching on a platform, and a backend that answers `None` makes `/files` a 501.

**Android** opens at `/sdcard`, and navigation is *not* fenced to it: adb runs
unrooted, so the device's own permissions are the real boundary and a second one
drawn in the provider would only be decoration.

**iOS has no single filesystem**, so its root is synthetic and its paths carry a
scheme the browser treats as opaque:

| Path | Service |
|---|---|
| `/` | Neither — a listing of the two trees below |
| `media:/DCIM` | `com.apple.afc`: photos, downloads, books |
| `app:<bundle>:/Documents` | `house_arrest` `VendDocuments`, per app |

The app tree starts at `/Documents` rather than the container root because a
`VendDocuments` session **answers `Afc(PermDenied)` for everything above it** —
an error that reads exactly like a broken feature. Apps are listed by
`UIFileSharingEnabled`, which is the same flag that decides whether they appear
under "On My iPhone" in the Files app, so the browser shows the set the phone
shows. Where an app suite ships several bundles under one display name, the
ambiguous rows carry their bundle id and the rest stay clean. Pointer coordinates in `InputEvent` are **normalised 0..1**,
so the browser needs no knowledge of the true resolution and a mid-gesture
rotation cannot desynchronise an in-flight event.

`PointerDown`/`Move`/`Up` are deliberately three variants rather than one `tap`.
The backend's pointer state machine needs them to tell a drag from a tap — the
regression `stf-ios-provider/src/control.rs` documents at length.

`InputEvent::Key` carries the browser's key *name*, not a keycode, and each
backend owns its own table: `keycode_for` in `backend-android` (scrcpy
`INJECT_KEYCODE`, one injection per edge) and `named_button` then `special_key`
in `backend-ios` (HID, where a hardware button presses and releases on the down
edge and the up edge is discarded). An unmapped name is logged and dropped
rather than guessed at. The full vocabulary is in
[PROTOCOL.md](PROTOCOL.md#session-plane--browser--provider--built) — note in
particular that `Home` is the hardware button and `MoveHome` is the keyboard
key, which `named_button` being tried first on iOS is exactly why.

## Docker

`debian:bookworm-slim` rather than `distroless/cc`, because the Android backend needs the
`adb` binary and the scrcpy jar. `ca-certificates` is **required**, not
optional: the control plane and JWKS fetch both use the OS trust store, which is
what lets an on-prem farm terminate TLS with a private CA.

The builder uses the stub-source dependency-cache trick from
`stf-ios-provider/Dockerfile`; dependency compilation dominates the build.

The runtime stage runs as **root**, unlike the coordinator's. `privileged: true`
grants access to the USB devices, not permission on them — the `/dev/bus/usb`
nodes are root-owned, and the adb server has to write to them. The entrypoint
starts that server; the container's only writable path is the scratch tmpfs.

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
