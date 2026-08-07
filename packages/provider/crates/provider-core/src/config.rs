//! One real YAML file, parsed once.
//!
//! Replaces `stf-ios-provider/provider.sh` — 13.5k of awk-parsed YAML and docker
//! orchestration that produced one container, one ZMQ connection and one config
//! file *per device*. This process supervises every device on the host.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use serde::Deserialize;

fn default_bind() -> String {
    "0.0.0.0:7100".into()
}
fn default_scratch() -> PathBuf {
    PathBuf::from("/var/lib/farm/scratch")
}
fn default_max_upload_mb() -> u64 {
    2048
}
fn default_metrics_enabled() -> bool {
    true
}
fn default_metrics_bind() -> String {
    "0.0.0.0:9100".into()
}
fn default_metrics_interval() -> u64 {
    30
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Must match the provider row the coordinator issued the token for.
    pub id: String,
    pub name: String,

    /// Coordinator base URL, e.g. `https://farm.example.com`.
    pub coordinator_url: String,

    /// Bearer credential from `/admin/providers`. Prefer `token_file` or the
    /// `FARM_PROVIDER_TOKEN` env var so a secret is not sitting in the YAML.
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub token_file: Option<PathBuf>,

    /// The URL the **browser** uses to reach this provider directly. Must be
    /// HTTPS in production: WebCodecs requires a secure context from any
    /// non-loopback origin.
    pub public_base_url: String,

    /// Where the session and artifact planes listen.
    #[serde(default = "default_bind")]
    pub bind: String,

    /// Staged uploads land here and are deleted after each install. A tmpfs is
    /// a good choice — nothing here is meant to outlive an install.
    #[serde(default = "default_scratch")]
    pub scratch_dir: PathBuf,

    #[serde(default = "default_max_upload_mb")]
    pub max_upload_mb: u64,

    #[serde(default)]
    pub devices: Vec<DeviceConfig>,

    /// Prometheus metrics. Absent means off; see [`MetricsConfig`].
    #[serde(default)]
    pub metrics: MetricsConfig,
}

/// Where the metrics exporter listens and how often it samples.
///
/// A listener of its own rather than a route on [`Config::bind`]: that port is
/// browser-facing, carries a CORS layer and session tokens, and a scraper has
/// neither. There is no auth here on purpose — the operator is expected to bind
/// it to an interface only their monitoring can reach.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsConfig {
    /// Note the asymmetry with [`Default`] below, which is deliberate: an absent
    /// `metrics:` block is off, but writing the block and omitting `enabled` is
    /// on, because writing it at all is the intent.
    #[serde(default = "default_metrics_enabled")]
    pub enabled: bool,

    #[serde(default = "default_metrics_bind")]
    pub bind: String,

    /// How often every device is sampled. The exporter serves a cache, so a
    /// scrape never waits on a phone and two scrapers cannot double the load.
    #[serde(default = "default_metrics_interval")]
    pub interval_secs: u64,

    /// Per-app CPU and memory, Android only. Globbed against the *process* name,
    /// so `com.foo.bar:push` stays distinct from `com.foo.bar`. Empty means the
    /// backend skips the expensive `dumpsys meminfo` round trip entirely.
    #[serde(default)]
    pub app_patterns: Vec<String>,
}

impl Default for MetricsConfig {
    /// Used only when the `metrics:` key is absent, which is why `enabled` is
    /// false here and true in [`default_metrics_enabled`].
    fn default() -> Self {
        Self {
            enabled: false,
            bind: default_metrics_bind(),
            interval_secs: default_metrics_interval(),
            app_patterns: Vec::new(),
        }
    }
}

impl MetricsConfig {
    pub fn interval(&self) -> Duration {
        Duration::from_secs(self.interval_secs)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceConfig {
    /// udid (iOS) or serial (Android).
    pub udid: String,
    pub backend: BackendKind,
    #[serde(default)]
    pub name: Option<String>,
    /// Backend-specific settings, validated by the backend itself.
    #[serde(default)]
    pub options: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackendKind {
    Ios,
    Android,
    /// Synthetic device. Lets the whole provider run — and be tested — with no
    /// hardware attached, the same way the TS fake provider does for the
    /// coordinator.
    Mock,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        Self::load_with_env(path, std::env::var("FARM_PROVIDER_TOKEN").ok())
    }

    /// The env override is a parameter rather than read in here so tests can
    /// exercise token precedence without mutating process-global state — which
    /// is shared across parallel tests and produces order-dependent flakes.
    pub fn load_with_env(path: &Path, token_from_env: Option<String>) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let mut config: Config = serde_yaml_ng::from_str(&raw)
            .with_context(|| format!("parsing config {}", path.display()))?;
        config.resolve_token(path, token_from_env)?;
        config.validate()?;
        Ok(config)
    }

    /// Env var wins, then `token_file`, then the inline `token`.
    fn resolve_token(&mut self, config_path: &Path, from_env: Option<String>) -> Result<()> {
        if let Some(from_env) = from_env {
            let trimmed = from_env.trim();
            if !trimmed.is_empty() {
                self.token = Some(trimmed.to_owned());
                return Ok(());
            }
        }

        if let Some(file) = &self.token_file {
            // Relative paths resolve against the config, not the working
            // directory, so a systemd unit and a shell run agree.
            let path = if file.is_absolute() {
                file.clone()
            } else {
                config_path.parent().unwrap_or(Path::new(".")).join(file)
            };
            let contents = std::fs::read_to_string(&path)
                .with_context(|| format!("reading token_file {}", path.display()))?;
            self.token = Some(contents.trim().to_owned());
        }

        Ok(())
    }

    fn validate(&self) -> Result<()> {
        if self.token.as_deref().unwrap_or("").is_empty() {
            bail!(
                "no provider token: set FARM_PROVIDER_TOKEN, `token_file`, or `token`. \
                 Issue one under /admin/providers in the coordinator."
            );
        }
        if !self.coordinator_url.starts_with("http") {
            bail!(
                "coordinator_url must be an http(s) URL, got {:?}",
                self.coordinator_url
            );
        }
        if !self.public_base_url.starts_with("http") {
            bail!(
                "public_base_url must be an http(s) URL, got {:?}",
                self.public_base_url
            );
        }

        let mut seen = std::collections::HashSet::new();
        for device in &self.devices {
            if !seen.insert(&device.udid) {
                bail!("duplicate device udid {:?}", device.udid);
            }
        }

        self.validate_metrics()?;
        Ok(())
    }

    /// Checked at load, so a typo is a startup error rather than a scrape target
    /// that silently never appears in Prometheus.
    fn validate_metrics(&self) -> Result<()> {
        if !self.metrics.enabled {
            return Ok(());
        }

        if self.metrics.bind.parse::<SocketAddr>().is_err() {
            bail!(
                "metrics.bind must be an address:port, got {:?}",
                self.metrics.bind
            );
        }
        if self.metrics.bind == self.bind {
            bail!(
                "metrics.bind {:?} collides with bind {:?}; the metrics listener is \
                 deliberately separate from the browser-facing planes",
                self.metrics.bind,
                self.bind
            );
        }

        // The floor is not arbitrary: below ~5s the Android backend's own adb
        // round trips exceed the interval, and `dumpsys meminfo` at that rate is
        // measurable load on the phone being tested.
        if !(5..=3600).contains(&self.metrics.interval_secs) {
            bail!(
                "metrics.interval_secs must be between 5 and 3600, got {}",
                self.metrics.interval_secs
            );
        }

        for pattern in &self.metrics.app_patterns {
            let trimmed = pattern.trim();
            if trimmed.is_empty() {
                bail!("metrics.app_patterns contains an empty pattern");
            }
            if trimmed.chars().all(|c| c == '*' || c == '?') {
                bail!(
                    "metrics.app_patterns entry {pattern:?} matches every process, which \
                     would export a Prometheus series per process per device. Name the \
                     apps you care about, e.g. \"*.example.*\"."
                );
            }
        }

        Ok(())
    }

    /// Trailing slashes make every later URL join ambiguous.
    pub fn coordinator_base(&self) -> &str {
        self.coordinator_url.trim_end_matches('/')
    }

    pub fn public_base(&self) -> &str {
        self.public_base_url.trim_end_matches('/')
    }

    pub fn control_url(&self) -> String {
        let base = self.coordinator_base();
        let ws = if let Some(rest) = base.strip_prefix("https") {
            format!("wss{rest}")
        } else if let Some(rest) = base.strip_prefix("http") {
            format!("ws{rest}")
        } else {
            base.to_owned()
        };
        format!("{ws}/api/providers/connect")
    }

    pub fn jwks_url(&self) -> String {
        format!("{}{}", self.coordinator_base(), farm_protocol::JWKS_PATH)
    }

    pub fn max_upload_bytes(&self) -> u64 {
        self.max_upload_mb * 1024 * 1024
    }

    pub fn token(&self) -> &str {
        self.token.as_deref().unwrap_or_default()
    }
}

/// Backoff for the control-plane reconnect loop.
pub const RECONNECT_MIN: Duration = Duration::from_secs(1);
pub const RECONNECT_MAX: Duration = Duration::from_secs(30);

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn write(contents: &str) -> (tempdir::Dir, PathBuf) {
        let dir = tempdir::Dir::new();
        let path = dir.path().join("provider.yaml");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(contents.as_bytes()).unwrap();
        (dir, path)
    }

    const MINIMAL: &str = r#"
id: lab-1
name: Lab 1
coordinator_url: https://farm.example.com/
public_base_url: https://lab-1.example.com/
token: pft_secret
devices:
  - udid: 00008120-000C
    backend: ios
  - udid: R5CT10ABC
    backend: android
"#;

    #[test]
    fn parses_a_minimal_config() {
        let (_dir, path) = write(MINIMAL);
        let config = Config::load_with_env(&path, None).unwrap();

        assert_eq!(config.id, "lab-1");
        assert_eq!(config.devices.len(), 2);
        assert_eq!(config.devices[0].backend, BackendKind::Ios);
        assert_eq!(config.bind, "0.0.0.0:7100");
    }

    #[test]
    fn derives_control_and_jwks_urls_without_double_slashes() {
        let (_dir, path) = write(MINIMAL);
        let config = Config::load_with_env(&path, None).unwrap();

        assert_eq!(
            config.control_url(),
            "wss://farm.example.com/api/providers/connect"
        );
        assert_eq!(
            config.jwks_url(),
            "https://farm.example.com/.well-known/farm-jwks.json"
        );
        assert_eq!(config.public_base(), "https://lab-1.example.com");
    }

    #[test]
    fn http_coordinator_yields_ws_not_wss() {
        let (_dir, path) =
            write(&MINIMAL.replace("https://farm.example.com/", "http://localhost:3000"));
        let config = Config::load_with_env(&path, None).unwrap();
        assert_eq!(
            config.control_url(),
            "ws://localhost:3000/api/providers/connect"
        );
    }

    #[test]
    fn the_env_var_overrides_the_file() {
        let (_dir, path) = write(MINIMAL);
        let config = Config::load_with_env(&path, Some("pft_from_env".into())).unwrap();
        assert_eq!(config.token(), "pft_from_env");
    }

    #[test]
    fn an_empty_env_var_falls_back_rather_than_blanking_the_token() {
        let (_dir, path) = write(MINIMAL);
        let config = Config::load_with_env(&path, Some("   ".into())).unwrap();
        assert_eq!(config.token(), "pft_secret");
    }

    #[test]
    fn a_token_file_is_resolved_relative_to_the_config() {
        let dir = tempdir::Dir::new();
        let config_path = dir.path().join("provider.yaml");
        std::fs::write(dir.path().join("token.txt"), "pft_from_file\n").unwrap();
        std::fs::write(
            &config_path,
            MINIMAL.replace("token: pft_secret", "token_file: token.txt"),
        )
        .unwrap();

        let config = Config::load_with_env(&config_path, None).unwrap();
        assert_eq!(config.token(), "pft_from_file");
    }

    #[test]
    fn a_missing_token_is_a_startup_error_not_a_runtime_401() {
        let (_dir, path) = write(&MINIMAL.replace("token: pft_secret", ""));
        let err = Config::load_with_env(&path, None).unwrap_err().to_string();
        assert!(err.contains("no provider token"), "{err}");
    }

    #[test]
    fn duplicate_udids_are_rejected() {
        let (_dir, path) = write(&MINIMAL.replace("R5CT10ABC", "00008120-000C"));
        let err = Config::load_with_env(&path, None).unwrap_err().to_string();
        assert!(err.contains("duplicate device"), "{err}");
    }

    #[test]
    fn unknown_keys_are_rejected_rather_than_silently_ignored() {
        let (_dir, path) = write(&format!("{MINIMAL}\nscreen_port: 7400\n"));
        assert!(Config::load_with_env(&path, None).is_err());
    }

    #[test]
    fn metrics_are_off_when_the_block_is_absent() {
        let (_dir, path) = write(MINIMAL);
        let config = Config::load_with_env(&path, None).unwrap();
        assert!(!config.metrics.enabled);
    }

    /// Writing the block at all is the intent, so an omitted `enabled` is on —
    /// the opposite of what an absent block means.
    #[test]
    fn an_empty_metrics_block_turns_metrics_on() {
        let (_dir, path) = write(&format!("{MINIMAL}\nmetrics: {{}}\n"));
        let config = Config::load_with_env(&path, None).unwrap();

        assert!(config.metrics.enabled);
        assert_eq!(config.metrics.bind, "0.0.0.0:9100");
        assert_eq!(config.metrics.interval_secs, 30);
        assert!(config.metrics.app_patterns.is_empty());
    }

    #[test]
    fn a_full_metrics_block_parses() {
        let (_dir, path) = write(&format!(
            "{MINIMAL}\nmetrics:\n  enabled: true\n  bind: 127.0.0.1:9101\n  \
             interval_secs: 60\n  app_patterns:\n    - \"*.bmw.*\"\n"
        ));
        let config = Config::load_with_env(&path, None).unwrap();

        assert_eq!(config.metrics.bind, "127.0.0.1:9101");
        assert_eq!(config.metrics.interval(), Duration::from_secs(60));
        assert_eq!(config.metrics.app_patterns, vec!["*.bmw.*".to_owned()]);
    }

    #[test]
    fn an_unparseable_metrics_bind_fails_at_load() {
        let (_dir, path) = write(&format!("{MINIMAL}\nmetrics:\n  bind: not-an-address\n"));
        let err = Config::load_with_env(&path, None).unwrap_err().to_string();
        assert!(err.contains("metrics.bind"), "{err}");
    }

    #[test]
    fn metrics_may_not_share_the_session_planes_port() {
        let (_dir, path) = write(&format!("{MINIMAL}\nmetrics:\n  bind: 0.0.0.0:7100\n"));
        let err = Config::load_with_env(&path, None).unwrap_err().to_string();
        assert!(err.contains("collides with bind"), "{err}");
    }

    #[test]
    fn too_frequent_sampling_is_rejected() {
        let (_dir, path) = write(&format!("{MINIMAL}\nmetrics:\n  interval_secs: 1\n"));
        let err = Config::load_with_env(&path, None).unwrap_err().to_string();
        assert!(err.contains("interval_secs"), "{err}");
    }

    /// A bare `*` is a cardinality bomb: a series per process per device.
    #[test]
    fn a_match_everything_app_pattern_is_rejected() {
        let (_dir, path) = write(&format!(
            "{MINIMAL}\nmetrics:\n  app_patterns:\n    - \"*\"\n"
        ));
        let err = Config::load_with_env(&path, None).unwrap_err().to_string();
        assert!(err.contains("matches every process"), "{err}");
    }

    /// A disabled block should not be able to block startup.
    #[test]
    fn a_disabled_metrics_block_is_not_validated() {
        let (_dir, path) = write(&format!(
            "{MINIMAL}\nmetrics:\n  enabled: false\n  bind: nonsense\n  interval_secs: 0\n"
        ));
        assert!(Config::load_with_env(&path, None).is_ok());
    }

    #[test]
    fn unknown_keys_inside_the_metrics_block_are_rejected_too() {
        let (_dir, path) = write(&format!("{MINIMAL}\nmetrics:\n  scrape_port: 9100\n"));
        assert!(Config::load_with_env(&path, None).is_err());
    }

    /// Minimal scoped temp directory; avoids a dev-dependency for six tests.
    mod tempdir {
        use std::path::{Path, PathBuf};

        pub struct Dir(PathBuf);

        /// A counter, not a timestamp. Two `Dir::new()` calls in different
        /// threads can land in the same nanosecond, and then one test's `Drop`
        /// deletes the directory another is still reading from — which showed
        /// up only under the full workspace run, as an intermittent failure.
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

        impl Dir {
            pub fn new() -> Self {
                let path = std::env::temp_dir().join(format!(
                    "farm-config-test-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                ));
                std::fs::create_dir_all(&path).unwrap();
                Self(path)
            }

            pub fn path(&self) -> &Path {
                &self.0
            }
        }

        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
}
