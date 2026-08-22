//! Device metrics, sampled in the background and exposed for Prometheus.
//!
//! ```text
//!   run_sampler ── DeviceBackend::metrics ──▶ MetricsCache
//!                                                  │
//!   prometheus ─── GET /metrics ── encode ─────────┘
//! ```
//!
//! **A listener of its own, not a route on the session plane.** That port is
//! browser-facing, carries a CORS layer and session tokens, and is publicly
//! TLS-terminated; a scraper has none of those. Adding `/metrics` there "for
//! convenience" would inherit the CORS layer, which is the opposite of what this
//! wants.
//!
//! **The scrape reads a cache, never a device.** Sampling on scrape would put N
//! adb round trips on the request path and let a second scraper double the load
//! on the phones being tested.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{extract::State, Router};
use futures::StreamExt as _;
use tracing::{debug, info, warn};
use yard_protocol::DeviceStatus;

use crate::backend::{AppFilter, BackendError, DeviceMetrics};
use crate::config::Config;
use crate::supervisor::{Device, Supervisor};

/// The documented exposition format. axum's default `text/plain` works, but the
/// `version` parameter is the contract Prometheus actually publishes.
const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// How many devices are sampled at once.
///
/// The supervisor's own poll loop is sequential, but this one must not be: a
/// sample is several adb round trips plus a `dumpsys` that walks every process,
/// so a dozen devices in series would spend a third of the interval in one pass
/// and one wedged device would starve every other device's metrics.
const CONCURRENCY: usize = 8;

/// A sample older than this many intervals is not served at all.
const MAX_AGE_INTERVALS: u32 = 3;

/// Ceiling on how long one device may hold up a pass. A timeout is recorded as a
/// failed sample like any other.
const MAX_SAMPLE_TIMEOUT: Duration = Duration::from_secs(10);

/// At most this many processes per device per sample.
///
/// The real defence against label cardinality. Config validation refuses a bare
/// `*`, but a pattern like `com.*` is just as expensive and impossible to reject
/// on sight, so the cap is what keeps a scrape bounded.
const MAX_APPS_PER_DEVICE: usize = 32;

#[derive(Clone)]
pub struct MetricsState {
    pub config: Arc<Config>,
    pub supervisor: Arc<Supervisor>,
    pub cache: MetricsCache,
}

/// The newest sample for every device.
#[derive(Clone, Default)]
pub struct MetricsCache {
    inner: Arc<tokio::sync::RwLock<HashMap<String, DeviceSample>>>,
}

struct DeviceSample {
    at: Instant,
    /// Unix seconds of the last *successful* read, carried across failures so
    /// `time() - yard_device_metrics_sample_timestamp_seconds` stays a usable
    /// freshness signal rather than vanishing at the first error.
    succeeded_at: Option<f64>,
    /// A failure is stored rather than dropped: the error counter has to
    /// advance, and the previous good sample must not keep being served.
    result: Result<DeviceMetrics, String>,
    /// Failures counted since boot. A counter, so it survives the sample it
    /// belongs to being replaced.
    errors: u64,
}

impl MetricsCache {
    pub fn new() -> Self {
        Self::default()
    }

    async fn record(&self, device_id: &str, result: Result<DeviceMetrics, String>) {
        let now = Instant::now();
        let mut guard = self.inner.write().await;
        let previous = guard.get(device_id);
        let errors = previous.map(|p| p.errors).unwrap_or(0) + u64::from(result.is_err());
        let succeeded_at = if result.is_ok() {
            Some(unix_seconds())
        } else {
            previous.and_then(|p| p.succeeded_at)
        };

        guard.insert(
            device_id.to_owned(),
            DeviceSample {
                at: now,
                succeeded_at,
                result,
                errors,
            },
        );
    }

    /// The number of devices sampled so far, for tests.
    pub async fn len(&self) -> usize {
        self.inner.read().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.len().await == 0
    }
}

/// Samples every device forever, or logs once and parks when metrics are off.
///
/// Parking rather than not being spawned keeps `main` to a single code path:
/// every long-lived task is spawned, aborted on shutdown, and fatal if it
/// returns, exactly like the session plane.
pub async fn run_sampler(state: MetricsState) {
    if !state.config.metrics.enabled {
        info!("metrics disabled; not sampling devices");
        return std::future::pending().await;
    }

    let interval_duration = state.config.metrics.interval();
    let mut ticker = tokio::time::interval(interval_duration);
    // A slow pass must not be followed by a burst of catch-up ticks.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    info!(interval = ?interval_duration, "sampling device metrics");

    loop {
        ticker.tick().await;
        sample_once(&state).await;
    }
}

/// One pass over every device. Public so a test can drive it without a ticker.
pub async fn sample_once(state: &MetricsState) {
    let filter = AppFilter::new(&state.config.metrics.app_patterns);
    let timeout = state
        .config
        .metrics
        .interval()
        .checked_div(2)
        .unwrap_or(MAX_SAMPLE_TIMEOUT)
        .min(MAX_SAMPLE_TIMEOUT);

    let devices: Vec<Arc<Device>> = state.supervisor.devices().cloned().collect();

    futures::stream::iter(devices)
        .for_each_concurrent(CONCURRENCY, |device| {
            let cache = state.cache.clone();
            let filter = &filter;
            async move {
                let result = sample_device(&device, filter, timeout).await;
                if let Err(error) = &result {
                    warn!(device = %device.id, %error, "device metrics sample failed");
                }
                cache.record(&device.id, result).await;
            }
        })
        .await;
}

async fn sample_device(
    device: &Device,
    filter: &AppFilter,
    timeout: Duration,
) -> Result<DeviceMetrics, String> {
    match tokio::time::timeout(timeout, device.backend.metrics(filter)).await {
        Ok(Ok(mut metrics)) => {
            if metrics.apps.len() > MAX_APPS_PER_DEVICE {
                warn!(
                    device = %device.id,
                    matched = metrics.apps.len(),
                    cap = MAX_APPS_PER_DEVICE,
                    "app patterns matched more processes than the per-device cap; \
                     narrow them or the scrape grows a series per process"
                );
                metrics.apps.truncate(MAX_APPS_PER_DEVICE);
            }
            Ok(metrics)
        }
        // A backend that has no metrics at all is a *success* holding nothing —
        // every iOS device answers this for CPU and memory, and counting it as an
        // error would make a healthy farm look permanently broken.
        Ok(Err(BackendError::Unsupported(_))) => Ok(DeviceMetrics::default()),
        Ok(Err(err)) => Err(err.to_string()),
        Err(_) => Err(format!("timed out after {timeout:?}")),
    }
}

pub fn router(state: MetricsState) -> Router {
    // Deliberately no CORS layer and no auth layer, and this comment is here so
    // the next reader does not "fix" that by copying `server.rs`. This plane is
    // operator-facing: it is bound to a private interface by whoever runs the
    // provider, and it carries no user data. Allowing a browser origin here
    // would make device and app names readable from any allowed page.
    Router::new()
        .route("/metrics", get(scrape))
        .route("/health", get(|| async { StatusCode::OK }))
        .with_state(state)
}

pub async fn serve(state: MetricsState) -> Result<()> {
    if !state.config.metrics.enabled {
        info!("metrics disabled; not listening");
        return std::future::pending().await;
    }

    let addr: SocketAddr = state
        .config
        .metrics
        .bind
        .parse()
        .with_context(|| format!("parsing metrics bind {:?}", state.config.metrics.bind))?;

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;

    info!(%addr, "metrics listening");
    axum::serve(listener, router(state))
        .await
        .context("serving metrics")
}

async fn scrape(State(state): State<MetricsState>) -> Response {
    let body = encode(&state).await;
    (
        [(header::CONTENT_TYPE, HeaderValue::from_static(CONTENT_TYPE))],
        body,
    )
        .into_response()
}

/// Renders the whole exposition, reading in-process state live and device state
/// from the cache.
pub async fn encode(state: &MetricsState) -> String {
    let mut out = Encoder::default();
    let max_age = state.config.metrics.interval() * MAX_AGE_INTERVALS;
    let provider = &state.config.id;

    out.gauge(
        "yard_provider_build_info",
        "Provider version, always 1.",
        &[
            ("provider", provider),
            ("version", env!("CARGO_PKG_VERSION")),
        ],
        1.0,
    );
    out.gauge(
        "yard_provider_control_connected",
        "1 when the coordinator has acked this provider's hello.",
        &[("provider", provider)],
        f64::from(u8::from(state.supervisor.is_registered().await)),
    );

    let cache = state.cache.inner.read().await;
    let now = Instant::now();

    // Sorted so two scrapes of unchanged state produce byte-identical output,
    // which makes a diff of two scrapes mean something.
    let mut devices: Vec<&Arc<Device>> = state.supervisor.devices().collect();
    devices.sort_by(|a, b| a.id.cmp(&b.id));

    // Operational metrics first, and for *every* device: these are read live and
    // are never suppressed, which is what keeps "the device is gone" (status
    // present, no CPU series) distinguishable from "the exporter is broken"
    // (nothing at all).
    for device in &devices {
        let id: &str = &device.id;
        let video = device.backend.video();
        let (installs_ok, installs_failed) = device.install_counts();
        let session_active = state.supervisor.sessions().current(id).await.is_some();
        let status = effective_status(device.status().await, session_active);

        // One series per status with 1 on the current one, node_exporter's enum
        // idiom, so an alert is `== 1` rather than `absent()`.
        for candidate in ALL_STATUSES {
            out.gauge(
                "yard_device_status",
                "1 on the device's current status, 0 on the others.",
                &[("device", id), ("status", status_label(*candidate))],
                f64::from(u8::from(*candidate == status)),
            );
        }

        out.gauge(
            "yard_device_session_active",
            "1 while a reservation is authorized on this device.",
            &[("device", id)],
            f64::from(u8::from(session_active)),
        );
        out.gauge(
            "yard_device_viewers",
            "Live subscribers to this device's video stream.",
            &[("device", id)],
            video.viewer_count() as f64,
        );
        out.gauge(
            "yard_device_streaming",
            "1 when the device has broadcast a keyframe and is streaming.",
            &[("device", id)],
            f64::from(u8::from(video.is_streaming())),
        );
        out.counter(
            "yard_device_video_bytes_total",
            "Access-unit bytes produced by this device's encoder.",
            &[("device", id)],
            video.bytes_published() as f64,
        );
        out.counter(
            "yard_device_video_frames_total",
            "Access units produced by this device's encoder.",
            &[("device", id)],
            video.frames_published() as f64,
        );
        out.counter(
            "yard_device_installs_total",
            "Installs attempted on this device.",
            &[("device", id), ("result", "ok")],
            installs_ok as f64,
        );
        out.counter(
            "yard_device_installs_total",
            "Installs attempted on this device.",
            &[("device", id), ("result", "error")],
            installs_failed as f64,
        );
        out.counter(
            "yard_device_metrics_errors_total",
            "Failed metric samples for this device.",
            &[("device", id)],
            cache.get(id).map(|s| s.errors).unwrap_or(0) as f64,
        );
    }

    for device in &devices {
        let id: &str = &device.id;
        let Some(sample) = cache.get(id) else {
            continue;
        };

        if let Some(at) = sample.succeeded_at {
            out.gauge(
                "yard_device_metrics_sample_timestamp_seconds",
                "Unix time of the last successful sample for this device.",
                &[("device", id)],
                at,
            );
        }

        // A stale or failed device emits *none* of its device-sourced series.
        // Absence is how Prometheus spells "no data": `rate()` and
        // `avg_over_time` handle a gap correctly and `absent()` alerts on it.
        // Re-serving the last known value would be a lie — an unplugged phone
        // would show a flat, healthy 40 °C forever.
        let Ok(metrics) = &sample.result else {
            continue;
        };
        if now.duration_since(sample.at) > max_age {
            debug!(device = %id, "sample older than the staleness window; not exporting it");
            continue;
        }

        if let Some(cpu) = &metrics.cpu {
            for (mode, seconds) in cpu.modes() {
                out.counter(
                    "yard_device_cpu_seconds_total",
                    "Cumulative CPU seconds on the device, by mode.",
                    &[("device", id), ("mode", mode)],
                    seconds,
                );
            }
        }

        if let Some(memory) = &metrics.memory {
            out.gauge(
                "yard_device_memory_total_bytes",
                "Total physical memory on the device.",
                &[("device", id)],
                memory.total as f64,
            );
            if let Some(available) = memory.available {
                out.gauge(
                    "yard_device_memory_available_bytes",
                    "Memory available for new allocations without swapping.",
                    &[("device", id)],
                    available as f64,
                );
                out.gauge(
                    "yard_device_memory_used_bytes",
                    "Total memory less what is available.",
                    &[("device", id)],
                    memory.total.saturating_sub(available) as f64,
                );
            }
        }

        if let Some(level) = metrics.battery_level {
            out.gauge(
                "yard_device_battery_level_ratio",
                "Battery charge, 0 to 1.",
                &[("device", id)],
                level,
            );
        }
        if let Some(charging) = metrics.battery_charging {
            out.gauge(
                "yard_device_battery_charging",
                "1 while the device is charging.",
                &[("device", id)],
                f64::from(u8::from(charging)),
            );
        }
        if let Some(celsius) = metrics.battery_temperature_c {
            out.gauge(
                "yard_device_battery_temperature_celsius",
                "Battery temperature.",
                &[("device", id)],
                celsius,
            );
        }
        for zone in &metrics.thermal_zones {
            out.gauge(
                "yard_device_thermal_zone_celsius",
                "Thermal sensor reading, by zone.",
                &[("device", id), ("zone", &zone.name)],
                zone.celsius,
            );
        }

        for app in &metrics.apps {
            if let Some(seconds) = app.cpu_seconds {
                out.counter(
                    "yard_app_cpu_seconds_total",
                    "Cumulative CPU seconds for a watched process.",
                    &[("device", id), ("app", &app.process)],
                    seconds,
                );
            }
            if let Some(pss) = app.pss_bytes {
                out.gauge(
                    "yard_app_memory_pss_bytes",
                    "Proportional set size of a watched process.",
                    &[("device", id), ("app", &app.process)],
                    pss as f64,
                );
            }
        }
    }

    out.finish()
}

const ALL_STATUSES: &[DeviceStatus] = &[
    DeviceStatus::Absent,
    DeviceStatus::Present,
    DeviceStatus::Preparing,
    DeviceStatus::Ready,
    DeviceStatus::Busy,
    DeviceStatus::Unhealthy,
];

/// What an operator means by a device's status, which is not quite what the
/// provider tracks.
///
/// A provider's own status is only ever `preparing`, `ready` or `unhealthy` —
/// `busy` is the coordinator's word, assigned when a reservation goes active,
/// and [`crate::supervisor::Supervisor::refresh`] deliberately never writes it.
/// Exporting the raw value therefore made `busy` a permanently dead series and,
/// far worse, made every reserved device report `ready` — which on a dashboard
/// reads as "free, take it" about a device someone is actively using.
///
/// So the exporter reconstructs it. The provider holds the authoritative input:
/// an authorized reservation is exactly what the coordinator sets `busy` for,
/// and it is the same fact that admits a viewer to the session plane. This does
/// not change what the provider *reports upstream* — the control plane still
/// never says `busy` — it only makes the observability surface mean what
/// everyone reading it will assume.
///
/// Health wins over occupancy: a broken phone that happens to be reserved is
/// still broken, and must not hide behind `busy`.
fn effective_status(status: DeviceStatus, session_active: bool) -> DeviceStatus {
    match status {
        DeviceStatus::Ready if session_active => DeviceStatus::Busy,
        other => other,
    }
}

fn status_label(status: DeviceStatus) -> &'static str {
    match status {
        DeviceStatus::Absent => "absent",
        DeviceStatus::Present => "present",
        DeviceStatus::Preparing => "preparing",
        DeviceStatus::Ready => "ready",
        DeviceStatus::Busy => "busy",
        DeviceStatus::Cleaning => "cleaning",
        DeviceStatus::Unhealthy => "unhealthy",
    }
}

fn unix_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Writes the Prometheus text exposition format.
///
/// Hand-rolled rather than a registry crate because the values arrive as a
/// snapshot and this only transcribes them. The registries retain every label
/// set they have seen, and making a device's series *disappear* — which is the
/// whole staleness contract above — is more code through one of those than it is
/// to emit the bytes directly.
#[derive(Default)]
struct Encoder {
    /// Samples grouped by family, in first-seen order.
    ///
    /// Buffered rather than written straight out because Prometheus requires all
    /// of a family's samples to be contiguous under a single `# HELP`/`# TYPE`,
    /// and the natural way to gather these is device-major — every device in
    /// turn contributing a sample to a dozen different families. Grouping here
    /// means the caller can emit in whatever order reads best.
    families: Vec<Family>,
    index: HashMap<&'static str, usize>,
}

struct Family {
    name: &'static str,
    kind: &'static str,
    help: String,
    samples: Vec<String>,
}

impl Encoder {
    fn gauge(&mut self, name: &'static str, help: &str, labels: &[(&str, &str)], value: f64) {
        self.write(name, "gauge", help, labels, value);
    }

    fn counter(&mut self, name: &'static str, help: &str, labels: &[(&str, &str)], value: f64) {
        self.write(name, "counter", help, labels, value);
    }

    fn write(
        &mut self,
        name: &'static str,
        kind: &'static str,
        help: &str,
        labels: &[(&str, &str)],
        value: f64,
    ) {
        let at = *self.index.entry(name).or_insert_with(|| {
            self.families.push(Family {
                name,
                kind,
                help: help.to_owned(),
                samples: Vec::new(),
            });
            self.families.len() - 1
        });

        let mut line = String::from(name);
        if !labels.is_empty() {
            line.push('{');
            for (i, (key, value)) in labels.iter().enumerate() {
                if i > 0 {
                    line.push(',');
                }
                let _ = write!(line, "{key}=\"{}\"", escape(value));
            }
            line.push('}');
        }
        let _ = write!(line, " {}", format_value(value));

        self.families[at].samples.push(line);
    }

    fn finish(self) -> String {
        let mut out = String::new();
        for family in self.families {
            let _ = writeln!(out, "# HELP {} {}", family.name, family.help);
            let _ = writeln!(out, "# TYPE {} {}", family.name, family.kind);
            for sample in family.samples {
                out.push_str(&sample);
                out.push('\n');
            }
        }
        out
    }
}

/// Escapes a label *value*. Device and app names come off a device, so a stray
/// quote would otherwise corrupt every series after it in the same scrape.
fn escape(value: &str) -> std::borrow::Cow<'_, str> {
    if !value.contains(['\\', '"', '\n']) {
        return std::borrow::Cow::Borrowed(value);
    }
    std::borrow::Cow::Owned(
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n"),
    )
}

/// Prometheus spells the non-finite values differently from Rust, and Rust's
/// shortest round-trip float formatting is what keeps `0.77` from being written
/// as `0.7700000000000001`.
fn format_value(value: f64) -> String {
    if value.is_nan() {
        "NaN".into()
    } else if value.is_infinite() {
        if value.is_sign_positive() {
            "+Inf"
        } else {
            "-Inf"
        }
        .into()
    } else {
        format!("{value}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Samples are gathered device-major — every device in turn contributing to
    /// a dozen families — so families arrive interleaved. Prometheus requires
    /// them contiguous under one HELP/TYPE, which is why the encoder buffers.
    #[test]
    fn interleaved_families_are_regrouped_and_declared_once() {
        let mut out = Encoder::default();
        out.gauge("yard_one", "First.", &[("device", "a")], 1.0);
        out.counter("yard_two", "Second.", &[("device", "a")], 10.0);
        out.gauge("yard_one", "First.", &[("device", "b")], 2.0);
        out.counter("yard_two", "Second.", &[("device", "b")], 20.0);

        assert_eq!(
            out.finish(),
            "# HELP yard_one First.\n\
             # TYPE yard_one gauge\n\
             yard_one{device=\"a\"} 1\n\
             yard_one{device=\"b\"} 2\n\
             # HELP yard_two Second.\n\
             # TYPE yard_two counter\n\
             yard_two{device=\"a\"} 10\n\
             yard_two{device=\"b\"} 20\n"
        );
    }

    /// A device name is chosen by whoever wrote provider.yaml and an app name
    /// comes off the device itself. Neither is trusted to be quote-free, and an
    /// unescaped one corrupts every series after it.
    #[test]
    fn label_values_are_escaped() {
        let mut out = Encoder::default();
        out.gauge(
            "yard_test",
            "A test.",
            &[("device", "we\"ird\\one\nhere")],
            1.0,
        );

        assert!(out
            .finish()
            .contains(r#"yard_test{device="we\"ird\\one\nhere"} 1"#));
    }

    #[test]
    fn floats_are_written_the_way_prometheus_reads_them() {
        assert_eq!(format_value(0.77), "0.77");
        assert_eq!(format_value(1.0), "1");
        assert_eq!(format_value(f64::NAN), "NaN");
        assert_eq!(format_value(f64::INFINITY), "+Inf");
        assert_eq!(format_value(f64::NEG_INFINITY), "-Inf");
    }
}
