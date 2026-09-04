//! Resetting a device between users, driven through the real command handler.
//!
//! Most of what is asserted here is the property STF never had: **the device
//! always comes back**. A failing step, an unsupported one, a backend that has
//! fallen over, a run that blows its deadline — none of them may leave a device
//! parked in `cleaning`, because a device nobody can reserve is worse than the
//! dirty device cleanup was meant to prevent.

use std::sync::Arc;
use std::time::Duration;

use provider_core::backend::DeviceBackend;
use provider_core::control::{CommandHandler, ControlSender};
use provider_core::session::SessionRegistry;
use provider_core::supervisor::Supervisor;
use tokio::sync::mpsc::UnboundedReceiver;
use yard_protocol::{
    AppFilter, AppInfo, CleanupSteps, CommandPayload, DeviceStatus, Platform, ProviderMessage,
};

const DEVICE: &str = "mock-1";
const RESERVATION: &str = "res-1";

fn all_steps() -> CleanupSteps {
    CleanupSteps {
        uninstall_apps: true,
        reset_screen: true,
        clear_app_data: true,
        wipe_folders: true,
    }
}

fn only_uninstall() -> CleanupSteps {
    CleanupSteps {
        uninstall_apps: true,
        reset_screen: false,
        clear_app_data: false,
        wipe_folders: false,
    }
}

/// Every surviving app in scope, which is what an admin who has not narrowed
/// the policy gets.
fn no_filter() -> AppFilter {
    AppFilter {
        allow: vec![],
        deny: vec![],
    }
}

fn only_clear() -> CleanupSteps {
    CleanupSteps {
        uninstall_apps: false,
        reset_screen: false,
        clear_app_data: true,
        wipe_folders: false,
    }
}

/// A supervisor with one mock device and a control channel to read.
async fn harness(
    backend: Arc<dyn DeviceBackend>,
    cleanup_paths: Vec<String>,
) -> (Arc<Supervisor>, UnboundedReceiver<ProviderMessage>) {
    let mut supervisor = Supervisor::new(SessionRegistry::new());
    supervisor.add_with_cleanup_paths(DEVICE.into(), backend, cleanup_paths);
    let supervisor = Arc::new(supervisor);
    supervisor.bootstrap().await;

    let (sender, rx) = ControlSender::detached();
    supervisor.attach_control(sender).await;
    (supervisor, rx)
}

fn mock() -> Arc<backend_mock::MockBackend> {
    backend_mock::MockBackend::new(DEVICE, Platform::Android, "Mock Pixel")
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

async fn cleanup(supervisor: &Arc<Supervisor>, steps: CleanupSteps) {
    cleanup_filtered(supervisor, steps, no_filter()).await;
}

async fn cleanup_filtered(supervisor: &Arc<Supervisor>, steps: CleanupSteps, filter: AppFilter) {
    supervisor
        .handle(CommandPayload::DeviceCleanup {
            device_id: DEVICE.into(),
            steps,
            clear_app_data_filter: filter,
            timeout_seconds: 30,
        })
        .await
        .expect("cleanup command accepted");
}

/// Waits for the device to leave `cleaning`, which the spawned task does.
async fn settled(supervisor: &Arc<Supervisor>) -> DeviceStatus {
    let device = supervisor.device(DEVICE).expect("device");
    for _ in 0..200 {
        let status = device.status().await;
        if status != DeviceStatus::Cleaning {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("device never left `cleaning`");
}

fn finished(rx: &mut UnboundedReceiver<ProviderMessage>) -> ProviderMessage {
    while let Ok(msg) = rx.try_recv() {
        if matches!(msg, ProviderMessage::CleanupFinished { .. }) {
            return msg;
        }
    }
    panic!("no cleanup.finished was pushed");
}

/// Every app id currently on the device, sorted so assertions are stable.
async fn ids(backend: &backend_mock::MockBackend) -> Vec<String> {
    let mut ids: Vec<String> = backend
        .state
        .installed
        .lock()
        .await
        .iter()
        .map(|app| app.id.clone())
        .collect();
    ids.sort();
    ids
}

async fn install_one(backend: &backend_mock::MockBackend, name: &str) {
    // Unique per call: these tests run concurrently in one process and several
    // stage the same file name, so a shared path means one test's cleanup
    // deletes the staged file another is still installing from.
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("farm-cleanup-test-{seq}-{name}"));
    tokio::fs::write(&path, b"apk").await.unwrap();
    backend
        .install(&path, &provider_core::backend::NullProgress)
        .await
        .expect("install");
    let _ = tokio::fs::remove_file(&path).await;
}

#[tokio::test]
async fn uninstalls_what_the_session_installed_and_nothing_else() {
    let backend = mock();
    // Something the device came with. It must survive.
    backend
        .state
        .installed
        .lock()
        .await
        .push(preinstalled("com.example.harness"));

    let (supervisor, mut rx) = harness(backend.clone(), vec![]).await;
    authorize(&supervisor, RESERVATION).await;
    let before = ids(&backend).await;
    install_one(&backend, "sideloaded.apk").await;

    cleanup(&supervisor, only_uninstall()).await;
    assert_eq!(settled(&supervisor).await, DeviceStatus::Ready);

    // Exactly back where it started: the sideload gone, everything the device
    // came with — including the org's own harness — untouched.
    assert_eq!(ids(&backend).await, before);
    assert!(before.contains(&"com.example.harness".to_string()));

    let ProviderMessage::CleanupFinished {
        removed, errors, ..
    } = finished(&mut rx)
    else {
        unreachable!()
    };
    assert_eq!(removed, vec!["mock.sideloaded.apk"]);
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");
}

/// The bug STF has: a provider that restarted mid-session has no baseline, and
/// diffing against an empty set would uninstall every app on the device.
#[tokio::test]
async fn without_a_baseline_it_removes_nothing() {
    let backend = mock();
    backend
        .state
        .installed
        .lock()
        .await
        .push(preinstalled("com.example.harness"));

    // Deliberately no authorize: this provider never saw the session start.
    let (supervisor, mut rx) = harness(backend.clone(), vec![]).await;
    let before = ids(&backend).await;

    cleanup(&supervisor, only_uninstall()).await;
    assert_eq!(settled(&supervisor).await, DeviceStatus::Ready);

    assert_eq!(ids(&backend).await, before, "nothing may be removed");
    let ProviderMessage::CleanupFinished {
        removed, errors, ..
    } = finished(&mut rx)
    else {
        unreachable!()
    };
    assert!(removed.is_empty());
    assert_eq!(errors.len(), 1, "the skip must be reported, not silent");
    assert!(errors[0].contains("no baseline"), "{errors:?}");
}

/// A renew re-authorizes the same reservation. Re-snapshotting there would fold
/// everything the user had installed so far into the "was already here" set,
/// and cleanup would leave it all behind.
#[tokio::test]
async fn renewing_does_not_rebaseline() {
    let backend = mock();
    let (supervisor, _rx) = harness(backend.clone(), vec![]).await;

    authorize(&supervisor, RESERVATION).await;
    let before = ids(&backend).await;
    install_one(&backend, "sideloaded.apk").await;
    authorize(&supervisor, RESERVATION).await; // the renew

    cleanup(&supervisor, only_uninstall()).await;
    assert_eq!(settled(&supervisor).await, DeviceStatus::Ready);
    assert_eq!(
        ids(&backend).await,
        before,
        "the renew must not have re-baselined"
    );
}

/// A *different* reservation is a new session, and must re-baseline — otherwise
/// the second user's cleanup would remove the first user's leftovers, which is
/// not this feature's job and is a surprise on a device somebody is using.
#[tokio::test]
async fn a_new_reservation_rebaselines() {
    let backend = mock();
    let (supervisor, _rx) = harness(backend.clone(), vec![]).await;

    authorize(&supervisor, RESERVATION).await;
    install_one(&backend, "first.apk").await;
    let after_first = ids(&backend).await;
    authorize(&supervisor, "res-2").await;

    cleanup(&supervisor, only_uninstall()).await;
    assert_eq!(settled(&supervisor).await, DeviceStatus::Ready);
    assert_eq!(
        ids(&backend).await,
        after_first,
        "the second session's cleanup must not remove the first's leftovers"
    );
}

#[tokio::test]
async fn runs_every_step_and_reports_what_it_did() {
    let backend = mock();
    backend
        .state
        .installed
        .lock()
        .await
        .push(preinstalled("com.example.harness"));
    backend.clipboard_set("a secret").await.unwrap();
    backend.rotate(90).await.unwrap();

    let paths = vec!["/sdcard/Download".to_string()];
    let (supervisor, mut rx) = harness(backend.clone(), paths.clone()).await;
    authorize(&supervisor, RESERVATION).await;
    install_one(&backend, "sideloaded.apk").await;

    cleanup(&supervisor, all_steps()).await;
    assert_eq!(settled(&supervisor).await, DeviceStatus::Ready);

    let ProviderMessage::CleanupFinished {
        removed,
        cleared,
        wiped,
        errors,
        ..
    } = finished(&mut rx)
    else {
        unreachable!()
    };
    assert_eq!(removed, vec!["mock.sideloaded.apk"]);
    // Every app that stayed, and only those — the uninstalled one is gone by
    // the time the clear step lists apps again.
    assert!(cleared.contains(&"com.example.harness".to_string()));
    assert!(!cleared.contains(&"mock.sideloaded.apk".to_string()));
    assert_eq!(wiped, paths);
    assert!(errors.is_empty(), "{errors:?}");

    assert_eq!(backend.clipboard_get().await.unwrap(), None);
    assert_eq!(
        backend
            .state
            .rotation
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
}

/// `Unsupported` is the backend saying "not applicable here" — iOS has no
/// `pm clear`. It is not a failure and must not appear in the report.
#[tokio::test]
async fn an_unsupported_step_is_silent() {
    let backend = mock();
    backend
        .state
        .no_clear_app_data
        .store(true, std::sync::atomic::Ordering::Relaxed);
    backend
        .state
        .installed
        .lock()
        .await
        .push(preinstalled("com.example.harness"));

    let (supervisor, mut rx) = harness(backend.clone(), vec![]).await;
    authorize(&supervisor, RESERVATION).await;

    cleanup(&supervisor, all_steps()).await;
    assert_eq!(settled(&supervisor).await, DeviceStatus::Ready);

    let ProviderMessage::CleanupFinished {
        cleared, errors, ..
    } = finished(&mut rx)
    else {
        unreachable!()
    };
    assert!(cleared.is_empty());
    assert!(errors.is_empty(), "Unsupported is not an error: {errors:?}");
}

/// One step falling over must not cost the others: a device that could not
/// clear an app's data is still better off with its folders emptied.
#[tokio::test]
async fn a_failing_step_does_not_abort_the_run() {
    let backend = mock();
    *backend.state.screen_fault.lock().await = Some(backend_mock::ScreenFault::Fails);
    let paths = vec!["/sdcard/Download".to_string()];
    let (supervisor, mut rx) = harness(backend.clone(), paths.clone()).await;
    authorize(&supervisor, RESERVATION).await;

    cleanup(&supervisor, all_steps()).await;
    assert_eq!(settled(&supervisor).await, DeviceStatus::Ready);

    let ProviderMessage::CleanupFinished { wiped, errors, .. } = finished(&mut rx) else {
        unreachable!()
    };
    assert_eq!(wiped, paths, "the later step still ran");
    assert_eq!(errors.len(), 1);
    assert!(errors[0].starts_with("reset screen:"), "{errors:?}");
}

/// The deadline exists because a wedged phone will hang an adb call forever.
#[tokio::test]
async fn a_run_that_blows_its_deadline_still_returns_the_device() {
    let backend = mock();
    *backend.state.screen_fault.lock().await = Some(backend_mock::ScreenFault::Hangs);
    let (supervisor, mut rx) = harness(backend.clone(), vec![]).await;

    supervisor
        .handle(CommandPayload::DeviceCleanup {
            device_id: DEVICE.into(),
            steps: CleanupSteps {
                uninstall_apps: false,
                reset_screen: true,
                clear_app_data: false,
                wipe_folders: false,
            },
            clear_app_data_filter: no_filter(),
            timeout_seconds: 1,
        })
        .await
        .expect("cleanup command accepted");

    assert_eq!(settled(&supervisor).await, DeviceStatus::Ready);
    let ProviderMessage::CleanupFinished { errors, .. } = finished(&mut rx) else {
        unreachable!()
    };
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("abandoned"), "{errors:?}");
}

/// Health beats occupancy: a phone that fell off the bus mid-wipe must not be
/// announced as ready for the next user to discover.
#[tokio::test]
async fn an_unhealthy_device_lands_unhealthy_not_ready() {
    let backend = mock();
    let (supervisor, _rx) = harness(backend.clone(), vec![]).await;
    authorize(&supervisor, RESERVATION).await;

    backend
        .state
        .healthy
        .store(false, std::sync::atomic::Ordering::Relaxed);

    cleanup(&supervisor, only_uninstall()).await;
    assert_eq!(settled(&supervisor).await, DeviceStatus::Unhealthy);
}

/// The status poll runs every 15s regardless of what else is happening, and
/// `refresh` deciding a cleaning device is "recovering" would hand the next
/// user a half-wiped phone.
#[tokio::test]
async fn a_status_poll_does_not_interrupt_a_cleaning_device() {
    let backend = mock();
    *backend.state.screen_fault.lock().await = Some(backend_mock::ScreenFault::Hangs);
    let (supervisor, _rx) = harness(backend.clone(), vec![]).await;

    supervisor
        .handle(CommandPayload::DeviceCleanup {
            device_id: DEVICE.into(),
            steps: CleanupSteps {
                uninstall_apps: false,
                reset_screen: true,
                clear_app_data: false,
                wipe_folders: false,
            },
            clear_app_data_filter: no_filter(),
            timeout_seconds: 30,
        })
        .await
        .unwrap();

    let device = supervisor.device(DEVICE).unwrap();
    supervisor.refresh(&device).await;
    assert_eq!(device.status().await, DeviceStatus::Cleaning);
}

/// Revoking a session and cleaning it are separate commands, so a farm with
/// cleanup switched off behaves exactly as it did before this feature.
#[tokio::test]
async fn revoke_alone_leaves_the_device_untouched() {
    let backend = mock();
    let (supervisor, _rx) = harness(backend.clone(), vec![]).await;
    authorize(&supervisor, RESERVATION).await;
    install_one(&backend, "sideloaded.apk").await;

    let before = ids(&backend).await;
    supervisor
        .handle(CommandPayload::SessionRevoke {
            device_id: DEVICE.into(),
            reason: Some("released".into()),
        })
        .await
        .unwrap();

    assert_eq!(ids(&backend).await, before, "revoke touches no apps");
    assert!(supervisor
        .sessions()
        .check(DEVICE, RESERVATION)
        .await
        .is_err());
}

/// The point of the allow list: an org preinstalls a signed-in MDM agent and a
/// test harness, and clearing either of them breaks the phone for everyone
/// after. Naming what *may* be cleared survives someone preinstalling a fifth
/// thing next month; a deny list does not.
#[tokio::test]
async fn clearing_app_data_only_touches_apps_the_allow_list_names() {
    let backend = mock();
    {
        let mut installed = backend.state.installed.lock().await;
        installed.push(preinstalled("com.google.android.gm"));
        installed.push(preinstalled("com.acme.mdm"));
        installed.push(preinstalled("com.acme.harness"));
    }
    let (supervisor, mut rx) = harness(backend.clone(), vec![]).await;
    authorize(&supervisor, RESERVATION).await;

    cleanup_filtered(
        &supervisor,
        only_clear(),
        AppFilter {
            allow: vec!["*.google.*".into(), "com.acme.harness".into()],
            deny: vec![],
        },
    )
    .await;
    assert_eq!(settled(&supervisor).await, DeviceStatus::Ready);

    let ProviderMessage::CleanupFinished {
        cleared, errors, ..
    } = finished(&mut rx)
    else {
        unreachable!()
    };
    let mut cleared = cleared;
    cleared.sort();
    assert_eq!(cleared, vec!["com.acme.harness", "com.google.android.gm"]);
    assert!(errors.is_empty(), "{errors:?}");
}

/// Deny wins, so an admin can widen with a glob and still carve one app out
/// without having to enumerate the rest.
#[tokio::test]
async fn a_denied_app_is_spared_even_when_the_allow_list_matches_it() {
    let backend = mock();
    {
        let mut installed = backend.state.installed.lock().await;
        installed.push(preinstalled("com.acme.harness"));
        installed.push(preinstalled("com.acme.mdm"));
    }
    let (supervisor, mut rx) = harness(backend.clone(), vec![]).await;
    authorize(&supervisor, RESERVATION).await;

    cleanup_filtered(
        &supervisor,
        only_clear(),
        AppFilter {
            allow: vec!["com.acme.*".into()],
            deny: vec!["com.acme.mdm".into()],
        },
    )
    .await;
    assert_eq!(settled(&supervisor).await, DeviceStatus::Ready);

    let ProviderMessage::CleanupFinished { cleared, .. } = finished(&mut rx) else {
        unreachable!()
    };
    assert_eq!(cleared, vec!["com.acme.harness"]);
}

/// An app the filter skips is not a failed step: it is policy working.
#[tokio::test]
async fn an_out_of_scope_app_is_skipped_silently() {
    let backend = mock();
    let (supervisor, mut rx) = harness(backend.clone(), vec![]).await;
    authorize(&supervisor, RESERVATION).await;

    cleanup_filtered(
        &supervisor,
        only_clear(),
        AppFilter {
            allow: vec!["nothing.matches.this".into()],
            deny: vec![],
        },
    )
    .await;
    assert_eq!(settled(&supervisor).await, DeviceStatus::Ready);

    let ProviderMessage::CleanupFinished {
        cleared, errors, ..
    } = finished(&mut rx)
    else {
        unreachable!()
    };
    assert!(cleared.is_empty(), "{cleared:?}");
    assert!(errors.is_empty(), "{errors:?}");
    assert!(backend.state.cleared.lock().await.is_empty());
}

fn preinstalled(id: &str) -> AppInfo {
    AppInfo {
        id: id.to_owned(),
        name: None,
        version: None,
        system: Some(false),
    }
}
