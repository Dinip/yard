//! The seam between the provider's northbound half and a device technology.
//!
//! `backend-ios` implements this by delegating to the ported CoreDevice layer;
//! `backend-android` by driving adb and scrcpy. Everything above this trait —
//! the control plane, the session server, reservations, uploads — is written
//! once and knows nothing about either.

use std::path::Path;

use async_trait::async_trait;
use farm_protocol::{AppInfo, Display, FileListing, Platform};

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
    async fn remote_debug(&self) -> Result<RemoteDebug> {
        Err(BackendError::Unsupported("remote debugging"))
    }

    async fn remote_debug_stop(&self) -> Result<()> {
        Err(BackendError::Unsupported("remote debugging"))
    }

    /// Tears the device session down and brings it back up.
    async fn restart(&self) -> Result<()>;

    /// False when the backend has lost the device. The supervisor polls this to
    /// decide when to report `unhealthy` upstream.
    async fn is_healthy(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{join_path, parent_of};

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
