//! Android device backend: the adb host protocol and scrcpy, spoken directly.
//!
//! ```text
//!   adb.rs      the adb server's host protocol — transport, shell, sync, forward
//!   scrcpy.rs   pushing/starting the server, its two sockets, its control protocol
//!   h264.rs     Annex-B → avcC, so the browser gets the framing it wants
//!   lib.rs      session supervision and the DeviceBackend impl
//! ```
//!
//! One host dependency, and only one: an adb server owning the USB transport.
//! The scrcpy server is embedded in this binary and runs on the phone.

pub mod adb;
pub mod h264;
pub mod scrcpy;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context as _, Result};
use async_trait::async_trait;
use farm_protocol::{AppInfo, Display, Platform};
use provider_core::backend::{
    BackendError, DeviceBackend, DeviceInfo, InputEvent, ProgressSink, RemoteDebug,
    Result as BackendResult,
};
use provider_core::video::{channel, VideoHandle, VideoPublisher};
use tokio::net::TcpStream;
use tokio::sync::{watch, Mutex};
use tracing::{debug, info, warn};

use crate::adb::{parse_getprop, Adb};
use crate::scrcpy::{ScrcpyOptions, ScrcpySession};

/// Delay before rebuilding a session that dropped, so an unplugged or
/// rebooting device does not spin the loop.
const RECONNECT_DELAY: Duration = Duration::from_secs(5);

/// How long a request waits for a session before giving up.
const SESSION_WAIT: Duration = Duration::from_secs(20);

/// How long to wait for a device to come back after `adbd` restarts.
const ADB_RESTART_TIMEOUT: Duration = Duration::from_secs(15);

/// The port `adbd` listens on when told to. 5555 is the convention every
/// `adb connect` example assumes.
const DEVICE_ADB_PORT: u16 = 5555;

/// Longest a contact may stay down with no further samples before the provider
/// lifts it on its own. Same reasoning as the iOS backend: a browser that loses
/// a pointerup would otherwise pin a finger to the glass for the session.
const CONTACT_MAX: Duration = Duration::from_secs(10);

/// Per-device settings from `provider.yaml`'s `options:` map.
#[derive(Clone, Debug)]
pub struct AndroidOptions {
    pub serial: String,
    pub adb_server: String,
    pub scrcpy: ScrcpyOptions,
}

impl AndroidOptions {
    pub fn parse(
        serial: &str,
        options: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self> {
        let number = |key: &str| -> Result<Option<u32>> {
            options
                .get(key)
                .map(|value| {
                    value
                        .as_u64()
                        .map(|n| n as u32)
                        .ok_or_else(|| anyhow!("{key} must be a positive integer"))
                })
                .transpose()
        };

        let defaults = ScrcpyOptions::default();
        Ok(Self {
            serial: serial.to_owned(),
            adb_server: options
                .get("adb_server")
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| anyhow!("adb_server must be a host:port string"))
                })
                .transpose()?
                .unwrap_or_else(|| adb::DEFAULT_ADB_SERVER.to_owned()),
            scrcpy: ScrcpyOptions {
                max_size: number("max_size")?.unwrap_or(defaults.max_size),
                bit_rate: number("bit_rate")?.unwrap_or(defaults.bit_rate),
                max_fps: number("max_fps")?.unwrap_or(defaults.max_fps),
            },
        })
    }
}

/// The pointer state machine.
///
/// Same shape as the iOS backend's, and for the same reason: down/move/up map
/// 1:1 onto DOWN/MOVE/UP motion events, and collapsing them into a tap makes
/// every swipe read as a click.
#[derive(Debug)]
struct Pointer {
    down: bool,
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

/// What a live scrcpy session exposes to the backend.
struct Live {
    control: Mutex<TcpStream>,
    /// Not fixed for the session: a reset or a rotation re-sends it, and touch
    /// coordinates scale against whatever it is *now*.
    geometry: scrcpy::Geometry,
}

pub struct AndroidBackend {
    options: AndroidOptions,
    name: Option<String>,
    adb: Adb,
    video: VideoHandle,
    live: Arc<Mutex<Option<Arc<Live>>>>,
    ready: watch::Receiver<bool>,
    restart: Arc<tokio::sync::Notify>,
    pointer: Mutex<Pointer>,
    clipboard_sequence: std::sync::atomic::AtomicU64,
    exposed_port: Mutex<Option<u16>>,
}

impl AndroidBackend {
    pub fn new(options: AndroidOptions, name: Option<String>) -> Arc<Self> {
        let (video, publisher) = channel();
        let adb = Adb::new(options.adb_server.clone());
        let (ready_tx, ready_rx) = watch::channel(false);
        let live = Arc::new(Mutex::new(None));
        let restart = Arc::new(tokio::sync::Notify::new());

        let backend = Arc::new(Self {
            options: options.clone(),
            name,
            adb: adb.clone(),
            video,
            live: live.clone(),
            ready: ready_rx,
            restart: restart.clone(),
            pointer: Mutex::new(Pointer::default()),
            clipboard_sequence: std::sync::atomic::AtomicU64::new(1),
            exposed_port: Mutex::new(None),
        });

        tokio::spawn(supervise(Supervisor {
            options,
            adb,
            publisher,
            live,
            ready: ready_tx,
            restart,
        }));

        backend
    }

    async fn live(&self) -> BackendResult<Arc<Live>> {
        if !*self.ready.borrow() {
            let mut ready = self.ready.clone();
            let arrived = tokio::time::timeout(SESSION_WAIT, async {
                while ready.changed().await.is_ok() {
                    if *ready.borrow() {
                        return true;
                    }
                }
                false
            })
            .await
            .unwrap_or(false);

            if !arrived {
                return Err(BackendError::Unavailable(
                    "no scrcpy session — the device is unplugged or still starting".into(),
                ));
            }
        }

        self.live
            .lock()
            .await
            .clone()
            .ok_or_else(|| BackendError::Unavailable("the session went away".into()))
    }

    async fn send_control(&self, message: Vec<u8>) -> BackendResult<()> {
        use tokio::io::AsyncWriteExt as _;

        let live = self.live().await?;
        let mut control = live.control.lock().await;
        control
            .write_all(&message)
            .await
            .map_err(|err| BackendError::Unavailable(format!("control socket: {err}")))
    }

    /// Scale a normalised coordinate against the geometry the *server*
    /// reported. The panel's own size is the wrong scale whenever `max_size`
    /// shrank the stream.
    fn to_pixels(value: f64, extent: i64) -> i32 {
        (value.clamp(0.0, 1.0) * extent as f64).round() as i32
    }

    async fn pointer_event(&self, event: &InputEvent) -> BackendResult<()> {
        let live = self.live().await?;
        let (action, x, y, pointer_id) = match event {
            InputEvent::PointerDown { pointer_id, x, y } => {
                let mut pointer = self.pointer.lock().await;
                pointer.down = true;
                pointer.last = (*x, *y);
                pointer.deadline = Some(Instant::now() + CONTACT_MAX);
                (scrcpy::touch_action::DOWN, *x, *y, *pointer_id)
            }
            InputEvent::PointerMove { pointer_id, x, y } => {
                let mut pointer = self.pointer.lock().await;
                // A move with no contact down is dropped rather than promoted:
                // replaying it would start a phantom drag.
                if !pointer.down {
                    return Ok(());
                }
                pointer.last = (*x, *y);
                pointer.deadline = Some(Instant::now() + CONTACT_MAX);
                (scrcpy::touch_action::MOVE, *x, *y, *pointer_id)
            }
            InputEvent::PointerUp { pointer_id, x, y } => {
                let mut pointer = self.pointer.lock().await;
                pointer.down = false;
                pointer.deadline = None;
                (scrcpy::touch_action::UP, *x, *y, *pointer_id)
            }
            _ => return Ok(()),
        };

        let (width, height) = live.geometry.get();
        self.send_control(scrcpy::touch(
            action,
            pointer_id as u64,
            Self::to_pixels(x, width),
            Self::to_pixels(y, height),
            width,
            height,
        ))
        .await
    }

    /// Lift a contact held with no activity, so a lost pointerup cannot leave a
    /// finger pinned to the glass.
    async fn release_if_stale(&self) {
        let stale = {
            let pointer = self.pointer.lock().await;
            pointer.down && pointer.deadline.is_some_and(|at| Instant::now() >= at)
        };
        if !stale {
            return;
        }

        let (x, y) = {
            let mut pointer = self.pointer.lock().await;
            pointer.down = false;
            pointer.deadline = None;
            pointer.last
        };
        warn!(
            "contact held >{}s with no update — releasing",
            CONTACT_MAX.as_secs()
        );
        let _ = self
            .pointer_event(&InputEvent::PointerUp {
                pointer_id: 0,
                x,
                y,
            })
            .await;
    }

    /// Wait until the device reports the adb TCP port we are aiming for.
    ///
    /// `None` means "listening on nothing". Polling the property is what
    /// actually confirms the restart landed; transport presence does not,
    /// because the old transport is still there for a moment after the request.
    async fn await_adb_port(&self, want: Option<u16>) -> BackendResult<()> {
        let deadline = Instant::now() + ADB_RESTART_TIMEOUT;

        loop {
            // The shell itself fails while adbd is down, which is expected and
            // is why this polls rather than asking once.
            if let Ok(value) = self.shell("getprop service.adb.tcp.port").await {
                if listening_port(&value) == want {
                    return Ok(());
                }
            }
            if Instant::now() >= deadline {
                return Err(BackendError::Unavailable(format!(
                    "adbd did not settle on {} within {ADB_RESTART_TIMEOUT:?}",
                    want.map(|p| p.to_string()).unwrap_or_else(|| "usb".into())
                )));
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }

    async fn shell(&self, command: &str) -> BackendResult<String> {
        self.adb
            .shell(&self.options.serial, command)
            .await
            .map_err(|err| BackendError::Unavailable(format!("{err:#}")))
    }
}

#[async_trait]
impl DeviceBackend for AndroidBackend {
    async fn info(&self) -> BackendResult<DeviceInfo> {
        let props = parse_getprop(&self.shell("getprop").await?);
        let get = |key: &str| props.get(key).cloned().filter(|value| !value.is_empty());

        // The stream's geometry when there is a session, the panel's otherwise:
        // a device that is not streaming yet still has a screen worth
        // reporting, and the two differ whenever `max_size` scaled the stream.
        let display = match self.live.lock().await.clone() {
            Some(live) => {
                let (width, height) = live.geometry.get();
                Some(Display {
                    width,
                    height,
                    scale: None,
                    rotation: None,
                })
            }
            None => {
                let size = parse_wm_size(&self.shell("wm size").await.unwrap_or_default());
                // Density is only worth a round-trip if there is a size to
                // attach it to.
                let scale = match size {
                    Some(_) => parse_density(&self.shell("wm density").await.unwrap_or_default())
                        .map(|density| density as f64 / 160.0),
                    None => None,
                };
                size.map(|(width, height)| Display {
                    width,
                    height,
                    scale,
                    rotation: None,
                })
            }
        };

        let battery = self.shell("dumpsys battery").await.unwrap_or_default();

        Ok(DeviceInfo {
            id: self.options.serial.clone(),
            platform: Platform::Android,
            name: self.name.clone().or_else(|| get("ro.product.model")),
            model: get("ro.product.model"),
            manufacturer: get("ro.product.manufacturer"),
            os_version: get("ro.build.version.release"),
            abi: get("ro.product.cpu.abi"),
            sdk: get("ro.build.version.sdk").and_then(|sdk| sdk.parse().ok()),
            display,
            battery_level: parse_battery_level(&battery),
            battery_state: parse_battery_state(&battery),
        })
    }

    fn video(&self) -> VideoHandle {
        self.video.clone()
    }

    async fn input(&self, event: InputEvent) -> BackendResult<()> {
        match &event {
            InputEvent::PointerDown { .. }
            | InputEvent::PointerMove { .. }
            | InputEvent::PointerUp { .. } => self.pointer_event(&event).await,

            InputEvent::Key { key, down } => {
                let Some(code) = keycode_for(key) else {
                    debug!(key, "no Android keycode for this key");
                    return Ok(());
                };
                let action = if *down { 0 } else { 1 };
                self.send_control(scrcpy::keycode(action, code)).await
            }

            InputEvent::Text { text } => self.send_control(scrcpy::text(text)).await,
        }
    }

    /// `screencap -p` over a plain shell.
    async fn screenshot(&self) -> BackendResult<Vec<u8>> {
        use tokio::io::AsyncReadExt as _;

        let mut stream = self
            .adb
            .shell_stream(&self.options.serial, "screencap -p")
            .await
            .map_err(|err| BackendError::Unavailable(format!("{err:#}")))?;

        let mut png = Vec::new();
        stream
            .inner_mut()
            .read_to_end(&mut png)
            .await
            .map_err(|err| BackendError::Failed(format!("reading the screenshot: {err}")))?;

        if !png.starts_with(&[0x89, b'P', b'N', b'G']) {
            return Err(BackendError::Failed(
                "screencap did not return a PNG".into(),
            ));
        }
        Ok(png)
    }

    async fn clipboard_get(&self) -> BackendResult<Option<String>> {
        use tokio::io::AsyncWriteExt as _;

        let live = self.live().await?;
        let mut control = live.control.lock().await;
        control
            .write_all(&scrcpy::get_clipboard())
            .await
            .map_err(|err| BackendError::Unavailable(format!("control socket: {err}")))?;

        // An empty clipboard produces no reply at all, so this must not wait
        // forever for one.
        match tokio::time::timeout(
            Duration::from_secs(2),
            scrcpy::read_device_message(&mut control),
        )
        .await
        {
            Ok(Ok(text)) => Ok(text),
            Ok(Err(err)) => Err(BackendError::Failed(format!("{err:#}"))),
            Err(_) => Ok(None),
        }
    }

    async fn clipboard_set(&self, text: &str) -> BackendResult<()> {
        let sequence = self
            .clipboard_sequence
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.send_control(scrcpy::set_clipboard(sequence, text, false))
            .await
    }

    async fn apps(&self) -> BackendResult<Vec<AppInfo>> {
        let listing = self.shell("pm list packages -3").await?;
        Ok(listing
            .lines()
            .filter_map(|line| line.trim().strip_prefix("package:"))
            .filter(|id| !id.is_empty())
            .map(|id| AppInfo {
                id: id.to_owned(),
                // `pm list packages` carries no label, and resolving one costs
                // a shell round-trip per app. The UI falls back to the id.
                name: None,
                version: None,
                system: Some(false),
            })
            .collect())
    }

    async fn install(&self, staged: &Path, progress: &dyn ProgressSink) -> BackendResult<()> {
        let apk = tokio::fs::read(staged)
            .await
            .with_context(|| format!("reading {}", staged.display()))
            .map_err(BackendError::from)?;

        let remote = format!("/data/local/tmp/farm-install-{}.apk", staging_suffix());
        progress.report("uploading", None);
        self.adb
            .push(&self.options.serial, &remote, &apk, 0o644)
            .await
            .map_err(|err| BackendError::Failed(format!("{err:#}")))?;

        progress.report("installing", None);
        let output = self.shell(&format!("pm install -r -d {remote}")).await?;

        // The staged copy must go whatever happens: a provider that leaks an
        // APK per install fills /data/local/tmp and wedges the device.
        let _ = self.shell(&format!("rm -f {remote}")).await;

        if !output.contains("Success") {
            return Err(BackendError::Failed(format!(
                "install failed: {}",
                output.trim()
            )));
        }
        progress.report("done", Some(1.0));
        Ok(())
    }

    async fn uninstall(&self, app_id: &str) -> BackendResult<()> {
        let output = self.shell(&format!("pm uninstall {app_id}")).await?;
        if !output.contains("Success") {
            return Err(BackendError::Failed(format!(
                "uninstall failed: {}",
                output.trim()
            )));
        }
        Ok(())
    }

    async fn launch(&self, app_id: &str, args: &[String]) -> BackendResult<()> {
        let extra = args.join(" ");
        let output = self
            .shell(&format!(
                "monkey -p {app_id} -c android.intent.category.LAUNCHER 1 {extra}"
            ))
            .await?;
        if output.contains("No activities found") {
            return Err(BackendError::Failed(format!(
                "{app_id} has no launchable activity"
            )));
        }
        Ok(())
    }

    /// scrcpy rotates 90° at a time, so an absolute angle is walked to.
    async fn rotate(&self, degrees: i64) -> BackendResult<()> {
        let steps = degrees.div_euclid(90).rem_euclid(4);
        for _ in 0..steps {
            self.send_control(scrcpy::rotate_device()).await?;
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        Ok(())
    }

    async fn reboot(&self) -> BackendResult<()> {
        // `reboot` never answers — the transport dies with the device — so its
        // silence is success and only an early error is interesting.
        let _ = self.shell("reboot").await;
        Ok(())
    }

    /// Expose this device's adb transport on a provider port.
    ///
    /// This is remote debugging: a developer runs `adb connect provider:port`
    /// and gets the device on their own machine. `service.adb.tcp.port` is what
    /// makes the device listen at all; without it the forward reaches nothing.
    async fn remote_debug(&self) -> BackendResult<RemoteDebug> {
        if let Some(port) = *self.exposed_port.lock().await {
            return Ok(RemoteDebug { port });
        }

        // Make the device listen before forwarding to it. `tcpip:` is a
        // transport service; the setprop recipe needs root and fails silently.
        self.adb
            .tcpip(&self.options.serial, DEVICE_ADB_PORT)
            .await
            .map_err(|err| BackendError::Failed(format!("{err:#}")))?;

        // Wait for the *end state*, not for the transport to reappear: adbd
        // has not dropped yet at this point, so a presence check passes
        // immediately against the connection that is about to die, and
        // everything after it races the restart.
        self.await_adb_port(Some(DEVICE_ADB_PORT)).await?;

        let port = pick_port().map_err(|err| BackendError::Failed(err.to_string()))?;
        self.adb
            .forward(&self.options.serial, port)
            .await
            .map_err(|err| BackendError::Failed(format!("{err:#}")))?;

        *self.exposed_port.lock().await = Some(port);
        info!(serial = %self.options.serial, port, "adb transport exposed");
        Ok(RemoteDebug { port })
    }

    /// Stop remote debugging, and stop the *device* listening too.
    ///
    /// Removing the forward alone would leave `adbd` bound to port 5555 on
    /// whatever network the phone is on, reachable by anyone who can route to
    /// it, for as long as the device stays up. A farm device must not be left
    /// in that state because someone finished debugging.
    async fn remote_debug_stop(&self) -> BackendResult<()> {
        let exposed = self.exposed_port.lock().await.take();

        if let Some(port) = exposed {
            // Best effort. The forward may already be gone — an adb server
            // restart loses every one of them — and that must not stop the
            // part that closes the device's listener.
            if let Err(err) = self.adb.forward_remove(&self.options.serial, port).await {
                debug!(port, error = %format!("{err:#}"), "forward was already gone");
            }
        }

        // Runs even when no forward was recorded: a provider restart loses that
        // memory while the device keeps listening, and this is the only thing
        // that closes it.
        self.adb
            .usb_only(&self.options.serial)
            .await
            .map_err(|err| BackendError::Failed(format!("{err:#}")))?;
        // Same restart, same wait: returning before adbd is back makes the
        // next command fail for a reason that has nothing to do with it.
        self.await_adb_port(None).await?;
        info!(serial = %self.options.serial, "adb transport withdrawn");
        Ok(())
    }

    async fn restart(&self) -> BackendResult<()> {
        info!(serial = %self.options.serial, "restarting the scrcpy session");
        self.restart.notify_one();
        Ok(())
    }

    async fn is_healthy(&self) -> bool {
        // The health poll is also the only regular tick this backend gets, so
        // it is where a forgotten contact gets lifted.
        self.release_if_stale().await;
        *self.ready.borrow()
    }
}

struct Supervisor {
    options: AndroidOptions,
    adb: Adb,
    publisher: VideoPublisher,
    live: Arc<Mutex<Option<Arc<Live>>>>,
    ready: watch::Sender<bool>,
    restart: Arc<tokio::sync::Notify>,
}

/// Rebuild the scrcpy session forever.
///
/// The video socket ending *is* the session ending: the server exits with it,
/// and its control socket is then talking to nothing.
async fn supervise(supervisor: Supervisor) {
    loop {
        if let Err(err) = run_once(&supervisor).await {
            warn!(
                serial = %supervisor.options.serial,
                error = %format!("{err:#}"),
                "scrcpy session ended; retrying in {}s",
                RECONNECT_DELAY.as_secs()
            );
        }

        // Publish the teardown before sleeping, so the device reads as
        // unhealthy for the whole gap rather than only at its end.
        let _ = supervisor.ready.send(false);
        *supervisor.live.lock().await = None;
        supervisor.publisher.mark_stopped();

        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

async fn run_once(supervisor: &Supervisor) -> Result<()> {
    let serial = &supervisor.options.serial;
    let session = ScrcpySession::start(&supervisor.adb, serial, &supervisor.options.scrcpy).await?;

    let ScrcpySession {
        video,
        control,
        width,
        height,
        ..
    } = session;

    let geometry = scrcpy::Geometry::default();
    geometry.set(width, height);

    *supervisor.live.lock().await = Some(Arc::new(Live {
        control: Mutex::new(control),
        geometry: geometry.clone(),
    }));
    let _ = supervisor.ready.send(true);

    // Keyframe requests go through the same mutex as every other control
    // message: two writers on one socket would interleave mid-message.
    let keyframes = {
        let live = supervisor.live.clone();
        let publisher = supervisor.publisher.clone();
        tokio::spawn(async move {
            loop {
                publisher.keyframe_requested().await;
                let Some(live) = live.lock().await.clone() else {
                    return;
                };
                let mut control = live.control.lock().await;
                if let Err(err) = scrcpy::request_keyframe(&mut control).await {
                    debug!(error = %err, "keyframe request failed");
                    return;
                }
            }
        })
    };

    let outcome = tokio::select! {
        result = scrcpy::pump_video(video, supervisor.publisher.clone(), geometry) => result,
        _ = supervisor.restart.notified() => Err(anyhow!("restart requested")),
    };

    keyframes.abort();
    outcome
}

/// `wm size` prints `Physical size: WxH` and, when the user has overridden the
/// resolution, an `Override size:` line that is what the device actually
/// renders at. The override wins when present.
pub fn parse_wm_size(output: &str) -> Option<(i64, i64)> {
    let parse = |line: &str| -> Option<(i64, i64)> {
        let (width, height) = line.rsplit_once(": ")?.1.trim().split_once('x')?;
        Some((width.trim().parse().ok()?, height.trim().parse().ok()?))
    };

    let mut physical = None;
    for line in output.lines() {
        if line.contains("Override size:") {
            return parse(line);
        }
        if line.contains("Physical size:") {
            physical = parse(line);
        }
    }
    physical
}

pub fn parse_density(output: &str) -> Option<i64> {
    output
        .lines()
        .find_map(|line| line.rsplit_once(": ")?.1.trim().parse().ok())
}

/// Battery charge as a fraction.
///
/// `dumpsys battery` reports `level` against `scale` — 0..100 on every device
/// seen so far, but the scale is there precisely because that is not
/// guaranteed. The protocol carries 0..1, and reporting the raw level instead
/// renders as "9900%".
pub fn parse_battery_level(dump: &str) -> Option<f64> {
    let field = |name: &str| -> Option<f64> {
        dump.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            (key.trim() == name).then(|| value.trim().parse::<f64>().ok())?
        })
    };

    let level = field("level")?;
    let scale = field("scale").filter(|scale| *scale > 0.0).unwrap_or(100.0);
    Some((level / scale).clamp(0.0, 1.0))
}

pub fn parse_battery_state(dump: &str) -> Option<String> {
    let status = dump.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        (key.trim() == "status").then(|| value.trim().parse::<i64>().ok())?
    })?;

    // Android's BatteryManager.BATTERY_STATUS_* constants.
    Some(
        match status {
            2 => "charging",
            3 => "discharging",
            4 => "not_charging",
            5 => "full",
            _ => "unknown",
        }
        .to_owned(),
    )
}

/// Browser `KeyboardEvent.key` → Android `KEYCODE_*`.
///
/// Named keys only: printable characters arrive as `text` and go through
/// `INJECT_TEXT`, which handles anything a keycode table cannot express.
pub fn keycode_for(key: &str) -> Option<u32> {
    Some(match key {
        "Enter" => 66,
        "Backspace" => 67,
        "Tab" => 61,
        "Escape" => 111,
        "ArrowUp" => 19,
        "ArrowDown" => 20,
        "ArrowLeft" => 21,
        "ArrowRight" => 22,
        "Home" => 3,
        "Delete" => 112,
        "PageUp" => 92,
        "PageDown" => 93,
        // Android-specific names a control surface may send.
        "Back" => 4,
        "AppSwitch" | "Recents" => 187,
        "Power" => 26,
        "VolumeUp" => 24,
        "VolumeDown" => 25,
        _ => return None,
    })
}

/// The port `adbd` is listening on, if any.
///
/// Devices disagree about how to say "not listening": a Galaxy S25+ reports
/// `0`, others `-1`, and some an empty string. Anything that is not a positive
/// port number means the same thing, so this normalises rather than matching
/// one vendor's spelling.
fn listening_port(value: &str) -> Option<u16> {
    match value.trim().parse::<i32>() {
        Ok(port) if port > 0 => u16::try_from(port).ok(),
        _ => None,
    }
}

/// A free TCP port, chosen by binding one and letting the OS pick.
fn pick_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

/// A short unique suffix for staged filenames. It only has to keep two
/// concurrent installs on one device from colliding.
fn staging_suffix() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.subsec_nanos())
        .unwrap_or(0);
    format!("{nanos:08x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_override_resolution_wins_over_the_panel() {
        // A real S25+ with the display set to FHD+: what it renders is the
        // override, and touch coordinates scaled to the panel would be wrong.
        let output = "Physical size: 1440x3120\nOverride size: 1080x2340\n";
        assert_eq!(parse_wm_size(output), Some((1080, 2340)));

        let plain = "Physical size: 1080x2400\n";
        assert_eq!(parse_wm_size(plain), Some((1080, 2400)));
        assert_eq!(parse_wm_size(""), None);
    }

    #[test]
    fn reads_density_and_battery() {
        assert_eq!(parse_density("Physical density: 450\n"), Some(450));

        let dump = "Current Battery Service state:\n  level: 87\n  status: 2\n  scale: 100\n";
        // A fraction, not a percentage: the protocol carries 0..1 and the UI
        // multiplies. Reporting 87 here renders as "8700%".
        assert_eq!(parse_battery_level(dump), Some(0.87));
        assert_eq!(parse_battery_state(dump).as_deref(), Some("charging"));
    }

    #[test]
    fn not_listening_is_spelled_several_ways() {
        assert_eq!(listening_port("5555"), Some(5555));
        // A Galaxy S25+ says 0; the docs say -1; a fresh device says nothing.
        assert_eq!(listening_port("0"), None);
        assert_eq!(listening_port("-1"), None);
        assert_eq!(listening_port(""), None);
        assert_eq!(listening_port("\n"), None);
    }

    #[test]
    fn battery_honours_a_non_standard_scale() {
        // The scale field exists because 0..100 is convention, not guarantee.
        let dump = "  level: 128\n  scale: 255\n  status: 3\n";
        let level = parse_battery_level(dump).unwrap();
        assert!((level - 0.502).abs() < 0.001, "got {level}");

        // A missing or nonsense scale must not divide by zero.
        assert_eq!(parse_battery_level("  level: 50\n  scale: 0\n"), Some(0.5));
        assert_eq!(parse_battery_level("  level: 50\n"), Some(0.5));
    }

    #[test]
    fn key_names_are_the_browser_ones() {
        assert_eq!(keycode_for("Enter"), Some(66));
        assert_eq!(keycode_for("Backspace"), Some(67));
        assert_eq!(keycode_for("Home"), Some(3));
        // Printable characters go through INJECT_TEXT instead.
        assert_eq!(keycode_for("a"), None);
    }

    #[test]
    fn options_default_without_a_config_block() {
        let options = AndroidOptions::parse("R5CY82FT35T", &serde_json::Map::new()).unwrap();
        assert_eq!(options.adb_server, adb::DEFAULT_ADB_SERVER);
        assert_eq!(options.scrcpy.max_size, ScrcpyOptions::default().max_size);
    }

    #[test]
    fn a_mistyped_option_is_a_config_error_not_a_silent_default() {
        let mut options = serde_json::Map::new();
        options.insert("max_size".into(), serde_json::json!("big"));
        assert!(AndroidOptions::parse("serial", &options).is_err());
    }
}
