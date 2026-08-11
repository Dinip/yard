//! The technology-independent half of a provider.
//!
//! Owns the control plane (one outbound WSS to the coordinator), the session
//! and artifact planes (browser-facing), reservations, and device supervision.
//! Everything device-specific lives behind [`backend::DeviceBackend`].
//!
//! ```text
//!   coordinator ──── control.rs ────┐
//!                                   ├── supervisor.rs ── DeviceBackend
//!   browser ─── server.rs ──────────┘                      (ios / android / mock)
//! ```

pub mod auth;
pub mod backend;
pub mod config;
pub mod control;
pub mod metrics;
pub mod origins;
pub mod ports;
pub mod server;
pub mod session;
pub mod supervisor;
pub mod video;

pub use backend::{
    AppFilter, AppMetrics, BackendError, CpuTimes, DeviceBackend, DeviceInfo, DeviceMetrics,
    InputEvent, MemoryBytes, NullProgress, ProgressSink, RemoteDebug, ThermalZone,
};
pub use config::{BackendKind, Config, DeviceConfig, MetricsConfig};
pub use metrics::{MetricsCache, MetricsState};
pub use origins::WebOrigins;
pub use session::{Authorization, SessionRegistry};
pub use supervisor::{Device, Supervisor};
pub use video::{AccessUnit, CodecDescription, VideoHandle, VideoPublisher};
