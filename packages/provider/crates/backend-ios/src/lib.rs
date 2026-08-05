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
use farm_protocol::{AppInfo, Display, Platform};
use idevice::core_device::{
    AppServiceClient, ImageFormat, PasteboardPayload, PasteboardServiceClient, RotationDirection,
    ScreenCaptureServiceClient, GENERAL_PASTEBOARD,
};
use idevice::diagnostics_relay::DiagnosticsRelayClient;
use idevice::installation_proxy::InstallationProxyClient;
use idevice::services::afc::{opcode::AfcFopenMode, AfcClient};
use idevice::{IdeviceService as _, ReadWrite};
use provider_core::backend::{
    BackendError, DeviceBackend, DeviceInfo, InputEvent, ProgressSink, Result as BackendResult,
};
use provider_core::video::{channel, VideoGeometry, VideoHandle, VideoPublisher};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::device::{connect_service, DeviceHost};
use crate::hid::Input;

/// Where an IPA is staged on the device before `installation_proxy` installs it.
const STAGING_DIR: &str = "PublicStaging";

/// Longest a contact may stay down with no further samples before the provider
/// lifts it on its own. Generous enough for a deliberate long-press, short
/// enough that a browser that lost its pointerup does not leave a finger pinned
/// to the glass for the rest of the session.
const CONTACT_MAX: Duration = Duration::from_secs(10);

/// How long to wait for a session when a request needs one.
const SESSION_WAIT: Duration = Duration::from_secs(20);

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
        self.rotation.store(degrees.rem_euclid(360), Ordering::Relaxed);
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
            // The whole point on iOS: the frames are portrait and the content
            // inside them is not, so the viewer has to put it right.
            render_rotation: rotation,
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
                render_rotation: rotation,
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
            render_rotation: self.geometry.rotation(),
        })
    }
}

#[async_trait]
impl DeviceBackend for IosBackend {
    async fn info(&self) -> BackendResult<DeviceInfo> {
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
            display: self.display().await,
            // Battery state needs a diagnostics round-trip of its own; the poll
            // loop calls `info` every 15s and this is not worth that cost yet.
            battery_level: None,
            battery_state: None,
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
        let session = self.session().await?;
        let mut adapter = session.adapter;
        let mut client = connect_service!(
            AppServiceClient<Box<dyn ReadWrite>>,
            &mut adapter,
            &session.handshake
        )?;

        let apps = client
            .list_apps(false, true, false, false, false)
            .await
            .map_err(|err| BackendError::Failed(format!("list apps: {err:?}")))?;

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

        proxy
            .install_with_callback(
                remote_path,
                None,
                |(percent, _)| async move {
                    debug!(percent, "install progress");
                },
                (),
            )
            .await
            .map_err(|err| anyhow!("install: {err:?}"))?;

        progress.report("done", Some(1.0));
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
}
