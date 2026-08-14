//! Owns every device on the host and routes commands to them.
//!
//! Replaces `stf-ios-provider/src/provider.rs`. Upstream ran one *container*
//! per device; this is one process supervising all of them. Crash isolation is
//! kept — each device is an independently restartable task tree — while N
//! containers, N ZMQ connections and N config files collapse into one.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use farm_protocol::{
    AppFilter, Battery, CleanupSteps, CommandData, CommandPayload, DeviceSnapshot, DeviceStatus,
    ProviderMessage,
};
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::backend::{DeviceBackend, DeviceInfo};
use crate::control::{now_millis, CommandHandler, ControlSender};
use crate::session::{Authorization, SessionRegistry};

/// How often each device's info is re-read and pushed up if it changed.
const POLL_INTERVAL: Duration = Duration::from_secs(15);

/// At most one activity report per device per this long.
///
/// A drag is hundreds of pointer events a second and every one of them is
/// activity; the coordinator only needs to know the reservation is not idle,
/// so this is deliberately coarse relative to any idle timeout worth setting.
const ACTIVITY_INTERVAL: Duration = Duration::from_secs(30);

/// Lets one report through per [`ACTIVITY_INTERVAL`].
///
/// A plain mutex rather than an async one: this sits on the pointer-move path,
/// where a drag is hundreds of events a second, and the critical section is a
/// comparison of two instants.
#[derive(Default)]
struct ActivityThrottle {
    last: Mutex<Option<Instant>>,
}

impl ActivityThrottle {
    fn claim(&self, now: Instant) -> bool {
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

/// What was installed when a session started, so cleanup can tell the user's
/// apps from the device's own.
struct AppBaseline {
    /// Which reservation this was taken for. A renew re-authorizes the same
    /// reservation, and re-snapshotting then would quietly bless everything the
    /// user had installed so far.
    reservation_id: String,
    apps: HashSet<String>,
}

pub struct Device {
    pub id: String,
    pub backend: Arc<dyn DeviceBackend>,
    /// Directories cleanup empties, from this device's config. Empty is the
    /// normal case; the step is then a no-op rather than an error.
    cleanup_paths: Vec<String>,
    status: RwLock<DeviceStatus>,
    info: RwLock<Option<DeviceInfo>>,
    /// Rate limiter for activity reports, not a record of activity itself.
    activity: ActivityThrottle,
    /// App ids present when the current session was authorized, and the
    /// reservation that baseline belongs to.
    ///
    /// `None` means no session has been authorized since this provider started,
    /// so cleanup has nothing to diff against and declines to uninstall — see
    /// [`crate::cleanup::run`].
    baseline: RwLock<Option<AppBaseline>>,
    /// Installs attempted on this device, for the metrics exporter. Counted
    /// where the result is already reported upstream, so the session server
    /// needs no knowledge of metrics at all.
    installs_ok: AtomicU64,
    installs_failed: AtomicU64,
}

impl Device {
    pub async fn status(&self) -> DeviceStatus {
        *self.status.read().await
    }

    pub async fn set_status(&self, status: DeviceStatus) {
        *self.status.write().await = status;
    }

    pub async fn info(&self) -> Option<DeviceInfo> {
        self.info.read().await.clone()
    }

    /// Installs attempted, as `(ok, failed)`.
    pub fn install_counts(&self) -> (u64, u64) {
        (
            self.installs_ok.load(Ordering::Relaxed),
            self.installs_failed.load(Ordering::Relaxed),
        )
    }

    pub async fn snapshot(&self) -> DeviceSnapshot {
        let info = self.info.read().await.clone();
        let status = *self.status.read().await;
        // The codec the stream is actually running, read without waiting: a
        // device that is not streaming simply has none to report.
        let codec = self.backend.video().current_codec().map(|c| c.codec);
        snapshot_from(&self.id, status, info.as_ref(), codec)
    }
}

fn snapshot_from(
    id: &str,
    status: DeviceStatus,
    info: Option<&DeviceInfo>,
    stream_codec: Option<String>,
) -> DeviceSnapshot {
    match info {
        Some(info) => DeviceSnapshot {
            id: id.to_owned(),
            platform: info.platform,
            status,
            name: info.name.clone(),
            model: info.model.clone(),
            manufacturer: info.manufacturer.clone(),
            os_version: info.os_version.clone(),
            abi: info.abi.clone(),
            sdk: info.sdk,
            serial: info.serial.clone(),
            brand: info.brand.clone(),
            build_id: info.build_id.clone(),
            security_patch: info.security_patch.clone(),
            abi_list: info.abi_list.clone(),
            display: info.display.clone(),
            battery: (info.battery_level.is_some() || info.battery_state.is_some()).then(|| {
                Battery {
                    level: info.battery_level,
                    state: info.battery_state.clone(),
                }
            }),
            stream_codec,
            adb_port: None,
            note: None,
        },
        // Reported before the backend has managed to read the device — better
        // than omitting it, which the coordinator would reconcile as absent.
        None => DeviceSnapshot {
            id: id.to_owned(),
            platform: farm_protocol::Platform::Ios,
            status,
            name: None,
            model: None,
            manufacturer: None,
            os_version: None,
            abi: None,
            sdk: None,
            serial: None,
            brand: None,
            build_id: None,
            security_patch: None,
            abi_list: None,
            display: None,
            battery: None,
            stream_codec,
            adb_port: None,
            note: None,
        },
    }
}

pub struct Supervisor {
    devices: HashMap<String, Arc<Device>>,
    sessions: SessionRegistry,
    control: RwLock<Option<ControlSender>>,
}

impl Supervisor {
    pub fn new(sessions: SessionRegistry) -> Self {
        Self {
            devices: HashMap::new(),
            sessions,
            control: RwLock::new(None),
        }
    }

    pub fn add(&mut self, id: String, backend: Arc<dyn DeviceBackend>) {
        self.add_with_cleanup_paths(id, backend, Vec::new());
    }

    pub fn add_with_cleanup_paths(
        &mut self,
        id: String,
        backend: Arc<dyn DeviceBackend>,
        cleanup_paths: Vec<String>,
    ) {
        self.devices.insert(
            id.clone(),
            Arc::new(Device {
                id,
                backend,
                cleanup_paths,
                status: RwLock::new(DeviceStatus::Preparing),
                info: RwLock::new(None),
                activity: ActivityThrottle::default(),
                baseline: RwLock::new(None),
                installs_ok: AtomicU64::new(0),
                installs_failed: AtomicU64::new(0),
            }),
        );
    }

    pub async fn attach_control(&self, sender: ControlSender) {
        *self.control.write().await = Some(sender);
    }

    pub fn device(&self, id: &str) -> Option<Arc<Device>> {
        self.devices.get(id).cloned()
    }

    pub fn devices(&self) -> impl Iterator<Item = &Arc<Device>> {
        self.devices.values()
    }

    pub fn sessions(&self) -> &SessionRegistry {
        &self.sessions
    }

    /// Whether the coordinator has acked this provider's `hello`.
    pub async fn is_registered(&self) -> bool {
        self.control
            .read()
            .await
            .as_ref()
            .is_some_and(|sender| sender.is_registered())
    }

    async fn push(&self, msg: ProviderMessage) {
        if let Some(sender) = self.control.read().await.as_ref() {
            sender.send(msg);
        }
    }

    fn require(&self, device_id: &str) -> Result<Arc<Device>> {
        self.devices
            .get(device_id)
            .cloned()
            .ok_or_else(|| anyhow!("this provider does not own device {device_id}"))
    }

    /// Re-reads each device's info and reports anything that changed.
    ///
    /// Runs for the life of the process, independent of the control plane: if
    /// the coordinator is down we simply have nowhere to send the update, and
    /// the next `hello` carries the current state anyway.
    pub async fn run_poll_loop(self: Arc<Self>) {
        let mut ticker = tokio::time::interval(POLL_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            ticker.tick().await;
            for device in self.devices.values() {
                self.refresh(device).await;
            }
        }
    }

    /// Reads one device's info, updates its status, and pushes if anything moved.
    pub async fn refresh(&self, device: &Arc<Device>) {
        let healthy = device.backend.is_healthy().await;
        let previous_status = device.status().await;

        let next_status = match (healthy, previous_status) {
            (false, _) => DeviceStatus::Unhealthy,
            // Recovering from unhealthy, or finishing bring-up.
            (true, DeviceStatus::Unhealthy | DeviceStatus::Preparing | DeviceStatus::Absent) => {
                DeviceStatus::Ready
            }
            // A cleanup in flight owns this device's status and will set it
            // itself when it finishes. Spelled out rather than left to the
            // catch-all below because a poll racing a cleanup back to `ready`
            // would hand the next user a half-wiped phone — exactly the STF
            // bug this feature exists to avoid.
            (true, DeviceStatus::Cleaning) => DeviceStatus::Cleaning,
            // `busy` is the coordinator's word, not ours — a reserved device
            // stays reserved through a poll.
            (true, current) => current,
        };

        let info = match device.backend.info().await {
            Ok(info) => Some(info),
            Err(err) => {
                warn!(device = %device.id, error = %err, "reading device info failed");
                None
            }
        };

        let info_changed = {
            let mut held = device.info.write().await;
            let changed = info.is_some() && *held != info;
            if info.is_some() {
                *held = info;
            }
            changed
        };

        if next_status != previous_status {
            device.set_status(next_status).await;
            info!(device = %device.id, from = ?previous_status, to = ?next_status, "status changed");
        }

        if info_changed || next_status != previous_status {
            self.push(ProviderMessage::DeviceUpsert {
                device: device.snapshot().await,
            })
            .await;
        }
    }

    /// Records what was installed at the start of a session, for cleanup to
    /// diff against.
    ///
    /// A no-op when the reservation is already the one we baselined: renewing
    /// re-authorizes, and re-snapshotting then would fold everything the user
    /// had installed so far into the "was already here" set.
    async fn snapshot_baseline(&self, device: &Arc<Device>, reservation_id: &str) {
        {
            let held = device.baseline.read().await;
            if held
                .as_ref()
                .is_some_and(|held| held.reservation_id == reservation_id)
            {
                return;
            }
        }

        let apps = match device.backend.apps().await {
            Ok(apps) => apps.into_iter().map(|app| app.id).collect::<HashSet<_>>(),
            Err(err) => {
                // Leave the baseline unset rather than storing a partial one:
                // cleanup skips uninstalling without a baseline, and that is a
                // far better failure than diffing against a short list and
                // removing apps that were always there.
                warn!(device = %device.id, error = %err, "no app baseline for this session");
                *device.baseline.write().await = None;
                return;
            }
        };

        *device.baseline.write().await = Some(AppBaseline {
            reservation_id: reservation_id.to_owned(),
            apps,
        });
    }

    /// Reports that someone drove a device, at most once per 30s per device.
    ///
    /// The provider is the authoritative source for this: it sees input on the
    /// session plane and installs on the artifact plane, and it is the only
    /// thing that can see a device being used through an exposed adb transport
    /// at all — none of which the browser can vouch for.
    pub async fn note_activity(&self, device_id: &str) {
        let Some(device) = self.devices.get(device_id) else {
            return;
        };
        if !device.activity.claim(Instant::now()) {
            return;
        }
        self.push(ProviderMessage::DeviceActivity {
            device_id: device_id.to_owned(),
            at: now_millis(),
        })
        .await;
    }

    /// Reports an install upstream.
    ///
    /// The uploaded file is already deleted by the time this is sent, so this
    /// event is the *only* record the install happened — the coordinator turns
    /// it into an audit row. There is deliberately no artifact table.
    pub async fn push_install_result(
        &self,
        device_id: &str,
        user_id: &str,
        filename: &str,
        size: i64,
        sha256: &str,
        error: Option<String>,
    ) {
        if let Some(device) = self.devices.get(device_id) {
            let counter = if error.is_none() {
                &device.installs_ok
            } else {
                &device.installs_failed
            };
            counter.fetch_add(1, Ordering::Relaxed);
        }

        self.push(ProviderMessage::InstallFinished {
            device_id: device_id.to_owned(),
            user_id: user_id.to_owned(),
            filename: filename.to_owned(),
            size,
            sha256: sha256.to_owned(),
            ok: error.is_none(),
            error,
        })
        .await;
    }

    /// Reports a file taken off a device upstream.
    ///
    /// The same arrangement as [`Self::push_install_result`] and for the same
    /// reason: the download went browser↔provider, so this event is the only
    /// way the coordinator can know it happened. It matters more in this
    /// direction — this is the one operation that carries data *out* of a
    /// device — which is why the digest travels with it.
    pub async fn push_file_pulled(
        &self,
        device_id: &str,
        user_id: &str,
        path: &str,
        size: i64,
        sha256: &str,
    ) {
        self.push(ProviderMessage::FilePulled {
            device_id: device_id.to_owned(),
            user_id: user_id.to_owned(),
            path: path.to_owned(),
            size,
            sha256: sha256.to_owned(),
        })
        .await;
    }

    /// Brings every device to its initial state and reports it once.
    pub async fn bootstrap(self: &Arc<Self>) {
        for device in self.devices.values() {
            self.refresh(device).await;
        }
    }
}

#[async_trait::async_trait]
impl CommandHandler for Supervisor {
    async fn inventory(&self) -> Vec<DeviceSnapshot> {
        let mut out = Vec::with_capacity(self.devices.len());
        for device in self.devices.values() {
            out.push(device.snapshot().await);
        }
        out
    }

    /// The coordinator is the source of truth for authorization, so anything we
    /// were told before the socket dropped may be stale by the time it returns.
    /// It re-pushes on reconnect.
    async fn on_disconnected(&self) {
        self.sessions.revoke_all("control plane disconnected").await;
    }

    async fn handle(&self, payload: CommandPayload) -> Result<Option<CommandData>> {
        match payload {
            CommandPayload::SessionAuthorize {
                device_id,
                reservation_id,
                user_id,
            } => {
                let device = self.require(&device_id)?;
                self.snapshot_baseline(&device, &reservation_id).await;
                self.sessions
                    .authorize(
                        &device_id,
                        Authorization {
                            reservation_id,
                            user_id,
                        },
                    )
                    .await;
                Ok(None)
            }

            CommandPayload::SessionRevoke { device_id, reason } => {
                self.sessions
                    .revoke(&device_id, reason.as_deref().unwrap_or("revoked"))
                    .await;
                Ok(None)
            }

            CommandPayload::DeviceCleanup {
                device_id,
                steps,
                clear_app_data_filter,
                timeout_seconds,
            } => {
                let device = self.require(&device_id)?;
                device.set_status(DeviceStatus::Cleaning).await;
                self.push(ProviderMessage::DeviceStatus {
                    device_id,
                    status: DeviceStatus::Cleaning,
                    note: Some("cleaning".into()),
                })
                .await;

                // Answered now, run later. Two reasons, either sufficient: a
                // multi-package uninstall runs far past the gateway's
                // command-result timeout, and `handle` is awaited inline on the
                // control socket's read loop, so blocking here would stall this
                // provider's heartbeats for every device it owns.
                let sender = self.control.read().await.clone();
                tokio::spawn(run_cleanup(
                    device,
                    sender,
                    steps,
                    clear_app_data_filter,
                    timeout_seconds,
                ));
                Ok(None)
            }

            CommandPayload::DeviceReboot { device_id } => {
                let device = self.require(&device_id)?;
                device.backend.reboot().await?;
                device.set_status(DeviceStatus::Preparing).await;
                self.push(ProviderMessage::DeviceStatus {
                    device_id,
                    status: DeviceStatus::Preparing,
                    note: Some("rebooting".into()),
                })
                .await;
                Ok(None)
            }

            CommandPayload::DeviceRotate { device_id, degrees } => {
                self.require(&device_id)?.backend.rotate(degrees).await?;
                Ok(None)
            }

            CommandPayload::DeviceApps { device_id } => {
                let apps = self.require(&device_id)?.backend.apps().await?;
                Ok(Some(CommandData {
                    apps: Some(apps),
                    adb_port: None,
                }))
            }

            CommandPayload::DeviceLaunch {
                device_id,
                app_id,
                args,
            } => {
                self.require(&device_id)?
                    .backend
                    .launch(&app_id, &args.unwrap_or_default())
                    .await?;
                Ok(None)
            }

            CommandPayload::DeviceUninstall { device_id, app_id } => {
                self.require(&device_id)?.backend.uninstall(&app_id).await?;
                Ok(None)
            }

            CommandPayload::DeviceAdbExpose { device_id } => {
                let debug = self.require(&device_id)?.backend.remote_debug().await?;
                Ok(Some(CommandData {
                    apps: None,
                    adb_port: Some(debug.port as i64),
                }))
            }

            CommandPayload::DeviceAdbUnexpose { device_id } => {
                self.require(&device_id)?
                    .backend
                    .remote_debug_stop()
                    .await?;
                Ok(None)
            }

            CommandPayload::DeviceRestart { device_id } => {
                let device = self.require(&device_id)?;
                device.set_status(DeviceStatus::Preparing).await;
                device.backend.restart().await?;
                self.refresh(&device).await;
                Ok(None)
            }
        }
    }
}

/// Resets a device after its reservation ended, then puts it back in the pool.
///
/// A free function rather than a method because it outlives the command that
/// started it and must not borrow the supervisor for two minutes.
///
/// **The device always ends up out of `cleaning`.** Every early return, every
/// failed step and the deadline itself converge on the same landing at the
/// bottom, because a device stuck in `cleaning` is invisible inventory — worse
/// than the dirty device cleanup was meant to prevent.
async fn run_cleanup(
    device: Arc<Device>,
    sender: Option<ControlSender>,
    steps: CleanupSteps,
    clear_filter: AppFilter,
    timeout_seconds: i64,
) {
    let started = Instant::now();
    let baseline = device.baseline.read().await;
    let apps = baseline.as_ref().map(|held| &held.apps);

    let budget = Duration::from_secs(timeout_seconds.clamp(1, 3600) as u64);
    let mut report = match tokio::time::timeout(
        budget,
        crate::cleanup::run(
            device.backend.as_ref(),
            &steps,
            &clear_filter,
            apps,
            &device.cleanup_paths,
        ),
    )
    .await
    {
        Ok(report) => report,
        // The steps ran sequentially, so whatever the deadline interrupted is
        // simply lost: there is no partial report to recover. Say so plainly
        // rather than reporting an empty run as a clean one.
        Err(_) => {
            warn!(device = %device.id, seconds = timeout_seconds, "cleanup timed out");
            crate::cleanup::CleanupReport {
                errors: vec![format!(
                    "cleanup exceeded {timeout_seconds}s and was abandoned"
                )],
                ..Default::default()
            }
        }
    };
    drop(baseline);

    // The session is over either way; a stale baseline would be diffed against
    // the *next* user's session if the provider never sees its authorize.
    *device.baseline.write().await = None;

    if !report.errors.is_empty() {
        warn!(device = %device.id, errors = ?report.errors, "cleanup finished with errors");
    }
    info!(
        device = %device.id,
        removed = report.removed.len(),
        cleared = report.cleared.len(),
        "cleaned"
    );

    // Health decides the landing: a phone that fell off the bus mid-wipe is
    // unhealthy, not ready, and reporting `ready` would put it back in the pool
    // for the next user to discover.
    let status = if device.backend.is_healthy().await {
        DeviceStatus::Ready
    } else {
        DeviceStatus::Unhealthy
    };
    device.set_status(status).await;

    if let Some(sender) = sender {
        sender.send(ProviderMessage::CleanupFinished {
            device_id: device.id.clone(),
            removed: std::mem::take(&mut report.removed),
            cleared: std::mem::take(&mut report.cleared),
            wiped: std::mem::take(&mut report.wiped),
            errors: std::mem::take(&mut report.errors),
            duration_ms: started.elapsed().as_millis().min(i64::MAX as u128) as i64,
        });
        sender.send(ProviderMessage::DeviceStatus {
            device_id: device.id.clone(),
            status,
            note: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_report_per_interval() {
        let throttle = ActivityThrottle::default();
        let start = Instant::now();

        // A drag is hundreds of events; the coordinator needs to see one.
        assert!(throttle.claim(start));
        for step in 1..50 {
            assert!(!throttle.claim(start + Duration::from_millis(step * 10)));
        }

        assert!(throttle.claim(start + ACTIVITY_INTERVAL));
        assert!(!throttle.claim(start + ACTIVITY_INTERVAL));
    }
}
