//! Remote debugging as the coordinator sees it.
//!
//! The port is not just the reply to `device.adb.expose` — it has to be part of
//! every snapshot, because the coordinator reconciles a device from a whole
//! snapshot. A poll that omitted the port cleared the column the user had just
//! asked to be filled, and the UI dropped the `adb connect` line seconds after
//! showing it.

use std::sync::Arc;

use farm_protocol::{CommandPayload, Platform, ProviderMessage};
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
