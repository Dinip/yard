//! Resetting a device between users.
//!
//! STF did this in `lib/units/device/plugins/cleanup.js`: one package snapshot
//! at worker boot, a set difference on group leave, unbounded parallel
//! uninstalls, nobody awaiting any of it. The behaviour is worth having and the
//! implementation is not — see docs/CLEANUP.md for the four bugs this module
//! exists to avoid.
//!
//! What it guarantees:
//!
//! - **Steps are sequential.** STF left the reason in a commit message: "do
//!   only one adb command at a time to ensure they all are executed". A phone
//!   handed eight concurrent `pm` calls drops some of them.
//! - **A failing step does not abort the run.** Every error lands in the report
//!   and the next step still runs, because a device that could not clear one
//!   app's data is still better off with its scratch directories emptied.
//! - **A step the backend cannot do is not an error.** `Unsupported` is the
//!   backend saying "not applicable here" — iOS has no `pm clear` — and it is
//!   recorded as nothing at all.
//!
//! The deadline and the device's final status live in the supervisor, not here:
//! this function returns a report, and *nothing* it does can leave a device
//! parked out of the pool.

use std::collections::HashSet;

use farm_protocol::CleanupSteps;

use crate::backend::{BackendError, DeviceBackend};

/// What a run actually did, straight onto the wire as `cleanup.finished`.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct CleanupReport {
    pub removed: Vec<String>,
    pub cleared: Vec<String>,
    pub wiped: Vec<String>,
    /// One per failed step, already prefixed with the step's name.
    pub errors: Vec<String>,
}

impl CleanupReport {
    /// Records a step's outcome. `Unsupported` is silence, not a failure.
    fn note(&mut self, step: &str, result: Result<(), BackendError>) -> bool {
        match result {
            Ok(()) => true,
            Err(BackendError::Unsupported(_)) => false,
            Err(err) => {
                self.errors.push(format!("{step}: {err}"));
                false
            }
        }
    }
}

/// Runs the requested steps against one device.
///
/// `baseline` is the set of app ids present when the session was authorized.
/// `None` means the provider never saw the authorize — it restarted mid-session
/// — and the uninstall step is **skipped**, because the alternative is diffing
/// against an empty set and removing every app on the device. STF's equivalent
/// gap silently blesses leftovers; declining to act is the safer direction.
pub async fn run(
    backend: &dyn DeviceBackend,
    steps: &CleanupSteps,
    baseline: Option<&HashSet<String>>,
    paths: &[String],
) -> CleanupReport {
    let mut report = CleanupReport::default();

    if steps.uninstall_apps {
        match baseline {
            None => report
                .errors
                .push("uninstall: no baseline for this session, skipped".into()),
            Some(baseline) => uninstall_new(backend, baseline, &mut report).await,
        }
    }

    if steps.clear_app_data {
        clear_surviving(backend, baseline, &mut report).await;
    }

    if steps.reset_screen {
        let result = backend.reset_screen().await;
        report.note("reset screen", result);
    }

    if steps.wipe_folders && !paths.is_empty() {
        let result = backend.wipe_paths(paths).await;
        if report.note("wipe folders", result) {
            report.wiped = paths.to_vec();
        }
    }

    report
}

/// Uninstalls whatever appeared since the baseline was taken.
///
/// Deliberately not the inverse: an app in the baseline that is *gone* is the
/// user having uninstalled something, which is their business.
async fn uninstall_new(
    backend: &dyn DeviceBackend,
    baseline: &HashSet<String>,
    report: &mut CleanupReport,
) {
    let current = match backend.apps().await {
        Ok(apps) => apps,
        Err(BackendError::Unsupported(_)) => return,
        Err(err) => {
            report
                .errors
                .push(format!("uninstall: listing apps: {err}"));
            return;
        }
    };

    for app in current {
        if baseline.contains(&app.id) {
            continue;
        }
        match backend.uninstall(&app.id).await {
            Ok(()) => report.removed.push(app.id),
            Err(BackendError::Unsupported(_)) => return,
            Err(err) => report.errors.push(format!("uninstall {}: {err}", app.id)),
        }
    }
}

/// Clears the data of apps that are *staying* on the device.
///
/// Anything uninstalled above is already gone, and an app installed during the
/// session that survived a failed uninstall is not worth a second failure.
async fn clear_surviving(
    backend: &dyn DeviceBackend,
    baseline: Option<&HashSet<String>>,
    report: &mut CleanupReport,
) {
    let current = match backend.apps().await {
        Ok(apps) => apps,
        Err(BackendError::Unsupported(_)) => return,
        Err(err) => {
            report
                .errors
                .push(format!("clear app data: listing apps: {err}"));
            return;
        }
    };

    for app in current {
        // Without a baseline every surviving third-party app is fair game;
        // with one, only the apps that were here before the session.
        if baseline.is_some_and(|baseline| !baseline.contains(&app.id)) {
            continue;
        }
        match backend.clear_app_data(&app.id).await {
            Ok(()) => report.cleared.push(app.id),
            // The backend has no such operation at all — stop asking.
            Err(BackendError::Unsupported(_)) => return,
            Err(err) => report
                .errors
                .push(format!("clear app data {}: {err}", app.id)),
        }
    }
}
