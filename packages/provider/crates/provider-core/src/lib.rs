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
pub mod server;
pub mod session;
pub mod supervisor;
pub mod video;

pub use backend::{
    BackendError, DeviceBackend, DeviceInfo, InputEvent, NullProgress, ProgressSink, RemoteDebug,
};
pub use config::{BackendKind, Config, DeviceConfig};
pub use session::{Authorization, SessionRegistry};
pub use supervisor::{Device, Supervisor};
pub use video::{AccessUnit, CodecDescription, VideoHandle, VideoPublisher};
