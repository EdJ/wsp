//! `wip --json`: the census document, shaped from plain values (wsp-305).
//!
//! The CLI reads the host (`kill(2)`, the managers, the disks) and turns its
//! files into the contract's records, then hands the results here: each seat
//! as an [`Agent`], the machine's lines as a [`Head`]. Nothing here reads
//! anything; the key order and the nulls are the document's, byte for byte.

use serde_json::{json, Value};

/// One seat's row, as `wip --json` publishes it. The parts that are their own
/// documents (`waiting`, `wait`, `role`, `stuck`, `leg`) arrive rendered.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Agent {
    pub project: String,
    pub task: String,
    pub task_id: String,
    pub pane: String,
    pub workspace: String,
    pub state: String,
    pub turning: bool,
    pub needs_you: bool,
    pub seat: Option<String>,
    pub said: Option<String>,
    pub held: Option<String>,
    pub since: Option<String>,
    pub direction: Option<String>,
    pub waiting: Option<Value>,
    pub wait: Option<Value>,
    /// Published only for a governor's seat; `null` for any other.
    pub members_waiting: usize,
    pub kind: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub role: Value,
    pub answers_to: Option<String>,
    pub answers_to_empty: Option<String>,
    pub spawned_by: Option<Value>,
    pub unsent: bool,
    pub queued: usize,
    pub dropped: bool,
    pub full_suite: bool,
    pub stuck: Option<Value>,
    pub was_governor: Option<String>,
    pub closed_row: Option<String>,
    pub leg: Option<Value>,
    /// Whether `waiting` is owed (counted in the heading's `waiting`).
    pub owed: bool,
}

/// The document around the seats: what the machine reported, and the queues.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Head {
    pub attention: Value,
    pub daemon: Option<String>,
    pub unreachable: Option<String>,
    pub disk: Option<String>,
    pub load: Option<String>,
    pub throttle: Option<String>,
    pub idle_memory: Value,
    /// The tasks at `blocked`, as `Task::json` renders them.
    pub blocked: Vec<Value>,
    /// The tasks at `review`.
    pub review: Vec<Value>,
    /// Open tasks of no project.
    pub inbox: usize,
}

/// One seat's object.
pub fn agent_json(r: &Agent) -> Value {
    json!({
        "project": r.project, "task": r.task, "task_id": r.task_id,
        "pane": r.pane, "workspace": r.workspace, "state": r.state,
        "turning": r.turning, "needs_you": r.needs_you, "seat": r.seat,
        "said": r.said,
        "held": r.held,
        "since": r.since,
        "direction": r.direction,
        "waiting": r.waiting,
        "wait": r.wait,
        "members_waiting": r.seat.as_ref().map(|_| r.members_waiting),
        "kind": r.kind, "model": r.model, "effort": r.effort,
        "role": r.role,
        "answers_to": r.answers_to, "answers_to_empty": r.answers_to_empty,
        "spawned_by": r.spawned_by,
        "unsent": r.unsent, "queued": r.queued, "dropped": r.dropped, "full_suite": r.full_suite,
        "stuck": r.stuck, "was_governor": r.was_governor, "closed_row": r.closed_row,
        "leg": r.leg,
    })
}

/// The whole `wip --json` document.
/// Everything `wip --json` prints: the seats and the head, and the two keys
/// the CLI adds after [`wip_json`] only when it has them (wsp-743: one
/// function, so a reader of the `wip` reading prints what the CLI does).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Census {
    pub agents: Vec<Agent>,
    pub head: Head,
    /// `room::waiting_lines`: rows waiting at the seat cap; the key is left
    /// out when there are none.
    pub waiting_at_cap: Vec<String>,
    /// The planned lists wsp started on its own lately (`pickup`), present
    /// when it has lines to say (wsp-384), else the key is left out.
    pub planned_started: Option<Vec<Value>>,
    /// Follow-ups found by a closed row that no list and no seat holds
    /// (`wsp-345`), each `{id, title, found_by}`; the key is left out when
    /// there are none.
    pub unheld_follow_ups: Vec<Value>,
}

/// `wip --json`, whole.
pub fn census_json(c: &Census) -> Value {
    let mut v = wip_json(&c.agents, &c.head);
    if let (Some(o), false) = (v.as_object_mut(), c.waiting_at_cap.is_empty()) {
        o.insert("waiting_at_cap".into(), json!(c.waiting_at_cap));
    }
    if let (Some(o), Some(started)) = (v.as_object_mut(), &c.planned_started) {
        o.insert("planned_started".into(), json!(started));
    }
    if let (Some(o), false) = (v.as_object_mut(), c.unheld_follow_ups.is_empty()) {
        o.insert("unheld_follow_ups".into(), json!(c.unheld_follow_ups));
    }
    v
}

pub fn wip_json(rows: &[Agent], head: &Head) -> Value {
    json!({
        "attention": head.attention,
        "agents": rows.iter().map(agent_json).collect::<Vec<_>>(),
        "turning": rows.iter().filter(|r| r.turning).count(),
        "needs_you": rows.iter().filter(|r| r.needs_you).count(),
        // Seats waiting on somebody: one waiting on its own shell says so on
        // its row and is nobody's to count (`wsp-214`).
        "waiting": rows.iter().filter(|r| r.owed).count(),
        // The sentence, not a boolean (`wsp-145`).
        "daemon": head.daemon,
        "unreachable": head.unreachable,
        "disk": head.disk,
        "load": head.load,
        "throttle": head.throttle,
        "idle_memory": head.idle_memory,
        "blocked": head.blocked,
        "review": head.review,
        "inbox": head.inbox,
    })
}

/// `wip --json` keys the CLI's mirror writes (wsp-308), beside `common::key`.
pub mod key {
    /// On a seat: the seat's object as [`super::agent_json`] prints it, JSON
    /// text. Present only on a seat `wip` draws.
    pub const AGENT: &str = "wip";
    /// On a seat: `"true"` when its `waiting` is owed (counted in the heading).
    pub const OWED: &str = "wip_owed";
    /// On this host's machine: the document's machine lines — `attention`,
    /// `daemon`, `unreachable`, `disk`, `load`, `throttle`, `idle_memory` —
    /// as one JSON object text.
    pub const HEAD: &str = "wip_head";
}

/// `wip --json` over `State` alone: the seats that carry [`key::AGENT`]
/// (sorted by project then pane, as the CLI draws them), the host machine's
/// [`key::HEAD`], and the queues derived from the rows. `_now` is the clock the
/// fold hands every projection; the seats' ages were worked out producer-side.
pub fn of_state(state: &wsp_store_contract::State, _now: i64) -> Value {
    use crate::common;
    let mut seats: Vec<(String, String, Value, bool)> = state
        .seats
        .values()
        .filter_map(|s| {
            let v: Value = serde_json::from_str(s.fields.get(key::AGENT)?).ok()?;
            let owed = s.fields.get(key::OWED).is_some_and(|o| o == "true");
            let text = |n: &str| v.get(n).and_then(Value::as_str).unwrap_or_default().to_string();
            Some((text("project"), text("pane"), v, owed))
        })
        .collect();
    seats.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    let head: Value = state
        .machines
        .values()
        .filter_map(|m| serde_json::from_str::<Value>(m.facts.get(key::HEAD)?).ok())
        .next()
        .unwrap_or(Value::Null);
    let h = |n: &str| head.get(n).cloned().unwrap_or(Value::Null);
    let count = |n: &str| seats.iter().filter(|s| s.2.get(n).and_then(Value::as_bool) == Some(true)).count();
    let queue = |word: &str| -> Vec<Value> {
        state.rows.values().filter(|r| common::status(r) == word).map(common::task_json).collect()
    };
    json!({
        "attention": h("attention"),
        "agents": seats.iter().map(|s| s.2.clone()).collect::<Vec<_>>(),
        "turning": count("turning"),
        "needs_you": count("needs_you"),
        "waiting": seats.iter().filter(|s| s.3).count(),
        "daemon": h("daemon"),
        "unreachable": h("unreachable"),
        "disk": h("disk"),
        "load": h("load"),
        "throttle": h("throttle"),
        "idle_memory": h("idle_memory"),
        "blocked": queue("blocked"),
        "review": queue("review"),
        "inbox": state.rows.values().filter(|r| common::project(r).is_none() && common::is_open(common::status(r))).count(),
    })
}
