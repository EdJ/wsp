//! The host's facts for the manager's beat (`docs/agent-manager.md` 3.2's
//! `beat`): load, memory pressure, swap, disk free per volume and thermal
//! state, read in three asks of [`InProcess`] (one `sysctl`, one `df`, one
//! `pmset`) and turned into `wsp-host-api`'s [`HostFacts`].
//!
//! [`HostFacts`] has no "unknown" in its fields, so a fact this host does not
//! answer is reported at its quiet value (load 0, memory `normal`, swap 0,
//! thermal `nominal`) and named in [`Read::missing`] for the reader's log.

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
    assemble(&sys, &df, therm.as_deref())
}

/// The facts out of the three answers.
pub fn assemble(sysctl: &str, df: &str, pmset: Option<&str>) -> Read {
    let mut missing = Vec::new();
    let load_centi = match sysctl_value(sysctl, "vm.loadavg").and_then(parse_load1) {
        Some(l) => (l * 100.0).round() as u32,
        None => {
            missing.push("load");
            0
        }
    };
    let swap_used_bytes = match sysctl_value(sysctl, "vm.swapusage").and_then(parse_swap_used) {
        Some(b) => b,
        None => {
            missing.push("swap");
            0
        }
    };
    let memory = match sysctl_value(sysctl, "kern.memorystatus_vm_pressure_level").and_then(parse_pressure) {
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
    let thermal = match pmset {
        Some(out) => parse_thermal(out),
        None => {
            missing.push("thermal");
            Thermal::Nominal
        }
    };
    let cpus = std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(1);
    Read { facts: HostFacts { load_centi, cpus, memory, swap_used_bytes, disks, thermal }, missing }
}

/// The value of `name` in `sysctl`'s `name: value` lines.
pub fn sysctl_value<'a>(out: &'a str, name: &str) -> Option<&'a str> {
    out.lines().find_map(|l| l.strip_prefix(name)?.strip_prefix(':').map(str::trim))
}

/// The one-minute load out of `{ 0.50 0.40 0.30 }`.
pub fn parse_load1(s: &str) -> Option<f64> {
    s.split(|c: char| c.is_whitespace() || c == '{' || c == '}').find(|w| !w.is_empty())?.parse().ok()
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
    let limit = out
        .lines()
        .find(|l| l.trim_start().starts_with("CPU_Speed_Limit"))
        .and_then(|l| l.split('=').nth(1))
        .and_then(|v| v.trim().parse::<u32>().ok());
    match limit {
        None => Thermal::Nominal,
        Some(n) if n >= 100 => Thermal::Nominal,
        Some(n) if n >= 80 => Thermal::Fair,
        Some(n) if n >= 50 => Thermal::Serious,
        Some(_) => Thermal::Critical,
    }
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
        /dev/disk5s1    1953378304 900000000 1053378304    46%    /Volumes/Crucial X9\n\
        /dev/disk3s1s1   482797652  13436052 203818460     7%    /\n";
    const PMSET: &str = "Note: No thermal warning level has been recorded\n\
        Note: No performance warning level has been recorded\n";

    #[test]
    fn the_three_answers_make_the_facts() {
        let r = assemble(SYSCTL, DF, Some(PMSET));
        assert!(r.missing.is_empty(), "{:?}", r.missing);
        assert_eq!(r.facts.load_centi, 247);
        assert_eq!(r.facts.memory, MemoryPressure::Warn);
        assert_eq!(r.facts.swap_used_bytes, (43.19f64 * 1024.0 * 1024.0).round() as u64);
        assert_eq!(r.facts.thermal, Thermal::Nominal);
        assert_eq!(
            r.facts.disks,
            vec![
                DiskFree { volume: "/".into(), free_bytes: 203818460 * 1024, total_bytes: 482797652 * 1024 },
                DiskFree {
                    volume: "/Volumes/Crucial X9".into(),
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
    fn swap_reads_each_unit() {
        assert_eq!(parse_swap_used("total = 2.00G  used = 512.00K  free = 1.99G"), Some(512 * 1024));
        assert_eq!(parse_swap_used("total = 2.00G  used = 1.50G  free = 0.50G"), Some(3 * 512 * 1024 * 1024));
        assert_eq!(parse_swap_used("nonsense"), None);
    }

    /// The live host answers all three asks: the beat carries real facts.
    #[cfg(target_os = "macos")]
    #[test]
    fn this_mac_answers_every_fact() {
        let r = read(&InProcess::default(), &[std::env::temp_dir()]);
        assert!(r.missing.is_empty(), "missing: {:?}", r.missing);
        assert_eq!(r.facts.disks.len(), 1);
        assert!(r.facts.disks[0].total_bytes > 0);
    }
}
