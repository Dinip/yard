//! Manual probe of the iOS diagnostics relay, against a real device.
//!
//! `cargo run -p backend-ios --example diagnostics_probe -- <udid>`
//!
//! Not a test: it needs hardware. Same reason as
//! `backend-android/examples/scrcpy_probe.rs` — the exact key spellings and
//! units this service answers with are read off a device rather than inferred,
//! because inferring them is how you end up dividing a temperature by the wrong
//! power of ten.
//!
//! Two questions it exists to settle:
//!
//! 1. **Battery.** Which of `gasguage` / `ioregistry` answers, and what are the
//!    keys and units? `Temperature` is usually hundredths of a degree and
//!    sometimes tenths, and only a dump settles which.
//! 2. **Anything CPU, memory or thermal.** Expectation: *nothing*. iOS exposes
//!    `host_statistics`/`vm_statistics` to on-device code only, and this relay is
//!    a lockdown service with a fixed dictionary and no `sysctl` surface. This
//!    probe is how that expectation gets confirmed and written down, so the
//!    question stops being re-asked.
//!
//! Record whatever it prints in docs/PROVIDER.md, including a negative result.

use backend_ios::device::usbmux_provider;
use idevice::services::diagnostics_relay::DiagnosticsRelayClient;
use idevice::IdeviceService as _;

/// Words worth grepping the `all()` dump for.
const INTERESTING: &[&str] = &[
    "Temperature",
    "Thermal",
    "CPU",
    "Memory",
    "PageSize",
    "Load",
    "Capacity",
    "Charging",
];

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let udid = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: diagnostics_probe <udid>"))?;

    let provider = usbmux_provider(&udid).await?;

    // The gas gauge is the expected win: AppleSmartBattery's own readings,
    // typically CurrentCapacity, MaxCapacity, Temperature, Voltage, IsCharging.
    dump("gasguage", {
        let mut relay = connect(&*provider).await?;
        relay.gasguage().await.ok().flatten()
    });

    // The fallback, if the gas gauge comes back empty on a modern iOS.
    dump("ioregistry AppleSmartBattery", {
        let mut relay = connect(&*provider).await?;
        relay
            .ioregistry(None, Some("AppleSmartBattery"), None)
            .await
            .ok()
            .flatten()
    });

    for class in [
        "IOPMrootDomain",
        "AppleARMPMUCharger",
        "IOPlatformExpertDevice",
    ] {
        dump(&format!("ioregistry {class}"), {
            let mut relay = connect(&*provider).await?;
            relay
                .ioregistry(None, Some(class), None)
                .await
                .ok()
                .flatten()
        });
    }

    // The whole tree. Large; grepped rather than printed in full.
    let mut relay = connect(&*provider).await?;
    match relay.all().await {
        Ok(Some(all)) => {
            let text = format!("{all:#?}");
            println!("\n=== all() — {} bytes, greppable hits ===", text.len());
            for needle in INTERESTING {
                let hits = text.matches(needle).count();
                if hits > 0 {
                    println!("  {needle}: {hits} occurrence(s)");
                    for line in text.lines().filter(|l| l.contains(needle)).take(5) {
                        println!("      {}", line.trim());
                    }
                }
            }
        }
        other => println!("\n=== all() === {other:?}"),
    }

    Ok(())
}

async fn connect(
    provider: &dyn idevice::provider::IdeviceProvider,
) -> anyhow::Result<DiagnosticsRelayClient> {
    // Reconnected per call rather than cached, matching the backend: a replug
    // invalidates the client, and a cached one would surface that as a
    // permanently dead reading rather than a connect error.
    DiagnosticsRelayClient::connect(provider)
        .await
        .map_err(|err| anyhow::anyhow!("diagnostics relay connect: {err:?}"))
}

fn dump(label: &str, value: Option<plist::Dictionary>) {
    println!("\n=== {label} ===");
    match value {
        Some(dict) if !dict.is_empty() => {
            for (key, value) in &dict {
                println!("  {key}: {value:?}");
            }
        }
        Some(_) => println!("  (empty)"),
        None => println!("  (no answer)"),
    }
}
