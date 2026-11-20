//! `machine ls --json` (and the machine object `add`, `show` and `set` emit):
//! pure functions over plain values the CLI has already read. The live facts
//! of the fleet come in as inputs, so nothing here asks a daemon or a socket.

use std::collections::BTreeMap;

use serde_json::{json, Value};

/// The contract's `link` word, as every machine object carries it.
pub use wsp_store_contract::LINK_REQUIRED as LINK;

/// A machine's durable record, as the file says it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Standing {
    /// The machine it is on; `""` is this seat.
    pub machine: String,
    pub pane: String,
    pub agent: String,
    pub state: String,
    pub holding: String,
}

/// Everything `machine ls --json` draws.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
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
