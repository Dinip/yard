//! Parking an idle device's screen, and bringing it back for whoever reserves it.
//!
//! The property under test is that the provider's belief about a screen is
//! never allowed to go stale: anything that could have lit the panel behind its
//! back — a cleanup run pressing Home, a reboot, a device falling over and
//! coming back — must leave the device parked again, and nothing may press a
//! button on a device somebody is using.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use provider_core::control::{CommandHandler, ControlSender};
use provider_core::session::SessionRegistry;
use provider_core::supervisor::Supervisor;
use tokio::sync::mpsc::UnboundedReceiver;
use yard_protocol::{
    AppFilter, CleanupSteps, CommandPayload, DeviceStatus, Platform, ProviderMessage,
};

const DEVICE: &str = "mock-1";

/// The receiver is held rather than read: dropping it would make every push
/// from the supervisor a send to a closed channel.
type Harness = (
    Arc<Supervisor>,
    Arc<backend_mock::MockBackend>,
    UnboundedReceiver<ProviderMessage>,
);

/// A supervisor with one mock device, already bootstrapped — which is itself
/// the first park, because a provider starting up finds every device idle.
async fn harness(blank_idle_screens: bool) -> Harness {
    let backend = backend_mock::MockBackend::new(DEVICE, Platform::Android, "Mock Pixel");
    let mut supervisor = Supervisor::new(SessionRegistry::new());
    supervisor.set_blank_idle_screens(blank_idle_screens);
    supervisor.add(DEVICE.into(), backend.clone());
    let supervisor = Arc::new(supervisor);
    supervisor.bootstrap().await;

    let (sender, rx) = ControlSender::detached();
    supervisor.attach_control(sender).await;
    (supervisor, backend, rx)
}

async fn poll(supervisor: &Arc<Supervisor>) {
    let device = supervisor.device(DEVICE).expect("device");
    supervisor.refresh(&device).await;
}

async fn authorize(supervisor: &Arc<Supervisor>, reservation_id: &str) {
    supervisor
        .handle(CommandPayload::SessionAuthorize {
            device_id: DEVICE.into(),
            reservation_id: reservation_id.into(),
            user_id: "user-1".into(),
            adb_keys: Vec::new(),
        })
        .await
        .expect("authorize");
}

async fn revoke(supervisor: &Arc<Supervisor>) {
    supervisor
        .handle(CommandPayload::SessionRevoke {
            device_id: DEVICE.into(),
            reason: Some("released".into()),
        })
        .await
        .expect("revoke");
}

async fn screen_calls(backend: &Arc<backend_mock::MockBackend>) -> Vec<bool> {
    backend.state.screen_power.lock().await.clone()
}

#[tokio::test]
async fn an_idle_device_is_parked_once_and_then_left_alone() {
    let (supervisor, backend, _rx) = harness(true).await;
    assert_eq!(screen_calls(&backend).await, vec![false]);

    // Fifteen seconds later, and fifteen after that.
    poll(&supervisor).await;
    poll(&supervisor).await;
    assert_eq!(screen_calls(&backend).await, vec![false]);
}

#[tokio::test]
async fn reserving_wakes_the_screen_and_releasing_parks_it_again() {
    let (supervisor, backend, _rx) = harness(true).await;

    authorize(&supervisor, "res-1").await;
    assert_eq!(screen_calls(&backend).await, vec![false, true]);

    // A reserved device is idle by status but not by session, so a poll in the
    // middle of somebody's session must not touch the screen.
    poll(&supervisor).await;
    assert_eq!(screen_calls(&backend).await, vec![false, true]);

    revoke(&supervisor).await;
    poll(&supervisor).await;
    assert_eq!(screen_calls(&backend).await, vec![false, true, false]);
}

#[tokio::test]
async fn renewing_does_not_yank_a_working_user_back_to_the_home_screen() {
    let (supervisor, backend, _rx) = harness(true).await;

    authorize(&supervisor, "res-1").await;
    // A renew re-authorizes the same reservation, every renewal interval, for
    // as long as somebody is using the device.
    authorize(&supervisor, "res-1").await;
    authorize(&supervisor, "res-1").await;

    assert_eq!(screen_calls(&backend).await, vec![false, true]);
}

#[tokio::test]
async fn a_cleanup_run_leaves_the_device_parked_again() {
    let (supervisor, backend, _rx) = harness(true).await;
    authorize(&supervisor, "res-1").await;
    revoke(&supervisor).await;

    // `reset_screen` presses Home, which lights the panel back up. If the park
    // were a one-shot hook on release, this is where a device would be left on
    // all night.
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
        .expect("cleanup accepted");

    let device = supervisor.device(DEVICE).expect("device");
    for _ in 0..200 {
        if device.status().await != DeviceStatus::Cleaning {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    poll(&supervisor).await;
    assert_eq!(screen_calls(&backend).await, vec![false, true, false]);
}

#[tokio::test]
async fn a_device_that_fell_over_is_parked_again_when_it_comes_back() {
    let (supervisor, backend, _rx) = harness(true).await;

    backend.state.healthy.store(false, Ordering::Relaxed);
    poll(&supervisor).await;
    assert_eq!(
        supervisor.device(DEVICE).unwrap().status().await,
        DeviceStatus::Unhealthy
    );

    // A device that dropped out and came back has been through whatever a
    // reconnect or a reboot does to its screen; the old belief is worthless.
    backend.state.healthy.store(true, Ordering::Relaxed);
    poll(&supervisor).await;
    assert_eq!(screen_calls(&backend).await, vec![false, false]);
}

#[tokio::test]
async fn a_wake_that_did_not_land_is_tried_again_on_the_next_one() {
    let (supervisor, backend, _rx) = harness(true).await;
    assert_eq!(screen_calls(&backend).await, vec![false]);

    // The press fails the way it does when the device's HID surfaces are
    // rebuilding under it. The screen is still parked, so the belief has to
    // survive — losing it here hands the holder a black phone with nothing
    // left that would ever light it.
    backend
        .state
        .screen_power_fails
        .store(true, Ordering::Relaxed);
    authorize(&supervisor, "res-1").await;
    assert_eq!(screen_calls(&backend).await, vec![false]);

    backend
        .state
        .screen_power_fails
        .store(false, Ordering::Relaxed);
    authorize(&supervisor, "res-1").await;
    assert_eq!(screen_calls(&backend).await, vec![false, true]);
}

#[tokio::test]
async fn a_backend_that_cannot_park_is_asked_exactly_once() {
    let backend = backend_mock::MockBackend::new(DEVICE, Platform::Ios, "Mock iPhone");
    backend.state.no_screen_power.store(true, Ordering::Relaxed);

    let mut supervisor = Supervisor::new(SessionRegistry::new());
    supervisor.add(DEVICE.into(), backend.clone());
    let supervisor = Arc::new(supervisor);
    supervisor.bootstrap().await;

    poll(&supervisor).await;
    poll(&supervisor).await;

    assert!(screen_calls(&backend).await.is_empty());
    assert_eq!(
        backend.state.screen_power_attempts.load(Ordering::Relaxed),
        1
    );
}

#[tokio::test]
async fn a_provider_told_to_leave_screens_alone_leaves_them_alone() {
    let (supervisor, backend, _rx) = harness(false).await;

    poll(&supervisor).await;
    authorize(&supervisor, "res-1").await;
    revoke(&supervisor).await;
    poll(&supervisor).await;

    assert_eq!(
        backend.state.screen_power_attempts.load(Ordering::Relaxed),
        0
    );
}
