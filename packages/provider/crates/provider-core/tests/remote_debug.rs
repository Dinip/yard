//! Remote debugging as the coordinator sees it.
//!
//! The port is not just the reply to `device.adb.expose` — it has to be part of
//! every snapshot, because the coordinator reconciles a device from a whole
//! snapshot. A poll that omitted the port cleared the column the user had just
//! asked to be filled, and the UI dropped the `adb connect` line seconds after
//! showing it.

use std::sync::Arc;

use yard_protocol::{CommandPayload, Platform, ProviderMessage};
use provider_core::backend::DeviceBackend;
use provider_core::control::{CommandHandler, ControlSender};
use provider_core::session::SessionRegistry;
use provider_core::supervisor::Supervisor;
use tokio::sync::mpsc::UnboundedReceiver;

const DEVICE: &str = "mock-1";

async fn harness(
    backend: Arc<dyn DeviceBackend>,
) -> (Arc<Supervisor>, UnboundedReceiver<ProviderMessage>) {
    let mut supervisor = Supervisor::new(SessionRegistry::new());
    supervisor.add(DEVICE.into(), backend);
    let supervisor = Arc::new(supervisor);
    supervisor.bootstrap().await;

    let (sender, rx) = ControlSender::detached();
    supervisor.attach_control(sender).await;
    (supervisor, rx)
}

async fn snapshot_port(supervisor: &Arc<Supervisor>) -> Option<i64> {
    supervisor
        .device(DEVICE)
        .expect("device")
        .snapshot()
        .await
        .adb_port
}

#[tokio::test]
async fn an_exposed_port_survives_the_next_poll() {
    let backend = backend_mock::MockBackend::new(DEVICE, Platform::Android, "Mock Pixel");
    let (supervisor, _rx) = harness(backend).await;

    assert_eq!(snapshot_port(&supervisor).await, None);

    let data = supervisor
        .handle(CommandPayload::DeviceAdbExpose {
            device_id: DEVICE.into(),
        })
        .await
        .expect("expose");
    let port = data.and_then(|data| data.adb_port).expect("a port");

    // The upsert the poll pushes carries this; reporting `None` here is what
    // wiped the coordinator's row a few seconds after it was written.
    assert_eq!(snapshot_port(&supervisor).await, Some(port));

    supervisor
        .handle(CommandPayload::DeviceAdbUnexpose {
            device_id: DEVICE.into(),
        })
        .await
        .expect("unexpose");
    assert_eq!(snapshot_port(&supervisor).await, None);
}

/// Waits for the exposure to go away, so the test does not depend on how
/// quickly the revocation task is scheduled.
async fn await_withdrawn(supervisor: &Arc<Supervisor>) {
    let deadline = std::time::Duration::from_secs(5);
    let withdrawn = tokio::time::timeout(deadline, async {
        while snapshot_port(supervisor).await.is_some() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        withdrawn.is_ok(),
        "the adb port outlived the session that granted it"
    );
}

async fn expose(supervisor: &Arc<Supervisor>) {
    supervisor
        .handle(CommandPayload::DeviceAdbExpose {
            device_id: DEVICE.into(),
        })
        .await
        .expect("expose");
    assert!(snapshot_port(supervisor).await.is_some());
}

async fn authorize(supervisor: &Arc<Supervisor>, reservation: &str) {
    supervisor
        .handle(CommandPayload::SessionAuthorize {
            device_id: DEVICE.into(),
            reservation_id: reservation.into(),
            user_id: "user-1".into(),
            adb_keys: Vec::new(),
        })
        .await
        .expect("authorize");
}

#[tokio::test]
async fn releasing_a_device_withdraws_its_adb_bridge() {
    let backend = backend_mock::MockBackend::new(DEVICE, Platform::Android, "Mock Pixel");
    let (supervisor, _rx) = harness(backend).await;
    tokio::spawn(supervisor.clone().run_revocation_loop());

    authorize(&supervisor, "res-1").await;
    expose(&supervisor).await;

    supervisor
        .handle(CommandPayload::SessionRevoke {
            device_id: DEVICE.into(),
            reason: Some("released".into()),
        })
        .await
        .expect("revoke");

    // The listener going with the session is what closes the connections under
    // it: an authenticated `adb connect` is checked at the handshake and never
    // again, so nothing else would ever hang up on the previous holder.
    await_withdrawn(&supervisor).await;
}

#[tokio::test]
async fn a_new_reservation_withdraws_the_previous_holders_bridge() {
    let backend = backend_mock::MockBackend::new(DEVICE, Platform::Android, "Mock Pixel");
    let (supervisor, _rx) = harness(backend).await;
    tokio::spawn(supervisor.clone().run_revocation_loop());

    authorize(&supervisor, "res-1").await;
    expose(&supervisor).await;

    // An admin taking a device back hands it straight to somebody else without
    // a `session.revoke` in between.
    authorize(&supervisor, "res-2").await;
    await_withdrawn(&supervisor).await;
}

#[tokio::test]
async fn renewing_the_same_reservation_leaves_the_bridge_alone() {
    let backend = backend_mock::MockBackend::new(DEVICE, Platform::Android, "Mock Pixel");
    let (supervisor, _rx) = harness(backend).await;
    tokio::spawn(supervisor.clone().run_revocation_loop());

    authorize(&supervisor, "res-1").await;
    expose(&supervisor).await;

    // A renew, and the coordinator re-pushing on reconnect, are both this. An
    // `adb connect` must survive them.
    authorize(&supervisor, "res-1").await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(snapshot_port(&supervisor).await.is_some());
}
