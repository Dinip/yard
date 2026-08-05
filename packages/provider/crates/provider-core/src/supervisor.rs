//! Owns every device on the host and routes commands to them.
//!
//! Replaces `stf-ios-provider/src/provider.rs`. Upstream ran one *container*
//! per device; this is one process supervising all of them. Crash isolation is
//! kept — each device is an independently restartable task tree — while N
//! containers, N ZMQ connections and N config files collapse into one.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use farm_protocol::{
    Battery, CommandData, CommandPayload, DeviceSnapshot, DeviceStatus, ProviderMessage,
};
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::backend::{DeviceBackend, DeviceInfo};
use crate::control::{CommandHandler, ControlSender};
use crate::session::{Authorization, SessionRegistry};

/// How often each device's info is re-read and pushed up if it changed.
const POLL_INTERVAL: Duration = Duration::from_secs(15);

pub struct Device {
    pub id: String,
    pub backend: Arc<dyn DeviceBackend>,
    status: RwLock<DeviceStatus>,
    info: RwLock<Option<DeviceInfo>>,
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
        self.devices.insert(
            id.clone(),
            Arc::new(Device {
                id,
                backend,
                status: RwLock::new(DeviceStatus::Preparing),
                info: RwLock::new(None),
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
                self.require(&device_id)?;
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
