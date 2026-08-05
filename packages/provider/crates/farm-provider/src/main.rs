//! The provider binary.
//!
//! One process per host, supervising every device configured for it. Replaces
//! `stf-ios-provider`'s one-container-per-device model and the 13.5k of
//! awk-parsed YAML (`provider.sh`) that orchestrated it.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context as _, Result};
use clap::Parser;
use farm_protocol::Platform;
use provider_core::auth::TokenVerifier;
use provider_core::config::{BackendKind, Config};
use provider_core::control::ControlClient;
use provider_core::origins::WebOrigins;
use provider_core::server::{self, ServerState};
use provider_core::session::SessionRegistry;
use provider_core::supervisor::Supervisor;
use provider_core::DeviceBackend;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "farm-provider", version, about = "Device provider")]
struct Args {
    /// Path to provider.yaml.
    #[arg(
        short,
        long,
        env = "FARM_CONFIG",
        default_value = "/etc/farm/provider.yaml"
    )]
    config: PathBuf,

    /// Parse the config, report what would run, and exit.
    #[arg(long)]
    check: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("FARM_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    let config = Arc::new(Config::load(&args.config)?);

    info!(
        provider = %config.id,
        devices = config.devices.len(),
        coordinator = %config.coordinator_base(),
        public = %config.public_base(),
        "farm-provider {}",
        env!("CARGO_PKG_VERSION")
    );

    if !config.public_base().starts_with("https://")
        && !config.public_base().contains("localhost")
        && !config.public_base().contains("127.0.0.1")
    {
        // Not fatal — a TLS-terminating proxy in front is a valid setup — but
        // a plain-http public URL means WebCodecs will refuse to run, and that
        // failure surfaces in the browser as a blank screen with no clue.
        warn!(
            public = %config.public_base(),
            "public_base_url is not https: browsers will refuse WebCodecs outside a secure context"
        );
    }

    let sessions = SessionRegistry::new();
    let mut supervisor = Supervisor::new(sessions.clone());

    for device in &config.devices {
        let backend = build_backend(device)?;
        info!(udid = %device.udid, backend = ?device.backend, "device configured");
        supervisor.add(device.udid.clone(), backend);
    }

    if config.devices.is_empty() {
        warn!("no devices configured — this provider will register with an empty inventory");
    }

    let supervisor = Arc::new(supervisor);

    // Fetched once and cached. A provider that cannot reach the JWKS at startup
    // cannot verify any session token, so failing here beats accepting
    // connections it will reject one by one.
    let verifier = Arc::new(TokenVerifier::new(
        config.jwks_url(),
        config.id.clone(),
        config.coordinator_base().to_owned(),
    ));

    if args.check {
        println!("config OK: {} device(s)", config.devices.len());
        return Ok(());
    }

    verifier
        .refresh()
        .await
        .with_context(|| format!("fetching JWKS from {}", config.jwks_url()))?;
    verifier
        .self_test()
        .await
        .context("session-token verification self-test failed")?;

    supervisor.bootstrap().await;

    let web_origins = WebOrigins::new();
    let control = ControlClient::new(
        config.clone(),
        supervisor.clone(),
        web_origins.clone(),
        verifier.clone(),
    );
    supervisor.attach_control(control.sender()).await;

    let state = ServerState {
        config: config.clone(),
        supervisor: supervisor.clone(),
        verifier,
        web_origins,
    };

    let mut session_plane = tokio::spawn(server::serve(state));
    let mut control_plane = tokio::spawn(control.run());
    let mut poll_loop = tokio::spawn(supervisor.clone().run_poll_loop());

    tokio::select! {
        result = &mut session_plane => {
            match result {
                // The session plane is the whole point of the process; if it
                // stops, so do we.
                Ok(Ok(())) => bail!("session plane exited unexpectedly"),
                Ok(Err(err)) => return Err(err).context("session plane failed"),
                Err(err) => bail!("session plane panicked: {err}"),
            }
        }
        // `ControlClient::run` reconnects forever, so reaching here means the
        // task itself died.
        result = &mut control_plane => {
            error!(?result, "control plane task ended");
            bail!("control plane task ended")
        }
        result = &mut poll_loop => {
            error!(?result, "device poll loop ended");
            bail!("device poll loop ended")
        }
        _ = shutdown() => {
            info!("shutting down");
            session_plane.abort();
            control_plane.abort();
            poll_loop.abort();
            Ok(())
        }
    }
}

fn build_backend(device: &provider_core::config::DeviceConfig) -> Result<Arc<dyn DeviceBackend>> {
    let name = device.name.clone().unwrap_or_else(|| device.udid.clone());

    match device.backend {
        BackendKind::Mock => {
            // `platform` decides which codec the synthetic stream advertises.
            let platform = match device
                .options
                .get("platform")
                .and_then(|v| v.as_str())
                .unwrap_or("ios")
            {
                "android" => Platform::Android,
                _ => Platform::Ios,
            };
            Ok(backend_mock::MockBackend::new(
                device.udid.clone(),
                platform,
                name,
            ))
        }
        BackendKind::Ios => {
            let options = backend_ios::IosOptions::parse(&device.udid, &device.options)
                .with_context(|| format!("device {}", device.udid))?;
            Ok(backend_ios::IosBackend::new(options, device.name.clone()))
        }
        BackendKind::Android => {
            let options = backend_android::AndroidOptions::parse(&device.udid, &device.options)
                .with_context(|| format!("device {}", device.udid))?;
            Ok(backend_android::AndroidBackend::new(
                options,
                device.name.clone(),
            ))
        }
    }
}

async fn shutdown() {
    let ctrl_c = tokio::signal::ctrl_c();

    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(term) => term,
            Err(_) => {
                let _ = ctrl_c.await;
                return;
            }
        };
        tokio::select! {
            _ = ctrl_c => {}
            _ = term.recv() => {}
        }
    }

    #[cfg(not(unix))]
    let _ = ctrl_c.await;
}
