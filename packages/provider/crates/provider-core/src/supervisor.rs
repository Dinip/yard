//! Owns every device on the host and routes commands to them.
//!
//! Replaces `stf-ios-provider/src/provider.rs`. Upstream ran one *container*
//! per device; this is one process supervising all of them. Crash isolation is
//! kept — each device is an independently restartable task tree — while N
//! containers, N ZMQ connections and N config files collapse into one.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use tokio::sync::{broadcast, Mutex as AsyncMutex, RwLock};
use tracing::{info, warn};
use yard_protocol::{
    AppFilter, Battery, CleanupSteps, CommandData, CommandPayload, DeviceSnapshot, DeviceStatus,
    InstallMode, Platform, PreloadInfo, ProviderMessage,
};

use crate::adb_auth::{AdbAuthWaiters, AdbAuthority};
use crate::backend::{BackendError, DeviceBackend, DeviceInfo, NullProgress};
use crate::control::{now_millis, AdbAuthDecision, CommandHandler, ControlSender};
use crate::preload::{PreloadStore, ProtectedPreload};
use crate::session::{Authorization, SessionRegistry};

/// How often each device's info is re-read and pushed up if it changed.
const POLL_INTERVAL: Duration = Duration::from_secs(15);

/// At most one activity report per device per this long.
///
/// A drag is hundreds of pointer events a second and every one of them is
/// activity; the coordinator only needs to know the reservation is not idle,
/// so this is deliberately coarse relative to any idle timeout worth setting.
const ACTIVITY_INTERVAL: Duration = Duration::from_secs(30);

fn protocol_platform(value: &str) -> Option<Platform> {
    match value {
        "android" => Some(Platform::Android),
        "ios" => Some(Platform::Ios),
        _ => None,
    }
}

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
    /// True while we believe we have parked this device's screen.
    ///
    /// Belief, not fact: it is cleared whenever anything else could have
    /// touched the display — a cleanup run presses Home, a reboot comes back
    /// lit — so the next poll re-parks rather than trusting a stale read.
    screen_blanked: AtomicBool,
    /// Serializes installs, session authorization and cleanup on one device.
    /// A preload must not begin while a user session is being authorized, and a
    /// cleanup must not run over an install that just acquired an idle grant.
    operation: AsyncMutex<()>,
}

impl Device {
    pub async fn lock_operation(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.operation.lock().await
    }

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
        let adb_port = self.backend.remote_debug_port().await;
        snapshot_from(&self.id, status, info.as_ref(), codec, adb_port)
    }
}

fn snapshot_from(
    id: &str,
    status: DeviceStatus,
    info: Option<&DeviceInfo>,
    stream_codec: Option<String>,
    adb_port: Option<u16>,
) -> DeviceSnapshot {
    let adb_port = adb_port.map(i64::from);

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
            adb_port,
            note: None,
        },
        // Reported before the backend has managed to read the device — better
        // than omitting it, which the coordinator would reconcile as absent.
        None => DeviceSnapshot {
            id: id.to_owned(),
            platform: yard_protocol::Platform::Ios,
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
            adb_port,
            note: None,
        },
    }
}

pub struct Supervisor {
    devices: HashMap<String, Arc<Device>>,
    sessions: SessionRegistry,
    control: RwLock<Option<ControlSender>>,
    preloads: PreloadStore,
    /// `adb connect` attempts parked on an answer from the coordinator.
    adb_auth: AdbAuthWaiters,
    /// Whether idle devices have their screens parked. See [`Supervisor::reconcile_screen`].
    blank_idle_screens: bool,
}

impl Supervisor {
    pub fn new(sessions: SessionRegistry) -> Self {
        Self::with_preload_store(sessions, PreloadStore::in_memory())
    }

    pub fn with_preload_store(sessions: SessionRegistry, preloads: PreloadStore) -> Self {
        Self {
            devices: HashMap::new(),
            sessions,
            control: RwLock::new(None),
            preloads,
            adb_auth: AdbAuthWaiters::new(),
            blank_idle_screens: true,
        }
    }

    /// Turns idle-screen parking off, for a wall of devices meant to stay lit.
    pub fn set_blank_idle_screens(&mut self, on: bool) {
        self.blank_idle_screens = on;
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
                screen_blanked: AtomicBool::new(false),
                operation: AsyncMutex::new(()),
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

    /// Returns whether a preload may start on this device right now.
    ///
    /// The coordinator checks the same state before minting a grant, but the
    /// provider must repeat it because a reservation can win the race after the
    /// browser receives its grant.
    pub async fn can_preload(&self, device_id: &str) -> bool {
        let Some(device) = self.devices.get(device_id) else {
            return false;
        };
        if self.sessions.current(device_id).await.is_some() {
            return false;
        }
        matches!(
            device.status().await,
            DeviceStatus::Ready | DeviceStatus::Present
        )
    }

    /// Identifies and records a package as a protected preload. The caller
    /// holds the device operation lock so the desired state and install cannot
    /// race another install, cleanup, or farm-issued uninstall.
    #[allow(
        clippy::too_many_arguments,
        reason = "arguments describe one uploaded package"
    )]
    pub async fn protect_preload(
        &self,
        device_id: &str,
        platform: &str,
        user_id: &str,
        filename: &str,
        size: i64,
        sha256: &str,
        staged: &std::path::Path,
    ) -> Result<ProtectedPreload> {
        let device = self.require(device_id)?;
        let app_id = device.backend.artifact_app_id(staged).await?;
        self.preloads
            .protect(
                device_id, &app_id, platform, user_id, filename, size, sha256, staged,
            )
            .await
    }

    /// Prevents the farm's own uninstall command from deleting a protected
    /// preload. A user action on the phone can remove it during a session; the
    /// cleanup run below repairs that case before the device becomes ready.
    pub async fn is_preload_protected(&self, device_id: &str, app_id: &str) -> bool {
        self.preloads.is_protected(device_id, app_id).await
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

    /// Withdraws a device's `adb connect` bridge the moment its session ends.
    ///
    /// The exposure is the reservation's, not the device's: a listener that
    /// outlived the session would hold an authenticated client that was
    /// checked at its handshake and is never checked again. Driven off the
    /// same revocation broadcast that drops live viewers, so every way a
    /// session can end — released, force-released, swept, replaced by the next
    /// reservation, or the control plane going away — closes adb too, without
    /// each of them having to remember to.
    ///
    /// Not an `async fn`: the subscription is taken when this is *called*, so
    /// a revocation between here and the task being scheduled is still seen.
    pub fn run_revocation_loop(self: Arc<Self>) -> impl std::future::Future<Output = ()> {
        let mut revocations = self.sessions.subscribe_revocations();
        async move {
            loop {
                match revocations.recv().await {
                    Ok(revocation) => self.withdraw_adb(&revocation.device_id).await,
                    // Lagging means revocations were missed, and a missed one is
                    // exactly the case that must not leave a bridge up. Withdraw
                    // every device: an exposure is cheap to ask for again and this
                    // cannot happen to a provider under any normal load.
                    Err(broadcast::error::RecvError::Lagged(dropped)) => {
                        warn!(dropped, "missed revocations; withdrawing every adb bridge");
                        for device in self.devices.values() {
                            self.withdraw_adb(&device.id).await;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        }
    }

    /// Best-effort teardown: a device with nothing exposed, or a backend with
    /// no notion of one, is the ordinary case rather than a failure.
    async fn withdraw_adb(&self, device_id: &str) {
        let Some(device) = self.devices.get(device_id) else {
            return;
        };
        match device.backend.remote_debug_stop().await {
            Ok(()) | Err(BackendError::Unsupported(_)) => {}
            Err(err) => warn!(device = %device_id, %err, "could not withdraw the adb bridge"),
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

        self.reconcile_screen(device, next_status).await;
    }

    /// Parks the screen of a device nobody is using, and only that device.
    ///
    /// Driven from the poll loop rather than hung off `session.revoke`,
    /// because the end of a session is not the only way a device goes idle: a
    /// provider restart, a device coming back healthy, and the cleanup run
    /// that follows a release — whose `reset_screen` presses Home and lights
    /// the panel straight back up — all land here too. One rule that reads the
    /// current state cannot leave a device lit the way four hooks that each
    /// have to remember to would.
    ///
    /// The waking half is not here: it belongs to `session.authorize`, which
    /// must take effect before the holder's first frame rather than up to a
    /// poll interval later.
    async fn reconcile_screen(&self, device: &Arc<Device>, status: DeviceStatus) {
        if !self.blank_idle_screens {
            return;
        }

        // `ready` and no session is the whole definition of idle. A reserved
        // device is `ready` too — `busy` is the coordinator's word — so the
        // session registry is what separates them.
        let idle =
            status == DeviceStatus::Ready && self.sessions.current(&device.id).await.is_none();
        if !idle {
            // Anything that is not a plain reserved device — cleaning,
            // rebooting, unhealthy, absent — may have lit the screen behind
            // our back, so stop claiming we parked it.
            if status != DeviceStatus::Ready {
                device.screen_blanked.store(false, Ordering::Relaxed);
            }
            return;
        }

        if device.screen_blanked.load(Ordering::Relaxed) {
            return;
        }

        match device.backend.set_screen_awake(false).await {
            Ok(()) => {
                device.screen_blanked.store(true, Ordering::Relaxed);
                info!(device = %device.id, "parked an idle screen");
            }
            // Latched rather than retried every fifteen seconds: a backend
            // that cannot reach the display will not learn to.
            Err(BackendError::Unsupported(_)) => {
                device.screen_blanked.store(true, Ordering::Relaxed);
            }
            Err(err) => warn!(device = %device.id, %err, "could not park the screen"),
        }
    }

    /// Brings a device back for the user who just reserved it.
    ///
    /// Gated on having parked it ourselves, which is also what makes a renew
    /// safe: renewing re-authorizes the same reservation, and waking on every
    /// authorize would yank a working user back to their home screen every
    /// renewal interval.
    async fn wake_screen(&self, device: &Arc<Device>) {
        // Claimed rather than read, so two authorizes landing together press
        // the side button once between them; pressing it twice would park the
        // screen again.
        if device
            .screen_blanked
            .compare_exchange(true, false, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
        {
            return;
        }
        match device.backend.set_screen_awake(true).await {
            Ok(()) | Err(BackendError::Unsupported(_)) => {}
            // Give the belief back. Dropping it on a failed press — the HID
            // surfaces were rebuilding, say — loses the device: nothing would
            // ever try to light that screen again, and the holder reserves a
            // black phone. Seen in the field on 2026-08-28.
            Err(err) => {
                device.screen_blanked.store(true, Ordering::Relaxed);
                warn!(device = %device.id, %err, "could not wake the screen");
            }
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
    /// The coordinator turns this into an audit row. Session uploads are
    /// deleted after install; protected preload artifacts remain provider-local
    /// and are referenced by the manifest rather than sent through the wire.
    #[allow(
        clippy::too_many_arguments,
        reason = "arguments mirror the install audit event"
    )]
    pub async fn push_install_result(
        &self,
        device_id: &str,
        user_id: &str,
        filename: &str,
        size: i64,
        sha256: &str,
        error: Option<String>,
        mode: Option<&str>,
        app_id: Option<&str>,
        platform: Option<&str>,
    ) {
        let mode = match mode {
            Some("preload") => Some(InstallMode::Preload),
            Some("preload.repair") => Some(InstallMode::PreloadRepair),
            Some(other) => {
                warn!(
                    mode = other,
                    "unknown install mode omitted from protocol event"
                );
                None
            }
            None => None,
        };
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
            mode,
            app_id: app_id.map(str::to_owned),
            platform: platform.and_then(protocol_platform),
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

    async fn preload_inventory(&self) -> Vec<PreloadInfo> {
        self.preloads
            .all()
            .await
            .into_iter()
            .filter_map(|entry| {
                let platform = protocol_platform(&entry.platform).or_else(|| {
                    warn!(
                        device = %entry.device_id,
                        app = %entry.app_id,
                        platform = %entry.platform,
                        "ignoring protected preload with an unsupported platform"
                    );
                    None
                })?;
                Some(PreloadInfo {
                    device_id: entry.device_id,
                    app_id: entry.app_id,
                    platform,
                    filename: entry.filename,
                    size: entry.size,
                    sha256: entry.sha256,
                })
            })
            .collect()
    }

    /// The coordinator is the source of truth for authorization, so anything we
    /// were told before the socket dropped may be stale by the time it returns.
    /// It re-pushes on reconnect.
    async fn on_disconnected(&self) {
        self.sessions.revoke_all("control plane disconnected").await;
        self.adb_auth.abandon_all().await;
    }

    async fn on_adb_auth_decision(&self, decision: AdbAuthDecision) {
        self.adb_auth.resolve(decision).await;
    }

    async fn handle(&self, payload: CommandPayload) -> Result<Option<CommandData>> {
        match payload {
            CommandPayload::SessionAuthorize {
                device_id,
                reservation_id,
                user_id,
                adb_keys,
            } => {
                let device = self.require(&device_id)?;
                let _operation = device.operation.lock().await;
                self.snapshot_baseline(&device, &reservation_id).await;
                self.wake_screen(&device).await;
                self.sessions
                    .authorize(
                        &device_id,
                        Authorization {
                            reservation_id,
                            user_id,
                            adb_keys,
                        },
                    )
                    .await;
                Ok(None)
            }

            CommandPayload::DeviceAdbKeys { device_id, keys } => {
                self.sessions.set_adb_keys(&device_id, keys).await;
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
                    self.preloads.clone(),
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
                let device = self.require(&device_id)?;
                let _operation = device.lock_operation().await;
                if self.preloads.is_protected(&device_id, &app_id).await {
                    return Err(anyhow!(
                        "cannot uninstall protected preload {app_id}; remove its preload policy first"
                    ));
                }
                device.backend.uninstall(&app_id).await?;
                Ok(None)
            }

            CommandPayload::DevicePreloadRemove { device_id, app_id } => {
                let device = self.require(&device_id)?;
                let _operation = device.lock_operation().await;
                if !self.can_preload(&device_id).await {
                    return Err(anyhow!("device is busy or unavailable for preload removal"));
                }
                if !self.preloads.is_protected(&device_id, &app_id).await {
                    return Err(anyhow!(
                        "{app_id} is not a protected preload on this device"
                    ));
                }

                let installed = device
                    .backend
                    .apps()
                    .await?
                    .into_iter()
                    .any(|app| app.id == app_id);
                if installed {
                    device.backend.uninstall(&app_id).await?;
                }
                self.preloads
                    .remove(&device_id, &app_id)
                    .await?
                    .ok_or_else(|| anyhow!("{app_id} is not a protected preload on this device"))?;
                Ok(None)
            }

            CommandPayload::DeviceAdbExpose { device_id } => {
                let authority = Arc::new(AdbAuthority::new(
                    device_id.clone(),
                    self.sessions.clone(),
                    self.adb_auth.clone(),
                    self.control
                        .read()
                        .await
                        .clone()
                        .ok_or_else(|| anyhow!("not connected to the coordinator"))?,
                ));
                let debug = self
                    .require(&device_id)?
                    .backend
                    .remote_debug(authority)
                    .await?;
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

/// Checks provider-owned preloads at the end of a session cleanup and restores
/// any app the user removed. The caller holds the device operation lock, so a
/// session install, cleanup action, or farm-issued uninstall cannot race this
/// check.
async fn repair_preloads(
    device: &Device,
    preloads: &PreloadStore,
    sender: Option<&ControlSender>,
    report: &mut crate::cleanup::CleanupReport,
) -> bool {
    let entries = preloads.for_device(&device.id).await;
    if entries.is_empty() {
        return true;
    }

    let current = match device.backend.apps().await {
        Ok(apps) => apps,
        Err(err) => {
            report
                .errors
                .push(format!("preload repair: listing apps: {err}"));
            warn!(device = %device.id, %err, "could not inspect protected preloads during cleanup");
            return false;
        }
    };
    let mut installed: HashSet<String> = current.into_iter().map(|app| app.id).collect();
    let mut all_ok = true;

    for entry in entries {
        if installed.contains(&entry.app_id) {
            continue;
        }

        let Some(artifact) = preloads.artifact_path(&entry) else {
            let error = format!("preload repair {}: no safe artifact path", entry.app_id);
            report.errors.push(error.clone());
            warn!(device = %device.id, app = %entry.app_id, "protected preload has no safe artifact path");
            all_ok = false;
            continue;
        };
        if tokio::fs::metadata(&artifact).await.is_err() {
            let error = format!("preload repair {}: artifact is missing", entry.app_id);
            report.errors.push(error.clone());
            warn!(
                device = %device.id,
                app = %entry.app_id,
                path = %artifact.display(),
                "protected preload artifact is missing"
            );
            all_ok = false;
            continue;
        }

        info!(
            device = %device.id,
            app = %entry.app_id,
            "repairing removed protected preload during cleanup"
        );
        let outcome = device.backend.install(&artifact, &NullProgress).await;
        let error = outcome.as_ref().err().map(ToString::to_string);
        if error.is_none() {
            device.installs_ok.fetch_add(1, Ordering::Relaxed);
        } else {
            device.installs_failed.fetch_add(1, Ordering::Relaxed);
        }
        if let Some(sender) = sender {
            sender.send(ProviderMessage::InstallFinished {
                device_id: device.id.clone(),
                user_id: entry.user_id.clone(),
                filename: entry.filename.clone(),
                size: entry.size,
                sha256: entry.sha256.clone(),
                ok: error.is_none(),
                error: error.clone(),
                mode: Some(InstallMode::PreloadRepair),
                app_id: Some(entry.app_id.clone()),
                platform: protocol_platform(&entry.platform),
            });
        }

        match outcome {
            Ok(()) => {
                installed.insert(entry.app_id);
            }
            Err(err) => {
                report
                    .errors
                    .push(format!("preload repair {}: {err}", entry.app_id));
                warn!(
                    device = %device.id,
                    app = %entry.app_id,
                    %err,
                    "protected preload repair failed"
                );
                all_ok = false;
            }
        }
    }

    all_ok
}

/// Checks and resets a device after its reservation ended, then puts it back in
/// the pool. Protected preloads are repaired even when ordinary reset steps are
/// disabled.
///
/// A free function rather than a method because it outlives the command that
/// started it and must not borrow the supervisor for two minutes.
///
/// **The device always ends up out of `cleaning`.** Every early return, every
/// failed step and the deadline itself converge on the same landing at the
/// bottom, because a device stuck in `cleaning` is invisible inventory — worse
/// than the dirty device cleanup was meant to prevent. A failed protected
/// preload repair lands on `unhealthy` rather than returning an incomplete
/// device to the pool.
async fn run_cleanup(
    device: Arc<Device>,
    sender: Option<ControlSender>,
    steps: CleanupSteps,
    clear_filter: AppFilter,
    timeout_seconds: i64,
    preloads: PreloadStore,
) {
    let _operation = device.operation.lock().await;
    let started = Instant::now();
    let baseline = device.baseline.read().await;
    let apps = baseline.as_ref().map(|held| &held.apps);
    let protected = preloads
        .for_device(&device.id)
        .await
        .into_iter()
        .map(|entry| entry.app_id)
        .collect::<HashSet<_>>();

    let budget = Duration::from_secs(timeout_seconds.clamp(1, 3600) as u64);
    let mut report = match tokio::time::timeout(
        budget,
        crate::cleanup::run_with_protected(
            device.backend.as_ref(),
            &steps,
            &clear_filter,
            apps,
            &device.cleanup_paths,
            &protected,
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

    // Preload repair is deliberately part of cleanup. A user can remove a
    // protected app while working, and the farm puts it back before this
    // device becomes available to somebody else.
    let remaining = budget.saturating_sub(started.elapsed());
    let preloads_ok = if remaining.is_zero() {
        if preloads.for_device(&device.id).await.is_empty() {
            true
        } else {
            report.errors.push(format!(
                "preload repair exceeded {timeout_seconds}s and was abandoned"
            ));
            warn!(device = %device.id, seconds = timeout_seconds, "preload repair timed out");
            false
        }
    } else {
        match tokio::time::timeout(
            remaining,
            repair_preloads(&device, &preloads, sender.as_ref(), &mut report),
        )
        .await
        {
            Ok(ok) => ok,
            Err(_) => {
                report.errors.push(format!(
                    "preload repair exceeded {timeout_seconds}s and was abandoned"
                ));
                warn!(device = %device.id, seconds = timeout_seconds, "preload repair timed out");
                false
            }
        }
    };

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
    let status = if preloads_ok && device.backend.is_healthy().await {
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
