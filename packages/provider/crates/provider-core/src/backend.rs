//! The seam between the provider's northbound half and a device technology.
//!
//! `backend-ios` implements this by delegating to the ported CoreDevice layer;
//! `backend-android` by driving adb and scrcpy. Everything above this trait —
//! the control plane, the session server, reservations, uploads — is written
//! once and knows nothing about either.

use std::path::Path;

use async_trait::async_trait;
use farm_protocol::{AppInfo, Display, Platform};

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
