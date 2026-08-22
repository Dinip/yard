//! The metrics listener, end to end and in-process.
//!
//! Runs the real router and the real sampler against the real mock backends,
//! with no coordinator, no database and no device — and, unlike
//! `session_plane.rs`, **no JWKS and no signer at all**. That absence is the
//! headline assertion: this plane is deliberately unauthenticated, and a
//! refactor that "fixes" it by copying the session plane's layers is exactly
//! what these tests exist to catch.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use yard_protocol::Platform;
use provider_core::config::Config;
use provider_core::metrics::{router, sample_once, MetricsCache, MetricsState};
use provider_core::session::{Authorization, SessionRegistry};
use provider_core::supervisor::Supervisor;

const PROVIDER_ID: &str = "test-provider";
const IOS_DEVICE: &str = "mock-ios-1";
const ANDROID_DEVICE: &str = "mock-android-1";

struct Harness {
    base: String,
    state: MetricsState,
    sessions: SessionRegistry,
    android: Arc<backend_mock::MockBackend>,
}

async fn start(app_patterns: &[&str]) -> Harness {
    let patterns = app_patterns
        .iter()
        .map(|p| format!("    - \"{p}\"\n"))
        .collect::<String>();
    let patterns = if patterns.is_empty() {
        "  app_patterns: []\n".to_owned()
    } else {
        format!("  app_patterns:\n{patterns}")
    };

    let config: Config = serde_yaml_ng::from_str(&format!(
        r#"
id: {PROVIDER_ID}
name: test
coordinator_url: http://localhost:3000
public_base_url: http://localhost:7100
token: pft_test
metrics:
  enabled: true
  bind: 127.0.0.1:0
  interval_secs: 30
{patterns}"#
    ))
    .unwrap();

    let sessions = SessionRegistry::new();
    let mut supervisor = Supervisor::new(sessions.clone());
    let android = backend_mock::MockBackend::new(ANDROID_DEVICE, Platform::Android, "Mock Pixel");
    supervisor.add(
        IOS_DEVICE.into(),
        backend_mock::MockBackend::new(IOS_DEVICE, Platform::Ios, "Mock iPhone"),
    );
    supervisor.add(ANDROID_DEVICE.into(), android.clone());
    let supervisor = Arc::new(supervisor);
    supervisor.bootstrap().await;

    let state = MetricsState {
        config: Arc::new(config),
        supervisor,
        cache: MetricsCache::new(),
    };

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let served = state.clone();
    tokio::spawn(async move { axum::serve(listener, router(served)).await });

    Harness {
        base,
        state,
        sessions,
        android,
    }
}

impl Harness {
    async fn sample(&self) {
        sample_once(&self.state).await;
    }

    async fn scrape(&self) -> reqwest::Response {
        reqwest::get(format!("{}/metrics", self.base))
            .await
            .unwrap()
    }

    async fn text(&self) -> String {
        self.scrape().await.text().await.unwrap()
    }
}

/// Finds the value of one series, by its exact `name{labels}` prefix.
fn series(body: &str, key: &str) -> Option<f64> {
    body.lines()
        .find(|line| line.starts_with(key) && line[key.len()..].starts_with(' '))
        .and_then(|line| line[key.len()..].trim().parse().ok())
}

fn has(body: &str, key: &str) -> bool {
    series(body, key).is_some()
}

/// The deliberate difference from the session plane, and the single thing a
/// future refactor is most likely to break.
#[tokio::test]
async fn a_scrape_needs_no_token_and_no_origin() {
    let h = start(&[]).await;
    let response = h.scrape().await;

    assert_eq!(response.status(), 200);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("text/plain; version=0.0.4; charset=utf-8")
    );
}

#[tokio::test]
async fn the_metrics_router_does_not_serve_the_session_plane() {
    let h = start(&[]).await;
    let response = reqwest::get(format!("{}/s/{IOS_DEVICE}", h.base))
        .await
        .unwrap();

    assert_eq!(response.status(), 404);
}

#[tokio::test]
async fn a_sampled_android_device_reports_cpu_memory_and_temperature() {
    let h = start(&[]).await;
    h.sample().await;
    let body = h.text().await;

    assert!(has(
        &body,
        &format!("yard_device_cpu_seconds_total{{device=\"{ANDROID_DEVICE}\",mode=\"user\"}}")
    ));
    assert!(has(
        &body,
        &format!("yard_device_memory_total_bytes{{device=\"{ANDROID_DEVICE}\"}}")
    ));
    assert!(has(
        &body,
        &format!(
            "yard_device_thermal_zone_celsius{{device=\"{ANDROID_DEVICE}\",zone=\"mock-cpu\"}}"
        )
    ));

    let level = series(
        &body,
        &format!("yard_device_battery_level_ratio{{device=\"{ANDROID_DEVICE}\"}}"),
    )
    .expect("battery level");
    assert!((0.0..=1.0).contains(&level), "{level}");
}

/// iOS has no CPU or memory to report, and an absent series is how that is
/// spelled — not a zero.
#[tokio::test]
async fn an_ios_device_reports_battery_but_no_cpu() {
    let h = start(&[]).await;
    h.sample().await;
    let body = h.text().await;

    assert!(has(
        &body,
        &format!("yard_device_battery_level_ratio{{device=\"{IOS_DEVICE}\"}}")
    ));
    assert!(!has(
        &body,
        &format!("yard_device_cpu_seconds_total{{device=\"{IOS_DEVICE}\",mode=\"user\"}}")
    ));
    assert!(!has(
        &body,
        &format!("yard_device_memory_total_bytes{{device=\"{IOS_DEVICE}\"}}")
    ));
}

#[tokio::test]
async fn only_processes_matching_a_pattern_get_a_series() {
    let h = start(&["*.demo.*"]).await;
    h.sample().await;
    let body = h.text().await;

    assert!(has(
        &body,
        &format!(
            "yard_app_memory_pss_bytes{{device=\"{ANDROID_DEVICE}\",app=\"com.example.demo.player\"}}"
        )
    ));
    assert!(!has(
        &body,
        &format!(
            "yard_app_memory_pss_bytes{{device=\"{ANDROID_DEVICE}\",app=\"com.example.mock.app\"}}"
        )
    ));
}

#[tokio::test]
async fn no_patterns_means_no_per_app_series_at_all() {
    let h = start(&[]).await;
    h.sample().await;
    let body = h.text().await;

    assert!(!body.contains("yard_app_memory_pss_bytes"));
    assert!(!body.contains("yard_app_cpu_seconds_total"));
}

/// node_exporter's enum idiom: every status is emitted so an alert can be
/// `== 1` rather than having to reason about `absent()`.
#[tokio::test]
async fn device_status_emits_one_series_per_status() {
    let h = start(&[]).await;
    let body = h.text().await;

    let ready = series(
        &body,
        &format!("yard_device_status{{device=\"{IOS_DEVICE}\",status=\"ready\"}}"),
    );
    let unhealthy = series(
        &body,
        &format!("yard_device_status{{device=\"{IOS_DEVICE}\",status=\"unhealthy\"}}"),
    );

    assert_eq!(ready, Some(1.0));
    assert_eq!(unhealthy, Some(0.0));
}

/// A reserved device must not read as `ready`.
///
/// `ready` on a dashboard means "free, take it". The provider's own status can
/// never be `busy` — that word belongs to the coordinator — so the exporter has
/// to combine the two, or every in-use device advertises itself as available.
#[tokio::test]
async fn a_reserved_device_reports_busy_rather_than_ready() {
    let h = start(&[]).await;
    let ready = format!("yard_device_status{{device=\"{IOS_DEVICE}\",status=\"ready\"}}");
    let busy = format!("yard_device_status{{device=\"{IOS_DEVICE}\",status=\"busy\"}}");

    let body = h.text().await;
    assert_eq!(series(&body, &ready), Some(1.0));
    assert_eq!(series(&body, &busy), Some(0.0));

    h.sessions
        .authorize(
            IOS_DEVICE,
            Authorization {
                reservation_id: "res-1".into(),
                user_id: "user-1".into(),
                adb_keys: Vec::new(),
            },
        )
        .await;

    let body = h.text().await;
    assert_eq!(series(&body, &busy), Some(1.0), "a reserved device is busy");
    assert_eq!(
        series(&body, &ready),
        Some(0.0),
        "and must not still advertise itself as ready"
    );

    // Releasing gives it back.
    h.sessions.revoke(IOS_DEVICE, "test").await;
    let body = h.text().await;
    assert_eq!(series(&body, &ready), Some(1.0));
    assert_eq!(series(&body, &busy), Some(0.0));
}

/// An unhealthy device is unhealthy whether or not someone holds it: a broken
/// phone that happens to be reserved must not hide behind `busy`.
#[tokio::test]
async fn a_reservation_does_not_mask_an_unhealthy_device() {
    let h = start(&[]).await;
    h.android.state.healthy.store(false, Ordering::Relaxed);
    h.state.supervisor.bootstrap().await;

    h.sessions
        .authorize(
            ANDROID_DEVICE,
            Authorization {
                reservation_id: "res-2".into(),
                user_id: "user-1".into(),
                adb_keys: Vec::new(),
            },
        )
        .await;

    let body = h.text().await;
    assert_eq!(
        series(
            &body,
            &format!("yard_device_status{{device=\"{ANDROID_DEVICE}\",status=\"unhealthy\"}}")
        ),
        Some(1.0)
    );
    assert_eq!(
        series(
            &body,
            &format!("yard_device_status{{device=\"{ANDROID_DEVICE}\",status=\"busy\"}}")
        ),
        Some(0.0)
    );
}

#[tokio::test]
async fn an_authorized_reservation_shows_as_an_active_session() {
    let h = start(&[]).await;

    let key = format!("yard_device_session_active{{device=\"{IOS_DEVICE}\"}}");
    assert_eq!(series(&h.text().await, &key), Some(0.0));

    h.sessions
        .authorize(
            IOS_DEVICE,
            Authorization {
                reservation_id: "res-1".into(),
                user_id: "user-1".into(),
                adb_keys: Vec::new(),
            },
        )
        .await;

    assert_eq!(series(&h.text().await, &key), Some(1.0));
}

/// The behaviour most worth locking down. A device that stops answering must
/// *lose* its series rather than keep serving the last good numbers: a flat,
/// healthy 40 °C from an unplugged phone is worse than no data at all.
#[tokio::test]
async fn a_failing_device_loses_its_series_but_keeps_its_operational_ones() {
    let h = start(&["*.demo.*"]).await;
    h.sample().await;

    let cpu = format!("yard_device_cpu_seconds_total{{device=\"{ANDROID_DEVICE}\",mode=\"user\"}}");
    assert!(has(&h.text().await, &cpu));

    h.android.state.healthy.store(false, Ordering::Relaxed);
    h.sample().await;
    let body = h.text().await;

    assert!(
        !has(&body, &cpu),
        "a failed sample must not still be served"
    );
    assert!(!body.contains("com.example.demo.player"));
    // But we can still tell the difference between "this device is gone" and
    // "the exporter is broken".
    assert_eq!(
        series(
            &body,
            &format!("yard_device_metrics_errors_total{{device=\"{ANDROID_DEVICE}\"}}")
        ),
        Some(1.0)
    );
    assert!(has(
        &body,
        &format!("yard_device_status{{device=\"{ANDROID_DEVICE}\",status=\"ready\"}}")
    ));
    // The iOS device is unaffected: one device failing is not a farm outage.
    assert!(has(
        &body,
        &format!("yard_device_battery_level_ratio{{device=\"{IOS_DEVICE}\"}}")
    ));
}

/// Proves the scrape reads the cache rather than the device. The mock's
/// metrics advance with wall-clock time, so two scrapes returning identical
/// numbers can only mean neither of them sampled.
#[tokio::test]
async fn two_scrapes_without_a_sampler_pass_are_identical() {
    let h = start(&[]).await;
    h.sample().await;

    let first = h.text().await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let second = h.text().await;

    let key = format!("yard_device_cpu_seconds_total{{device=\"{ANDROID_DEVICE}\",mode=\"user\"}}");
    assert_eq!(series(&first, &key), series(&second, &key));

    // ...and a pass does move them, so the test above is not passing because
    // nothing works.
    h.sample().await;
    assert_ne!(series(&first, &key), series(&h.text().await, &key));
}

#[tokio::test]
async fn a_family_is_declared_once_however_many_devices_there_are() {
    let h = start(&[]).await;
    h.sample().await;
    let body = h.text().await;

    assert_eq!(body.matches("# TYPE yard_device_status ").count(), 1);
    assert_eq!(body.matches("# TYPE yard_device_viewers ").count(), 1);
}

/// The provider has not registered in this harness — there is no coordinator at
/// all — and that must read as disconnected rather than as missing.
#[tokio::test]
async fn the_provider_reports_itself_unregistered_with_no_coordinator() {
    let h = start(&[]).await;
    let body = h.text().await;

    assert_eq!(
        series(
            &body,
            &format!("yard_provider_control_connected{{provider=\"{PROVIDER_ID}\"}}")
        ),
        Some(0.0)
    );
}
