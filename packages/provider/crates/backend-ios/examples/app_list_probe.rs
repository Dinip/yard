//! Manual probe of the iOS app list, against a real device.
//!
//! `cargo run -p backend-ios --example app_list_probe -- <udid>`
//!
//! Not a test: it needs hardware, and it needs the provider **stopped**, since
//! it builds its own CoreDevice tunnel to the same device.
//!
//! It exists because `list_apps` is the one CoreDevice request whose accepted
//! shape has already changed under us: iOS 26 added a required
//! `requireContainerAccess` key to the options dictionary and refuses anything
//! without it (NSCocoaErrorDomain 4865). See `src/app_list.rs`. When a new iOS
//! breaks app listing again, this is the shortest path from "the audit row has
//! a plist dump in it" to knowing which key the device wants.

use std::time::Duration;

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
