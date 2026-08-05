//! Manual probe against a real adb server and device.
//!
//! `cargo run -p backend-android --example probe`. Not a test: it needs
//! hardware, and the point of it is to validate the client against the real
//! server *before* anything is built on top — the highest-uncertainty part of
//! this phase.

use backend_android::adb::{parse_getprop, Adb, DEFAULT_ADB_SERVER};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let adb = Adb::new(DEFAULT_ADB_SERVER);

    println!("server version: {:#x}", adb.version().await?);

    let devices = adb.devices().await?;
    println!("devices: {devices:#?}");

    let Some(device) = devices.iter().find(|d| d.is_usable()) else {
        println!("no usable device attached");
        return Ok(());
    };

    let props = parse_getprop(&adb.shell(&device.serial, "getprop").await?);
    for key in [
        "ro.product.model",
        "ro.product.manufacturer",
        "ro.build.version.release",
        "ro.build.version.sdk",
        "ro.product.cpu.abi",
    ] {
        println!("{key} = {:?}", props.get(key));
    }

    let size = adb.shell(&device.serial, "wm size").await?;
    let density = adb.shell(&device.serial, "wm density").await?;
    println!("{}{}", size.trim(), format_args!("\n{}", density.trim()));

    Ok(())
}
