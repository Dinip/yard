//! Manual probe of the iOS app list, against a real device.
//!
//! `cargo run -p backend-ios --example app_list_probe -- <udid>`
//!
//! Not a test: it needs hardware, and it needs the provider **stopped**, since
//! it builds its own CoreDevice tunnel to the same device.
//!
//! It exists because app listing is the one CoreDevice request whose accepted
//! shape has already changed under us twice: iOS 26 added required option keys
//! and refuses anything without them (NSCocoaErrorDomain 4865), and the
//! one-shot listing hangs on a real app list, so `apps()` streams. See the
//! app-listing section of docs/PROVIDER.md. When a new iOS breaks it again,
//! this is the shortest path from "the audit row has a plist dump in it" to
//! knowing what the device wants.

use std::time::Duration;

use backend_ios::ddi::{DdiCache, DdiConfig};
use backend_ios::{IosBackend, IosOptions};
use provider_core::backend::DeviceBackend as _;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    let udid = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: app_list_probe <udid>"))?;

    let backend = IosBackend::new(
        IosOptions {
            udid: udid.clone(),
            display_id: 0,
            motion_idr: false,
            // `ensure_mounted` looks the image up before it does anything, so
            // this is a no-op against a device the provider already mounted and
            // the one step that makes the probe work on a bare machine — an
            // unmounted phone offers no `com.apple.coredevice.*` service at all.
            auto_mount_ddi: true,
            ddi: Some(DdiCache::new(DdiConfig {
                enabled: true,
                // Not the packaged default: that is a path a provider host owns,
                // and the probe is run by hand from a checkout.
                cache_dir: std::env::temp_dir().join("yard-ddi"),
                ..DdiConfig::default()
            })),
        },
        None,
    );

    // The backend brings its session up in the background; `apps()` waits on it
    // but the tunnel handshake is slower than the default patience here.
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // A sleeping phone sends no video, and the session watchdog restarts on
    // that — tearing down the app-service stream mid-request. Pressing HOME
    // wakes the screen so frames flow for as long as the listing takes.
    backend.reset_screen().await.ok();
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let apps = backend.apps().await?;
    println!("{} apps on {udid}", apps.len());
    for app in &apps {
        println!(
            "  {:<45} {:<28} {}",
            app.id,
            app.name.as_deref().unwrap_or("-"),
            if app.system == Some(true) {
                "system"
            } else {
                "third-party"
            },
        );
    }

    Ok(())
}
