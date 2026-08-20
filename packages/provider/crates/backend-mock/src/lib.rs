//! A synthetic device.
//!
//! Same idea as the TypeScript fake provider one layer up: it lets the whole
//! provider — control plane, session plane, uploads, reservations — be run and
//! tested with nothing plugged in. It is also what proves the
//! [`DeviceBackend`] seam is the right shape *before* the iOS port is written
//! against it.
//!
//! The video it produces is not decodable, and deliberately so: synthesising a
//! real H.264 stream would mean shipping an encoder to serve a test fixture.
//! What it does reproduce faithfully is the *shape* of a stream — a codec
//! handshake, keyframes on request, a steady delta cadence — which is what the
//! session server and the fan-out logic actually depend on.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU16, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use farm_protocol::{AppInfo, Display, FileEntry, FileKind, FileListing, Platform};
use provider_core::adb_auth::AdbAuthority;
use provider_core::backend::{
    parent_of, AppFilter, AppMetrics, BackendError, CpuTimes, DeviceBackend, DeviceInfo,
    DeviceMetrics, InputEvent, MemoryBytes, ProgressSink, RemoteDebug, Result, ThermalZone,
};
use provider_core::video::{
    channel, AccessUnit, CodecDescription, VideoGeometry, VideoHandle, VideoPublisher,
};
use tokio::sync::Mutex;
use tokio::time::Instant;
use tracing::{debug, info};

/// Frame cadence. 10fps is enough to exercise fan-out and shedding without
/// making test logs unreadable.
const FRAME_INTERVAL: Duration = Duration::from_millis(100);
/// One keyframe every 30 frames, as a real long-GOP encoder would.
const GOP: u64 = 30;

/// A real 1×1 PNG, so a browser that downloads it gets a valid image rather
/// than a broken one. Serves as both the screenshot and the synthetic photo in
/// the file tree below.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];

/// Where the mock's file browser opens, mirroring Android's.
const FILES_ROOT: &str = "/sdcard";

/// Half a battery cycle: twenty minutes draining, then twenty charging.
const BATTERY_HALF_CYCLE: f64 = 20.0 * 60.0;
/// Period of the synthetic load curve that drives CPU, memory and temperature.
const LOAD_PERIOD: f64 = 5.0 * 60.0;
/// A four-core synthetic device, so `idle` outpaces `user` the way a real
/// `/proc/stat` does.
const CORES: f64 = 4.0;
const MEMORY_TOTAL: u64 = 8 * 1024 * 1024 * 1024;

/// The two processes the mock reports for per-app metrics.
///
/// The names are chosen so the `*.demo.*` pattern in `provider.example.yaml`
/// matches exactly one of them: that makes the app filter, and the difference
/// between a matched and an unmatched process, visible in a scrape with no
/// hardware attached.
const MOCK_APPS: &[(&str, u64)] = &[
    ("com.example.mock.app", 180 * 1024 * 1024),
    ("com.example.demo.player", 320 * 1024 * 1024),
];

/// A synthetic device filesystem: `(path, contents)`, `None` for a directory.
///
/// Small and static on purpose. It exists so the browse-and-download path —
/// routes, auth, the audit push, the dialog, the download — is exercisable with
/// nothing plugged in, which is what every other feature in this provider can
/// already claim.
const TREE: &[(&str, Option<&[u8]>)] = &[
    ("/sdcard/DCIM", None),
    ("/sdcard/DCIM/IMG_0001.png", Some(PNG)),
    ("/sdcard/Download", None),
    (
        "/sdcard/Download/notes.txt",
        Some(b"a synthetic file, from a synthetic device\n"),
    ),
];

pub struct MockBackend {
    id: String,
    platform: Platform,
    name: String,
    video: VideoHandle,
    /// Kept so a rotation can publish the new geometry, the same way a real
    /// backend does when its encoder restarts.
    publisher: VideoPublisher,
    /// Everything a test or a UI click can observe.
    pub state: Arc<MockState>,
    /// Origin for every synthetic metric. Deriving them from elapsed time rather
    /// than storing them means `info()` and `metrics()` cannot disagree about the
    /// battery — a UI showing 77% beside a Grafana panel showing 41% is a bug
    /// report waiting to happen.
    ///
    /// `tokio::time::Instant`, not the std one, so a test can fast-forward
    /// twenty minutes of battery drain instead of waiting for it.
    started: Instant,
}

#[derive(Default)]
pub struct MockState {
    pub installed: Mutex<Vec<AppInfo>>,
    pub clipboard: Mutex<Option<String>>,
    pub events: Mutex<Vec<InputEvent>>,
    pub rotation: AtomicI64,
    pub reboots: AtomicI64,
    pub healthy: AtomicBool,
    /// App ids whose data was cleared, in order — the only trace `pm clear`
    /// leaves on a real device, so the synthetic one records it explicitly.
    pub cleared: Mutex<Vec<String>>,
    pub wiped: Mutex<Vec<String>>,
    /// How `reset_screen` should misbehave, so a test can exercise cleanup's
    /// failure paths without a second backend implementation. Same idea as
    /// [`MockState::healthy`]: the synthetic device is where faults are
    /// injected, because that is the whole reason it exists.
    pub screen_fault: Mutex<Option<ScreenFault>>,
    /// Answer `clear_app_data` with `Unsupported`, the way iOS does.
    pub no_clear_app_data: AtomicBool,
    adb_port: AtomicU16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenFault {
    /// Returns an error, which cleanup records and carries on past.
    Fails,
    /// Never returns at all — a wedged adb call, which only the deadline ends.
    Hangs,
}

impl MockBackend {
    pub fn new(id: impl Into<String>, platform: Platform, name: impl Into<String>) -> Arc<Self> {
        let (video, publisher) = channel();
        let state = Arc::new(MockState {
            installed: Mutex::new(vec![
                AppInfo {
                    id: "com.example.app".into(),
                    name: Some("Example".into()),
                    version: Some("1.0.0".into()),
                    system: Some(false),
                },
                AppInfo {
                    id: "com.apple.Preferences".into(),
                    name: Some("Settings".into()),
                    version: None,
                    system: Some(true),
                },
            ]),
            healthy: AtomicBool::new(true),
            ..Default::default()
        });

        let backend = Arc::new(Self {
            id: id.into(),
            platform,
            name: name.into(),
            video,
            publisher: publisher.clone(),
            state,
            started: Instant::now(),
        });

        let (width, height) = backend.panel_size();
        publisher.set_geometry(VideoGeometry {
            width,
            height,
            rotation: Some(0),
            render_rotation: Some(0),
        });

        tokio::spawn(synthesize(publisher, platform));
        backend
    }

    pub fn video_handle(&self) -> VideoHandle {
        self.video.clone()
    }

    /// The write side, so a test can drive a mid-session codec change.
    pub fn publisher(&self) -> VideoPublisher {
        self.publisher.clone()
    }

    /// Unrotated panel dimensions.
    fn panel_size(&self) -> (i64, i64) {
        match self.platform {
            Platform::Ios => (1179, 2556),
            Platform::Android => (1080, 2400),
        }
    }

    /// Battery as `(level 0..1, charging)`, on a 40-minute saw: it drains from
    /// full to 20% over twenty minutes, then charges back.
    ///
    /// Shared by `info()` and `metrics()` so the control plane and the exporter
    /// never disagree.
    fn battery(&self) -> (f64, bool) {
        let phase = self.started.elapsed().as_secs_f64() % (2.0 * BATTERY_HALF_CYCLE);
        if phase < BATTERY_HALF_CYCLE {
            let t = phase / BATTERY_HALF_CYCLE;
            (1.0 - 0.8 * t, false)
        } else {
            let t = (phase - BATTERY_HALF_CYCLE) / BATTERY_HALF_CYCLE;
            (0.2 + 0.8 * t, true)
        }
    }

    /// How busy the synthetic CPU is right now, 0..1, on a five-minute sine.
    ///
    /// One curve drives CPU, memory and temperature together, so the mock shows
    /// the correlation an operator would actually be looking for rather than
    /// three unrelated wobbles.
    fn load(&self) -> f64 {
        let t = self.started.elapsed().as_secs_f64() / LOAD_PERIOD;
        0.15 + 0.10 * (t * std::f64::consts::TAU).sin()
    }

    /// Cumulative busy core-seconds: the *integral* of [`Self::load`], not a
    /// sample of it.
    ///
    /// The exporter emits these as counters, and `rate()` reads a value that
    /// dips as a counter reset — a whole scrape interval of spurious spike. So
    /// the wobble has to live in the derivative, which is what integrating
    /// gives: ∫(a + b·sin(2πt/P)) dt, whose integrand never reaches zero.
    fn busy_core_seconds(&self) -> f64 {
        let elapsed = self.started.elapsed().as_secs_f64();
        let turns = elapsed / LOAD_PERIOD * std::f64::consts::TAU;
        let integral =
            0.15 * elapsed - 0.10 * LOAD_PERIOD / std::f64::consts::TAU * (turns.cos() - 1.0);
        integral * CORES
    }

    /// The dimensions a viewer sees at the current rotation.
    fn stream_size(&self) -> (i64, i64) {
        let (width, height) = self.panel_size();
        // A rotated device really does report swapped dimensions; the popout
        // window sizing depends on this being right.
        if self.state.rotation.load(Ordering::Relaxed) % 180 == 0 {
            (width, height)
        } else {
            (height, width)
        }
    }
}

/// Produces a stream-shaped sequence of access units.
///
/// Emits a keyframe on request and at each GOP boundary, deltas in between.
/// The bytes are filler; the cadence and the key/delta pattern are the parts
/// the session server depends on.
async fn synthesize(publisher: VideoPublisher, platform: Platform) {
    let codec = match platform {
        Platform::Ios => CodecDescription {
            codec: "hev1.1.6.L93.B0".into(),
            // A plausible-looking hvcC; never fed to a real decoder.
            description: vec![0x01, 0x01, 0x60, 0x00, 0x00, 0x00, 0x90, 0x00],
        },
        Platform::Android => CodecDescription {
            codec: "avc1.640028".into(),
            description: vec![0x01, 0x64, 0x00, 0x28, 0xff, 0xe1, 0x00, 0x09],
        },
    };
    publisher.set_codec(codec);

    let mut counter: u64 = 0;
    let mut ticker = tokio::time::interval(FRAME_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        let forced_key = tokio::select! {
            _ = ticker.tick() => false,
            _ = publisher.keyframe_requested() => true,
        };

        let is_key = forced_key || counter % GOP == 0;
        // 4-byte length prefix then filler, matching hvcC/avcC sample format so
        // anything that walks the NALU structure sees something well-formed.
        let payload_len = if is_key { 512usize } else { 96 };
        let mut data = Vec::with_capacity(payload_len + 4);
        data.extend_from_slice(&(payload_len as u32).to_be_bytes());
        data.extend(std::iter::repeat_n(counter as u8, payload_len));

        publisher.publish(AccessUnit { data, is_key });
        counter = counter.wrapping_add(1);
    }
}

#[async_trait]
impl DeviceBackend for MockBackend {
    async fn info(&self) -> Result<DeviceInfo> {
        if !self.state.healthy.load(Ordering::Relaxed) {
            return Err(BackendError::Unavailable("mock marked unhealthy".into()));
        }

        let rotation = self.state.rotation.load(Ordering::Relaxed);
        let (width, height) = self.stream_size();
        let (level, charging) = self.battery();

        Ok(DeviceInfo {
            id: self.id.clone(),
            platform: self.platform,
            name: Some(self.name.clone()),
            model: Some(match self.platform {
                Platform::Ios => "iPhone16,1".into(),
                Platform::Android => "Pixel 8".into(),
            }),
            manufacturer: Some(match self.platform {
                Platform::Ios => "Apple".into(),
                Platform::Android => "Google".into(),
            }),
            os_version: Some(match self.platform {
                Platform::Ios => "17.4.1".into(),
                Platform::Android => "14".into(),
            }),
            abi: Some(match self.platform {
                Platform::Ios => "arm64e".into(),
                Platform::Android => "arm64-v8a".into(),
            }),
            sdk: matches!(self.platform, Platform::Android).then_some(34),
            // Android-shaped identity, so the device page can be exercised
            // without hardware. iOS reports none of it, which is also what the
            // real backend does.
            serial: Some(self.id.clone()),
            brand: matches!(self.platform, Platform::Android).then(|| "google".into()),
            build_id: matches!(self.platform, Platform::Android).then(|| "UD1A.230803.041".into()),
            security_patch: matches!(self.platform, Platform::Android).then(|| "2026-05-01".into()),
            abi_list: matches!(self.platform, Platform::Android)
                .then(|| "arm64-v8a,armeabi-v7a,armeabi".into()),
            display: Some(Display {
                width,
                height,
                scale: Some(3.0),
                rotation: Some(rotation),
                render_rotation: Some(0),
            }),
            battery_level: Some(level),
            battery_state: Some(if charging { "charging" } else { "discharging" }.into()),
        })
    }

    async fn metrics(&self, apps: &AppFilter) -> Result<DeviceMetrics> {
        if !self.state.healthy.load(Ordering::Relaxed) {
            return Err(BackendError::Unavailable("mock marked unhealthy".into()));
        }

        let elapsed = self.started.elapsed().as_secs_f64();
        let busy = self.busy_core_seconds();
        let (battery_level, charging) = self.battery();
        let load = self.load();

        // Memory and temperature ride the same curve as the CPU, so the mock
        // shows a correlation rather than three unrelated wobbles.
        let available = (5.0 - 3.0 * (load - 0.05) / 0.20) * 1024.0 * 1024.0 * 1024.0;
        let available = (available as u64).min(MEMORY_TOTAL);

        // A real iOS device reports battery and nothing else; the mock mirrors
        // that so the "no CPU series for iOS" behaviour is testable.
        let (cpu, memory, thermal_zones) = match self.platform {
            Platform::Ios => (None, None, Vec::new()),
            Platform::Android => (
                Some(CpuTimes {
                    // 5:1 user:system, and idle is whatever the four cores did
                    // not spend — the same arithmetic /proc/stat implies.
                    user: busy * 5.0 / 6.0,
                    system: busy / 6.0,
                    idle: CORES * elapsed - busy,
                    ..Default::default()
                }),
                Some(MemoryBytes {
                    total: MEMORY_TOTAL,
                    available: Some(available),
                    free: Some(available / 2),
                }),
                vec![ThermalZone {
                    name: "mock-cpu".into(),
                    celsius: 34.0 + 48.0 * (load - 0.05),
                }],
            ),
        };

        let apps = match self.platform {
            Platform::Ios => Vec::new(),
            Platform::Android => MOCK_APPS
                .iter()
                .filter(|(process, _)| apps.matches(process))
                .map(|(process, pss)| AppMetrics {
                    process: (*process).into(),
                    cpu_seconds: Some(busy / 8.0),
                    pss_bytes: Some(*pss),
                })
                .collect(),
        };

        Ok(DeviceMetrics {
            cpu,
            memory,
            battery_level: Some(battery_level),
            battery_charging: Some(charging),
            battery_temperature_c: Some(30.0 + 40.0 * (load - 0.05)),
            thermal_zones,
            apps,
        })
    }

    fn video(&self) -> VideoHandle {
        self.video.clone()
    }

    async fn input(&self, event: InputEvent) -> Result<()> {
        debug!(device = %self.id, ?event, "mock input");
        self.state.events.lock().await.push(event);
        Ok(())
    }

    async fn screenshot(&self) -> Result<Vec<u8>> {
        Ok(PNG.to_vec())
    }

    async fn clipboard_get(&self) -> Result<Option<String>> {
        Ok(self.state.clipboard.lock().await.clone())
    }

    async fn clipboard_set(&self, text: &str) -> Result<()> {
        *self.state.clipboard.lock().await = Some(text.to_owned());
        Ok(())
    }

    async fn apps(&self) -> Result<Vec<AppInfo>> {
        Ok(self.state.installed.lock().await.clone())
    }

    async fn install(&self, staged: &Path, progress: &dyn ProgressSink) -> Result<()> {
        let size = tokio::fs::metadata(staged)
            .await
            .map_err(|e| BackendError::Failed(format!("staged file unreadable: {e}")))?
            .len();

        progress.report("uploading", Some(1.0));
        progress.report("installing", None);
        tokio::time::sleep(Duration::from_millis(50)).await;

        let id = staged
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.split('-').next_back())
            .unwrap_or("com.example.installed")
            .to_owned();

        self.state.installed.lock().await.push(AppInfo {
            id: format!("mock.{id}"),
            name: Some(id),
            version: Some("1.0.0".into()),
            system: Some(false),
        });

        info!(device = %self.id, size, "mock install complete");
        progress.report("done", Some(1.0));
        Ok(())
    }

    async fn uninstall(&self, app_id: &str) -> Result<()> {
        let mut installed = self.state.installed.lock().await;
        let before = installed.len();
        installed.retain(|a| a.id != app_id);
        if installed.len() == before {
            return Err(BackendError::Failed(format!("{app_id} is not installed")));
        }
        Ok(())
    }

    async fn clear_app_data(&self, app_id: &str) -> Result<()> {
        if self.state.no_clear_app_data.load(Ordering::Relaxed) {
            return Err(BackendError::Unsupported("clearing app data"));
        }
        let installed = self.state.installed.lock().await;
        if !installed.iter().any(|a| a.id == app_id) {
            return Err(BackendError::Failed(format!("{app_id} is not installed")));
        }
        drop(installed);
        self.state.cleared.lock().await.push(app_id.to_owned());
        Ok(())
    }

    async fn reset_screen(&self) -> Result<()> {
        match *self.state.screen_fault.lock().await {
            Some(ScreenFault::Fails) => {
                return Err(BackendError::Failed("screen is wedged".into()))
            }
            Some(ScreenFault::Hangs) => std::future::pending::<()>().await,
            None => {}
        }
        self.rotate(0).await?;
        *self.state.clipboard.lock().await = None;
        Ok(())
    }

    async fn wipe_paths(&self, paths: &[String]) -> Result<()> {
        self.state.wiped.lock().await.extend_from_slice(paths);
        Ok(())
    }

    async fn launch(&self, app_id: &str, _args: &[String]) -> Result<()> {
        let installed = self.state.installed.lock().await;
        if !installed.iter().any(|a| a.id == app_id) {
            return Err(BackendError::Failed(format!("{app_id} is not installed")));
        }
        Ok(())
    }

    fn files_root(&self) -> Option<&'static str> {
        Some(FILES_ROOT)
    }

    async fn list_files(&self, path: &str) -> Result<FileListing> {
        let path = path.trim_end_matches('/');
        let path = if path.is_empty() { "/" } else { path };

        if path != FILES_ROOT && !TREE.iter().any(|(p, body)| *p == path && body.is_none()) {
            return Err(BackendError::Failed(format!("{path}: no such directory")));
        }

        let entries = TREE
            .iter()
            .filter(|(p, _)| parent_of(p) == Some(path))
            .map(|(p, body)| FileEntry {
                name: p.rsplit('/').next().unwrap_or(p).to_owned(),
                path: (*p).to_owned(),
                kind: match body {
                    Some(_) => FileKind::File,
                    None => FileKind::Directory,
                },
                size: body.map(|b| b.len() as i64),
                modified_at: None,
            })
            .collect();

        Ok(FileListing {
            path: path.to_owned(),
            // Null at the root, so the browser hides "..". The mock does not
            // pretend to have anything above /sdcard.
            parent: (path != FILES_ROOT).then(|| parent_of(path).unwrap_or("/").to_owned()),
            entries,
        })
    }

    async fn pull_file(&self, path: &str, dest: &Path) -> Result<u64> {
        let body = TREE
            .iter()
            .find(|(p, body)| *p == path && body.is_some())
            .and_then(|(_, body)| *body)
            .ok_or_else(|| BackendError::Failed(format!("{path}: no such file")))?;

        tokio::fs::write(dest, body)
            .await
            .map_err(|e| BackendError::Failed(format!("staging {path}: {e}")))?;
        Ok(body.len() as u64)
    }

    async fn rotate(&self, degrees: i64) -> Result<()> {
        self.state
            .rotation
            .store(degrees.rem_euclid(360), Ordering::Relaxed);
        // A real encoder restarts at the new dimensions and the session server
        // pushes them to viewers; publishing here is what makes the whole
        // rotation path exercisable with no hardware.
        let (width, height) = self.stream_size();
        self.publisher.set_geometry(VideoGeometry {
            width,
            height,
            rotation: Some(self.state.rotation.load(Ordering::Relaxed)),
            // The mock streams like Android does: the picture it produces is
            // already the right way up, so a viewer rotates nothing.
            render_rotation: Some(0),
        });
        Ok(())
    }

    async fn reboot(&self) -> Result<()> {
        self.state.reboots.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// The authority is ignored: there is no ADB protocol here to authenticate,
    /// and a mock device has nothing to get a shell on.
    async fn remote_debug(&self, _authority: Arc<AdbAuthority>) -> Result<RemoteDebug> {
        if self.platform != Platform::Android {
            return Err(BackendError::Unsupported("remote debugging"));
        }
        // Deterministic, so a test can assert on it.
        let port = 15037;
        self.state.adb_port.store(port, Ordering::Relaxed);
        Ok(RemoteDebug { port })
    }

    async fn remote_debug_stop(&self) -> Result<()> {
        self.state.adb_port.store(0, Ordering::Relaxed);
        Ok(())
    }

    async fn remote_debug_port(&self) -> Option<u16> {
        match self.state.adb_port.load(Ordering::Relaxed) {
            0 => None,
            port => Some(port),
        }
    }

    async fn restart(&self) -> Result<()> {
        self.state.healthy.store(true, Ordering::Relaxed);
        Ok(())
    }

    async fn is_healthy(&self) -> bool {
        self.state.healthy.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn android() -> Arc<MockBackend> {
        MockBackend::new("mock-android-1", Platform::Android, "Mock Pixel")
    }

    /// The exporter emits CPU as a counter, and a counter that dips reads as a
    /// reset — a whole scrape interval of spurious `rate()` spike. This is the
    /// property that makes the mock usable for eyeballing a dashboard.
    #[tokio::test(start_paused = true)]
    async fn synthetic_cpu_counters_only_ever_advance() {
        let backend = android();
        let filter = AppFilter::default();
        let mut previous = CpuTimes::default();

        // A whole load period, sampled far more finely than the sine wobbles.
        for _ in 0..60 {
            tokio::time::advance(Duration::from_secs(5)).await;
            let cpu = backend.metrics(&filter).await.unwrap().cpu.unwrap();

            assert!(
                cpu.user >= previous.user,
                "{} < {}",
                cpu.user,
                previous.user
            );
            assert!(cpu.system >= previous.system);
            assert!(cpu.idle >= previous.idle);
            previous = cpu;
        }

        assert!(previous.user > 0.0, "the curve never advanced at all");
    }

    /// `info()` and `metrics()` must read the same battery: a UI showing one
    /// number beside a Grafana panel showing another is a bug report waiting to
    /// happen.
    #[tokio::test(start_paused = true)]
    async fn the_control_plane_and_the_exporter_agree_on_the_battery() {
        let backend = android();
        tokio::time::advance(Duration::from_secs(11 * 60)).await;

        let info = backend.info().await.unwrap();
        let metrics = backend.metrics(&AppFilter::default()).await.unwrap();

        assert_eq!(info.battery_level, metrics.battery_level);
        assert_eq!(
            info.battery_state.as_deref() == Some("charging"),
            metrics.battery_charging.unwrap()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_battery_drains_then_charges() {
        let backend = android();

        tokio::time::advance(Duration::from_secs(10 * 60)).await;
        let draining = backend.metrics(&AppFilter::default()).await.unwrap();
        assert!(!draining.battery_charging.unwrap());
        assert!((0.55..0.65).contains(&draining.battery_level.unwrap()));

        tokio::time::advance(Duration::from_secs(20 * 60)).await;
        let charging = backend.metrics(&AppFilter::default()).await.unwrap();
        assert!(charging.battery_charging.unwrap());
    }

    /// One of the two mock apps matches the documented `*.demo.*` pattern and
    /// the other does not, so a scrape shows the filter working.
    #[tokio::test]
    async fn only_matching_processes_are_reported() {
        let backend = android();

        let none = backend.metrics(&AppFilter::default()).await.unwrap();
        assert!(none.apps.is_empty());

        let filtered = backend
            .metrics(&AppFilter::new(&["*.demo.*".to_owned()]))
            .await
            .unwrap();
        let names: Vec<_> = filtered.apps.iter().map(|a| a.process.as_str()).collect();
        assert_eq!(names, vec!["com.example.demo.player"]);
    }

    /// Mirrors the real iOS backend, whose diagnostics relay offers battery and
    /// no CPU or memory at all.
    #[tokio::test]
    async fn the_ios_mock_reports_battery_but_no_cpu_or_memory() {
        let backend = MockBackend::new("mock-ios-1", Platform::Ios, "Mock iPhone");
        let metrics = backend
            .metrics(&AppFilter::new(&["*.demo.*".to_owned()]))
            .await
            .unwrap();

        assert!(metrics.battery_level.is_some());
        assert!(metrics.cpu.is_none());
        assert!(metrics.memory.is_none());
        assert!(metrics.apps.is_empty());
    }

    #[tokio::test]
    async fn an_unhealthy_mock_fails_rather_than_inventing_numbers() {
        let backend = android();
        backend.state.healthy.store(false, Ordering::Relaxed);

        assert!(backend.metrics(&AppFilter::default()).await.is_err());
    }
}
