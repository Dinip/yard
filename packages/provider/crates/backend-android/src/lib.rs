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
pub mod bridge;
pub mod h264;
pub mod metrics;
pub mod scrcpy;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use adb_bridge::Bridge;
use anyhow::{anyhow, Context as _, Result};
use async_trait::async_trait;
use farm_protocol::{AppInfo, Display, FileEntry, FileKind, FileListing, Platform};
use provider_core::adb_auth::AdbAuthority;
use provider_core::backend::{
    join_path, parent_of, AppFilter, BackendError, DeviceBackend, DeviceInfo, DeviceMetrics,
    InputEvent, ProgressSink, RemoteDebug, Result as BackendResult,
};
use provider_core::ports::{PortLease, PortPool};

use crate::bridge::{DeviceAuthorizer, DeviceServices};
use provider_core::video::{channel, VideoGeometry, VideoHandle, VideoPublisher};
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

/// Where the file browser opens. Shared external storage is where anything a
/// tester wants off a phone ends up — screenshots, exports, downloads — and it
/// is what an unrooted adb can actually read.
const FILES_ROOT: &str = "/sdcard";

/// Longest a contact may stay down with no further samples before the provider
/// lifts it on its own. Same reasoning as the iOS backend: a browser that loses
/// a pointerup would otherwise pin a finger to the glass for the session.
const CONTACT_MAX: Duration = Duration::from_secs(10);

/// Per-device settings from `provider.yaml`'s `options:` map.
#[derive(Clone, Debug)]
pub struct AndroidOptions {
    pub serial: String,
    pub adb_server: String,
    /// Where remote debugging draws its listening port from. Absent means the
    /// device cannot be exposed at all — set by the provider from
    /// `remote_debug.ports`, not by this device's own `options:` block.
    pub debug_ports: Option<Arc<PortPool>>,
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
            debug_ports: None,
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
    exposed: Mutex<Option<Exposed>>,
}

/// A live remote-debugging listener: the accept loop, and the port it holds.
struct Exposed {
    lease: PortLease,
    task: tokio::task::JoinHandle<()>,
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
            exposed: Mutex::new(None),
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

        // Reported, not assumed: `rotate` walks a delta against it, so a device
        // that is already at 270 must not look like it is at 0.
        let rotation = current_rotation(&self.adb, &self.options.serial).await;

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
                    rotation,
                    render_rotation: Some(0),
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
                    rotation,
                    render_rotation: Some(0),
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
            // All from the single `getprop` read above — no extra round-trip.
            serial: get("ro.serialno").or_else(|| Some(self.options.serial.clone())),
            brand: get("ro.product.brand"),
            build_id: get("ro.build.display.id"),
            security_patch: get("ro.build.version.security_patch"),
            abi_list: get("ro.product.cpu.abilist"),
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

    async fn clear_app_data(&self, app_id: &str) -> BackendResult<()> {
        let output = self.shell(&format!("pm clear {app_id}")).await?;
        if !output.contains("Success") {
            return Err(BackendError::Failed(format!(
                "clearing {app_id} failed: {}",
                output.trim()
            )));
        }
        Ok(())
    }

    async fn reset_screen(&self) -> BackendResult<()> {
        // The hardware Home key, not the keyboard's — `KEYCODE_HOME` is 3, and
        // the two were deliberately split apart in phase 15.
        self.send_control(scrcpy::keycode(0, 3)).await?;
        self.send_control(scrcpy::keycode(1, 3)).await?;
        self.rotate(0).await?;
        // Whatever the last user copied off the device is theirs, not the next
        // user's. Best-effort: a device with no clipboard service is not a
        // reason to fail the run.
        let _ = self.clipboard_set("").await;
        Ok(())
    }

    async fn wipe_paths(&self, paths: &[String]) -> BackendResult<()> {
        for path in paths {
            // The contents, not the directory: removing `/sdcard/Download`
            // itself leaves apps that expect it writing into nothing. The
            // quoting is what keeps a path with a space from becoming two
            // arguments to `rm -rf`.
            let output = self
                .shell(&format!("rm -rf '{}'/* '{}'/.[!.]*", path, path))
                .await?;
            // `.[!.]*` matches nothing when there are no dotfiles and the shell
            // passes the pattern through literally, so "No such file" here is
            // the expected case, not a failure.
            let trimmed = output.trim();
            if !trimmed.is_empty() && !trimmed.contains("No such file") {
                return Err(BackendError::Failed(format!("wiping {path}: {trimmed}")));
            }
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

    fn files_root(&self) -> Option<&'static str> {
        Some(FILES_ROOT)
    }

    /// One directory, straight off the sync protocol's `LIST`.
    ///
    /// Navigation is deliberately not fenced to [`FILES_ROOT`]: adb runs
    /// unrooted here, so the device's own permissions are the real boundary and
    /// a second one drawn in the provider would only be decoration — it would
    /// hide `/sdcard/Android/data` while `/data/data` still answered "Permission
    /// denied" on its own. A directory adb cannot read says so, in the device's
    /// own words.
    async fn list_files(&self, path: &str) -> BackendResult<FileListing> {
        let path = path.trim_end_matches('/');
        let path = if path.is_empty() { "/" } else { path };

        let entries = self
            .adb
            .list(&self.options.serial, path)
            .await
            .map_err(|err| BackendError::Failed(format!("{err:#}")))?;

        let entries = entries
            .iter()
            .map(|entry| FileEntry {
                path: join_path(path, &entry.name),
                name: entry.name.clone(),
                kind: if entry.is_dir() {
                    FileKind::Directory
                } else if entry.is_file() {
                    FileKind::File
                } else {
                    FileKind::Other
                },
                size: entry.is_file().then_some(entry.size as i64),
                // Seconds on the wire, milliseconds everywhere in this project.
                modified_at: (entry.mtime > 0).then(|| entry.mtime as i64 * 1000),
            })
            .collect();

        Ok(FileListing {
            path: path.to_owned(),
            parent: parent_of(path).map(str::to_owned),
            entries,
        })
    }

    async fn pull_file(&self, path: &str, dest: &Path) -> BackendResult<u64> {
        self.adb
            .pull(&self.options.serial, path, dest)
            .await
            .map_err(|err| BackendError::Failed(format!("{err:#}")))
    }

    /// scrcpy rotates 90° at a time, so an absolute angle is walked to.
    ///
    /// The walk is a *delta* against where the device actually is. Treating the
    /// target as the step count works only while rotation is never reported —
    /// the moment it is, rotating from 270 asks for 0 and would take no steps
    /// at all.
    async fn rotate(&self, degrees: i64) -> BackendResult<()> {
        let current = current_rotation(&self.adb, &self.options.serial)
            .await
            .unwrap_or(0);
        let steps = rotation_steps(current, degrees);
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

    /// Expose this device on a provider port for `adb connect`.
    ///
    /// The provider answers that connection itself — see the `adb-bridge`
    /// crate — rather than forwarding it to the device's own `adbd`. That is
    /// what makes remote debugging work without enrolling every developer's key
    /// on every phone: the only key a device trusts is this provider's.
    ///
    /// It is also why nothing here touches `tcpip:` any more. The device never
    /// listens on the network, so there is no `adbd` restart to survive and no
    /// port left open on whatever Wi-Fi the phone is on.
    ///
    /// The listener is the provider's own, on a port claimed from the pool
    /// `remote_debug.ports` configures. `adb forward` cannot serve this: its
    /// socket binds the adb server's loopback, so a containerised provider
    /// forwards to a port nothing outside the container can reach, on an
    /// ephemeral number nothing could have published.
    async fn remote_debug(&self, authority: Arc<AdbAuthority>) -> BackendResult<RemoteDebug> {
        if let Some(exposed) = self.exposed.lock().await.as_ref() {
            return Ok(RemoteDebug {
                port: exposed.lease.port(),
            });
        }

        let pool = self.options.debug_ports.as_ref().ok_or_else(|| {
            BackendError::Failed("remote debugging is not configured on this provider".into())
        })?;

        let (lease, listener) = bind_from_pool(pool)
            .await
            .map_err(|err| BackendError::Failed(format!("{err:#}")))?;
        let port = lease.port();

        let task = tokio::spawn(serve_adb_bridge(
            listener,
            self.adb.clone(),
            self.options.serial.clone(),
            authority,
        ));

        *self.exposed.lock().await = Some(Exposed { lease, task });
        info!(serial = %self.options.serial, port, "adb bridge listening");
        Ok(RemoteDebug { port })
    }

    /// Stop accepting `adb connect` clients and drop the ones connected.
    ///
    /// Aborting the task drops the listener and every connection under it: a
    /// developer must not keep driving a device after the reservation that
    /// granted it ended. Dropping the lease returns the port to the pool.
    ///
    /// There is nothing to undo on the device. It was never asked to listen.
    async fn remote_debug_stop(&self) -> BackendResult<()> {
        if let Some(exposed) = self.exposed.lock().await.take() {
            exposed.task.abort();
            info!(serial = %self.options.serial, "adb bridge withdrawn");
        }
        Ok(())
    }

    async fn restart(&self) -> BackendResult<()> {
        info!(serial = %self.options.serial, "restarting the scrcpy session");
        self.restart.notify_one();
        Ok(())
    }

    async fn remote_debug_port(&self) -> Option<u16> {
        self.exposed
            .lock()
            .await
            .as_ref()
            .map(|exposed| exposed.lease.port())
    }

    async fn is_healthy(&self) -> bool {
        // The health poll is also the only regular tick this backend gets, so
        // it is where a forgotten contact gets lifted.
        self.release_if_stale().await;
        *self.ready.borrow()
    }

    /// Two adb round trips, or three when app patterns are configured.
    ///
    /// Deliberately separate from `info()`, which already makes 3-5 every 15s on
    /// the supervisor's cadence. See `metrics.rs` for why the system reads are
    /// batched into one `sh -c`.
    async fn metrics(&self, apps: &AppFilter) -> BackendResult<DeviceMetrics> {
        let batch = self.shell(metrics::SYSTEM_BATCH).await?;
        let sections = metrics::split_sections(&batch);
        let section = |name: &str| sections.get(name).copied().unwrap_or_default();

        let battery = section("batt");
        let mut out = DeviceMetrics {
            cpu: metrics::parse_proc_stat(section("stat")),
            memory: metrics::parse_meminfo(section("mem")),
            battery_level: parse_battery_level(battery),
            battery_charging: parse_battery_state(battery).map(|state| state == "charging"),
            battery_temperature_c: metrics::parse_battery_temperature(battery),
            thermal_zones: metrics::parse_thermal_zones(section("ztype"), section("ztemp")),
            apps: Vec::new(),
        };

        if apps.is_empty() {
            return Ok(out);
        }

        // `dumpsys meminfo` walks every process on the device — a few hundred
        // milliseconds — which is why it is skipped entirely above rather than
        // gathered and filtered.
        let dump = self.shell("dumpsys meminfo").await?;
        let processes = metrics::parse_meminfo_pss(&dump);

        let pids: Vec<i64> = processes
            .iter()
            .filter(|(_, process, _)| apps.matches(process))
            .map(|(pid, _, _)| *pid)
            .collect();

        let cpu_by_pid = if pids.is_empty() {
            Default::default()
        } else {
            self.shell(&metrics::pid_stat_command(&pids))
                .await
                .unwrap_or_default()
                .lines()
                .filter_map(metrics::parse_pid_stat)
                .collect()
        };

        out.apps = metrics::assemble_apps(&processes, &cpu_by_pid, apps);
        Ok(out)
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
    close_legacy_tcp_listener(&supervisor.adb, &supervisor.options.serial).await;

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

    // Announcing geometry lives here rather than inside `pump_video` because
    // the rotation it carries costs a shell round-trip, and the video loop must
    // never block on the device to read the next packet.
    let announce = {
        let mut changes = geometry.watch();
        let publisher = supervisor.publisher.clone();
        let adb = supervisor.adb.clone();
        let serial = serial.clone();
        tokio::spawn(async move {
            loop {
                let (width, height) = *changes.borrow_and_update();
                if width > 0 && height > 0 {
                    let rotation = current_rotation(&adb, &serial).await;
                    publisher.set_geometry(VideoGeometry {
                        width,
                        height,
                        rotation,
                        // scrcpy re-encodes at the rotated dimensions, so what
                        // arrives is already the right way up.
                        render_rotation: Some(0),
                    });
                }
                if changes.changed().await.is_err() {
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
    announce.abort();
    outcome
}

/// The display's current rotation, in degrees.
///
/// One `dumpsys` call. `None` when the device does not answer — a missing
/// rotation is reported as unknown rather than guessed as zero, because
/// [`AndroidBackend::rotate`] walks a delta against it.
async fn current_rotation(adb: &Adb, serial: &str) -> Option<i64> {
    let output = adb.shell(serial, "dumpsys window displays").await.ok()?;
    parse_rotation(&output)
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

/// How many 90° steps to walk from `current` to `target`.
pub fn rotation_steps(current: i64, target: i64) -> i64 {
    ((target - current).div_euclid(90)).rem_euclid(4)
}

/// Rotation out of `dumpsys window displays`, in degrees.
///
/// Two spellings in the wild and both appear on devices this farm will see:
/// `mCurrentRotation=ROTATION_90` on Android 12+, and a bare quarter-turn count
/// (`mRotation=1`) before it. A quarter-turn count is multiplied out; anything
/// already in degrees is taken as-is.
pub fn parse_rotation(output: &str) -> Option<i64> {
    for key in ["mCurrentRotation=", "mRotation=", "rotation="] {
        for line in output.lines() {
            let Some(rest) = line.split(key).nth(1) else {
                continue;
            };
            let token: String = rest
                .chars()
                .take_while(|c| !c.is_whitespace() && *c != ',')
                .collect();
            let value: i64 = token
                .strip_prefix("ROTATION_")
                .unwrap_or(&token)
                .parse()
                .ok()?;
            return Some(match value {
                0..=3 => value * 90,
                degrees => degrees.rem_euclid(360),
            });
        }
    }
    None
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
        // The *text-editing* keys. `Home` is not one of them: it is the
        // hardware button below, and a browser that sent the keyboard's Home
        // key as `Home` threw the device to the launcher mid-sentence.
        "MoveHome" => 122,
        "MoveEnd" => 123,
        "Delete" => 112,
        "PageUp" => 92,
        "PageDown" => 93,
        // Hardware buttons, and Android-specific names a control surface may
        // send. These leave whatever app is in front.
        "Home" => 3,
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

/// Claim a port from the pool and listen on it, skipping any the host already
/// has taken — the pool tracks what this provider handed out, not what else on
/// the machine happens to be bound.
async fn bind_from_pool(pool: &Arc<PortPool>) -> Result<(PortLease, tokio::net::TcpListener)> {
    // Held rather than dropped as we go, so a port that failed to bind is not
    // immediately handed back out and retried in the same loop.
    let mut taken = Vec::new();

    let outcome = loop {
        let Some(lease) = pool.claim() else {
            break Err(anyhow!(
                "every remote-debugging port is in use; widen remote_debug.ports"
            ));
        };

        match tokio::net::TcpListener::bind(("0.0.0.0", lease.port())).await {
            Ok(listener) => break Ok((lease, listener)),
            Err(err) => {
                debug!(port = lease.port(), error = %err, "debug port is not bindable");
                taken.push(lease);
            }
        }
    };

    drop(taken);
    outcome
}

/// Accept `adb connect` clients and serve each one the ADB protocol.
///
/// One task per connection, and a refused or broken client closes only itself:
/// the listener outlives it, because a phone that drops mid-debug otherwise
/// takes the port with it until the session ends.
///
/// Those tasks are children of this one, in a `JoinSet`, rather than free
/// `tokio::spawn`s. That is what makes withdrawing the bridge close the
/// connections under it: a detached task holds its socket for as long as the
/// client keeps it open, so a developer went on driving a device after the
/// reservation that granted it had ended.
async fn serve_adb_bridge(
    listener: tokio::net::TcpListener,
    adb: Adb,
    serial: String,
    authority: Arc<AdbAuthority>,
) {
    // Shared across connections: the entitled key set is read per handshake, so
    // one instance stays current, and the banner is cached behind it.
    let authorizer = Arc::new(DeviceAuthorizer::new(authority.clone()));
    let services = Arc::new(DeviceServices::new(adb, serial.clone(), authority));
    let mut clients = tokio::task::JoinSet::new();

    loop {
        let client = tokio::select! {
            accepted = listener.accept() => accepted,
            // Reaping finished clients is the only reason to wake for them; a
            // set never drained grows one entry per `adb connect` for as long
            // as the device stays exposed.
            Some(_) = clients.join_next() => continue,
        };
        let Ok((client, peer)) = client else {
            return;
        };

        let bridge = Bridge::new(authorizer.clone(), services.clone());
        let serial = serial.clone();
        clients.spawn(async move {
            if let Err(err) = bridge.serve(client, &peer.to_string()).await {
                // Refusals are ordinary — an unregistered key, a holder who
                // said no — so this is not a device fault.
                debug!(%serial, %peer, error = %err, "adb client refused");
            }
        });
    }
}

/// Close a network `adbd` port left open by an older provider.
///
/// Remote debugging used to work by putting the device into `tcpip:` mode and
/// forwarding to its own `adbd`. The bridge does not, so nothing turns that
/// port off any more — and a device upgraded mid-exposure would sit there
/// listening on whatever network it is on, indefinitely, for anyone whose key
/// it already trusts. This is the one thing that closes it.
///
/// Checked before acting because `adb usb` restarts `adbd`: doing it
/// unconditionally would bounce every device on every provider start.
async fn close_legacy_tcp_listener(adb: &Adb, serial: &str) {
    let Ok(value) = adb.shell(serial, "getprop service.adb.tcp.port").await else {
        return;
    };
    let Some(port) = listening_port(&value) else {
        return;
    };

    warn!(
        %serial,
        port,
        "this device is still listening for adb over the network, from before the bridge; closing it"
    );
    if let Err(err) = adb.usb_only(serial).await {
        warn!(%serial, error = %format!("{err:#}"), "could not close the legacy adb port");
    }
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
    fn rotation_is_read_in_either_spelling() {
        // Android 12+.
        assert_eq!(
            parse_rotation("  Display: mDisplayId=0\n  mCurrentRotation=ROTATION_90\n"),
            Some(90)
        );
        assert_eq!(parse_rotation("mCurrentRotation=ROTATION_0"), Some(0));
        // Older: a quarter-turn count, not degrees.
        assert_eq!(parse_rotation("  mRotation=3 mLastRotation=0"), Some(270));
        assert_eq!(parse_rotation("nothing here"), None);
    }

    #[test]
    fn rotating_walks_the_delta_not_the_target() {
        // The bug this replaces: `rotate(0)` from 270 took zero steps, so a
        // device already turned could never be turned back.
        assert_eq!(rotation_steps(270, 0), 1);
        assert_eq!(rotation_steps(0, 90), 1);
        assert_eq!(rotation_steps(90, 90), 0);
        assert_eq!(rotation_steps(0, 270), 3);
        assert_eq!(rotation_steps(180, 90), 3);
    }

    #[test]
    fn the_home_button_and_the_home_key_are_different_keys() {
        // The regression: the browser sent the keyboard's Home key as `Home`,
        // so pressing it while typing left the app for the launcher.
        assert_eq!(keycode_for("Home"), Some(3), "KEYCODE_HOME, the button");
        assert_eq!(keycode_for("MoveHome"), Some(122));
        assert_eq!(keycode_for("MoveEnd"), Some(123));
        assert_eq!(keycode_for("End"), None, "the browser sends MoveEnd");
        assert_eq!(keycode_for("Back"), Some(4));
        assert_eq!(keycode_for("AppSwitch"), keycode_for("Recents"));
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

    #[tokio::test]
    async fn a_bound_port_is_skipped_and_kept_out_of_the_next_claim() {
        let pool = PortPool::new(7200..=7201);

        // Stand on the first port the pool would hand out, as another process
        // on the host might be.
        let squatter = tokio::net::TcpListener::bind(("0.0.0.0", 7200))
            .await
            .expect("7200 free in the test environment");

        let (lease, listener) = bind_from_pool(&pool).await.unwrap();
        assert_eq!(lease.port(), 7201, "skipped the port already bound");
        assert_eq!(pool.available(), 1, "7200 went back, 7201 is held");

        drop(squatter);
        drop(listener);
        drop(lease);
        assert_eq!(pool.available(), 2);
    }

    #[tokio::test]
    async fn an_exhausted_pool_is_an_error_not_a_random_port() {
        let pool = PortPool::new(7200..=7200);
        let held = pool.claim().unwrap();
        assert!(bind_from_pool(&pool).await.is_err());
        drop(held);
    }

    /// A fake adb server that answers `host:transport:` then `tcp:5555`, and
    /// echoes whatever the proxy forwards afterwards — standing in for the
    /// `adbd` an `adb connect` would be talking to.
    async fn fake_transport() -> String {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();

        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    for _ in 0..2 {
                        let mut length = [0u8; 4];
                        if socket.read_exact(&mut length).await.is_err() {
                            return;
                        }
                        let n = usize::from_str_radix(std::str::from_utf8(&length).unwrap(), 16)
                            .unwrap();
                        let mut service = vec![0u8; n];
                        socket.read_exact(&mut service).await.unwrap();
                        socket.write_all(b"OKAY").await.unwrap();
                    }

                    let mut buffer = [0u8; 64];
                    while let Ok(n) = socket.read(&mut buffer).await {
                        if n == 0 || socket.write_all(&buffer[..n]).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });

        addr
    }

    /// An authority with nobody entitled and no coordinator to ask.
    fn lonely_authority() -> Arc<AdbAuthority> {
        let (control, _rx) = provider_core::control::ControlSender::detached();
        Arc::new(AdbAuthority::new(
            "R5CY82FT35T",
            provider_core::session::SessionRegistry::new(),
            provider_core::adb_auth::AdbAuthWaiters::new(),
            control,
        ))
    }

    #[tokio::test]
    async fn the_bridge_challenges_a_client_rather_than_forwarding_it() {
        use tokio::io::AsyncWriteExt as _;

        // The point of the whole change: bytes from `adb connect` are answered
        // here, not handed to the phone. A client that connects gets *our*
        // authentication challenge, and the device is never involved.
        let server = fake_transport().await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(serve_adb_bridge(
            listener,
            Adb::new(server),
            "R5CY82FT35T".to_owned(),
            lonely_authority(),
        ));

        let mut client = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        adb_bridge::Message::new(
            adb_bridge::Command::Cnxn,
            adb_bridge::VERSION,
            adb_bridge::MAX_PAYLOAD,
            &b"host::\0"[..],
        )
        .write(&mut client)
        .await
        .unwrap();

        let answer = adb_bridge::Message::read(&mut client).await.unwrap();
        assert_eq!(answer.command, adb_bridge::Command::Auth);
        assert_eq!(
            answer.payload.len(),
            20,
            "a 20-byte challenge, as adbd issues"
        );

        client.shutdown().await.unwrap();
        task.abort();
    }

    #[tokio::test]
    async fn the_listener_survives_a_client_it_refuses() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        // No adb server at all, and nobody entitled. The first client is turned
        // away and the listener must stay up — a phone unplugged mid-debug
        // otherwise takes the port with it until the session is torn down.
        let dead = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dead_addr = dead.local_addr().unwrap().to_string();
        drop(dead);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(serve_adb_bridge(
            listener,
            Adb::new(dead_addr),
            "R5CY82FT35T".to_owned(),
            lonely_authority(),
        ));

        let mut doomed = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        doomed.write_all(b"nonsense not adb at all!").await.unwrap();
        let mut buffer = [0u8; 1];
        assert_eq!(
            doomed.read(&mut buffer).await.unwrap(),
            0,
            "closed, not hung"
        );

        let mut second = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        assert!(second.write_all(b"CNXN").await.is_ok());

        task.abort();
    }

    #[tokio::test]
    async fn withdrawing_the_bridge_drops_a_client_already_connected() {
        use tokio::io::AsyncReadExt as _;

        // Releasing a device withdraws the bridge, and a developer must lose
        // the connection there and then. Aborting the accept loop only closes
        // the listener, so a client mid-session survived it and kept driving
        // the phone the next user had just been given.
        let server = fake_transport().await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(serve_adb_bridge(
            listener,
            Adb::new(server),
            "R5CY82FT35T".to_owned(),
            lonely_authority(),
        ));

        let mut client = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        adb_bridge::Message::new(
            adb_bridge::Command::Cnxn,
            adb_bridge::VERSION,
            adb_bridge::MAX_PAYLOAD,
            &b"host::\0"[..],
        )
        .write(&mut client)
        .await
        .unwrap();
        // Connected and served: the challenge proves a task is holding this
        // socket, which is the thing the abort has to take with it.
        assert_eq!(
            adb_bridge::Message::read(&mut client)
                .await
                .unwrap()
                .command,
            adb_bridge::Command::Auth
        );

        task.abort();

        let mut buffer = [0u8; 1];
        let read = tokio::time::timeout(Duration::from_secs(5), client.read(&mut buffer)).await;
        assert_eq!(
            read.expect("the connection must close, not hang").unwrap(),
            0,
            "an established adb client outlived the bridge being withdrawn"
        );
    }
}
