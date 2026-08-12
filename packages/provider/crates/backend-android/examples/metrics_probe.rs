//! Manual probe of the metrics reads, against a real device.
//!
//! `cargo run -p backend-android --example metrics_probe [-- <serial> <pattern>…]`
//!
//! Not a test: it needs hardware. Runs the same batched `sh -c` and the same
//! parsers the backend does, and prints both what the device said and what came
//! out — so a parser that silently reads the wrong field is visible rather than
//! showing up later as a plausible-looking number on a dashboard.
//!
//! Thermal is expected to be empty on most retail devices; the probe says so
//! explicitly rather than leaving you wondering.

use backend_android::adb::{Adb, DEFAULT_ADB_SERVER};
use backend_android::metrics;
use provider_core::backend::AppFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let wanted = args.next();
    let patterns: Vec<String> = args.collect();

    let adb = Adb::new(DEFAULT_ADB_SERVER);
    let devices = adb.devices().await?;

    let Some(device) = devices
        .iter()
        .filter(|d| d.is_usable())
        .find(|d| wanted.as_ref().is_none_or(|want| &d.serial == want))
    else {
        println!("no usable device attached (saw {devices:#?})");
        return Ok(());
    };
    println!("device: {}\n", device.serial);

    let batch = adb.shell(&device.serial, metrics::SYSTEM_BATCH).await?;
    let sections = metrics::split_sections(&batch);
    let section = |name: &str| sections.get(name).copied().unwrap_or_default();

    println!("--- /proc/stat ---\n{}", section("stat").trim());
    match metrics::parse_proc_stat(section("stat")) {
        Some(cpu) => println!("parsed: {:?}\n", cpu.modes()),
        None => println!("parsed: NOTHING — the aggregate `cpu` line was not found\n"),
    }

    match metrics::parse_meminfo(section("mem")) {
        Some(mem) => println!(
            "--- memory ---\ntotal {:.2} GiB, available {:?} GiB, free {:?} GiB\n",
            mem.total as f64 / 1024.0 / 1024.0 / 1024.0,
            mem.available.map(|b| b as f64 / 1024.0 / 1024.0 / 1024.0),
            mem.free.map(|b| b as f64 / 1024.0 / 1024.0 / 1024.0),
        ),
        None => println!("--- memory ---\nNOTHING — no MemTotal\n"),
    }

    let battery = section("batt");
    println!(
        "--- battery ---\nlevel {:?}, state {:?}, temperature {:?} °C\n",
        backend_android::parse_battery_level(battery),
        backend_android::parse_battery_state(battery),
        metrics::parse_battery_temperature(battery),
    );

    let zones = metrics::parse_thermal_zones(section("ztype"), section("ztemp"));
    if zones.is_empty() {
        // The common case, and not a fault: /sys/class/thermal is SELinux-denied
        // to an unrooted shell on most retail devices, and some expose only
        // cooling_device* with no thermal_zone* at all.
        println!(
            "--- thermal ---\nno readable zones ({} type lines, {} temp lines) — \
             normal on an unrooted retail device\n",
            section("ztype")
                .lines()
                .filter(|l| !l.trim().is_empty())
                .count(),
            section("ztemp")
                .lines()
                .filter(|l| !l.trim().is_empty())
                .count(),
        );
    } else {
        println!("--- thermal ---");
        for zone in &zones {
            println!("  {}: {:.1} °C", zone.name, zone.celsius);
        }
        println!();
    }

    let dump = adb.shell(&device.serial, "dumpsys meminfo").await?;
    let processes = metrics::parse_meminfo_pss(&dump);
    println!(
        "--- processes ---\n{} parsed out of the PSS section",
        processes.len()
    );

    let filter = AppFilter::new(&patterns);
    if patterns.is_empty() {
        println!("(no patterns given; showing the five largest)");
        for (pid, process, pss) in processes.iter().take(5) {
            println!(
                "  {process} (pid {pid}): {:.1} MiB",
                *pss as f64 / 1024.0 / 1024.0
            );
        }
        return Ok(());
    }

    let pids: Vec<i64> = processes
        .iter()
        .filter(|(_, process, _)| filter.matches(process))
        .map(|(pid, _, _)| *pid)
        .collect();
    println!("{} matched {patterns:?}", pids.len());

    let cpu_by_pid = if pids.is_empty() {
        Default::default()
    } else {
        adb.shell(&device.serial, &metrics::pid_stat_command(&pids))
            .await?
            .lines()
            .filter_map(metrics::parse_pid_stat)
            .collect()
    };

    for app in metrics::assemble_apps(&processes, &cpu_by_pid, &filter) {
        println!(
            "  {}: {:.1} MiB, {:?} cpu-seconds",
            app.process,
            app.pss_bytes.unwrap_or(0) as f64 / 1024.0 / 1024.0,
            app.cpu_seconds,
        );
    }

    Ok(())
}
