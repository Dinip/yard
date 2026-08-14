//! iOS 17.4+ device backend, driving CoreDevice over a root-free RSD tunnel.
//!
//! Ported from `stf-ios-provider`, whose device layer this keeps almost
//! unchanged — see the module headers for what moved and why. What is gone is
//! everything that existed to talk to STF: ZeroMQ, the protobuf wire, the
//! per-device container, WebDriverAgent. What is new is only this file, which
//! adapts the ported layer to [`DeviceBackend`].
//!
//! ```text
//!   device.rs   tunnel + session supervision   (RSD, one rebuild loop)
//!   media.rs    RTP in, RTCP out, access units (the hard-won part)
//!   hevc.rs     depacketisation, hvcC, codec string
//!   hid.rs      touch, keyboard, buttons, rotation
//!   lib.rs      the pointer state machine and the trait impl
//! ```
//!
//! Requires iOS 17.4+: below that the root-free CoreDeviceProxy tunnel does not
//! exist, and the backend fails loudly at session bring-up rather than
//! half-working.

pub mod app_list;
pub mod device;
pub mod hevc;
pub mod hid;
pub mod media;

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context as _, Result};
use async_trait::async_trait;
use farm_protocol::{AppInfo, Display, FileEntry, FileKind, FileListing, Platform};
use idevice::core_device::{
    AppServiceClient, ImageFormat, PasteboardPayload, PasteboardServiceClient, RotationDirection,
    ScreenCaptureServiceClient, GENERAL_PASTEBOARD,
};
use idevice::diagnostics_relay::DiagnosticsRelayClient;
use idevice::installation_proxy::InstallationProxyClient;
use idevice::provider::IdeviceProvider;
use idevice::services::afc::{opcode::AfcFopenMode, AfcClient};
use idevice::services::house_arrest::HouseArrestClient;
use idevice::{IdeviceService as _, ReadWrite};
use provider_core::backend::{
    join_path, parent_of, AppFilter, BackendError, DeviceBackend, DeviceInfo, DeviceMetrics,
    InputEvent, ProgressSink, Result as BackendResult,
};
use provider_core::video::{channel, VideoGeometry, VideoHandle, VideoPublisher};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::app_list::AppList;
use crate::device::{connect_service, DeviceHost};
use crate::hid::Input;

/// Where an IPA is staged on the device before `installation_proxy` installs it.
const STAGING_DIR: &str = "PublicStaging";

/// Read size for pulling a file off the device over AFC.
const AFC_CHUNK: usize = 256 * 1024;

/// Where iOS file browsing can actually reach, as a path the browser carries
/// around opaquely.
///
/// An iPhone has no single filesystem to walk. `com.apple.afc` vends the *media*
/// domain — `DCIM`, `Downloads`, `Books` — while everything a user saves through
/// "Save to Files → On My iPhone" lands in some **app's** container, which is a
/// separate `house_arrest` vend per app and cannot be reached from the media
/// root at all. Those are two disjoint trees, so the path carries which one it
/// means rather than pretending there is one root above them.
enum IosPath {
    /// The synthetic top: lists the media domain and every app that shares files.
    Root,
    /// `media:<absolute>` — the AFC media domain.
    Media(String),
    /// `app:<bundle>:<absolute>` — one app's Documents, via `house_arrest`.
    App { bundle: String, path: String },
}

const MEDIA_PREFIX: &str = "media:";
const APP_PREFIX: &str = "app:";

/// The only directory a `VendDocuments` AFC session may read.
///
/// The vend hands over the app's *container*, then denies everything in it
/// except this — listing `/` answers `Afc(PermDenied)`, which reads exactly
/// like a broken feature. So the app tree starts here rather than one level up
/// at a root that exists and refuses.
const APP_DOCUMENTS: &str = "/Documents";

impl IosPath {
    fn parse(raw: &str) -> Self {
        if let Some(rest) = raw.strip_prefix(MEDIA_PREFIX) {
            return Self::Media(normalize(rest));
        }
        if let Some(rest) = raw.strip_prefix(APP_PREFIX) {
            // `app:<bundle>:<path>` — split on the first colon only, since a
            // bundle id never contains one and a path may.
            if let Some((bundle, path)) = rest.split_once(':') {
                return Self::App {
                    bundle: bundle.to_owned(),
                    path: normalize(path),
                };
            }
        }
        Self::Root
    }

    /// The path one level up, or `None` at a tree's own root — which is what
    /// makes the browser show ".." exactly where there is somewhere to go.
    fn parent(raw: &str) -> Option<String> {
        match Self::parse(raw) {
            Self::Root => None,
            // Up from the top of either tree is the synthetic root, so the two
            // disjoint trees are still navigable as one browse.
            Self::Media(path) if path == "/" => Some("/".into()),
            Self::App { path, .. } if path == APP_DOCUMENTS => Some("/".into()),
            Self::Media(path) => Some(format!("{MEDIA_PREFIX}{}", parent_of(&path)?)),
            Self::App { bundle, path } => {
                Some(format!("{APP_PREFIX}{bundle}:{}", parent_of(&path)?))
            }
        }
    }
}

/// Deletes an upload from the staging directory, on its own AFC connection.
///
/// A fresh connection because the install's was closed with the file: this runs
/// after `install_with_callback` either way, including the failure path, and
/// reusing a client that may have died with the install would turn a leak into
/// a second error.
async fn remove_staged(provider: &dyn IdeviceProvider, path: &str) -> Result<()> {
    let mut afc = AfcClient::connect(provider)
        .await
        .map_err(|err| anyhow!("afc connect: {err:?}"))?;
    afc.remove(path)
        .await
        .map_err(|err| anyhow!("remove {path}: {err:?}"))?;
    Ok(())
}

fn normalize(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Longest a contact may stay down with no further samples before the provider
/// lifts it on its own. Generous enough for a deliberate long-press, short
/// enough that a browser that lost its pointerup does not leave a finger pinned
/// to the glass for the rest of the session.
const CONTACT_MAX: Duration = Duration::from_secs(10);

/// How long to wait for a session when a request needs one.
const SESSION_WAIT: Duration = Duration::from_secs(20);

/// How long an app listing may take end to end, stream included. Under the
/// coordinator's 15s command timeout so a slow device surfaces as itself rather
/// than as an unresponsive provider.
const APPS_TIMEOUT: Duration = Duration::from_secs(12);

/// How stale a battery reading `info()` will accept before paying for a fresh
/// diagnostics round trip.
///
/// The poll loop runs every 15s; without this, iOS would make four relay round
/// trips a minute for a number that moves by fractions of a percent. Metrics
/// sampling refreshes it on its own cadence, so with metrics enabled at any
/// interval below this, `info()` never makes one at all.
const BATTERY_TTL: Duration = Duration::from_secs(60);

/// Which diagnostics call a battery reading came from. See [`IosBackend::read_battery`]
/// for why the registry is tried first.
#[derive(Debug, Clone, Copy)]
enum BatterySource {
    Registry,
    GasGauge,
}

/// Reads a gas-gauge or `AppleSmartBattery` dictionary.
///
/// Key spellings and units vary by iOS version, which is what
/// `examples/diagnostics_probe.rs` exists to settle on a real device. Percentage
/// is preferred where offered; capacity over max capacity is the fallback, since
/// `MaxCapacity` is the *design* capacity on some versions and the current full
/// charge on others — either way it is the right denominator for a charge level.
pub fn parse_battery(values: &plist::Dictionary) -> BatteryReading {
    // `gasguage` nests its answer under a `GasGauge` key, the same shape
    // `mobilegestalt` uses. `ioregistry` does not nest, so this unwraps when
    // there is something to unwrap and passes through otherwise.
    let values = values
        .get("GasGauge")
        .and_then(plist::Value::as_dictionary)
        .unwrap_or(values);

    let number = |key: &str| -> Option<f64> {
        values.get(key).and_then(|value| {
            value
                .as_real()
                .or_else(|| value.as_signed_integer().map(|n| n as f64))
                .or_else(|| value.as_unsigned_integer().map(|n| n as f64))
        })
    };
    let boolean = |key: &str| -> Option<bool> {
        values.get(key).and_then(|value| {
            value
                .as_boolean()
                .or_else(|| value.as_unsigned_integer().map(|n| n != 0))
        })
    };

    let level = number("CurrentCapacity")
        .and_then(|current| match number("MaxCapacity") {
            Some(max) if max > 0.0 => Some(current / max),
            // No usable denominator: some versions report `CurrentCapacity` as
            // an outright percentage instead. Anything above 100 there is a raw
            // mAh reading with its `MaxCapacity` missing, which is unusable —
            // better no series than a battery pinned at 100%.
            _ => (current <= 100.0).then_some(current / 100.0),
        })
        .map(|level| level.clamp(0.0, 1.0));

    BatteryReading {
        level,
        charging: boolean("IsCharging").or_else(|| boolean("ExternalConnected")),
        temperature_c: number("Temperature").and_then(scale_battery_temperature),
    }
}

/// The gas gauge reports temperature in hundredths of a degree, and a few iOS
/// versions in tenths. Guessing by magnitude is the only option — the same
/// problem, and the same shape of answer, as Android's thermal zones.
fn scale_battery_temperature(raw: f64) -> Option<f64> {
    let celsius = if raw.abs() > 1000.0 {
        raw / 100.0
    } else if raw.abs() > 100.0 {
        raw / 10.0
    } else {
        raw
    };

    // A phone being tested is neither freezing nor boiling; outside this the
    // unit guess was wrong and no reading beats a wrong one.
    (-10.0..=150.0).contains(&celsius).then_some(celsius)
}

/// Per-device settings from `provider.yaml`'s `options:` map.
#[derive(Clone, Debug)]
pub struct IosOptions {
    pub udid: String,
    /// Which display to mirror. 0 is the built-in screen.
    pub display_id: i64,
    /// Force a fresh IDR while the screen is moving, to undo iOS's motion
    /// resolution-collapse. See `media.rs`.
    pub motion_idr: bool,
}

impl IosOptions {
    pub fn parse(udid: &str, options: &serde_json::Map<String, serde_json::Value>) -> Result<Self> {
        Ok(Self {
            udid: udid.to_owned(),
            display_id: options
                .get("display_id")
                .map(|value| {
                    value
                        .as_i64()
                        .ok_or_else(|| anyhow!("display_id must be an integer"))
                })
                .transpose()?
                .unwrap_or(0),
            motion_idr: options
                .get("motion_idr")
                .map(|value| {
                    value
                        .as_bool()
                        .ok_or_else(|| anyhow!("motion_idr must be a boolean"))
                })
                .transpose()?
                .unwrap_or(true),
        })
    }
}

/// The device's orientation → how far the *viewer* must turn the picture.
///
/// The inverse, not the same number. iOS draws the rotated UI inside a capture
/// buffer that never moves, so a device at 90° hands over a picture already
/// turned 90° — and undoing that means turning it back the other way. Setting
/// the two equal renders every landscape upside down, which is exactly what a
/// real iPhone showed.
pub(crate) fn render_rotation_for(orientation: i64) -> i64 {
    (360 - orientation.rem_euclid(360)).rem_euclid(360)
}

/// The pointer state machine.
///
/// The pointer is a *state machine*, not a sequence of independent gestures.
/// The browser streams pointerdown / pointermove / pointerup, and each maps 1:1
/// onto a CONTACT / CONTACT / RELEASE report — which is what a real drag looks
/// like on the wire. Collapsing a touchdown into a complete tap means the
/// device has already dispatched the click before the drag samples arrive,
/// which is why swipes and scrolls used to read as taps.
#[derive(Debug)]
struct Pointer {
    down: bool,
    /// Where the contact currently is, so a release with no coordinates lifts
    /// there instead of teleporting to the screen centre.
    last: (f64, f64),
    deadline: Option<Instant>,
}

impl Default for Pointer {
    fn default() -> Self {
        Self {
            down: false,
            last: (0.5, 0.5),
            deadline: None,
        }
    }
}

/// The encoded frame size, published by the media loop as soon as it parses an
/// SPS and read back by `info`, plus the device's orientation.
///
/// The orientation lives here rather than beside it because the two are only
/// meaningful together: the frame size never changes when an iPhone rotates —
/// CoreDevice captures the native portrait buffer whatever the device is doing
/// and draws the rotated UI inside it — so orientation is the *only* thing that
/// tells a viewer the picture is sideways.
#[derive(Clone, Default)]
pub struct Geometry {
    size: Arc<std::sync::Mutex<Option<(i64, i64)>>>,
    /// Degrees, 0/90/180/270. Unknown until the device is rotated through us:
    /// there is no orientation query in CoreDevice, only a rotate that answers
    /// with the state it landed in.
    rotation: Arc<AtomicI64>,
    known: Arc<AtomicBool>,
}

impl Geometry {
    pub fn set(&self, size: (i64, i64)) {
        if let Ok(mut slot) = self.size.lock() {
            *slot = Some(size);
        }
    }

    pub fn get(&self) -> Option<(i64, i64)> {
        self.size.lock().ok().and_then(|slot| *slot)
    }

    pub fn set_rotation(&self, degrees: i64) {
        self.rotation
            .store(degrees.rem_euclid(360), Ordering::Relaxed);
        self.known.store(true, Ordering::Relaxed);
    }

    /// `None` until the device's orientation is actually known — reporting an
    /// unrotated 0 would make [`IosBackend::rotate`]'s delta walk wrong.
    pub fn rotation(&self) -> Option<i64> {
        self.known
            .load(Ordering::Relaxed)
            .then(|| self.rotation.load(Ordering::Relaxed))
    }
}

pub struct IosBackend {
    options: IosOptions,
    name: Option<String>,
    host: Arc<DeviceHost>,
    video: VideoHandle,
    /// Kept so a rotation can reach live viewers: it produces no new parameter
    /// sets and no new frame size, so nothing else would ever announce it.
    publisher: VideoPublisher,
    geometry: Geometry,
    pointer: Mutex<Pointer>,
    /// Last diagnostics-relay battery read, with the time it was taken.
    ///
    /// A relay round trip is far too expensive to make on `info()`'s 15s
    /// cadence, which is why iOS reported no battery at all until now. Sharing
    /// one cached read between `info()` and `metrics()` amortises it: the worst
    /// case is one round trip a minute, and with metrics enabled at any interval
    /// under [`BATTERY_TTL`] `info()` never makes one at all.
    battery: Mutex<Option<(Instant, BatteryReading)>>,
}

/// What the gas gauge tells us about the battery.
#[derive(Debug, Clone, Copy, Default)]
pub struct BatteryReading {
    /// 0..1, to match every other backend and the protocol.
    pub level: Option<f64>,
    pub charging: Option<bool>,
    pub temperature_c: Option<f64>,
}

impl IosBackend {
    pub fn new(options: IosOptions, name: Option<String>) -> Arc<Self> {
        let (video, publisher) = channel();
        let geometry = Geometry::default();
        let host = Arc::new(DeviceHost::spawn(
            options.clone(),
            publisher.clone(),
            geometry.clone(),
        ));

        Arc::new(Self {
            options,
            name,
            host,
            video,
            publisher,
            geometry,
            pointer: Mutex::new(Pointer::default()),
            battery: Mutex::new(None),
        })
    }

    /// Announce the current geometry and orientation to live viewers.
    fn publish_geometry(&self) {
        let Some((width, height)) = self.geometry.get() else {
            return;
        };
        let rotation = self.geometry.rotation();
        self.publisher.set_geometry(VideoGeometry {
            width,
            height,
            rotation,
            render_rotation: rotation.map(render_rotation_for),
        });
    }

    /// Wait for a live session's RSD handles.
    ///
    /// Callers that need the tunnel get `Unavailable` rather than a hard error
    /// when it is rebuilding: the device is fine, it is just not reachable this
    /// second, and the supervisor reports that as `unhealthy` upstream.
    async fn session(&self) -> BackendResult<crate::device::Session> {
        if !self.host.wait_ready(SESSION_WAIT).await {
            return Err(BackendError::Unavailable(
                "no device session — the tunnel is down or rebuilding".into(),
            ));
        }
        self.host
            .session()
            .await
            .ok_or_else(|| BackendError::Unavailable("the device session went away".into()))
    }

    /// The synthetic root: the media domain, then every app that shares files.
    ///
    /// `UIFileSharingEnabled` is exactly the flag that decides whether an app
    /// appears under "On My iPhone" in the Files app, so listing by it means
    /// this browser shows the same set the phone does. Apps are asked for over
    /// `installation_proxy`, which returns each one's whole Info.plist — the
    /// flag is already in the answer, so this costs one round-trip, not one per
    /// app.
    async fn list_file_domains(
        &self,
        provider: &dyn idevice::provider::IdeviceProvider,
    ) -> BackendResult<FileListing> {
        let mut entries = vec![FileEntry {
            name: "Media".into(),
            path: format!("{MEDIA_PREFIX}/"),
            kind: FileKind::Directory,
            size: None,
            modified_at: None,
        }];

        let mut proxy = InstallationProxyClient::connect(provider)
            .await
            .map_err(|err| BackendError::Failed(format!("installation_proxy connect: {err:?}")))?;

        let apps = proxy
            .get_apps(Some("User"), None)
            .await
            .map_err(|err| BackendError::Failed(format!("list apps: {err:?}")))?;

        let mut sharing: Vec<(String, String)> = apps
            .into_iter()
            .filter_map(|(bundle, info)| {
                let dict = info.as_dictionary()?;
                let shares = dict
                    .get("UIFileSharingEnabled")
                    .and_then(plist::Value::as_boolean)
                    .unwrap_or(false);
                if !shares {
                    return None;
                }
                let name = dict
                    .get("CFBundleDisplayName")
                    .or_else(|| dict.get("CFBundleName"))
                    .and_then(plist::Value::as_string)
                    .unwrap_or(&bundle)
                    .to_owned();
                Some((name, bundle))
            })
            .collect();
        sharing.sort_by_key(|(name, _)| name.to_lowercase());

        // An app suite ships several bundles under one display name — this
        // phone lists "My BMW" three times — and three identical rows is not a
        // choice anyone can make. Only the ambiguous ones get the bundle id,
        // so the common case stays clean.
        let duplicated: std::collections::HashSet<&str> = sharing
            .iter()
            .enumerate()
            .filter(|(i, (name, _))| {
                sharing
                    .iter()
                    .enumerate()
                    .any(|(j, (other, _))| j != *i && other == name)
            })
            .map(|(_, (name, _))| name.as_str())
            .collect();
        let duplicated: std::collections::HashSet<String> =
            duplicated.into_iter().map(str::to_owned).collect();

        entries.extend(sharing.into_iter().map(|(name, bundle)| FileEntry {
            name: if duplicated.contains(&name) {
                format!("{name} ({bundle})")
            } else {
                name
            },
            path: format!("{APP_PREFIX}{bundle}:{APP_DOCUMENTS}"),
            kind: FileKind::Directory,
            size: None,
            modified_at: None,
        }));

        Ok(FileListing {
            path: "/".into(),
            parent: None,
            entries,
        })
    }

    async fn send(&self, input: Input) {
        match self.host.input().await {
            Some(handle) => handle.send(input),
            None => debug!("no session — dropping a HID report"),
        }
    }

    async fn pointer_down(&self, x: f64, y: f64) {
        let mut pointer = self.pointer.lock().await;
        pointer.down = true;
        pointer.last = (x, y);
        pointer.deadline = Some(Instant::now() + CONTACT_MAX);
        drop(pointer);

        self.send(Input::Contact {
            x: hid::to_hid(x),
            y: hid::to_hid(y),
        })
        .await;
    }

    /// A move that arrives with no contact down is dropped rather than promoted
    /// to a down: the browser binds its move listener inside pointerdown, so a
    /// stray move means we already released and replaying it would start a
    /// phantom drag.
    async fn pointer_move(&self, x: f64, y: f64) {
        let mut pointer = self.pointer.lock().await;
        if !pointer.down {
            return;
        }
        pointer.last = (x, y);
        pointer.deadline = Some(Instant::now() + CONTACT_MAX);
        drop(pointer);

        self.send(Input::Contact {
            x: hid::to_hid(x),
            y: hid::to_hid(y),
        })
        .await;
    }

    async fn pointer_up(&self, x: f64, y: f64) {
        let mut pointer = self.pointer.lock().await;
        // Prefer the coordinates that came with the release, falling back to
        // the last sample — releasing at a screen centre would turn every
        // gesture into a drag to the middle of the display.
        let (x, y) = if x.is_finite() && y.is_finite() {
            (x, y)
        } else {
            pointer.last
        };
        pointer.down = false;
        pointer.deadline = None;
        drop(pointer);

        self.send(Input::Release {
            x: hid::to_hid(x),
            y: hid::to_hid(y),
        })
        .await;
    }

    /// Auto-lift a contact held with no activity.
    ///
    /// A browser can lose a pointerup to a window blur, a dropped socket, or the
    /// tab going away, and the device would then sit with a finger pinned to the
    /// glass — every later tap reads as a drag from wherever that finger was.
    /// Driven by the health poll.
    async fn release_if_stale(&self) {
        let mut pointer = self.pointer.lock().await;
        let Some(deadline) = pointer.deadline else {
            return;
        };
        if !pointer.down || Instant::now() < deadline {
            return;
        }
        let (x, y) = pointer.last;
        pointer.down = false;
        pointer.deadline = None;
        drop(pointer);

        warn!(
            "contact held >{}s with no update — releasing",
            CONTACT_MAX.as_secs()
        );
        self.send(Input::Release {
            x: hid::to_hid(x),
            y: hid::to_hid(y),
        })
        .await;
    }

    /// Display geometry.
    ///
    /// The stream's own SPS is the primary source: `mobilegestalt`, which
    /// `stf-ios-provider` used, answers `MobileGestaltDeprecated` on iOS 26/27
    /// and returns no dimensions at all. It is still tried as a fallback for
    /// older devices, and because it is the only one of the two that knows the
    /// backing-store scale.
    ///
    /// `None` is an acceptable answer either way: the browser derives geometry
    /// from the first decoded frame, and the input path never depends on it —
    /// HID coordinates are normalised end to end.
    async fn display(&self) -> Option<Display> {
        if let Some((width, height)) = self.geometry.get() {
            let rotation = self.geometry.rotation();
            return Some(Display {
                width,
                height,
                // The SPS describes pixels, not points, so there is no scale to
                // report — and inventing one would misreport the geometry the
                // popout window is sized from.
                scale: None,
                rotation,
                render_rotation: rotation.map(render_rotation_for),
            });
        }
        match self.try_display().await {
            Ok(display) => Some(display),
            Err(err) => {
                debug!(%err, "display geometry unavailable — reporting unknown");
                None
            }
        }
    }

    /// The cached battery, refreshing it when `max_age` has passed.
    ///
    /// `metrics()` passes `ZERO` — it is already on its own, slower cadence —
    /// and `info()` passes [`BATTERY_TTL`], so the 15s poll loop does not turn
    /// into four relay round trips a minute.
    async fn battery(&self, max_age: Duration) -> BatteryReading {
        let mut cached = self.battery.lock().await;
        if let Some((at, reading)) = *cached {
            if at.elapsed() < max_age {
                return reading;
            }
        }

        match self.read_battery().await {
            Ok(reading) => {
                *cached = Some((Instant::now(), reading));
                reading
            }
            Err(err) => {
                debug!(%err, "battery unavailable — reporting unknown");
                // The stale reading is dropped rather than re-served: a phone
                // that has gone away must stop reporting a charge level.
                *cached = None;
                BatteryReading::default()
            }
        }
    }

    /// Reads the battery, preferring the registry entry over the gas gauge.
    ///
    /// That order is the opposite of what the service names suggest, and it was
    /// settled by `examples/diagnostics_probe.rs` against an iPhone 13: `gasguage`
    /// answers a thin summary — `CycleCount`, `FullChargeCapacity`, `Status`, all
    /// nested under a `GasGauge` key — with **no charge level in it at all**.
    /// `ioregistry` on `AppleSmartBattery` is where `CurrentCapacity`,
    /// `MaxCapacity` and `IsCharging` actually live.
    ///
    /// Each source is judged by whether a level came out of it, not by whether
    /// the dictionary was non-empty: the gas gauge's reply is non-empty and
    /// useless, so an emptiness check would take it and never fall through.
    async fn read_battery(&self) -> Result<BatteryReading> {
        let provider = device::usbmux_provider(&self.options.udid).await?;

        let registry = self
            .relay_battery(&*provider, BatterySource::Registry)
            .await;
        if let Ok(reading) = &registry {
            if reading.level.is_some() {
                return registry;
            }
        }

        // Kept as a fallback rather than deleted: it is the documented battery
        // service, and a version that populates it properly should still work.
        let gauge = self
            .relay_battery(&*provider, BatterySource::GasGauge)
            .await;
        match (&gauge, registry) {
            (Ok(reading), _) if reading.level.is_some() => gauge,
            // Neither had a level. Prefer whichever answered at all, so a
            // temperature or charging flag on its own is not thrown away.
            (_, Ok(reading)) => Ok(reading),
            _ => gauge,
        }
    }

    async fn relay_battery(
        &self,
        provider: &dyn idevice::provider::IdeviceProvider,
        source: BatterySource,
    ) -> Result<BatteryReading> {
        let mut relay = DiagnosticsRelayClient::connect(provider)
            .await
            .map_err(|err| anyhow!("diagnostics relay connect: {err:?}"))?;

        let values = match source {
            BatterySource::Registry => relay
                .ioregistry(None, Some("AppleSmartBattery"), None)
                .await
                .map_err(|err| anyhow!("ioregistry: {err:?}"))?,
            BatterySource::GasGauge => relay
                .gasguage()
                .await
                .map_err(|err| anyhow!("gasguage: {err:?}"))?,
        }
        .ok_or_else(|| anyhow!("{source:?} returned nothing"))?;

        Ok(parse_battery(&values))
    }

    async fn try_display(&self) -> Result<Display> {
        let provider = device::usbmux_provider(&self.options.udid).await?;
        let mut relay = DiagnosticsRelayClient::connect(&*provider)
            .await
            .map_err(|err| anyhow!("diagnostics relay connect: {err:?}"))?;

        let keys = ["MainScreenWidth", "MainScreenHeight", "MainScreenScale"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let values = relay
            .mobilegestalt(Some(keys))
            .await
            .map_err(|err| anyhow!("mobilegestalt: {err:?}"))?
            .ok_or_else(|| anyhow!("mobilegestalt returned nothing"))?;

        // The reply nests the answers under a "MobileGestalt" dictionary.
        let values = values
            .get("MobileGestalt")
            .and_then(plist::Value::as_dictionary)
            .unwrap_or(&values);

        let number = |key: &str| -> f64 {
            values
                .get(key)
                .and_then(|value| {
                    value
                        .as_unsigned_integer()
                        .map(|n| n as f64)
                        .or_else(|| value.as_real())
                })
                .unwrap_or(0.0)
        };

        let (width, height) = (number("MainScreenWidth"), number("MainScreenHeight"));
        if width == 0.0 || height == 0.0 {
            return Err(anyhow!("mobilegestalt reported no screen dimensions"));
        }
        Ok(Display {
            width: width as i64,
            height: height as i64,
            scale: Some(number("MainScreenScale").max(1.0)),
            rotation: self.geometry.rotation(),
            render_rotation: self.geometry.rotation().map(render_rotation_for),
        })
    }
}

#[async_trait]
impl DeviceBackend for IosBackend {
    async fn info(&self) -> BackendResult<DeviceInfo> {
        let battery = self.battery(BATTERY_TTL).await;
        let identity = self
            .host
            .identity()
            .await
            .map_err(|err| BackendError::Unavailable(format!("{err:#}")))?;

        Ok(DeviceInfo {
            id: self.options.udid.clone(),
            platform: Platform::Ios,
            name: self
                .name
                .clone()
                .or_else(|| Some(identity.name.clone()).filter(|name| !name.is_empty())),
            model: Some(identity.product_type.clone()).filter(|model| !model.is_empty()),
            manufacturer: Some("Apple".into()),
            os_version: Some(identity.version.clone()).filter(|version| !version.is_empty()),
            abi: Some(identity.cpu.clone()).filter(|cpu| !cpu.is_empty()),
            // Android's API level. iOS has no equivalent, and inventing one
            // from the product version would only mislead.
            sdk: None,
            // The UDID *is* the serial for the purpose of naming a device in a
            // bug report; the rest of this block is Android build metadata with
            // no iOS counterpart worth inventing.
            serial: Some(self.options.udid.clone()),
            brand: None,
            build_id: Some(identity.build.clone()).filter(|build| !build.is_empty()),
            security_patch: None,
            abi_list: None,
            display: self.display().await,
            // Shared with `metrics()` through a TTL cache — see `battery()`. The
            // round trip is worth making now precisely *because* it is amortised
            // rather than paid on every 15s poll.
            battery_level: battery.level,
            battery_state: battery
                .charging
                .map(|charging| if charging { "charging" } else { "discharging" }.to_owned()),
        })
    }

    /// Battery only.
    ///
    /// iOS has no CPU or memory to give: `host_statistics`/`vm_statistics` are
    /// available to on-device code only, and the diagnostics relay is a lockdown
    /// service with a fixed dictionary and no `sysctl` surface —
    /// `examples/diagnostics_probe.rs` is how that was checked. Those gauges are
    /// therefore simply absent for an iPhone, which is what an absent Prometheus
    /// series is for.
    ///
    /// This answers `Ok` with mostly `None` rather than `Unsupported`, so it does
    /// not advance the exporter's error counter: there is nothing wrong here.
    async fn metrics(&self, _apps: &AppFilter) -> BackendResult<DeviceMetrics> {
        // Duration::ZERO: the metrics sampler is already on its own, slower
        // cadence, so it always takes a fresh reading and `info()` rides on it.
        let battery = self.battery(Duration::ZERO).await;

        Ok(DeviceMetrics {
            battery_level: battery.level,
            battery_charging: battery.charging,
            battery_temperature_c: battery.temperature_c,
            ..Default::default()
        })
    }

    fn video(&self) -> VideoHandle {
        self.video.clone()
    }

    async fn input(&self, event: InputEvent) -> BackendResult<()> {
        match event {
            InputEvent::PointerDown { x, y, .. } => self.pointer_down(x, y).await,
            InputEvent::PointerMove { x, y, .. } => self.pointer_move(x, y).await,
            InputEvent::PointerUp { x, y, .. } => self.pointer_up(x, y).await,

            // Hardware buttons are edge-triggered on the device: it wants a
            // press-and-hold-and-release, not our down/up pair, so only the
            // down edge acts and the up edge is swallowed.
            InputEvent::Key { key, down } => {
                if let Some((page, usage, hold)) = hid::named_button(&key) {
                    if down {
                        self.send(Input::Button(
                            Box::leak(key.into_boxed_str()),
                            page,
                            usage,
                            hold,
                        ))
                        .await;
                    }
                } else if let Some(usage) = hid::special_key(&key) {
                    if down {
                        self.send(Input::KeyUsage(usage)).await;
                    }
                } else {
                    debug!(key, "no HID mapping for this key");
                }
            }

            InputEvent::Text { text } => {
                for character in text.chars() {
                    self.send(Input::Character(character)).await;
                }
            }
        }
        Ok(())
    }

    async fn screenshot(&self) -> BackendResult<Vec<u8>> {
        let session = self.session().await?;
        let mut adapter = session.adapter;
        let mut client = connect_service!(
            ScreenCaptureServiceClient<Box<dyn ReadWrite>>,
            &mut adapter,
            &session.handshake
        )?;
        client
            .take_screenshot(None, ImageFormat::Png)
            .await
            .map_err(|err| BackendError::Failed(format!("screenshot: {err:?}")))
    }

    async fn clipboard_get(&self) -> BackendResult<Option<String>> {
        let session = self.session().await?;
        let mut adapter = session.adapter;
        let mut client = connect_service!(
            PasteboardServiceClient<Box<dyn ReadWrite>>,
            &mut adapter,
            &session.handshake
        )?;

        let snapshot = client
            .get(GENERAL_PASTEBOARD)
            .await
            .map_err(|err| BackendError::Failed(format!("pasteboard read: {err:?}")))?;

        Ok(snapshot
            .items
            .iter()
            .flat_map(|item| &item.data)
            .filter(|entry| entry.uti.contains("text"))
            .find_map(|entry| match &entry.payload {
                PasteboardPayload::Inline(bytes) => String::from_utf8(bytes.clone()).ok(),
                _ => None,
            }))
    }

    async fn clipboard_set(&self, text: &str) -> BackendResult<()> {
        let session = self.session().await?;
        let mut adapter = session.adapter;
        let mut client = connect_service!(
            PasteboardServiceClient<Box<dyn ReadWrite>>,
            &mut adapter,
            &session.handshake
        )?;
        client
            .set_text(text, GENERAL_PASTEBOARD)
            .await
            .map_err(|err| BackendError::Failed(format!("pasteboard write: {err:?}")))
    }

    async fn apps(&self) -> BackendResult<Vec<AppInfo>> {
        // The bound covers opening the stream as well as the request, because
        // *opening* it is what hangs on a device whose app service is unwell —
        // a timeout around the request alone never fires, and the caller waits
        // forever on a future that is stuck a step earlier.
        let listed = tokio::time::timeout(APPS_TIMEOUT, async {
            let session = self.session().await?;
            let mut adapter = session.adapter;
            let mut client = connect_service!(AppList, &mut adapter, &session.handshake)?;
            client
                .list_apps()
                .await
                .map_err(|err| BackendError::Failed(format!("list apps: {err:?}")))
        })
        .await;

        let apps = match listed {
            Ok(result) => result?,
            Err(_) => {
                return Err(BackendError::Failed(format!(
                    "the device did not answer a list-apps request within {}s; \
                     its app service may need the device rebooted",
                    APPS_TIMEOUT.as_secs()
                )))
            }
        };

        Ok(apps
            .into_iter()
            .filter(|app| !app.bundle_identifier.is_empty())
            .map(|app| AppInfo {
                name: Some(app.name).filter(|name| !name.is_empty()),
                version: app.version,
                system: Some(!app.is_removable),
                id: app.bundle_identifier,
            })
            .collect())
    }

    /// Install over lockdown/usbmux rather than the tunnel.
    ///
    /// `afc` and `installation_proxy` are classic lockdown services, reachable
    /// over plain usbmux regardless of the RSD tunnel. Running them there keeps
    /// a multi-hundred-megabyte upload off the userspace TCP stack that is also
    /// carrying the video.
    async fn install(&self, staged: &Path, progress: &dyn ProgressSink) -> BackendResult<()> {
        let provider = device::usbmux_provider(&self.options.udid).await?;
        let remote_path = format!("{STAGING_DIR}/{}.ipa", self.options.udid);

        progress.report("uploading", None);
        {
            let ipa = tokio::fs::read(staged)
                .await
                .with_context(|| format!("reading {}", staged.display()))?;

            let mut afc = AfcClient::connect(&*provider)
                .await
                .map_err(|err| anyhow!("afc connect: {err:?}"))?;
            // Already present on most devices; a failure here is not fatal.
            let _ = afc.mk_dir(STAGING_DIR).await;

            let mut file = afc
                .open(remote_path.clone(), AfcFopenMode::Wr)
                .await
                .map_err(|err| anyhow!("open {remote_path}: {err:?}"))?;
            file.write_entire(&ipa)
                .await
                .map_err(|err| anyhow!("upload {remote_path}: {err:?}"))?;
            file.close()
                .await
                .map_err(|err| anyhow!("close {remote_path}: {err:?}"))?;
        }

        progress.report("installing", None);
        let mut proxy = InstallationProxyClient::connect(&*provider)
            .await
            .map_err(|err| anyhow!("installation_proxy connect: {err:?}"))?;

        let outcome = proxy
            .install_with_callback(
                remote_path.clone(),
                None,
                |(percent, _)| async move {
                    debug!(percent, "install progress");
                },
                (),
            )
            .await
            .map_err(|err| anyhow!("install: {err:?}"));

        // The staged copy must go whatever happened, exactly as the Android
        // backend deletes its pushed APK: the path is per-device, so it was
        // only ever overwritten by the *next* install on the same phone, and a
        // device that never gets another one holds an IPA forever.
        let _ = remove_staged(&*provider, &remote_path).await;

        outcome?;
        progress.report("done", Some(1.0));
        Ok(())
    }

    /// Home, and rotation back to upright.
    ///
    /// No clipboard step: `clipboard_set` here goes through the pasteboard
    /// service, which is a *sync* with the host rather than a write, and an
    /// empty sync is not the same as clearing what the device holds. Leaving
    /// the step out is better than reporting a clear that did not happen.
    async fn reset_screen(&self) -> BackendResult<()> {
        self.input(InputEvent::Key {
            key: "Home".into(),
            down: true,
        })
        .await?;
        self.input(InputEvent::Key {
            key: "Home".into(),
            down: false,
        })
        .await?;
        self.rotate(0).await
    }

    /// Empties the staging directory installs upload through.
    ///
    /// This is the whole of iOS's filesystem reach: AFC sees the media
    /// partition, not an app's container, so there is nothing else a wipe could
    /// touch. Configured `cleanup_paths` are interpreted relative to that same
    /// AFC root.
    async fn wipe_paths(&self, paths: &[String]) -> BackendResult<()> {
        let provider = device::usbmux_provider(&self.options.udid).await?;
        let mut afc = AfcClient::connect(&*provider)
            .await
            .map_err(|err| anyhow!("afc connect: {err:?}"))?;

        for path in paths {
            let path = path.trim_start_matches('/');
            afc.remove_all(path)
                .await
                .map_err(|err| anyhow!("wiping {path}: {err:?}"))?;
        }
        Ok(())
    }

    async fn uninstall(&self, app_id: &str) -> BackendResult<()> {
        let session = self.session().await?;
        let mut adapter = session.adapter;
        let mut client = connect_service!(
            AppServiceClient<Box<dyn ReadWrite>>,
            &mut adapter,
            &session.handshake
        )?;
        client
            .uninstall_app(app_id)
            .await
            .map_err(|err| BackendError::Failed(format!("uninstall {app_id}: {err:?}")))
    }

    async fn launch(&self, app_id: &str, args: &[String]) -> BackendResult<()> {
        let session = self.session().await?;
        let mut adapter = session.adapter;
        let mut client = connect_service!(
            AppServiceClient<Box<dyn ReadWrite>>,
            &mut adapter,
            &session.handshake
        )?;
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        client
            .launch_application(app_id, &args, false, false, None, None, None)
            .await
            .map_err(|err| BackendError::Failed(format!("launch {app_id}: {err:?}")))?;
        Ok(())
    }

    fn files_root(&self) -> Option<&'static str> {
        Some("/")
    }

    /// One directory, from whichever of the two trees the path names.
    ///
    /// **An iPhone has no single filesystem to browse**, so the root here is
    /// synthetic: it lists the AFC media domain (`DCIM`, `Downloads`, `Books`)
    /// alongside every installed app that shares files. Those are separate
    /// services — `com.apple.afc` and a per-app `house_arrest` vend — and
    /// nothing joins them on the device. Anything saved through
    /// "Save to Files → On My iPhone" lives in an app container, which is why
    /// the media domain alone left people unable to find their own file.
    ///
    /// One `get_file_info` round-trip per entry, because AFC has no batched
    /// stat. Fine for a directory a person is looking at; it is the reason this
    /// is a browse rather than a recursive search.
    async fn list_files(&self, path: &str) -> BackendResult<FileListing> {
        let provider = device::usbmux_provider(&self.options.udid).await?;

        let (mut afc, prefix, inner) = match IosPath::parse(path) {
            IosPath::Root => return self.list_file_domains(&*provider).await,
            IosPath::Media(inner) => {
                let afc = AfcClient::connect(&*provider)
                    .await
                    .map_err(|err| BackendError::Failed(format!("afc connect: {err:?}")))?;
                (afc, MEDIA_PREFIX.to_owned(), inner)
            }
            IosPath::App {
                bundle,
                path: inner,
            } => {
                let house = HouseArrestClient::connect(&*provider)
                    .await
                    .map_err(|err| {
                        BackendError::Failed(format!("house_arrest connect: {err:?}"))
                    })?;
                // Documents rather than the whole container: the rest is denied
                // for an app that has not opted in, and the error that produces
                // says nothing a user can act on.
                let afc = house.vend_documents(bundle.clone()).await.map_err(|err| {
                    BackendError::Failed(format!("{bundle} does not share files: {err:?}"))
                })?;
                (afc, format!("{APP_PREFIX}{bundle}:"), inner)
            }
        };
        let names = afc
            .list_dir(inner.clone())
            .await
            .map_err(|err| BackendError::Failed(format!("list {inner}: {err:?}")))?;

        let mut entries = Vec::new();
        for name in names {
            if name == "." || name == ".." {
                continue;
            }
            let child = join_path(&inner, &name);
            // A single unreadable entry must not lose the whole listing — the
            // directory is still worth showing, with that one as `other`.
            let info = afc.get_file_info(child.clone()).await.ok();
            let kind = match info.as_ref().map(|i| i.st_ifmt.as_str()) {
                Some("S_IFDIR") => FileKind::Directory,
                Some("S_IFREG") => FileKind::File,
                _ => FileKind::Other,
            };
            entries.push(FileEntry {
                name,
                path: format!("{prefix}{child}"),
                size: (kind == FileKind::File).then(|| info.as_ref().map_or(0, |i| i.size as i64)),
                modified_at: info.map(|i| i.modified.and_utc().timestamp_millis()),
                kind,
            });
        }

        Ok(FileListing {
            path: path.to_owned(),
            parent: IosPath::parent(path),
            entries,
        })
    }

    async fn pull_file(&self, path: &str, dest: &Path) -> BackendResult<u64> {
        use tokio::io::AsyncWriteExt as _;

        let provider = device::usbmux_provider(&self.options.udid).await?;
        let (mut afc, inner) = match IosPath::parse(path) {
            IosPath::Root => return Err(BackendError::Failed("that is not a file".into())),
            IosPath::Media(inner) => {
                let afc = AfcClient::connect(&*provider)
                    .await
                    .map_err(|err| BackendError::Failed(format!("afc connect: {err:?}")))?;
                (afc, inner)
            }
            IosPath::App {
                bundle,
                path: inner,
            } => {
                let house = HouseArrestClient::connect(&*provider)
                    .await
                    .map_err(|err| {
                        BackendError::Failed(format!("house_arrest connect: {err:?}"))
                    })?;
                let afc = house.vend_documents(bundle.clone()).await.map_err(|err| {
                    BackendError::Failed(format!("{bundle} does not share files: {err:?}"))
                })?;
                (afc, inner)
            }
        };
        let mut remote = afc
            .open(inner.clone(), AfcFopenMode::RdOnly)
            .await
            .map_err(|err| BackendError::Failed(format!("open {inner}: {err:?}")))?;

        let mut file = tokio::fs::File::create(dest)
            .await
            .with_context(|| format!("creating {}", dest.display()))?;
        let mut written: u64 = 0;

        // Chunked rather than `read_entire`: the point of staging to disk is
        // that a video off a phone never sits in the provider's memory, and
        // reading it whole here would undo that one line before writing it out.
        loop {
            let chunk = remote
                .read_n(AFC_CHUNK)
                .await
                .map_err(|err| BackendError::Failed(format!("read {inner}: {err:?}")))?;
            if chunk.is_empty() {
                break;
            }
            written += chunk.len() as u64;
            file.write_all(&chunk)
                .await
                .context("writing staged file")?;
            if chunk.len() < AFC_CHUNK {
                break;
            }
        }

        file.flush().await.context("flushing staged file")?;
        let _ = remote.close().await;
        Ok(written)
    }

    /// The session plane sends an absolute angle; CoreDevice's orientation
    /// service only steps 90° at a time, so we walk there.
    ///
    /// The walk is a delta against where the device actually is, which is only
    /// possible because the service *answers* with the orientation it landed
    /// in — there is no way to ask an iPhone which way up it is. Until the
    /// first rotation that answer is unknown, and an unknown orientation is
    /// walked as a single step, which is what the browser means by "rotate"
    /// when it has been told nothing.
    async fn rotate(&self, degrees: i64) -> BackendResult<()> {
        let steps = match self.geometry.rotation() {
            Some(current) => ((degrees - current).div_euclid(90)).rem_euclid(4),
            None => 1,
        };

        let Some(input) = self.host.input().await else {
            return Err(BackendError::Unavailable("no device session".into()));
        };

        let mut landed = None;
        for _ in 0..steps {
            landed = input
                .request(|reply| Input::Rotate {
                    direction: RotationDirection::Left,
                    reply,
                })
                .await
                .flatten();
            // backboardd animates the rotation; stepping faster than it can
            // settle drops the later steps on the floor.
            tokio::time::sleep(Duration::from_millis(400)).await;
        }

        // The device's own answer, not what we asked for: rotation lock, or a
        // step the animation swallowed, means the two can differ, and the
        // viewer must be told where the screen really is.
        match landed {
            Some(degrees) => {
                self.geometry.set_rotation(degrees);
                self.publish_geometry();
                info!(rotation = degrees, "device orientation reported");
            }
            // Flat on a desk, or a variant this crate does not know: leaving it
            // unknown keeps the next rotate at one honest step rather than
            // walking a delta against a number we invented.
            None => debug!("the device reported no usable orientation"),
        }

        Ok(())
    }

    /// Restart via the diagnostics relay.
    ///
    /// Not the CoreDevice diagnostics service — that one only captures a
    /// sysdiagnose.
    async fn reboot(&self) -> BackendResult<()> {
        let provider = device::usbmux_provider(&self.options.udid).await?;
        let mut relay = DiagnosticsRelayClient::connect(&*provider)
            .await
            .map_err(|err| anyhow!("diagnostics relay connect: {err:?}"))?;
        relay
            .restart()
            .await
            .map_err(|err| BackendError::Failed(format!("restart: {err:?}")))
    }

    /// Force a session rebuild.
    ///
    /// There is no "restart just the media stream": the HID surfaces
    /// authenticate against the live stream, so tunnel, media and HID come back
    /// together or not at all. Dropping the tunnel is how you ask for that.
    async fn restart(&self) -> BackendResult<()> {
        info!(udid = %self.options.udid, "restarting the device session");
        self.host.drop_session().await;
        Ok(())
    }

    async fn is_healthy(&self) -> bool {
        // The health poll is also the only regular tick this backend gets, so
        // it is where a forgotten contact gets lifted.
        self.release_if_stale().await;
        self.host.is_ready()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_file_trees_stay_apart_but_navigate_as_one() {
        assert!(matches!(IosPath::parse("/"), IosPath::Root));

        // Media, and its walk back up to the synthetic root.
        assert!(matches!(IosPath::parse("media:/DCIM"), IosPath::Media(p) if p == "/DCIM"));
        assert_eq!(
            IosPath::parent("media:/DCIM/100APPLE").as_deref(),
            Some("media:/DCIM")
        );
        assert_eq!(IosPath::parent("media:/DCIM").as_deref(), Some("media:/"));
        assert_eq!(IosPath::parent("media:/").as_deref(), Some("/"));

        // An app container. The bundle id must survive a path that has its own
        // colons, which is why only the first one splits.
        match IosPath::parse("app:com.example.App:/Documents/Reports") {
            IosPath::App { bundle, path } => {
                assert_eq!(bundle, "com.example.App");
                assert_eq!(path, "/Documents/Reports");
            }
            _ => panic!("an app path did not parse as one"),
        }
        assert_eq!(
            IosPath::parent("app:com.example.App:/Documents/Reports").as_deref(),
            Some("app:com.example.App:/Documents"),
        );
        // Documents is the top of an app tree — a VendDocuments session denies
        // everything above it, so walking up goes to the synthetic root.
        assert_eq!(
            IosPath::parent("app:com.example.App:/Documents").as_deref(),
            Some("/"),
        );

        // The top has nowhere above it, so the browser hides "..".
        assert_eq!(IosPath::parent("/"), None);
    }

    /// The constant this file exists to get right.
    ///
    /// A device at 90° hands over a picture that is *already* turned 90°, so the
    /// viewer turns it back by 270°, not by 90°. Reporting the orientation
    /// itself renders every landscape upside down — which is what a real iPhone
    /// showed, twice.
    #[test]
    fn the_viewer_turns_the_picture_back_not_the_same_way_again() {
        assert_eq!(render_rotation_for(0), 0);
        assert_eq!(render_rotation_for(90), 270);
        assert_eq!(render_rotation_for(180), 180);
        assert_eq!(render_rotation_for(270), 90);

        // Applying the orientation and then the render rotation must land back
        // where it started, whichever orientation the device is in.
        for orientation in [0, 90, 180, 270] {
            assert_eq!((orientation + render_rotation_for(orientation)) % 360, 0);
        }
    }

    #[test]
    fn options_default_to_the_built_in_display_with_motion_idr_on() {
        let options = IosOptions::parse("udid-1", &serde_json::Map::new()).unwrap();
        assert_eq!(options.display_id, 0);
        assert!(options.motion_idr);
    }

    #[test]
    fn a_mistyped_option_is_a_config_error_not_a_silent_default() {
        let mut options = serde_json::Map::new();
        options.insert("motion_idr".into(), serde_json::json!("yes"));
        assert!(IosOptions::parse("udid-1", &options).is_err());
    }

    fn dict(entries: &[(&str, plist::Value)]) -> plist::Dictionary {
        let mut out = plist::Dictionary::new();
        for (key, value) in entries {
            out.insert((*key).to_owned(), value.clone());
        }
        out
    }

    /// The real `ioregistry AppleSmartBattery` shape, off an iPhone 13: capacity
    /// is already a percentage, and there is **no Temperature key at all**.
    #[test]
    fn the_real_registry_shape_yields_a_level_and_a_state_but_no_temperature() {
        let reading = parse_battery(&dict(&[
            ("CurrentCapacity", 100u64.into()),
            ("MaxCapacity", 100u64.into()),
            ("IsCharging", false.into()),
            ("ExternalConnected", true.into()),
            ("CycleCount", 53u64.into()),
        ]));

        assert_eq!(reading.level, Some(1.0));
        assert_eq!(reading.charging, Some(false));
        assert_eq!(reading.temperature_c, None);
    }

    /// The real `gasguage` reply, off the same phone: nested under `GasGauge`,
    /// and carrying no charge level. It must unwrap cleanly *and* report no
    /// level, which is what makes `read_battery` fall through to the registry.
    #[test]
    fn the_real_gas_gauge_shape_unwraps_but_offers_no_level() {
        let mut gauge = plist::Dictionary::new();
        gauge.insert(
            "GasGauge".into(),
            plist::Value::Dictionary(dict(&[
                ("CycleCount", 53u64.into()),
                ("FullChargeCapacity", 100u64.into()),
                ("Status", "Success".into()),
            ])),
        );

        let reading = parse_battery(&gauge);

        assert_eq!(reading.level, None);
        assert_eq!(reading.charging, None);
    }

    #[test]
    fn a_gas_gauge_reading_becomes_a_level_a_state_and_a_temperature() {
        let reading = parse_battery(&dict(&[
            ("CurrentCapacity", 2100u64.into()),
            ("MaxCapacity", 3200u64.into()),
            ("IsCharging", true.into()),
            // Hundredths of a degree, the usual gas-gauge unit.
            ("Temperature", 3120u64.into()),
        ]));

        assert_eq!(reading.level.map(|l| (l * 100.0).round()), Some(66.0));
        assert_eq!(reading.charging, Some(true));
        assert_eq!(reading.temperature_c, Some(31.2));
    }

    /// Some versions report `CurrentCapacity` as a percentage with no usable
    /// denominator beside it.
    #[test]
    fn a_capacity_with_no_maximum_is_read_as_a_percentage() {
        let reading = parse_battery(&dict(&[("CurrentCapacity", 77u64.into())]));
        assert_eq!(reading.level, Some(0.77));
    }

    /// A raw mAh reading with its maximum missing would otherwise pin the
    /// battery at 100% forever, which looks like a working feature.
    #[test]
    fn a_raw_capacity_with_no_maximum_is_refused() {
        let reading = parse_battery(&dict(&[("CurrentCapacity", 2100u64.into())]));
        assert_eq!(reading.level, None);
    }

    #[test]
    fn battery_temperature_units_are_guessed_by_magnitude() {
        let hundredths = parse_battery(&dict(&[("Temperature", 3120u64.into())]));
        let tenths = parse_battery(&dict(&[("Temperature", 312u64.into())]));
        let degrees = parse_battery(&dict(&[("Temperature", 31u64.into())]));

        assert_eq!(hundredths.temperature_c, Some(31.2));
        assert_eq!(tenths.temperature_c, Some(31.2));
        assert_eq!(degrees.temperature_c, Some(31.0));
    }

    #[test]
    fn an_implausible_battery_temperature_is_no_reading() {
        let reading = parse_battery(&dict(&[("Temperature", 9_000_000u64.into())]));
        assert_eq!(reading.temperature_c, None);
    }

    #[test]
    fn an_empty_dictionary_reports_nothing_rather_than_zero() {
        let reading = parse_battery(&plist::Dictionary::new());

        assert_eq!(reading.level, None);
        assert_eq!(reading.charging, None);
        assert_eq!(reading.temperature_c, None);
    }

    /// The fallback key on versions where the gas gauge omits `IsCharging`.
    #[test]
    fn external_power_stands_in_for_a_missing_charging_flag() {
        let reading = parse_battery(&dict(&[("ExternalConnected", 1u64.into())]));
        assert_eq!(reading.charging, Some(true));
    }
}
