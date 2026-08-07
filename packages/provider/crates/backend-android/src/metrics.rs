//! Reading CPU, memory, thermal and per-app usage off a phone over adb.
//!
//! Every [`Adb::shell`](crate::adb::Adb::shell) call opens its own TCP transport,
//! so **round trips are the budget**, not device work: each read below is
//! ~5-40 ms on the phone against ~15-30 ms of transport setup. Hence one batched
//! `sh -c` for the system numbers, and the per-app reads skipped entirely when no
//! patterns are configured.
//!
//! Nothing here is on `info()`'s path. That already makes 3-5 round trips every
//! 15s on the supervisor's cadence; metrics run on their own, slower one.

use std::collections::HashMap;

use provider_core::backend::{AppFilter, AppMetrics, CpuTimes, MemoryBytes, ThermalZone};

/// Reads the whole system set in one transport.
///
/// The sentinels are echoed rather than the sections being parsed positionally:
/// any one of these can be missing (thermal usually is) and a missing section
/// must not shift the meaning of the next one.
pub const SYSTEM_BATCH: &str = "sh -c 'echo ---stat; cat /proc/stat 2>/dev/null | head -1; \
     echo ---mem; cat /proc/meminfo 2>/dev/null; \
     echo ---batt; dumpsys battery 2>/dev/null; \
     echo ---ztype; cat /sys/class/thermal/thermal_zone*/type 2>/dev/null; \
     echo ---ztemp; cat /sys/class/thermal/thermal_zone*/temp 2>/dev/null'";

/// `USER_HZ`, which is 100 on every Android ABI.
///
/// Hardcoded rather than read from `getconf CLK_TCK`, which would be a whole
/// extra round trip for a constant the kernel ABI fixes.
const JIFFIES_PER_SECOND: f64 = 100.0;

/// Splits the batched output into its sections. A section absent from the output
/// is absent from the map rather than empty.
pub fn split_sections(output: &str) -> HashMap<&str, &str> {
    let mut sections = HashMap::new();
    let mut name: Option<&str> = None;
    let mut start = 0usize;
    let mut cursor = 0usize;

    for line in output.split_inclusive('\n') {
        if let Some(marker) = line.trim_end().strip_prefix("---") {
            if let Some(previous) = name.take() {
                sections.insert(previous, &output[start..cursor]);
            }
            name = Some(marker);
            start = cursor + line.len();
        }
        cursor += line.len();
    }
    if let Some(previous) = name {
        sections.insert(previous, &output[start..]);
    }
    sections
}

/// The aggregate `cpu` line of `/proc/stat`, in seconds.
///
/// Per-core lines are deliberately ignored: they are cardinality nobody watching
/// a farm needs, multiplied by every device.
pub fn parse_proc_stat(section: &str) -> Option<CpuTimes> {
    let line = section.lines().find(|line| {
        let mut parts = line.split_whitespace();
        parts.next() == Some("cpu")
    })?;

    let fields: Vec<f64> = line
        .split_whitespace()
        .skip(1)
        .map(|value| value.parse::<f64>().map(|v| v / JIFFIES_PER_SECOND))
        .collect::<Result<_, _>>()
        .ok()?;

    // A truncated line is not a partial reading: user/nice/system/idle are the
    // four every kernel has, and anything shorter is garbage rather than an old
    // kernel.
    if fields.len() < 4 {
        return None;
    }

    Some(CpuTimes {
        user: fields[0],
        nice: Some(fields[1]),
        system: fields[2],
        idle: fields[3],
        iowait: fields.get(4).copied(),
        irq: fields.get(5).copied(),
        softirq: fields.get(6).copied(),
        steal: fields.get(7).copied(),
    })
}

/// `/proc/meminfo`, kB → bytes.
///
/// `MemAvailable` and `MemFree` are kept apart on purpose. A kernel too old to
/// report the former gets `None`, not a fallback to the latter: `free` excludes
/// reclaimable page cache, so substituting it would overstate memory pressure by
/// however much the device is caching — usually most of it.
pub fn parse_meminfo(section: &str) -> Option<MemoryBytes> {
    let field = |name: &str| -> Option<u64> {
        section.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            if key.trim() != name {
                return None;
            }
            value.split_whitespace().next()?.parse::<u64>().ok()
        })
    };

    Some(MemoryBytes {
        total: field("MemTotal")? * 1024,
        available: field("MemAvailable").map(|kb| kb * 1024),
        free: field("MemFree").map(|kb| kb * 1024),
    })
}

/// `dumpsys battery`'s `temperature:` line, in tenths of a degree.
///
/// Nothing else parsed this line, though it has always been there. A device with
/// no sensor reports a sentinel — `-1` in practice, and `0` on a couple of
/// emulators — and -0.1 °C on a dashboard is worse than no reading at all.
pub fn parse_battery_temperature(dump: &str) -> Option<f64> {
    let tenths = dump.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        (key.trim() == "temperature").then(|| value.trim().parse::<i64>().ok())?
    })?;

    (tenths > 0).then_some(tenths as f64 / 10.0)
}

/// Pairs `thermal_zone*/type` with `thermal_zone*/temp`.
///
/// **Usually empty, and that is normal.** `/sys/class/thermal` is SELinux-denied
/// to an unrooted shell on most retail devices, so a missing set is the common
/// case rather than a fault worth logging every interval.
pub fn parse_thermal_zones(types: &str, temps: &str) -> Vec<ThermalZone> {
    let names: Vec<&str> = types
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let readings: Vec<&str> = temps
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();

    // The two globs expand in the same order, so zipping is right — but only if
    // both produced the same count. Mismatched lengths mean one glob was partly
    // denied, and zipping then attaches each temperature to the wrong sensor,
    // which is worse than reporting nothing.
    if names.is_empty() || names.len() != readings.len() {
        return Vec::new();
    }

    names
        .into_iter()
        .zip(readings)
        .filter_map(|(name, reading)| {
            let raw: f64 = reading.parse().ok()?;
            Some(ThermalZone {
                name: name.to_owned(),
                celsius: scale_thermal(raw)?,
            })
        })
        .collect()
}

/// Thermal zones report in whatever unit their driver felt like.
///
/// Millidegrees is the documented convention and by far the most common, but
/// tenths and plain degrees are both out there. Guessing by magnitude is the only
/// option — and getting it wrong is what puts a 45000 °C spike on a dashboard.
fn scale_thermal(raw: f64) -> Option<f64> {
    let celsius = if raw.abs() > 1000.0 {
        raw / 1000.0
    } else if raw.abs() > 200.0 {
        raw / 10.0
    } else {
        raw
    };

    // No phone is below freezing or above the boiling point of water while it is
    // being tested; either means the unit guess was wrong.
    (-10.0..=150.0).contains(&celsius).then_some(celsius)
}

/// `dumpsys meminfo`'s "Total PSS by process" section → `(pid, process, bytes)`.
///
/// One call yields both the pid↔name map and every PSS number, which is what
/// makes the per-app path three round trips rather than one per app.
pub fn parse_meminfo_pss(dump: &str) -> Vec<(i64, String, u64)> {
    let mut out = Vec::new();
    let mut inside = false;

    for line in dump.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("Total PSS by process") {
            inside = true;
            continue;
        }
        if !inside {
            continue;
        }
        // The section ends at the first blank line; what follows is "Total PSS
        // by OOM adjustment", which lists the same processes again.
        if trimmed.is_empty() {
            break;
        }

        // `123,456K: com.example.app (pid 1234 / activities)`
        let Some((size, rest)) = trimmed.split_once(':') else {
            continue;
        };
        // Values are comma-grouped. Parsing without stripping them yields 123 for
        // a 123 MB process, which reads as a plausible number and is not.
        let Ok(kb) = size
            .trim()
            .trim_end_matches('K')
            .replace(',', "")
            .parse::<u64>()
        else {
            continue;
        };

        let rest = rest.trim();
        let Some(open) = rest.find("(pid ") else {
            continue;
        };
        let process = rest[..open].trim();
        if process.is_empty() {
            continue;
        }
        let after = &rest[open + "(pid ".len()..];
        let pid: i64 = match after
            .split(|c: char| !c.is_ascii_digit())
            .find(|s| !s.is_empty())
            .and_then(|s| s.parse().ok())
        {
            Some(pid) => pid,
            None => continue,
        };

        out.push((pid, process.to_owned(), kb * 1024));
    }

    out
}

/// `utime + stime` out of `/proc/<pid>/stat`, in seconds.
///
/// The trap this exists for: field 2 is the executable name in parentheses, and
/// it may itself contain spaces *and* parentheses — a naive whitespace split
/// silently reads the wrong fields. Splitting on the **last** `)` is the standard
/// answer and the only correct one.
pub fn parse_pid_stat(line: &str) -> Option<(i64, f64)> {
    let pid: i64 = line.split_whitespace().next()?.parse().ok()?;
    let after_comm = &line[line.rfind(')')? + 1..];

    let fields: Vec<&str> = after_comm.split_whitespace().collect();
    // utime and stime are fields 14 and 15 of the line, which are indexes 11 and
    // 12 of what follows the comm.
    let utime: f64 = fields.get(11)?.parse().ok()?;
    let stime: f64 = fields.get(12)?.parse().ok()?;

    Some((pid, (utime + stime) / JIFFIES_PER_SECOND))
}

/// Builds the one `cat` that reads every matched process's stat file.
pub fn pid_stat_command(pids: &[i64]) -> String {
    let paths: Vec<String> = pids.iter().map(|pid| format!("/proc/{pid}/stat")).collect();
    format!("cat {} 2>/dev/null", paths.join(" "))
}

/// Joins PSS and CPU into the per-app metrics for the processes that matched.
pub fn assemble_apps(
    processes: &[(i64, String, u64)],
    cpu_by_pid: &HashMap<i64, f64>,
    filter: &AppFilter,
) -> Vec<AppMetrics> {
    processes
        .iter()
        .filter(|(_, process, _)| filter.matches(process))
        .map(|(pid, process, pss)| AppMetrics {
            process: process.clone(),
            // A process that died between the two reads simply has no CPU this
            // interval. Reporting zero would look like an idle app rather than a
            // gone one, and on a counter it reads as a reset.
            cpu_seconds: cpu_by_pid.get(pid).copied(),
            pss_bytes: Some(*pss),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_are_split_on_their_markers() {
        let out = "---stat\ncpu 1 2\n---mem\nMemTotal: 4 kB\n---batt\nlevel: 50\n";
        let sections = split_sections(out);

        assert_eq!(sections["stat"], "cpu 1 2\n");
        assert_eq!(sections["mem"], "MemTotal: 4 kB\n");
        assert_eq!(sections["batt"], "level: 50\n");
    }

    /// Thermal is denied on most retail devices, so the batch routinely comes
    /// back with two empty sections. That must not disturb the others.
    #[test]
    fn an_empty_section_does_not_shift_the_next_one() {
        let sections = split_sections("---ztype\n---ztemp\n---batt\nlevel: 50\n");

        assert_eq!(sections["ztype"], "");
        assert_eq!(sections["ztemp"], "");
        assert_eq!(sections["batt"], "level: 50\n");
    }

    #[test]
    fn proc_stat_converts_jiffies_to_seconds() {
        let cpu = parse_proc_stat("cpu  100 200 300 400 500 600 700 800\n").unwrap();

        assert_eq!(cpu.user, 1.0);
        assert_eq!(cpu.nice, Some(2.0));
        assert_eq!(cpu.system, 3.0);
        assert_eq!(cpu.idle, 4.0);
        assert_eq!(cpu.iowait, Some(5.0));
        assert_eq!(cpu.steal, Some(8.0));
    }

    /// Older kernels stop at the first few columns; that is a shorter line, not
    /// a broken one.
    #[test]
    fn a_short_proc_stat_line_still_parses_what_it_has() {
        let cpu = parse_proc_stat("cpu 100 200 300 400\n").unwrap();

        assert_eq!(cpu.user, 1.0);
        assert_eq!(cpu.idle, 4.0);
        assert_eq!(cpu.iowait, None);
    }

    #[test]
    fn a_truncated_proc_stat_line_is_nothing_rather_than_partial() {
        assert!(parse_proc_stat("cpu 100 200\n").is_none());
        assert!(parse_proc_stat("").is_none());
    }

    /// `cpu0` is not `cpu`: matching it would report one core as the whole
    /// device.
    #[test]
    fn per_core_lines_are_ignored() {
        let cpu = parse_proc_stat("cpu0 999 999 999 999\ncpu 100 200 300 400\n").unwrap();
        assert_eq!(cpu.user, 1.0);
    }

    #[test]
    fn meminfo_converts_kb_to_bytes() {
        let mem = parse_meminfo(
            "MemTotal:        7654321 kB\nMemFree:          123456 kB\nMemAvailable:  3000000 kB\n",
        )
        .unwrap();

        assert_eq!(mem.total, 7654321 * 1024);
        assert_eq!(mem.available, Some(3000000 * 1024));
        assert_eq!(mem.free, Some(123456 * 1024));
    }

    /// `free` excludes reclaimable cache, so standing in for `available` would
    /// overstate memory pressure by however much the device is caching.
    #[test]
    fn a_kernel_without_memavailable_reports_none_rather_than_memfree() {
        let mem = parse_meminfo("MemTotal: 100 kB\nMemFree: 40 kB\n").unwrap();

        assert_eq!(mem.available, None);
        assert_eq!(mem.free, Some(40 * 1024));
    }

    #[test]
    fn battery_temperature_is_tenths_of_a_degree() {
        assert_eq!(
            parse_battery_temperature("  temperature: 350\n"),
            Some(35.0)
        );
    }

    /// A device with no sensor answers a sentinel, and -0.1 °C on a dashboard is
    /// worse than no reading.
    #[test]
    fn a_sentinel_battery_temperature_is_no_reading() {
        assert_eq!(parse_battery_temperature("temperature: -1\n"), None);
        assert_eq!(parse_battery_temperature("temperature: 0\n"), None);
        assert_eq!(parse_battery_temperature("level: 50\n"), None);
    }

    #[test]
    fn thermal_zones_handle_every_unit_a_driver_might_use() {
        let zones = parse_thermal_zones("cpu-therm\nbattery\nskin\n", "45000\n312\n38\n");

        assert_eq!(zones.len(), 3);
        assert_eq!(zones[0].name, "cpu-therm");
        assert_eq!(zones[0].celsius, 45.0);
        assert_eq!(zones[1].celsius, 31.2);
        assert_eq!(zones[2].celsius, 38.0);
    }

    /// A partly-denied glob would otherwise attach each temperature to the wrong
    /// sensor, which is worse than reporting none.
    #[test]
    fn mismatched_zone_counts_drop_the_whole_set() {
        assert!(parse_thermal_zones("a\nb\nc\n", "45000\n46000\n").is_empty());
        assert!(parse_thermal_zones("", "45000\n").is_empty());
    }

    #[test]
    fn an_implausible_zone_reading_is_dropped() {
        // 45000 °C: what a millidegree value looks like when the guess is wrong.
        assert!(parse_thermal_zones("cpu\n", "45000000\n").is_empty());
    }

    /// Comma grouping is the likeliest parser bug here: without stripping it, a
    /// 123 MB process reports 123 KB, which is plausible enough to go unnoticed.
    #[test]
    fn pss_values_are_comma_grouped() {
        let dump = "Total PSS by process:\n\
             \x20   335,544K: com.example.demo.player (pid 1234 / activities)\n\
             \x20   180,224K: com.example.mock.app (pid 5678)\n\
             \x20     1,024K: com.example.demo.player:push (pid 9012)\n\
             \n\
             Total PSS by OOM adjustment:\n\
             \x20   999,999K: should.not.appear (pid 1)\n";

        let parsed = parse_meminfo_pss(dump);

        assert_eq!(parsed.len(), 3);
        assert_eq!(
            parsed[0],
            (1234, "com.example.demo.player".into(), 335_544 * 1024)
        );
        assert_eq!(
            parsed[1],
            (5678, "com.example.mock.app".into(), 180_224 * 1024)
        );
        // A `:push` process stays distinct from its parent — a leaked one is
        // exactly what someone watching these numbers is looking for.
        assert_eq!(parsed[2].1, "com.example.demo.player:push");
    }

    #[test]
    fn a_missing_pss_section_yields_nothing_rather_than_garbage() {
        assert!(parse_meminfo_pss("Applications Memory Usage (in Kilobytes):\n").is_empty());
    }

    /// The classic `/proc/<pid>/stat` trap: the comm field is parenthesised and
    /// may contain both spaces and parens, so only the *last* `)` is a reliable
    /// boundary.
    #[test]
    fn a_process_name_with_spaces_and_parens_still_parses() {
        let line = "1234 (com.foo (bar) baz) S 1 1234 0 0 -1 4194304 100 0 0 0 \
                    250 130 0 0 20 0 30 0 5000";

        let (pid, seconds) = parse_pid_stat(line).unwrap();

        assert_eq!(pid, 1234);
        // utime 250 + stime 130 = 380 jiffies.
        assert_eq!(seconds, 3.8);
    }

    #[test]
    fn a_garbage_pid_stat_line_is_refused() {
        assert!(parse_pid_stat("").is_none());
        assert!(parse_pid_stat("1234 (short) S 1 2 3").is_none());
        assert!(parse_pid_stat("not a stat line at all").is_none());
    }

    #[test]
    fn one_command_reads_every_matched_pid() {
        assert_eq!(
            pid_stat_command(&[12, 34]),
            "cat /proc/12/stat /proc/34/stat 2>/dev/null"
        );
    }

    #[test]
    fn only_matching_processes_are_assembled() {
        let processes = vec![
            (1, "com.example.demo.player".to_owned(), 100),
            (2, "com.example.mock.app".to_owned(), 200),
        ];
        let cpu = HashMap::from([(1, 5.0)]);
        let filter = AppFilter::new(&["*.demo.*".to_owned()]);

        let apps = assemble_apps(&processes, &cpu, &filter);

        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].process, "com.example.demo.player");
        assert_eq!(apps[0].cpu_seconds, Some(5.0));
        assert_eq!(apps[0].pss_bytes, Some(100));
    }

    /// A process that died between the PSS read and the CPU read has no CPU this
    /// interval. Zero would read as an idle app, and on a counter as a reset.
    #[test]
    fn a_process_that_died_between_reads_reports_no_cpu() {
        let processes = vec![(1, "com.example.demo.player".to_owned(), 100)];
        let filter = AppFilter::new(&["*.demo.*".to_owned()]);

        let apps = assemble_apps(&processes, &HashMap::new(), &filter);

        assert_eq!(apps[0].cpu_seconds, None);
        assert_eq!(apps[0].pss_bytes, Some(100));
    }
}
