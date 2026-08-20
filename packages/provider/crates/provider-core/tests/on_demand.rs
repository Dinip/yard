//! Capture runs when something needs it and stops when nothing does.
//!
//! The property being protected is the one that makes this safe to ship: a
//! device that has stopped streaming because nobody is watching **stays in the
//! pool**. Getting that wrong trades a hot phone for an invisible one.
//!
//! All of it runs against `backend-mock`, whose synthetic capture loop is gated
//! on the same [`Demand`] the real backends use, so none of this needs hardware.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use farm_protocol::{
    AppFilter, CleanupSteps, CommandPayload, DeviceStatus, Platform, ProviderMessage,
};
use provider_core::backend::DeviceBackend;
use provider_core::control::{CommandHandler, ControlSender};
use provider_core::demand::IDLE_GRACE;
use provider_core::session::SessionRegistry;
use provider_core::supervisor::Supervisor;
use tokio::sync::mpsc::UnboundedReceiver;

const DEVICE: &str = "mock-1";

fn mock() -> Arc<backend_mock::MockBackend> {
    backend_mock::MockBackend::new(DEVICE, Platform::Android, "Mock Pixel")
}

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

/// Lets the mock's capture task run without letting a whole grace window pass.
async fn settle() {
    tokio::time::advance(Duration::from_millis(50)).await;
}

fn streaming(backend: &backend_mock::MockBackend) -> bool {
    backend.state.streaming.load(Ordering::Relaxed)
}

fn starts(backend: &backend_mock::MockBackend) -> i64 {
    backend.state.stream_starts.load(Ordering::Relaxed)
}

/// The regression that would make this whole change a net loss: an idle device
/// must not read as a broken one.
#[tokio::test(start_paused = true)]
async fn an_idle_device_stays_in_the_pool() {
    let backend = mock();
    let (supervisor, _rx) = harness(backend.clone()).await;

    tokio::time::advance(IDLE_GRACE * 4).await;
    supervisor
        .refresh(&supervisor.device(DEVICE).expect("device"))
        .await;

    assert_eq!(
        supervisor.device(DEVICE).unwrap().status().await,
        DeviceStatus::Ready
    );
    assert!(!streaming(&backend), "nobody is watching");
    assert_eq!(starts(&backend), 0, "capture never had a reason to start");
}

#[tokio::test(start_paused = true)]
async fn a_viewer_brings_the_stream_up() {
    let backend = mock();
    assert!(backend.video_handle().current_codec().is_none());

    let _viewer = backend.demand().lease();
    settle().await;

    assert!(streaming(&backend));
    assert_eq!(starts(&backend), 1);
    // The parameter sets a viewer cannot decode without.
    assert!(backend
        .video_handle()
        .wait_for_codec(Duration::from_secs(1))
        .await
        .is_some());
}

#[tokio::test(start_paused = true)]
async fn the_last_viewer_leaving_stops_the_stream_after_the_grace() {
    let backend = mock();
    let viewer = backend.demand().lease();
    settle().await;
    assert!(streaming(&backend));

    drop(viewer);
    tokio::time::advance(IDLE_GRACE / 2).await;
    assert!(streaming(&backend), "the window has not run out yet");

    tokio::time::advance(IDLE_GRACE).await;
    settle().await;
    assert!(!streaming(&backend));
    // A viewer arriving now waits for bring-up rather than decoding against
    // parameter sets from a stream that has stopped.
    assert!(backend.video_handle().current_codec().is_none());
}

/// The page-refresh and popout-window case. A restart here would cost the user
/// a black screen for no reason.
#[tokio::test(start_paused = true)]
async fn a_reconnect_inside_the_window_never_restarts_the_session() {
    let backend = mock();
    let first = backend.demand().lease();
    settle().await;
    assert_eq!(starts(&backend), 1);

    drop(first);
    tokio::time::advance(IDLE_GRACE / 2).await;
    let _second = backend.demand().lease();

    tokio::time::advance(IDLE_GRACE * 2).await;
    assert!(streaming(&backend));
    assert_eq!(starts(&backend), 1, "the stream should never have stopped");
}

/// Input is demand too, and it is not a subscriber — this is the case a viewer
/// count can never represent.
#[tokio::test(start_paused = true)]
async fn input_alone_brings_a_device_up() {
    let backend = mock();

    backend
        .input(provider_core::InputEvent::Key {
            key: "Home".into(),
            down: true,
        })
        .await
        .expect("input should wait for bring-up, not be dropped");

    assert!(streaming(&backend));
    assert_eq!(backend.state.events.lock().await.len(), 1);

    // Input stopping is treated exactly like a viewer leaving.
    tokio::time::advance(IDLE_GRACE * 2).await;
    settle().await;
    assert!(!streaming(&backend));
}

/// Cleanup runs after the viewer has gone, and `reset_screen` is a Home press.
/// Without its own lease it would find nothing to press with.
#[tokio::test(start_paused = true)]
async fn cleanup_presses_home_with_no_viewer_attached() {
    let backend = mock();
    backend.state.rotation.store(90, Ordering::Relaxed);
    let (supervisor, mut rx) = harness(backend.clone()).await;

    assert!(!streaming(&backend), "no viewer, no stream");

    supervisor
        .handle(CommandPayload::DeviceCleanup {
            device_id: DEVICE.into(),
            steps: CleanupSteps {
                uninstall_apps: false,
                reset_screen: true,
                clear_app_data: false,
                wipe_folders: false,
            },
            clear_app_data_filter: AppFilter {
                allow: vec![],
                deny: vec![],
            },
            timeout_seconds: 30,
        })
        .await
        .expect("cleanup command accepted");

    let status = settled(&supervisor).await;
    assert_eq!(status, DeviceStatus::Ready);
    assert_eq!(backend.state.rotation.load(Ordering::Relaxed), 0);
    assert!(
        starts(&backend) >= 1,
        "cleanup should have woken the device"
    );
    assert_eq!(errors(&mut rx), Vec::<String>::new());
}

/// The other half of the same bug: a Home press that could not happen used to
/// be a `debug!` line and a success. It has to reach the report.
#[tokio::test(start_paused = true)]
async fn a_device_that_will_not_stream_fails_cleanup_rather_than_faking_it() {
    let backend = mock();
    backend.state.no_capture.store(true, Ordering::Relaxed);
    let (supervisor, mut rx) = harness(backend.clone()).await;

    supervisor
        .handle(CommandPayload::DeviceCleanup {
            device_id: DEVICE.into(),
            steps: CleanupSteps {
                uninstall_apps: false,
                reset_screen: true,
                clear_app_data: false,
                wipe_folders: false,
            },
            clear_app_data_filter: AppFilter {
                allow: vec![],
                deny: vec![],
            },
            timeout_seconds: 300,
        })
        .await
        .expect("cleanup command accepted");

    settled(&supervisor).await;
    let errors = errors(&mut rx);
    assert!(
        errors.iter().any(|error| error.contains("not streaming")),
        "{errors:?}"
    );
}

/// Waits for the device to leave `cleaning`, which the spawned task does.
async fn settled(supervisor: &Arc<Supervisor>) -> DeviceStatus {
    let device = supervisor.device(DEVICE).expect("device");
    for _ in 0..2000 {
        let status = device.status().await;
        if status != DeviceStatus::Cleaning {
            return status;
        }
        tokio::time::advance(Duration::from_millis(25)).await;
    }
    panic!("device never left `cleaning`");
}

fn errors(rx: &mut UnboundedReceiver<ProviderMessage>) -> Vec<String> {
    while let Ok(msg) = rx.try_recv() {
        if let ProviderMessage::CleanupFinished { errors, .. } = msg {
            return errors;
        }
    }
    panic!("no cleanup.finished was pushed");
}
