//! The seam between the provider's northbound half and a device technology.
//!
//! `backend-ios` implements this by delegating to the ported CoreDevice layer;
//! `backend-android` by driving adb and scrcpy. Everything above this trait —
//! the control plane, the session server, reservations, uploads — is written
//! once and knows nothing about either.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use yard_protocol::{AppInfo, Display, FileListing, Platform};
use wildmatch::WildMatch;

use crate::adb_auth::AdbAuthority;
use crate::video::VideoHandle;

pub type Result<T> = std::result::Result<T, BackendError>;

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    /// The device is not currently usable — unplugged, rebooting, tunnel down.
    #[error("device unavailable: {0}")]
    Unavailable(String),

    /// The backend does not implement this operation (e.g. adb on iOS).
    #[error("unsupported operation: {0}")]
    Unsupported(&'static str),

    #[error("{0}")]
    Failed(String),
}

impl From<anyhow::Error> for BackendError {
    fn from(err: anyhow::Error) -> Self {
        BackendError::Failed(format!("{err:#}"))
    }
}

/// The directory part of an absolute device path, or `None` at `/`.
///
/// Device paths are always `/`-separated whatever the provider host runs on, so
/// this is deliberately string arithmetic rather than [`std::path`] — a Windows
/// host must not start answering `\` for an Android phone.
pub fn parent_of(path: &str) -> Option<&str> {
    match path.trim_end_matches('/').rfind('/') {
        None => None,
        Some(0) if path.trim_end_matches('/').is_empty() => None,
        Some(0) => Some("/"),
        Some(cut) => Some(&path[..cut]),
    }
}

/// Joins a device path with a child name, for the same reason as above.
pub fn join_path(parent: &str, name: &str) -> String {
    if parent.ends_with('/') {
        format!("{parent}{name}")
    } else {
        format!("{parent}/{name}")
    }
}

/// What a backend knows about its device, refreshed as it changes.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceInfo {
    pub id: String,
    pub platform: Platform,
    pub name: Option<String>,
    pub model: Option<String>,
    pub manufacturer: Option<String>,
    pub os_version: Option<String>,
    pub abi: Option<String>,
    pub sdk: Option<i64>,
    /// Identity a tester needs to file a bug against the right device. Android
    /// reads all of it out of the `getprop` round-trip it already makes.
    pub serial: Option<String>,
    pub brand: Option<String>,
    pub build_id: Option<String>,
    pub security_patch: Option<String>,
    pub abi_list: Option<String>,
    pub display: Option<Display>,
    pub battery_level: Option<f64>,
    pub battery_state: Option<String>,
}

/// A point-in-time read of a device's own resource usage, for the exporter.
///
/// Every field is optional because "this device cannot tell us" and "this device
/// says zero" are different answers, and an *absent* Prometheus series is how the
/// first one is spelled — see [`crate::metrics`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeviceMetrics {
    pub cpu: Option<CpuTimes>,
    pub memory: Option<MemoryBytes>,
    /// 0..1, the same scale as [`DeviceInfo::battery_level`].
    pub battery_level: Option<f64>,
    pub battery_charging: Option<bool>,
    pub battery_temperature_c: Option<f64>,
    /// Empty means no zone was readable, which on Android is the common case:
    /// `/sys/class/thermal` is usually SELinux-denied to an unrooted shell.
    pub thermal_zones: Vec<ThermalZone>,
    pub apps: Vec<AppMetrics>,
}

/// Cumulative seconds since boot, per mode — deliberately not a percentage.
///
/// These are exported as counters and the scraper differentiates them. Computing
/// a percentage here would mean holding a previous sample *and* fixing the
/// averaging window at ours; `rate()` lets the operator pick their own, and it
/// already treats the counter reset of a rebooted device correctly. Nothing in
/// this crate keeps a previous CPU sample, and nothing should need to.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CpuTimes {
    pub user: f64,
    pub nice: Option<f64>,
    pub system: f64,
    pub idle: f64,
    pub iowait: Option<f64>,
    pub irq: Option<f64>,
    pub softirq: Option<f64>,
    pub steal: Option<f64>,
}

impl CpuTimes {
    /// The modes present, as `(label, seconds)` — the exporter's `mode` label.
    pub fn modes(&self) -> Vec<(&'static str, f64)> {
        let mut out = vec![
            ("user", self.user),
            ("system", self.system),
            ("idle", self.idle),
        ];
        for (label, value) in [
            ("nice", self.nice),
            ("iowait", self.iowait),
            ("irq", self.irq),
            ("softirq", self.softirq),
            ("steal", self.steal),
        ] {
            if let Some(value) = value {
                out.push((label, value));
            }
        }
        out
    }
}

/// `available` and `free` are deliberately separate: they mean different things,
/// and reporting `free` when the kernel offers no `MemAvailable` overstates
/// memory pressure by however much is sitting in reclaimable cache.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MemoryBytes {
    pub total: u64,
    pub available: Option<u64>,
    pub free: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThermalZone {
    pub name: String,
    pub celsius: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppMetrics {
    /// The full process name, not the package: a leaked `com.foo.bar:push` is
    /// precisely what someone watching these numbers is looking for, so it must
    /// not be collapsed into `com.foo.bar`.
    pub process: String,
    pub cpu_seconds: Option<f64>,
    pub pss_bytes: Option<u64>,
}

/// Compiled globs naming the processes worth per-app metrics.
///
/// Lives here rather than in [`crate::metrics`] so a backend need not depend on
/// the exporter to answer a question about its own device.
#[derive(Debug, Default)]
pub struct AppFilter {
    patterns: Vec<WildMatch>,
}

impl AppFilter {
    pub fn new(patterns: &[String]) -> Self {
        Self {
            patterns: patterns.iter().map(|p| WildMatch::new(p.trim())).collect(),
        }
    }

    /// Lets a backend skip an expensive read entirely rather than gathering
    /// every process and discarding all of them.
    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    pub fn matches(&self, process: &str) -> bool {
        self.patterns.iter().any(|p| p.matches(process))
    }
}

/// Pointer coordinates are normalised 0..1, not pixels.
///
/// The browser needs no knowledge of the device's true resolution, and a
/// mid-gesture rotation cannot desynchronise an in-flight event.
#[derive(Debug, Clone, PartialEq)]
pub enum InputEvent {
    /// `pointer_id` is carried through so a backend's pointer state machine can
    /// tell a drag from a tap — the regression `stf-ios-provider/src/control.rs`
    /// documents at length. Do not collapse these into a single `tap`.
    PointerDown {
        pointer_id: i64,
        x: f64,
        y: f64,
    },
    PointerMove {
        pointer_id: i64,
        x: f64,
        y: f64,
    },
    PointerUp {
        pointer_id: i64,
        x: f64,
        y: f64,
    },
    /// Named key: "Enter", "Backspace", "Home", …
    Key {
        key: String,
        down: bool,
    },
    /// Committed text, including IME output and paste.
    Text {
        text: String,
    },
}

/// Where an exposed adb transport is reachable. Android only.
#[derive(Debug, Clone)]
pub struct RemoteDebug {
    pub port: u16,
}

/// Progress for a running install, surfaced to the browser over the session
/// socket. `progress` is absent while the backend cannot estimate.
pub trait ProgressSink: Send + Sync {
    fn report(&self, stage: &str, progress: Option<f64>);
}

/// A no-op sink, for callers that do not care (tests, control-plane installs).
pub struct NullProgress;
impl ProgressSink for NullProgress {
    fn report(&self, _stage: &str, _progress: Option<f64>) {}
}

#[async_trait]
pub trait DeviceBackend: Send + Sync + 'static {
    async fn info(&self) -> Result<DeviceInfo>;

    /// Codec description + access-unit broadcast. Cheap and clonable; a viewer
    /// subscribing must not disturb the capture pipeline.
    fn video(&self) -> VideoHandle;

    async fn input(&self, event: InputEvent) -> Result<()>;

    /// Full-resolution PNG, straight from the device's own capture service.
    async fn screenshot(&self) -> Result<Vec<u8>>;

    async fn clipboard_get(&self) -> Result<Option<String>>;
    async fn clipboard_set(&self, text: &str) -> Result<()>;

    async fn apps(&self) -> Result<Vec<AppInfo>>;

    /// Installs an already-staged file.
    ///
    /// Takes a path, not bytes: the upload has already been streamed to the
    /// provider's scratch directory, so a 200 MB APK never sits in memory. The
    /// caller deletes the file afterwards in a guard, so a failed install
    /// cannot leak disk.
    async fn install(&self, staged: &Path, progress: &dyn ProgressSink) -> Result<()>;

    async fn uninstall(&self, app_id: &str) -> Result<()>;
    async fn launch(&self, app_id: &str, args: &[String]) -> Result<()>;

    /// The directory a file browser opens on, or `None` for a backend with no
    /// filesystem access at all.
    ///
    /// A backend answer rather than a platform check in the web app: what a
    /// device exposes is a property of how the provider reaches it, not of the
    /// logo on the front. `None` makes `/files` answer 501, which the browser
    /// shows as "this device does not offer file access".
    fn files_root(&self) -> Option<&'static str> {
        None
    }

    async fn list_files(&self, _path: &str) -> Result<FileListing> {
        Err(BackendError::Unsupported("file browsing"))
    }

    /// Copies a file off the device into `dest`, answering the bytes written.
    ///
    /// Takes a destination path rather than returning bytes for the same reason
    /// `install` takes one: a video pulled off a phone can be hundreds of
    /// megabytes, and the provider must never hold one in memory. The caller
    /// deletes `dest` in a guard once it has been served.
    async fn pull_file(&self, _path: &str, _dest: &Path) -> Result<u64> {
        Err(BackendError::Unsupported("file browsing"))
    }

    async fn rotate(&self, degrees: i64) -> Result<()>;
    async fn reboot(&self) -> Result<()>;

    /// Android only; the default refuses rather than silently doing nothing.
    ///
    /// `authority` is how the ADB bridge reaches the rest of the provider: the
    /// keys entitled to this session, the question to ask the holder about one
    /// that is not, and where to report that the device is being used. It is
    /// passed in rather than held by the backend because a backend is built
    /// before the control plane exists.
    async fn remote_debug(&self, _authority: Arc<AdbAuthority>) -> Result<RemoteDebug> {
        Err(BackendError::Unsupported("remote debugging"))
    }

    async fn remote_debug_stop(&self) -> Result<()> {
        Err(BackendError::Unsupported("remote debugging"))
    }

    /// The provider port this device is exposed on right now, if any.
    ///
    /// Read into every snapshot rather than only reported when it changes: the
    /// coordinator reconciles a device from the whole snapshot, so a poll that
    /// said nothing about the port would clear the one the user just asked for.
    async fn remote_debug_port(&self) -> Option<u16> {
        None
    }

    /// Tears the device session down and brings it back up.
    async fn restart(&self) -> Result<()>;

    /// Turns the display off or on without disturbing the session.
    ///
    /// Absolute, not a toggle: callers ask for the state they want and a
    /// backend that can only toggle is responsible for getting there. This is
    /// what lets an idle device sit dark — a phone nobody has reserved is
    /// still a phone burning its panel and its GPU, and on iOS that is enough
    /// to cook a device sat on a shelf.
    async fn set_screen_awake(&self, _on: bool) -> Result<()> {
        Err(BackendError::Unsupported("setting screen power"))
    }

    // ── resetting a device between users ───────────────────────────────────
    //
    // Narrow primitives rather than one `cleanup()` per backend: the ordering,
    // the deadline, the status transitions and the report are the same
    // everywhere and live once in `cleanup.rs`. A backend only declares what it
    // can physically do, and a step it cannot do is skipped rather than failing
    // the run. See docs/CLEANUP.md.

    /// Wipes an installed app's data without removing the app.
    async fn clear_app_data(&self, _app_id: &str) -> Result<()> {
        Err(BackendError::Unsupported("clearing app data"))
    }

    /// Back to a neutral screen: home, unrotated, nothing in the clipboard.
    async fn reset_screen(&self) -> Result<()> {
        Err(BackendError::Unsupported("resetting the screen"))
    }

    /// Empties each path, leaving the directory itself in place.
    ///
    /// The paths come from the device's config, not from the backend: they end
    /// in `rm -rf` on somebody's phone, so they are set by whoever runs the
    /// host and guarded at startup, never typed into a web form.
    async fn wipe_paths(&self, _paths: &[String]) -> Result<()> {
        Err(BackendError::Unsupported("wiping paths"))
    }

    /// False when the backend has lost the device. The supervisor polls this to
    /// decide when to report `unhealthy` upstream.
    async fn is_healthy(&self) -> bool {
        true
    }

    /// Resource usage, for the metrics exporter.
    ///
    /// The default refuses rather than answering an all-`None` struct, so "this
    /// backend has no metrics at all" stays distinguishable from "this device
    /// answered nothing this time" — the sampler treats the two differently, and
    /// only the second is an error worth counting.
    ///
    /// `apps` is passed in rather than read from config by the backend so the
    /// patterns stay one global concern, and so a backend can call
    /// [`AppFilter::is_empty`] to skip an expensive read.
    async fn metrics(&self, _apps: &AppFilter) -> Result<DeviceMetrics> {
        Err(BackendError::Unsupported("device metrics"))
    }
}

#[cfg(test)]
mod tests {
    use super::{join_path, parent_of, AppFilter};

    /// The `.` either side of the wildcard is what makes a pattern mean
    /// "this vendor" rather than "anything containing these letters".
    #[test]
    fn an_app_pattern_respects_package_boundaries() {
        let filter = AppFilter::new(&["*.demo.*".to_owned()]);

        assert!(filter.matches("com.example.demo.player"));
        // A separate process of a matched app is a distinct series on purpose.
        assert!(filter.matches("com.example.demo.player:push"));
        assert!(!filter.matches("com.example.demoted.app"));
        assert!(!filter.matches("com.example.mock.app"));
    }

    #[test]
    fn no_patterns_matches_nothing_and_says_so() {
        let filter = AppFilter::new(&[]);
        assert!(filter.is_empty());
        assert!(!filter.matches("com.example.demo.player"));
    }

    #[test]
    fn any_of_several_patterns_matches() {
        let filter = AppFilter::new(&["*.demo.*".to_owned(), "com.android.systemui".to_owned()]);
        assert!(filter.matches("com.android.systemui"));
        assert!(filter.matches("com.example.demo.player"));
        assert!(!filter.matches("com.android.settings"));
    }

    #[test]
    fn walking_up_stops_at_the_root() {
        assert_eq!(parent_of("/sdcard/DCIM/Camera"), Some("/sdcard/DCIM"));
        assert_eq!(parent_of("/sdcard/DCIM"), Some("/sdcard"));
        // One level below the root's parent *is* the root, not nothing —
        // getting this wrong hides ".." exactly one directory too early.
        assert_eq!(parent_of("/sdcard"), Some("/"));
        assert_eq!(parent_of("/"), None);
        assert_eq!(parent_of(""), None);
    }

    #[test]
    fn a_trailing_slash_does_not_shift_the_parent() {
        assert_eq!(parent_of("/sdcard/DCIM/"), Some("/sdcard"));
    }

    #[test]
    fn joining_never_doubles_the_separator() {
        assert_eq!(join_path("/sdcard", "DCIM"), "/sdcard/DCIM");
        assert_eq!(join_path("/", "DCIM"), "/DCIM");
    }
}
