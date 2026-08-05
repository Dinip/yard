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
use farm_protocol::{AppInfo, Display, Platform};
use provider_core::backend::{
    BackendError, DeviceBackend, DeviceInfo, InputEvent, ProgressSink, RemoteDebug, Result,
};
use provider_core::video::{channel, AccessUnit, CodecDescription, VideoHandle, VideoPublisher};
use tokio::sync::Mutex;
use tracing::{debug, info};

/// Frame cadence. 10fps is enough to exercise fan-out and shedding without
/// making test logs unreadable.
const FRAME_INTERVAL: Duration = Duration::from_millis(100);
/// One keyframe every 30 frames, as a real long-GOP encoder would.
const GOP: u64 = 30;

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
}

#[derive(Default)]
pub struct MockState {
    pub installed: Mutex<Vec<AppInfo>>,
    pub clipboard: Mutex<Option<String>>,
    pub events: Mutex<Vec<InputEvent>>,
    pub rotation: AtomicI64,
    pub reboots: AtomicI64,
    pub healthy: AtomicBool,
    adb_port: AtomicU16,
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
        });

        let (width, height) = backend.panel_size();
        publisher.set_geometry(width, height, Some(0));

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
            display: Some(Display {
                width,
                height,
                scale: Some(3.0),
                rotation: Some(rotation),
            }),
            battery_level: Some(0.77),
            battery_state: Some("discharging".into()),
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
        // A real 1×1 PNG, so a browser that downloads it gets a valid image
        // rather than a broken one.
        const PNG: &[u8] = &[
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9c, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];
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

    async fn launch(&self, app_id: &str, _args: &[String]) -> Result<()> {
        let installed = self.state.installed.lock().await;
        if !installed.iter().any(|a| a.id == app_id) {
            return Err(BackendError::Failed(format!("{app_id} is not installed")));
        }
        Ok(())
    }

    async fn rotate(&self, degrees: i64) -> Result<()> {
        self.state
            .rotation
            .store(degrees.rem_euclid(360), Ordering::Relaxed);
        // A real encoder restarts at the new dimensions and the session server
        // pushes them to viewers; publishing here is what makes the whole
        // rotation path exercisable with no hardware.
        let (width, height) = self.stream_size();
        self.publisher
            .set_geometry(width, height, Some(self.state.rotation.load(Ordering::Relaxed)));
        Ok(())
    }

    async fn reboot(&self) -> Result<()> {
        self.state.reboots.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    async fn remote_debug(&self) -> Result<RemoteDebug> {
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

    async fn restart(&self) -> Result<()> {
        self.state.healthy.store(true, Ordering::Relaxed);
        Ok(())
    }

    async fn is_healthy(&self) -> bool {
        self.state.healthy.load(Ordering::Relaxed)
    }
}
