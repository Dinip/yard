//! One real YAML file, parsed once.
//!
//! Replaces `stf-ios-provider/provider.sh` — 13.5k of awk-parsed YAML and docker
//! orchestration that produced one container, one ZMQ connection and one config
//! file *per device*. This process supervises every device on the host.

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
