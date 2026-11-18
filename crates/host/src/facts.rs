//! The host's facts for the manager's beat (`docs/agent-manager.md` 3.2's
//! `beat`): load, memory pressure, swap, disk free per volume and thermal
//! state, read in three asks of [`InProcess`] (one `sysctl`, one `df`, one
//! `pmset`; two more for the throttle's facts, below) and turned into `wsp-host-api`'s [`HostFacts`].
//!
//! [`HostFacts`] has no "unknown" in its first fields, so a fact this host does
//! not answer is reported at its quiet value (load 0, memory `normal`, swap 0,
//! thermal `nominal`) and named in [`Read::missing`] for the reader's log.
//!
//! On Linux the same facts come from `/proc` ([`assemble_linux`]): load from
//! `/proc/loadavg`, swap and memory from `/proc/meminfo`, disks from the same
//! `df`. Linux has no one thermal state to read, so thermal is missing there;
//! the throttle's free memory and pageouts come from `/proc/meminfo` and
//! `/proc/vmstat` ([`throttle_facts_linux`]).
//!
//! The throttle's facts (wsp-044) are optional. The five- and fifteen-minute
//! load and the speed limit come out of the same answers; HID idle (`ioreg`)
//! and free memory and pageouts (`vm_stat`) take two asks more (wsp-049), the
//! ones `crates/trees/src/throttle.rs` makes in its own process today. An
//! optional fact this host does not answer is `None`, and not named missing.

use std::path::{Path, PathBuf};

use wsp_host_api::{DiskFree, HostFacts, MemoryPressure, Thermal};

use crate::inproc::InProcess;

/// The sysctl names one beat asks for.
const SYSCTLS: &[&str] = &["vm.loadavg", "vm.swapusage", "kern.memorystatus_vm_pressure_level"];

/// One reading, and the facts it could not get.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Read {
    pub facts: HostFacts,
    pub missing: Vec<&'static str>,
}

/// Read the host's facts now, with disk free for the volumes holding `volumes`
/// (one entry per volume, however many of the paths it holds).
pub fn read(host: &InProcess, volumes: &[PathBuf]) -> Read {
    let sys = host.sysctls(SYSCTLS).unwrap_or_default();
    let paths: Vec<&Path> = volumes.iter().map(PathBuf::as_path).collect();
    let df = host.disks_free(&paths).unwrap_or_default();
    let therm = host.thermal();
    let mut read = assemble(&sys, &df, therm.as_deref());
    throttle_facts(&mut read.facts, host.idle().as_deref(), host.pageouts().as_deref());
    read
}

/// The throttle's facts that need asks of their own: HID idle out of `ioreg
/// -c IOHIDSystem`, free memory and pageouts out of `vm_stat`. A fact whose
/// answer is absent or unreadable is left as it was.
pub fn throttle_facts(facts: &mut HostFacts, ioreg: Option<&str>, vm_stat: Option<&str>) {
    if let Some(idle) = ioreg.and_then(parse_hid_idle) {
        facts.hid_idle_secs = Some(idle);
    }
    if let Some(out) = vm_stat {
        if let Some(free) = parse_free_memory(out) {
            facts.free_memory_bytes = Some(free);
        }
        if let Some(n) = parse_pageouts(out) {
            facts.pageouts = Some(n);
        }
    }
}

/// `"HIDIdleTime" = 125000000000` in `ioreg -c IOHIDSystem`, nanoseconds, in
/// whole seconds.
pub fn parse_hid_idle(ioreg: &str) -> Option<u64> {
    let line = ioreg.lines().find(|l| l.contains("\"HIDIdleTime\""))?;
    let ns: u64 = line.split('=').nth(1)?.trim().parse().ok()?;
    Some(ns / 1_000_000_000)
}

/// `vm_stat`'s page size, out of its first line `Mach Virtual Memory
/// Statistics: (page size of 16384 bytes)`: 4096 where it does not say.
fn page_size(vm_stat: &str) -> u64 {
    vm_stat
        .lines()
        .next()
        .and_then(|l| l.split("page size of").nth(1))
        .and_then(|r| r.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(4096)
}

/// The count on `vm_stat`'s `<label>:   12205611.` line.
fn vm_count(vm_stat: &str, label: &str) -> Option<u64> {
    let line = vm_stat.lines().find(|l| l.starts_with(label))?;
    line[label.len()..].trim().trim_end_matches('.').parse().ok()
}

/// Memory free now, in bytes: `vm_stat`'s `Pages free` times its page size.
pub fn parse_free_memory(vm_stat: &str) -> Option<u64> {
    Some(vm_count(vm_stat, "Pages free:")? * page_size(vm_stat))
}

/// Pages paged out since boot, in 4 KB pages whatever the host's page size,
/// out of `vm_stat`'s `Pageouts:`: the unit the throttle's pageout lines are
/// drawn in (throttle.rs's `parse_pageouts`).
pub fn parse_pageouts(vm_stat: &str) -> Option<u64> {
    Some(vm_count(vm_stat, "Pageouts:")? * page_size(vm_stat) / 4096)
}

/// The facts out of the three answers.
pub fn assemble(sysctl: &str, df: &str, pmset: Option<&str>) -> Read {
    gather(
        sysctl_value(sysctl, "vm.loadavg").map(parse_loads).unwrap_or_default(),
        sysctl_value(sysctl, "vm.swapusage").and_then(parse_swap_used),
        sysctl_value(sysctl, "kern.memorystatus_vm_pressure_level").and_then(parse_pressure),
        df,
        pmset.map(|out| (parse_thermal(out), Some(parse_speed_limit(out).unwrap_or(100)))),
    )
}

/// The facts on Linux, out of `/proc/loadavg`, `/proc/meminfo` and `df -Pk`.
pub fn assemble_linux(loadavg: &str, meminfo: &str, df: &str) -> Read {
    let kb = |k: &str| meminfo_kb(meminfo, k);
    let swap = match (kb("SwapTotal"), kb("SwapFree")) {
        (Some(t), Some(f)) => Some(t.saturating_sub(f) * 1024),
        _ => None,
    };
    let memory = match (kb("MemAvailable"), kb("MemTotal")) {
        (Some(a), Some(t)) if t > 0 => Some(pressure_of_available(a, t)),
        _ => None,
    };
    gather(parse_loads(loadavg), swap, memory, df, None)
}

/// A `/proc/meminfo` field in kB (`MemTotal:  16314496 kB`).
pub fn meminfo_kb(meminfo: &str, key: &str) -> Option<u64> {
    meminfo.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix(':')?.split_whitespace().next()?.parse().ok())
}

/// Linux's nearest to macOS's pressure level: how much of memory the kernel
/// says is available without swapping. Under 5% is critical, under 15% warn.
pub fn pressure_of_available(available_kb: u64, total_kb: u64) -> MemoryPressure {
    match available_kb * 100 / total_kb {
        0..5 => MemoryPressure::Critical,
        5..15 => MemoryPressure::Warn,
        _ => MemoryPressure::Normal,
    }
}

/// [`throttle_facts`] on Linux: free memory is `MemAvailable` out of
/// `/proc/meminfo` (what the throttle's own Linux reader, trees'
/// `parse_meminfo_free`, calls free), pageouts are `pswpout` in `/proc/vmstat`,
/// already in 4 KB pages. HID idle has no Linux answer and stays `None`.
pub fn throttle_facts_linux(facts: &mut HostFacts, meminfo: Option<&str>, vmstat: Option<&str>) {
    if let Some(kb) = meminfo.and_then(|m| meminfo_kb(m, "MemAvailable")) {
        facts.free_memory_bytes = Some(kb * 1024);
    }
    if let Some(n) = vmstat.and_then(|v| v.lines().find_map(|l| l.strip_prefix("pswpout ")?.trim().parse().ok())) {
        facts.pageouts = Some(n);
    }
}

/// The facts out of what each OS read, a `None` being a fact not answered;
/// `thermal` carries the speed limit with it.
fn gather(
    loads: Vec<f64>,
    swap: Option<u64>,
    memory: Option<MemoryPressure>,
    df: &str,
    thermal: Option<(Thermal, Option<u32>)>,
) -> Read {
    let mut missing = Vec::new();
    let centi = |l: f64| (l * 100.0).round() as u32;
    let load_centi = match loads.first() {
        Some(&l) => centi(l),
        None => {
            missing.push("load");
            0
        }
    };
    let (load5_centi, load15_centi) = (loads.get(1).copied().map(centi), loads.get(2).copied().map(centi));
    let swap_used_bytes = match swap {
        Some(b) => b,
        None => {
            missing.push("swap");
            0
        }
    };
    let memory = match memory {
        Some(m) => m,
        None => {
            missing.push("memory");
            MemoryPressure::Normal
        }
    };
    let disks = parse_df(df);
    if disks.is_empty() {
        missing.push("disks");
    }
    let (thermal, speed_limit_pct) = match thermal {
        Some(t) => t,
        None => {
            missing.push("thermal");
            (Thermal::Nominal, None)
        }
    };
    let cpus = std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(1);
    let facts = HostFacts {
        load_centi,
        cpus,
        memory,
        swap_used_bytes,
        disks,
        thermal,
        load5_centi,
        load15_centi,
        speed_limit_pct,
        hid_idle_secs: None,
        free_memory_bytes: None,
        pageouts: None,
    };
    Read { facts, missing }
}

/// The value of `name` in `sysctl`'s `name: value` lines.
pub fn sysctl_value<'a>(out: &'a str, name: &str) -> Option<&'a str> {
    out.lines().find_map(|l| l.strip_prefix(name)?.strip_prefix(':').map(str::trim))
}

/// The one-minute load out of `{ 0.50 0.40 0.30 }`.
pub fn parse_load1(s: &str) -> Option<f64> {
    parse_loads(s).first().copied()
}

/// The one-, five- and fifteen-minute loads out of `{ 0.50 0.40 0.30 }`, as
/// many as read in order (none, if the first does not).
pub fn parse_loads(s: &str) -> Vec<f64> {
    s.split(|c: char| c.is_whitespace() || c == '{' || c == '}')
        .filter(|w| !w.is_empty())
        .take(3)
        .map_while(|w| w.parse().ok())
        .collect()
}

/// Swap used, in bytes, out of `total = 1024.00M  used = 43.19M  free = …`.
pub fn parse_swap_used(s: &str) -> Option<u64> {
    let rest = s.split("used").nth(1)?.trim_start().strip_prefix('=')?.trim_start();
    let num: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    let v: f64 = num.parse().ok()?;
    let scale = match rest[num.len()..].chars().next()? {
        'K' => 1024.0,
        'M' => 1024.0 * 1024.0,
        'G' => 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((v * scale).round() as u64)
}

/// The kernel's pressure level: 1 normal, 2 warn, 4 critical.
pub fn parse_pressure(s: &str) -> Option<MemoryPressure> {
    match s.trim() {
        "1" => Some(MemoryPressure::Normal),
        "2" => Some(MemoryPressure::Warn),
        "4" => Some(MemoryPressure::Critical),
        _ => None,
    }
}

/// `pmset -g therm`'s `CPU_Speed_Limit` as macOS's four thermal states. No
/// limit recorded, or 100, is nominal; the rest is banded (80 and over fair,
/// 50 and over serious, below that critical).
pub fn parse_thermal(out: &str) -> Thermal {
    match parse_speed_limit(out) {
        None => Thermal::Nominal,
        Some(n) if n >= 100 => Thermal::Nominal,
        Some(n) if n >= 80 => Thermal::Fair,
        Some(n) if n >= 50 => Thermal::Serious,
        Some(_) => Thermal::Critical,
    }
}

/// `pmset -g therm`'s `CPU_Speed_Limit`, in percent, if one is recorded.
pub fn parse_speed_limit(out: &str) -> Option<u32> {
    out.lines()
        .find(|l| l.trim_start().starts_with("CPU_Speed_Limit"))
        .and_then(|l| l.split('=').nth(1))
        .and_then(|v| v.trim().parse::<u32>().ok())
}

/// `df -Pk`'s rows, one per volume (a volume named twice is listed once).
pub fn parse_df(out: &str) -> Vec<DiskFree> {
    let mut disks: Vec<DiskFree> = Vec::new();
    for row in out.lines().skip(1) {
        let f: Vec<&str> = row.split_whitespace().collect();
        let (Some(total), Some(free)) = (f.get(1).and_then(|n| n.parse::<u64>().ok()), f.get(3).and_then(|n| n.parse::<u64>().ok())) else {
            continue;
        };
        let volume = f.get(5..).map(|v| v.join(" ")).unwrap_or_default();
        if volume.is_empty() || disks.iter().any(|d| d.volume == volume) {
            continue;
        }
        disks.push(DiskFree { volume, free_bytes: free * 1024, total_bytes: total * 1024 });
    }
    disks
}

#[cfg(test)]
mod tests {
    use super::*;

    const SYSCTL: &str = "vm.loadavg: { 2.47 2.10 1.98 }\n\
        vm.swapusage: total = 1024.00M  used = 43.19M  free = 980.81M  (encrypted)\n\
        kern.memorystatus_vm_pressure_level: 2\n";
    const DF: &str = "Filesystem     1024-blocks      Used Available Capacity  Mounted on\n\
        /dev/disk3s1s1   482797652  13436052 203818460     7%    /\n\
        /dev/disk5s1    1953378304 900000000 1053378304    46%    /Volumes/External Disk\n\
        /dev/disk3s1s1   482797652  13436052 203818460     7%    /\n";
    const PMSET: &str = "Note: No thermal warning level has been recorded\n\
        Note: No performance warning level has been recorded\n";

    #[test]
    fn the_three_answers_make_the_facts() {
        let r = assemble(SYSCTL, DF, Some(PMSET));
        assert!(r.missing.is_empty(), "{:?}", r.missing);
        assert_eq!(r.facts.load_centi, 247);
        assert_eq!((r.facts.load5_centi, r.facts.load15_centi), (Some(210), Some(198)));
        assert_eq!(r.facts.speed_limit_pct, Some(100), "no limit recorded is 100");
        assert_eq!((r.facts.hid_idle_secs, r.facts.free_memory_bytes, r.facts.pageouts), (None, None, None));
        assert_eq!(r.facts.memory, MemoryPressure::Warn);
        assert_eq!(r.facts.swap_used_bytes, (43.19f64 * 1024.0 * 1024.0).round() as u64);
        assert_eq!(r.facts.thermal, Thermal::Nominal);
        assert_eq!(
            r.facts.disks,
            vec![
                DiskFree { volume: "/".into(), free_bytes: 203818460 * 1024, total_bytes: 482797652 * 1024 },
                DiskFree {
                    volume: "/Volumes/External Disk".into(),
                    free_bytes: 1053378304 * 1024,
                    total_bytes: 1953378304 * 1024
                },
            ]
        );
        assert!(r.facts.cpus >= 1);
    }

    #[test]
    fn a_fact_the_host_does_not_answer_is_named_missing() {
        let r = assemble("", "", None);
        assert_eq!(r.missing, vec!["load", "swap", "memory", "disks", "thermal"]);
        assert_eq!(r.facts.load_centi, 0);
        assert_eq!((r.facts.load5_centi, r.facts.load15_centi, r.facts.speed_limit_pct), (None, None, None));
    }

    #[test]
    fn a_speed_limit_is_carried_as_read() {
        let r = assemble(SYSCTL, DF, Some("CPU_Speed_Limit \t= 62\n"));
        assert_eq!(r.facts.speed_limit_pct, Some(62));
        assert_eq!(r.facts.thermal, Thermal::Serious);
        assert_eq!(parse_loads("{ 1.00 }"), vec![1.0], "a short answer gives what it has");
    }

    #[test]
    fn a_speed_limit_bands_the_thermal_state() {
        let at = |n: u32| parse_thermal(&format!("CPU_Speed_Limit \t= {n}\n"));
        assert_eq!(at(100), Thermal::Nominal);
        assert_eq!(at(85), Thermal::Fair);
        assert_eq!(at(60), Thermal::Serious);
        assert_eq!(at(30), Thermal::Critical);
    }

    #[test]
    fn linux_facts_come_from_proc() {
        let meminfo = "MemTotal:       16000000 kB\nMemFree:  100 kB\nMemAvailable:    1600000 kB\n\
            SwapTotal:       2097148 kB\nSwapFree:        2096124 kB\n";
        let r = assemble_linux("1.25 0.80 0.50 2/345 6789\n", meminfo, DF);
        assert_eq!(r.missing, vec!["thermal"], "Linux has no one thermal state");
        assert_eq!(r.facts.load_centi, 125);
        assert_eq!((r.facts.load5_centi, r.facts.load15_centi), (Some(80), Some(50)));
        assert_eq!(r.facts.swap_used_bytes, 1024 * 1024);
        assert_eq!(r.facts.memory, MemoryPressure::Warn, "10% available");
        assert_eq!(r.facts.disks.len(), 2);
        assert_eq!(pressure_of_available(4, 100), MemoryPressure::Critical);
        assert_eq!(pressure_of_available(50, 100), MemoryPressure::Normal);
        assert_eq!(assemble_linux("", "", "").missing, vec!["load", "swap", "memory", "disks", "thermal"]);
    }

    #[test]
    fn the_throttle_facts_on_linux_are_read_out_of_meminfo_and_vmstat() {
        let mut r = assemble_linux("0.5 0.4 0.3 1/200 99\n", "", DF);
        let meminfo = "MemTotal: 16000000 kB\nMemAvailable:    1600000 kB\n";
        throttle_facts_linux(&mut r.facts, Some(meminfo), Some("pgpgin 1\npswpout 4000\npswpin 2\n"));
        assert_eq!(r.facts.free_memory_bytes, Some(1600000 * 1024));
        assert_eq!(r.facts.pageouts, Some(4000));
        assert_eq!(r.facts.hid_idle_secs, None);
        let mut bare = assemble_linux("", "", "");
        throttle_facts_linux(&mut bare.facts, Some("nothing"), None);
        assert_eq!((bare.facts.free_memory_bytes, bare.facts.pageouts), (None, None));
    }

    /// The live host answers all its asks but thermal.
    #[cfg(target_os = "linux")]
    #[test]
    fn this_linux_answers_every_fact_but_thermal() {
        let r = read(&InProcess::default(), &[std::env::temp_dir()]);
        assert_eq!(r.missing, vec!["thermal"]);
        assert!(r.facts.disks[0].total_bytes > 0);
        assert!(r.facts.free_memory_bytes.is_some());
    }

    #[test]
    fn swap_reads_each_unit() {
        assert_eq!(parse_swap_used("total = 2.00G  used = 512.00K  free = 1.99G"), Some(512 * 1024));
        assert_eq!(parse_swap_used("total = 2.00G  used = 1.50G  free = 0.50G"), Some(3 * 512 * 1024 * 1024));
        assert_eq!(parse_swap_used("nonsense"), None);
    }

    #[test]
    fn the_throttle_facts_are_read_out_of_ioreg_and_vm_stat() {
        let mut r = assemble(SYSCTL, DF, Some(PMSET));
        let ioreg = "    | |   \"HIDIdleTime\" = 125000000000\n    | |   \"Other\" = 1\n";
        let vm = "Mach Virtual Memory Statistics: (page size of 16384 bytes)\n\
            Pages free:                               2919.\n\
            Pageouts:                                 1000.\n";
        throttle_facts(&mut r.facts, Some(ioreg), Some(vm));
        assert_eq!(r.facts.hid_idle_secs, Some(125));
        assert_eq!(r.facts.free_memory_bytes, Some(2919 * 16384));
        assert_eq!(r.facts.pageouts, Some(4000), "in 4 KB pages");
        let mut bare = assemble(SYSCTL, DF, Some(PMSET));
        throttle_facts(&mut bare.facts, None, Some("nothing"));
        assert_eq!((bare.facts.hid_idle_secs, bare.facts.free_memory_bytes, bare.facts.pageouts), (None, None, None));
        assert!(bare.missing.is_empty(), "an optional fact is not named missing");
    }

    /// The live host answers every ask: the beat carries real facts, the
    /// throttle's included.
    #[cfg(target_os = "macos")]
    #[test]
    fn this_mac_answers_every_fact() {
        let r = read(&InProcess::default(), &[std::env::temp_dir()]);
        assert!(r.missing.is_empty(), "missing: {:?}", r.missing);
        assert_eq!(r.facts.disks.len(), 1);
        assert!(r.facts.disks[0].total_bytes > 0);
        let f = &r.facts;
        assert!(f.load5_centi.is_some() && f.load15_centi.is_some() && f.speed_limit_pct.is_some(), "{f:?}");
        assert!(f.hid_idle_secs.is_some(), "ioreg's HID idle: {f:?}");
        assert!(f.free_memory_bytes.is_some_and(|b| b > 0), "vm_stat's free pages: {f:?}");
        assert!(f.pageouts.is_some(), "vm_stat's pageouts: {f:?}");
    }
}
