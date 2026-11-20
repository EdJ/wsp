//! `wip --json`: the census document, shaped from plain values (wsp-305).
//!
//! The CLI reads the host (`kill(2)`, the managers, the disks) and turns its
//! files into the contract's records, then hands the results here: each seat
//! as an [`Agent`], the machine's lines as a [`Head`]. Nothing here reads
//! anything; the key order and the nulls are the document's, byte for byte.

use serde_json::{json, Value};

/// One seat's row, as `wip --json` publishes it. The parts that are their own
/// documents (`waiting`, `wait`, `role`, `stuck`, `leg`) arrive rendered.
#[derive(Clone, Debug, Default)]
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
#[derive(Clone, Debug, Default)]
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
