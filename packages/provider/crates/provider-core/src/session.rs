//! Which reservation may currently use each device.
//!
//! This replaces `stf-ios-provider/src/group.rs`, which made the *device* the
//! arbiter of ownership and therefore needed idle-group expiry timers on every
//! device. Here the coordinator's database is the arbiter and simply tells the
//! provider the answer: `session.authorize` sets it, `session.revoke` clears
//! it. There is no arbitration and no timer.
//!
//! A token is accepted only if its `reservationId` matches what the coordinator
//! last authorized, so a token that outlives its reservation is refused even
//! though its signature and `exp` are still perfectly valid.

use std::collections::HashMap;
use std::sync::Arc;

use farm_protocol::AdbKey;
use tokio::sync::{broadcast, RwLock};
use tracing::info;

#[derive(Debug, Clone, PartialEq)]
pub struct Authorization {
    pub reservation_id: String,
    pub user_id: String,
    /// Every ADB key entitled to this session. An `adb connect` is admitted by
    /// matching a signature against these, without asking the coordinator.
    pub adb_keys: Vec<AdbKey>,
}

/// Broadcast when a device's authorization is withdrawn, so live viewers can be
/// dropped immediately rather than waiting for their token to expire.
#[derive(Debug, Clone)]
pub struct Revocation {
    pub device_id: String,
    pub reason: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("device {0} is not currently reserved")]
    NotAuthorized(String),
    #[error("this reservation is no longer current")]
    StaleReservation,
}

#[derive(Clone)]
pub struct SessionRegistry {
    inner: Arc<Inner>,
}

struct Inner {
    authorized: RwLock<HashMap<String, Authorization>>,
    revocations: broadcast::Sender<Revocation>,
}

impl SessionRegistry {
    pub fn new() -> Self {
        let (revocations, _) = broadcast::channel(64);
        Self {
            inner: Arc::new(Inner {
                authorized: RwLock::new(HashMap::new()),
                revocations,
            }),
        }
    }

    pub async fn authorize(&self, device_id: &str, auth: Authorization) {
        let previous = self
            .inner
            .authorized
            .write()
            .await
            .insert(device_id.to_owned(), auth.clone());

        // A new reservation replacing an old one must not leave the previous
        // holder's viewers connected.
        if let Some(previous) = previous {
            if previous.reservation_id != auth.reservation_id {
                let _ = self.inner.revocations.send(Revocation {
                    device_id: device_id.to_owned(),
                    reason: "replaced by a new reservation".into(),
                });
            }
        }

        info!(
            device = %device_id,
            reservation = %auth.reservation_id,
            user = %auth.user_id,
            "session authorized"
        );
    }

    /// Replace the keys allowed to `adb connect` this device.
    ///
    /// A no-op on an unauthorized device: there is no session for the keys to
    /// belong to, and `session.authorize` carries its own set anyway.
    pub async fn set_adb_keys(&self, device_id: &str, keys: Vec<AdbKey>) {
        if let Some(auth) = self.inner.authorized.write().await.get_mut(device_id) {
            auth.adb_keys = keys;
        }
    }

    pub async fn revoke(&self, device_id: &str, reason: &str) {
        self.inner.authorized.write().await.remove(device_id);
        let _ = self.inner.revocations.send(Revocation {
            device_id: device_id.to_owned(),
            reason: reason.to_owned(),
        });
        info!(device = %device_id, %reason, "session revoked");
    }

    /// Drops every authorization. Used when the control plane reconnects: the
    /// coordinator re-pushes the current state, and anything we were holding
    /// from before might be stale.
    pub async fn revoke_all(&self, reason: &str) {
        let devices: Vec<String> = self.inner.authorized.read().await.keys().cloned().collect();
        for device_id in devices {
            self.revoke(&device_id, reason).await;
        }
    }

    pub async fn current(&self, device_id: &str) -> Option<Authorization> {
        self.inner.authorized.read().await.get(device_id).cloned()
    }

    /// The check every session-plane connection makes after its JWT verifies.
    pub async fn check(&self, device_id: &str, reservation_id: &str) -> Result<(), SessionError> {
        match self.inner.authorized.read().await.get(device_id) {
            None => Err(SessionError::NotAuthorized(device_id.to_owned())),
            Some(auth) if auth.reservation_id != reservation_id => {
                Err(SessionError::StaleReservation)
            }
            Some(_) => Ok(()),
        }
    }

    pub fn subscribe_revocations(&self) -> broadcast::Receiver<Revocation> {
        self.inner.revocations.subscribe()
    }
}

impl Default for SessionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth(reservation: &str) -> Authorization {
        Authorization {
            reservation_id: reservation.into(),
            user_id: "user-1".into(),
            adb_keys: Vec::new(),
        }
    }

    #[tokio::test]
    async fn an_unauthorized_device_refuses_every_token() {
        let registry = SessionRegistry::new();
        assert!(matches!(
            registry.check("dev-1", "res-1").await,
            Err(SessionError::NotAuthorized(_))
        ));
    }

    #[tokio::test]
    async fn only_the_current_reservation_is_accepted() {
        let registry = SessionRegistry::new();
        registry.authorize("dev-1", auth("res-1")).await;

        assert!(registry.check("dev-1", "res-1").await.is_ok());
        // A token from a previous reservation is signed, unexpired, and wrong.
        assert!(matches!(
            registry.check("dev-1", "res-old").await,
            Err(SessionError::StaleReservation)
        ));
    }

    #[tokio::test]
    async fn revoking_takes_effect_immediately_and_notifies_viewers() {
        let registry = SessionRegistry::new();
        let mut revocations = registry.subscribe_revocations();
        registry.authorize("dev-1", auth("res-1")).await;

        registry.revoke("dev-1", "reservation released").await;

        assert!(registry.check("dev-1", "res-1").await.is_err());
        let event = revocations.recv().await.unwrap();
        assert_eq!(event.device_id, "dev-1");
    }

    #[tokio::test]
    async fn replacing_a_reservation_drops_the_previous_holder() {
        let registry = SessionRegistry::new();
        let mut revocations = registry.subscribe_revocations();

        registry.authorize("dev-1", auth("res-1")).await;
        registry.authorize("dev-1", auth("res-2")).await;

        let event = revocations.recv().await.unwrap();
        assert_eq!(event.device_id, "dev-1");
        assert!(registry.check("dev-1", "res-2").await.is_ok());
        assert!(registry.check("dev-1", "res-1").await.is_err());
    }

    #[tokio::test]
    async fn re_authorizing_the_same_reservation_does_not_disturb_viewers() {
        let registry = SessionRegistry::new();
        let mut revocations = registry.subscribe_revocations();

        registry.authorize("dev-1", auth("res-1")).await;
        // The coordinator re-pushes on reconnect; a popout window opening is
        // also the same reservation. Neither may kick the existing tab.
        registry.authorize("dev-1", auth("res-1")).await;

        assert!(registry.check("dev-1", "res-1").await.is_ok());
        assert!(revocations.try_recv().is_err());
    }
}
