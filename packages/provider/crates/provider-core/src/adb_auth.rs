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

use farm_protocol::{AdbKey, ProviderMessage};

use crate::control::{now_millis, AdbAuthDecision, ControlSender};
use crate::session::SessionRegistry;

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

    /// Start listening for an answer *before* the question is asked.
    ///
    /// Split from the wait on purpose: registering after sending would lose a
    /// decision that came back faster than this task was rescheduled.
    pub async fn register(&self, request_id: &str) -> Ticket {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(request_id.to_owned(), tx);
        Ticket {
            request_id: request_id.to_owned(),
            waiters: self.clone(),
            rx,
        }
    }

    /// Register and wait in one step, for callers with nothing to send.
    pub async fn wait(&self, request_id: &str) -> Option<AdbAuthDecision> {
        self.register(request_id).await.answer().await
    }

    async fn wait_on(
        &self,
        request_id: &str,
        rx: oneshot::Receiver<AdbAuthDecision>,
    ) -> Option<AdbAuthDecision> {
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

/// Everything the ADB bridge needs from the rest of the provider, for one
/// device.
///
/// Built per exposure rather than held by the backend, because a backend is
/// constructed before the control plane exists and must not outlive a
/// reservation's authorization. It carries clones — the session registry, the
/// waiter map, the upstream sender — so it needs no reference back to the
/// supervisor.
pub struct AdbAuthority {
    device_id: String,
    sessions: SessionRegistry,
    waiters: AdbAuthWaiters,
    control: ControlSender,
    activity: ActivityThrottle,
}

impl AdbAuthority {
    pub fn new(
        device_id: impl Into<String>,
        sessions: SessionRegistry,
        waiters: AdbAuthWaiters,
        control: ControlSender,
    ) -> Self {
        Self {
            device_id: device_id.into(),
            sessions,
            waiters,
            control,
            activity: ActivityThrottle::default(),
        }
    }

    /// The keys entitled to this device's current session.
    ///
    /// Empty when the device is not reserved, which refuses every connection —
    /// correct, and the reason an exposed port outliving a reservation is not a
    /// way in.
    pub async fn entitled_keys(&self) -> Vec<AdbKey> {
        self.sessions
            .current(&self.device_id)
            .await
            .map(|auth| auth.adb_keys)
            .unwrap_or_default()
    }

    /// Ask the holder about a key nobody recognised, and wait for the answer.
    ///
    /// Returns the owning user id on approval. A refusal, a timeout and a lost
    /// control plane are all `None`: from the connection's point of view they
    /// are the same answer.
    pub async fn approve(
        &self,
        fingerprint: &str,
        public_key: &str,
        comment: Option<&str>,
    ) -> Option<String> {
        let request_id = uuid::Uuid::new_v4().to_string();
        let ticket = self.waiters.register(&request_id).await;

        self.control.send(ProviderMessage::AdbAuthRequest {
            device_id: self.device_id.clone(),
            request_id,
            fingerprint: fingerprint.to_owned(),
            public_key: public_key.to_owned(),
            comment: comment.map(str::to_owned),
        });

        let decision = ticket.answer().await?;
        decision.allow.then_some(decision.user_id).flatten()
    }

    /// Report that somebody is driving this device, at most once an interval.
    ///
    /// This is the only signal that exists for a developer working entirely
    /// over `adb`: no browser is open, so nothing else can tell the coordinator
    /// the reservation is not idle.
    pub async fn note_activity(&self) {
        if !self.activity.claim(std::time::Instant::now()) {
            return;
        }
        self.control.send(ProviderMessage::DeviceActivity {
            device_id: self.device_id.clone(),
            at: now_millis(),
        });
    }
}

/// At most one activity report per device per this long.
const ACTIVITY_INTERVAL: Duration = Duration::from_secs(30);

/// Rate limiter for activity reports, not a record of activity itself.
#[derive(Default)]
struct ActivityThrottle {
    last: std::sync::Mutex<Option<std::time::Instant>>,
}

impl ActivityThrottle {
    fn claim(&self, now: std::time::Instant) -> bool {
        let mut last = self.last.lock().unwrap_or_else(|err| err.into_inner());
        match *last {
            Some(previous) if now.duration_since(previous) < ACTIVITY_INTERVAL => false,
            _ => {
                *last = Some(now);
                true
            }
        }
    }
}

/// A registered request, waiting to be asked and then answered.
pub struct Ticket {
    request_id: String,
    waiters: AdbAuthWaiters,
    rx: oneshot::Receiver<AdbAuthDecision>,
}

impl Ticket {
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    /// Block until the coordinator answers, the wait times out, or the control
    /// plane goes away. `None` means refused in all three cases.
    pub async fn answer(self) -> Option<AdbAuthDecision> {
        let Ticket {
            request_id,
            waiters,
            rx,
        } = self;
        waiters.wait_on(&request_id, rx).await
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
