//! `wsp throttle --json` as a pure function: the readings and the settings'
//! answers are plain inputs the CLI gathers; nothing here reads the machine.

use serde_json::{json, Value};

/// The memory readings, and the text of the memory hold if one is up.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Memory {
    pub free_percent: Option<u32>,
    /// Swap used and total, in MB.
    pub swap: Option<(u32, u32)>,
    pub swap_growth_mb_min: Option<i64>,
    pub pageouts_min: Option<u32>,
    /// The memory hold's text, when memory is what is holding.
    pub held_text: Option<String>,
}

/// `memory` as `wsp throttle --json` carries it.
pub fn memory_json(m: &Memory) -> Value {
    json!({
        "free_percent": m.free_percent,
        "swap_used_mb": m.swap.map(|s| s.0),
        "swap_total_mb": m.swap.map(|s| s.1),
        "swap_growth_mb_per_min": m.swap_growth_mb_min,
        "pageouts_per_min": m.pageouts_min,
        "held": m.held_text.is_some(),
        "held_text": m.held_text,
    })
}

/// Everything the throttle's JSON reports, already worked out.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Throttle {
    pub mode: String,
    pub in_force: bool,
    pub idle_secs: Option<u64>,
    pub cores: usize,
    pub slots: usize,
    pub jobs: usize,
    pub load: Option<(f64, f64, f64)>,
    pub load_warning: bool,
    /// [`memory_json`]'s value (null where it is not read).
    pub memory: Value,
    pub cpu_speed_limit: Option<u32>,
    pub thermal_below: u32,
    pub thermally_limited: bool,
    pub hot_slots: usize,
    pub hot_jobs: usize,
    /// Only when thermally limited.
    pub suggested_agents: Option<usize>,
    pub agents: Option<usize>,
    /// Each build's holder and slot.
    pub builds: Vec<(Value, Option<usize>)>,
}

pub fn json(t: &Throttle) -> Value {
    json!({
        "mode": t.mode,
        "in_force": t.in_force,
        "idle_secs": t.idle_secs,
        "cores": t.cores,
        "slots": t.slots,
        "jobs": t.jobs,
        "load": t.load.map(|(a, b, c)| json!([a, b, c])),
        "load_warning": t.load_warning,
        "memory": t.memory,
        "cpu_speed_limit": t.cpu_speed_limit,
        "thermal_below": t.thermal_below,
        "thermally_limited": t.thermally_limited,
        "hot_slots": t.hot_slots,
        "hot_jobs": t.hot_jobs,
        "suggested_agents": t.suggested_agents,
        "agents": t.agents,
        "builds": t.builds.iter().map(|(h, s)| json!({"holder": h, "slot": s})).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_free_percentage_is_shown_and_gates_nothing() {
        let j = memory_json(&Memory {
            free_percent: Some(32),
            swap: Some((4096, 5120)),
            swap_growth_mb_min: Some(120),
            pageouts_min: Some(1_500),
            held_text: None,
        });
        assert_eq!(j["free_percent"], 32);
        assert_eq!(j["swap_used_mb"], 4096);
        assert_eq!(j["swap_total_mb"], 5120);
        assert_eq!(j["swap_growth_mb_per_min"], 120);
        assert_eq!(j["pageouts_per_min"], 1_500);
        assert_eq!(j["held"], false);
    }
}
