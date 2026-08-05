//! Exercise the Android paths that a browser session does not cover.
//!
//! `cargo run -p backend-android --example device_ops`. Hardware, and
//! deliberately not a test: it launches an app and toggles the device's adb
//! listener, which no CI run should do.

use std::sync::Arc;

use backend_android::{AndroidBackend, AndroidOptions};
use provider_core::backend::{DeviceBackend, NullProgress};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    let serial = std::env::args().nth(1).expect("usage: device_ops <serial>");
    let options = AndroidOptions::parse(&serial, &serde_json::Map::new())?;
    let backend: Arc<AndroidBackend> = AndroidBackend::new(options, None);

    println!("apps:");
    let apps = backend.apps().await?;
    println!(
        "  {} user apps, first few: {:?}",
        apps.len(),
        &apps[..apps.len().min(3)]
    );

    println!("\nlaunch (settings):");
    match backend.launch("com.android.settings", &[]).await {
        Ok(()) => println!("  launched"),
        Err(err) => println!("  failed: {err}"),
    }

    println!("\ninstall of a file that is not an APK — the error path:");
    let bogus = std::env::temp_dir().join("farm-not-an-apk.apk");
    tokio::fs::write(&bogus, b"this is not a package").await?;
    match backend.install(&bogus, &NullProgress).await {
        Ok(()) => println!("  UNEXPECTED: reported success"),
        Err(err) => println!("  refused, as it should: {err}"),
    }
    tokio::fs::remove_file(&bogus).await.ok();

    println!("\nremote debug:");
    let exposed = backend.remote_debug().await?;
    println!("  exposed on port {}", exposed.port);
    println!("  device is listening: {}", listening(&serial).await);
    println!(
        "  connectable on the forwarded port: {}",
        connectable(exposed.port).await
    );

    backend.remote_debug_stop().await?;
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    println!("  withdrawn");
    println!("  device is listening: {}", listening(&serial).await);

    Ok(())
}

/// Whether adbd is actually listening on the device, as the device sees it.
async fn listening(serial: &str) -> bool {
    let adb = backend_android::adb::Adb::new(backend_android::adb::DEFAULT_ADB_SERVER);
    adb.shell(serial, "getprop service.adb.tcp.port")
        .await
        .map(|value| value.trim() == "5555")
        .unwrap_or(false)
}

/// Whether the forwarded port accepts a TCP connection — the thing a developer
/// running `adb connect` actually needs.
async fn connectable(port: u16) -> bool {
    tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .is_ok()
}
