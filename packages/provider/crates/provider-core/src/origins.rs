//! Which browser origins may talk to this provider.
//!
//! The session and artifact planes are *always* cross-origin: the browser
//! loads the app from the coordinator's origin and then connects straight here,
//! which is the whole point of keeping the coordinator off the data path. So
//! uploads and screenshots need CORS, and the WebSocket — which has none —
//! does not.
//!
//! The list arrives in `hello.ack` rather than sitting in provider.yaml. The
//! coordinator owns policy, and a provider configured separately would drift
//! out of step with it the first time the web app moved.
//!
//! It is empty until the provider registers, so a provider that has never
//! reached the coordinator refuses browser requests rather than guessing.

use std::sync::{Arc, RwLock};

#[derive(Clone, Default)]
pub struct WebOrigins(Arc<RwLock<Vec<String>>>);

impl WebOrigins {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the whole list — the coordinator sends all of it every time,
    /// so there is nothing to merge.
    pub fn set(&self, origins: Vec<String>) {
        if let Ok(mut guard) = self.0.write() {
            *guard = origins;
        }
    }

    pub fn allows(&self, origin: &str) -> bool {
        self.0
            .read()
            .map(|origins| origins.iter().any(|allowed| allowed == origin))
            .unwrap_or(false)
    }

    pub fn snapshot(&self) -> Vec<String> {
        self.0.read().map(|o| o.clone()).unwrap_or_default()
    }
}
