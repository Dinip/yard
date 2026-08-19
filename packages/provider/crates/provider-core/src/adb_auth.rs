//! Parked `adb connect` connections waiting on the coordinator.
//!
//! An unrecognised ADB key cannot be resolved locally — the provider is told
//! which keys are entitled to a session, not who exists in the farm — so the
//! connection waits here while the holder is asked. The wait is bounded and a
//! dropped control plane ends it: a decision can only arrive over the socket
//! that carried the question, so holding the connection open past that would
//! keep a developer looking at a hung `adb connect` for two minutes.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{oneshot, Mutex};
use tracing::{debug, warn};

use crate::control::AdbAuthDecision;

/// How long the holder has to answer before the connection is refused.
///
/// Matches STF, which parks its bridge's `auth()` for the same two minutes.
/// Long enough to notice a browser notification, short enough that a developer
/// who is not looking at the UI gets a refusal rather than a hang.
pub const ADB_AUTH_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Default)]
pub struct AdbAuthWaiters {
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<AdbAuthDecision>>>>,
}

impl AdbAuthWaiters {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a request and wait for its answer.
    ///
    /// `None` means refused, by silence or by the coordinator going away.
    pub async fn wait(&self, request_id: &str) -> Option<AdbAuthDecision> {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(request_id.to_owned(), tx);

        let outcome = tokio::time::timeout(ADB_AUTH_TIMEOUT, rx).await;
        // Whatever happened, this request is over; leaving the entry behind
        // would leak one map slot per abandoned `adb connect`.
        self.pending.lock().await.remove(request_id);

        match outcome {
            Ok(Ok(decision)) => Some(decision),
            Ok(Err(_)) => None,
            Err(_) => {
                debug!(request_id, "adb auth request timed out");
                None
            }
        }
    }

    pub async fn resolve(&self, decision: AdbAuthDecision) {
        match self.pending.lock().await.remove(&decision.request_id) {
            Some(tx) => {
                let _ = tx.send(decision);
            }
            // The connection gave up first, or the coordinator answered twice.
            None => warn!(
                request_id = %decision.request_id,
                "adb auth decision for a request nobody is waiting on"
            ),
        }
    }

    /// Refuse everything in flight, because the answer can no longer arrive.
    pub async fn abandon_all(&self) {
        let waiting = std::mem::take(&mut *self.pending.lock().await);
        if !waiting.is_empty() {
            debug!(count = waiting.len(), "refusing parked adb connections");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_decision_reaches_the_waiter() {
        let waiters = AdbAuthWaiters::new();
        let other = waiters.clone();
        let task = tokio::spawn(async move { other.wait("req-1").await });

        // Let the waiter register before answering: resolving first would find
        // nothing pending, which is the bug this ordering guards.
        tokio::task::yield_now().await;
        waiters
            .resolve(AdbAuthDecision {
                request_id: "req-1".into(),
                allow: true,
                user_id: Some("user-1".into()),
                reason: None,
            })
            .await;

        let decision = task.await.unwrap().expect("a decision was sent");
        assert!(decision.allow);
        assert_eq!(decision.user_id.as_deref(), Some("user-1"));
    }

    #[tokio::test]
    async fn a_lost_control_plane_refuses_rather_than_hangs() {
        let waiters = AdbAuthWaiters::new();
        let other = waiters.clone();
        let task = tokio::spawn(async move { other.wait("req-1").await });

        tokio::task::yield_now().await;
        waiters.abandon_all().await;

        assert!(
            task.await.unwrap().is_none(),
            "dropping the sender must refuse, not wait out the full timeout"
        );
    }

    #[tokio::test]
    async fn an_answer_to_nothing_is_ignored() {
        let waiters = AdbAuthWaiters::new();
        waiters
            .resolve(AdbAuthDecision {
                request_id: "gone".into(),
                allow: true,
                user_id: None,
                reason: None,
            })
            .await;
    }
}
