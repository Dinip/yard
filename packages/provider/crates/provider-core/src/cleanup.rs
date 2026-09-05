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

use yard_protocol::{AppFilter, CleanupSteps};

use crate::backend::{BackendError, DeviceBackend};

/// Whether an app id is in scope for a filtered step.
///
/// An empty allow list means everything; a non-empty one means only what it
/// matches. Deny wins, so a pattern in both lists excludes.
fn in_scope(filter: &AppFilter, app_id: &str) -> bool {
    if filter.deny.iter().any(|pattern| matches(pattern, app_id)) {
        return false;
    }
    filter.allow.is_empty() || filter.allow.iter().any(|pattern| matches(pattern, app_id))
}

/// Case-insensitive glob where `*` is the only wildcard and matches any run of
/// characters, dots included — `*.google.*` catches `com.google.android.gm`.
///
/// Hand-rolled rather than pulling in a glob crate: these patterns are matched
/// against app ids, not paths, so none of the path semantics a glob crate
/// carries (`/` boundaries, `**`, character classes) would be right here.
fn matches(pattern: &str, app_id: &str) -> bool {
    let pattern = pattern.to_ascii_lowercase();
    let app_id = app_id.to_ascii_lowercase();

    let segments: Vec<&str> = pattern.split('*').collect();
    let Some(mut rest) = app_id.strip_prefix(segments[0]) else {
        return false;
    };

    // A pattern with no `*` is a plain equality test.
    let Some((tail, middle)) = segments[1..].split_last() else {
        return rest.is_empty();
    };

    for segment in middle {
        match rest.find(segment) {
            Some(at) => rest = &rest[at + segment.len()..],
            None => return false,
        }
    }

    // `rest` has already been consumed past every earlier segment, so a tail
    // that overlaps one of them must not count as a match.
    rest.len() >= tail.len() && rest.ends_with(tail)
}

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
    clear_filter: &AppFilter,
    baseline: Option<&HashSet<String>>,
    paths: &[String],
) -> CleanupReport {
    run_with_protected(
        backend,
        steps,
        clear_filter,
        baseline,
        paths,
        &HashSet::new(),
    )
    .await
}

/// Runs cleanup while preserving apps owned by the provider's preload policy.
///
/// The extra set matters after a provider restart: there may be no session
/// baseline at all, but a protected app still must not be removed by a cleanup
/// pass.
pub async fn run_with_protected(
    backend: &dyn DeviceBackend,
    steps: &CleanupSteps,
    clear_filter: &AppFilter,
    baseline: Option<&HashSet<String>>,
    paths: &[String],
    protected: &HashSet<String>,
) -> CleanupReport {
    let mut report = CleanupReport::default();

    if steps.uninstall_apps {
        match baseline {
            None => report
                .errors
                .push("uninstall: no baseline for this session, skipped".into()),
            Some(baseline) => uninstall_new(backend, baseline, protected, &mut report).await,
        }
    }

    if steps.clear_app_data {
        clear_surviving(backend, clear_filter, baseline, &mut report).await;
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
    protected: &HashSet<String>,
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
        if baseline.contains(&app.id) || protected.contains(&app.id) {
            continue;
        }
        match backend.uninstall(&app.id).await {
            Ok(()) => report.removed.push(app.id),
            Err(BackendError::Unsupported(_)) => return,
            Err(err) => report.errors.push(format!("uninstall {}: {err}", app.id)),
        }
    }
}

/// Clears the data of apps that are *staying* on the device and that `filter`
/// puts in scope.
///
/// Anything uninstalled above is already gone, and an app installed during the
/// session that survived a failed uninstall is not worth a second failure.
async fn clear_surviving(
    backend: &dyn DeviceBackend,
    filter: &AppFilter,
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
        // Out of scope is not a failure: it is the farm's admin saying this app
        // is one whose state must survive a handover.
        if !in_scope(filter, &app.id) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(allow: &[&str], deny: &[&str]) -> AppFilter {
        AppFilter {
            allow: allow.iter().map(|s| (*s).to_owned()).collect(),
            deny: deny.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    #[test]
    fn a_glob_spans_dots() {
        assert!(matches("*.google.*", "com.google.android.gm"));
        assert!(matches("com.google.*", "com.google.android.gm"));
        assert!(matches("*", "anything"));
        assert!(matches("com.acme.harness", "com.acme.harness"));

        assert!(!matches("*.google.*", "com.google"));
        assert!(!matches("com.acme.harness", "com.acme.harness.debug"));
        assert!(!matches("com.google.*", "org.example.com.google.spoof"));
    }

    /// Bundle ids are written both ways in practice — `com.Apple.Preferences`
    /// in one console, lower case in another — and a policy that silently
    /// misses because of a capital is the wrong kind of surprise.
    #[test]
    fn matching_ignores_case() {
        assert!(matches("com.Apple.*", "com.apple.preferences"));
    }

    /// The tail must not reuse characters an earlier segment already consumed.
    #[test]
    fn segments_do_not_overlap() {
        assert!(!matches("ab*ab", "abab_"));
        assert!(matches("ab*ab", "abxab"));
        assert!(matches("ab*ab", "abab"));
    }

    #[test]
    fn an_empty_allow_list_means_everything() {
        assert!(in_scope(&filter(&[], &[]), "com.acme.harness"));
        assert!(!in_scope(&filter(&[], &["com.acme.*"]), "com.acme.harness"));
    }

    #[test]
    fn deny_beats_allow() {
        let filter = filter(&["com.acme.*"], &["com.acme.mdm"]);
        assert!(in_scope(&filter, "com.acme.harness"));
        assert!(!in_scope(&filter, "com.acme.mdm"));
        assert!(!in_scope(&filter, "com.google.android.gm"));
    }
}
