//! `machine ls --json` (and the machine object `add`, `show` and `set` emit):
//! pure functions over plain values the CLI has already read. The live facts
//! of the fleet come in as inputs, so nothing here asks a daemon or a socket.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use wsp_store_contract::{Machine, State, DEFAULT_LEASE};

/// `Machine::facts` keys, as `src/mirror.rs`'s `machines` writes them (wsp-308).
/// A record's fact is absent when the file leaves it empty; the live ones are
/// written only while the daemon has a record of the machine, so `reachable`
/// present is what says `live` is an object and not null.
pub mod key {
    pub const SSH: &str = "ssh";
    pub const OS: &str = "os";
    pub const ARCH: &str = "arch";
    pub const BACKEND_AT: &str = "backend_at";
    pub const EXEC_DIR: &str = "exec_dir";
    pub const SUP_AT: &str = "sup_at";
    pub const AGENTS: &str = "agents";
    pub const LEASE: &str = "lease";
    pub const STATUS: &str = "status";
    pub const ADDED: &str = "added";
    pub const REACHABLE: &str = "reachable";
    pub const LAST_SEEN: &str = "last_seen";
    pub const HEARD: &str = "heard";
    /// JSON object, seat -> stamp.
    pub const LEASES: &str = "leases";
    pub const TUNNEL: &str = "tunnel";
    pub const SOCKET: &str = "socket";
    pub const WIRE: &str = "wire";
    pub const WIRE_OLDEST: &str = "wire_oldest";
    pub const READY: &str = "ready";
    pub const CHECKED: &str = "checked";
    pub const CHECK_ERROR: &str = "check_error";
    pub const SHIM_WARNING: &str = "shim_warning";
    pub const ERROR: &str = "error";
    pub const TICK_SECS: &str = "tick_secs";
    pub const BEAT_AT: &str = "beat_at";
    /// On the machine the mirror names `local` only: this seat's hostname,
    /// its cap (absent for none) and the agents counted against it (JSON array).
    pub const SEAT: &str = "seat";
    pub const SEAT_CAP: &str = "seat_cap";
    pub const SEAT_WORKING: &str = "seat_working";
}

/// The contract's `link` word, as every machine object carries it.
pub use wsp_store_contract::LINK_REQUIRED as LINK;

/// A machine's durable record, as the file says it.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Record {
    pub name: String,
    pub ssh: String,
    pub os: String,
    pub arch: String,
    pub backend_at: String,
    pub exec_dir: String,
    pub sup_at: String,
    /// `None` is no cap: null in the JSON, never 0.
    pub agents: Option<usize>,
    /// The lease in seconds, the default already applied.
    pub lease: u32,
    pub status: String,
    pub added: String,
}

/// What the daemon last saw of a machine.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Live {
    pub reachable: bool,
    pub last_seen: String,
    pub heard: String,
    pub leases: BTreeMap<String, String>,
    pub tunnel: String,
    pub socket: String,
    pub wire: Option<u32>,
    pub wire_oldest: Option<u32>,
    pub ready: bool,
    pub checked: String,
    pub check_error: String,
    pub shim_warning: String,
    pub error: String,
    pub tick_secs: i64,
    pub beat_at: String,
}

impl Live {
    pub fn json(&self) -> Value {
        json!({
            "reachable": self.reachable,
            "last_seen": self.last_seen,
            "heard": self.heard,
            "leases": self.leases,
            "tunnel": self.tunnel,
            "socket": self.socket,
            "wire": self.wire,
            "wire_oldest": self.wire_oldest,
            "ready": self.ready,
            "checked": self.checked,
            "check_error": self.check_error,
            "shim_warning": self.shim_warning,
            "error": self.error,
            "tick_secs": self.tick_secs,
            "beat_at": self.beat_at,
        })
    }
}

/// One agent as the list draws it: which pane, what it is, what it holds.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Standing {
    /// The machine it is on; `""` is this seat.
    pub machine: String,
    pub pane: String,
    pub agent: String,
    pub state: String,
    pub holding: String,
}

/// Everything `machine ls --json` draws.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Fleet {
    pub machines: Vec<(Record, Option<Live>)>,
    pub agents: Vec<Standing>,
    pub seat: String,
    pub seat_cap: Option<usize>,
    pub seat_working: Vec<String>,
}

/// One machine's object, without its agents.
pub fn machine_json(m: &Record, live: Option<&Live>) -> Value {
    json!({
        "name": m.name,
        "ssh": m.ssh,
        "os": m.os,
        "arch": m.arch,
        "backend_at": m.backend_at,
        "exec_dir": m.exec_dir,
        "sup_at": m.sup_at,
        // Null and not 0 when there is no cap: a governor asking for the
        // number wants to tell "nobody has decided" from a number, and `0`
        // is the one answer that reads as an instruction.
        "agents": m.agents,
        "lease": m.lease,
        "link": LINK,
        "status": m.status,
        "added": m.added,
        "live": live.map(Live::json),
    })
}

/// The agents on `machine` (`""` is this seat), in the order given.
pub fn agents_on<'a>(all: &'a [Standing], machine: &str) -> Vec<&'a Standing> {
    all.iter().filter(|s| s.machine == machine).collect()
}

fn agents_json(rows: &[&Standing]) -> Value {
    json!(rows
        .iter()
        .map(|s| json!({ "pane": s.pane, "agent": s.agent, "state": s.state, "holding": s.holding }))
        .collect::<Vec<_>>())
}

/// `machine ls --json`.
pub fn list_json(f: &Fleet) -> Value {
    let rows: Vec<_> = f
        .machines
        .iter()
        .map(|(m, live)| {
            let mut v = machine_json(m, live.as_ref());
            v["agents"] = agents_json(&agents_on(&f.agents, &m.name));
            v
        })
        .collect();
    json!({
        "seat": f.seat,
        // Null and not 0 with no cap, for `machine_json`'s reason.
        "seat_cap": f.seat_cap,
        "seat_working": f.seat_working,
        "machines": rows,
        "seat_agents": agents_json(&agents_on(&f.agents, "")),
    })
}

fn fact<'a>(m: &'a Machine, k: &str) -> &'a str {
    m.facts.get(k).map(String::as_str).unwrap_or("")
}

fn num<T: std::str::FromStr>(m: &Machine, k: &str) -> Option<T> {
    fact(m, k).parse().ok()
}

fn record_of(m: &Machine) -> Record {
    Record {
        name: m.name.0.clone(),
        ssh: fact(m, key::SSH).to_string(),
        os: fact(m, key::OS).to_string(),
        arch: fact(m, key::ARCH).to_string(),
        backend_at: fact(m, key::BACKEND_AT).to_string(),
        exec_dir: fact(m, key::EXEC_DIR).to_string(),
        sup_at: fact(m, key::SUP_AT).to_string(),
        agents: num(m, key::AGENTS),
        lease: num(m, key::LEASE).unwrap_or(DEFAULT_LEASE),
        status: fact(m, key::STATUS).to_string(),
        added: fact(m, key::ADDED).to_string(),
    }
}

/// The live facts of a machine, `None` when the daemon has no record of it.
fn live_of(m: &Machine) -> Option<Live> {
    m.facts.get(key::REACHABLE)?;
    Some(Live {
        reachable: fact(m, key::REACHABLE) == "true",
        last_seen: fact(m, key::LAST_SEEN).to_string(),
        heard: fact(m, key::HEARD).to_string(),
        leases: serde_json::from_str(fact(m, key::LEASES)).unwrap_or_default(),
        tunnel: fact(m, key::TUNNEL).to_string(),
        socket: fact(m, key::SOCKET).to_string(),
        wire: num(m, key::WIRE),
        wire_oldest: num(m, key::WIRE_OLDEST),
        ready: fact(m, key::READY) == "true",
        checked: fact(m, key::CHECKED).to_string(),
        check_error: fact(m, key::CHECK_ERROR).to_string(),
        shim_warning: fact(m, key::SHIM_WARNING).to_string(),
        error: fact(m, key::ERROR).to_string(),
        tick_secs: num(m, key::TICK_SECS).unwrap_or(0),
        beat_at: fact(m, key::BEAT_AT).to_string(),
    })
}

/// `machine ls --json` over `State` alone: the machines that are records (they
/// carry `ssh`), by name, and the seat's line from the machine the mirror names
/// `local`. No agents are listed: nothing fills them in the CLI either.
pub fn ls_of_state(state: &State, _now: i64) -> Value {
    let seat = state.machines.values().find(|m| m.facts.contains_key(key::SEAT));
    let f = Fleet {
        machines: state
            .machines
            .values()
            .filter(|m| m.facts.contains_key(key::SSH))
            .map(|m| (record_of(m), live_of(m)))
            .collect(),
        agents: Vec::new(),
        seat: seat.map(|m| fact(m, key::SEAT).to_string()).unwrap_or_default(),
        seat_cap: seat.and_then(|m| num(m, key::SEAT_CAP)),
        seat_working: seat.and_then(|m| serde_json::from_str(fact(m, key::SEAT_WORKING)).ok()).unwrap_or_default(),
    };
    list_json(&f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_cap_is_null_and_agents_land_on_their_machine() {
        let rec = Record { name: "mb2".into(), lease: 180, ..Record::default() };
        let f = Fleet {
            machines: vec![(rec, None)],
            agents: vec![
                Standing { machine: "mb2".into(), pane: "p1".into(), ..Standing::default() },
                Standing { pane: "p2".into(), ..Standing::default() },
            ],
            seat: "h".into(),
            ..Fleet::default()
        };
        let v = list_json(&f);
        assert_eq!(v["seat_cap"], Value::Null);
        assert_eq!(v["machines"][0]["agents"][0]["pane"], "p1");
        assert_eq!(v["seat_agents"][0]["pane"], "p2");
        assert_eq!(v["machines"][0]["live"], Value::Null);
        assert_eq!(v["machines"][0]["link"], "required");
    }
}
