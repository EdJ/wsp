//! `wsp ls --json` and `wsp show --json` (wsp-305): a task's record off its
//! contract [`Row`] ([`task_json`]), and what each verb adds to it.
//!
//! What the CLI parses out of a task's prose (the passes, the legs, the
//! pieces, the stage and its timeline) arrives as the plain structs below;
//! this module owns the shape each is printed in, and nothing here reads a
//! file or the clock. The key order is the order the CLI always printed.

use serde_json::{json, Value};
use wsp_store_contract::Row;

use crate::common::task_json;

/// A stage's caption (`wsp-242`): the word wsp derives and the tail, as the
/// `stage` key prints them. A pass's `caption` is the same shape.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Told {
    pub word: String,
    pub tail: Option<String>,
    pub text: String,
    pub from: String,
    /// Empty where nothing dated it.
    pub at: String,
}

impl Told {
    pub fn json(&self) -> Value {
        let mut v = json!({ "word": self.word, "text": self.text, "from": self.from });
        // Absent where nothing dates the step, on the same rule as `tail`.
        if !self.at.is_empty() {
            v["at"] = json!(self.at);
        }
        if let Some(t) = &self.tail {
            v["tail"] = json!(t);
        }
        v
    }
}

/// One step of a task's timeline: the stage word it entered, and when.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Step {
    pub word: String,
    /// Empty where nothing dated it.
    pub at: String,
    pub tail: Option<String>,
}

impl Step {
    pub fn json(&self) -> Value {
        let mut v = json!({ "word": self.word });
        if !self.at.is_empty() {
            v["at"] = json!(self.at);
        }
        if let Some(t) = &self.tail {
            v["tail"] = json!(t);
        }
        v
    }
}

/// Where a task is and how it got there: the stage now and the steps before.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stage {
    pub now: Told,
    pub steps: Vec<Step>,
}

/// Since when the stage has held: the last step's time when it is under the
/// live word, else the later of the last step and the record's own.
pub fn since(now: &Told, steps: &[Step]) -> String {
    match steps.last() {
        Some(last) if last.word == now.word && !last.at.is_empty() => last.at.clone(),
        Some(last) => last.at.clone().max(now.at.clone()),
        None => now.at.clone(),
    }
}

/// The stage as `ls`, `find`, `show` and `worklist show` give it: the
/// caption's own fields and [`since`], absent on the same rule as `at`.
pub fn stage_json(s: &Stage) -> Value {
    let mut v = s.now.json();
    let since = since(&s.now, &s.steps);
    if !since.is_empty() {
        v["since"] = json!(since);
    }
    v
}

/// One check run a verifier did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ran {
    pub command: String,
    pub test: bool,
    pub outcome: String,
}

/// One file a verifier read, with the line ranges.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRead {
    pub path: String,
    pub lines: Vec<(u64, u64)>,
}

/// What a verifier did (`wsp-325`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Did {
    pub ran: Vec<Ran>,
    pub ran_more: usize,
    pub drove: Vec<String>,
    pub files: Vec<FileRead>,
    pub files_more: usize,
    pub diffs: Vec<String>,
    pub diffs_more: usize,
}

impl Did {
    fn json(&self) -> Value {
        json!({
            "ran": self.ran.iter().map(|c| json!({ "command": c.command, "test": c.test, "outcome": c.outcome })).collect::<Vec<_>>(),
            "ran_more": self.ran_more,
            "drove": self.drove,
            "read": {
                "files": self.files.iter().map(|f| json!({ "path": f.path, "lines": f.lines.iter().map(|(a, b)| json!([a, b])).collect::<Vec<_>>() })).collect::<Vec<_>>(),
                "files_more": self.files_more,
                "diffs": self.diffs,
                "diffs_more": self.diffs_more,
            },
        })
    }
}

/// One verifier pass (`wsp-241`). `caption` is set on a pass that has a
/// verdict (holds or blocks) and absent on one still reading or superseded.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pass {
    pub state: String,
    pub verdict: String,
    pub caption: Option<Told>,
    pub at: String,
    pub decided: Option<String>,
    pub read: Option<String>,
    pub landed: Option<String>,
    pub pane: Option<String>,
    pub did: Option<Did>,
    pub checked: Vec<(String, bool)>,
}

/// The passes as `wsp show --json` gives them, oldest first.
pub fn verification_json(passes: &[Pass]) -> Value {
    json!(passes
        .iter()
        .map(|p| json!({
            "state": p.state,
            "verdict": p.verdict,
            "caption": p.caption.as_ref().map(Told::json),
            "at": p.at,
            "decided": p.decided,
            "read": p.read,
            "landed": p.landed,
            "pane": p.pane,
            "did": p.did.as_ref().map(Did::json),
            "checked": p.checked.iter().map(|(item, held)| json!({ "item": item, "held": held })).collect::<Vec<_>>(),
        }))
        .collect::<Vec<_>>())
}

/// What a done leg cost (compound-561 A5).
#[derive(Clone, Debug, PartialEq)]
pub struct Spent {
    /// Null where the record kept nothing to price.
    pub usd: Option<f64>,
    pub turns: usize,
    pub peak: u64,
    pub seconds: i64,
}

/// One leg of a task's plan.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Leg {
    pub spent: Option<Spent>,
    pub name: String,
    pub agent: String,
    pub default: bool,
    pub why: Option<String>,
    pub state: String,
    pub ready: bool,
    pub after: Vec<String>,
    pub pane: Option<String>,
    pub tree: Option<String>,
    pub caption: Option<String>,
    pub skipped: Option<String>,
    pub started: Option<String>,
    pub ended: Option<String>,
    pub order: String,
    pub baton: String,
    pub rework_of: Option<Vec<String>>,
}

/// The legs as `wsp show --json` gives them.
pub fn legs_json(legs: &[Leg]) -> Value {
    json!(legs
        .iter()
        .map(|l| json!({
            "spent": match &l.spent {
                None => Value::Null,
                Some(s) => json!({ "usd": s.usd, "turns": s.turns, "peak": s.peak, "seconds": s.seconds }),
            },
            "name": l.name,
            "agent": l.agent,
            "default": l.default,
            "why": l.why,
            "state": l.state,
            "ready": l.ready,
            "after": l.after,
            "pane": l.pane,
            "tree": l.tree,
            "caption": l.caption,
            "skipped": l.skipped,
            "started": l.started,
            "ended": l.ended,
            "order": l.order,
            "baton": l.baton,
            "rework_of": l.rework_of,
        }))
        .collect::<Vec<_>>())
}

/// One piece of a task (`docs/pieces.md`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Piece {
    pub n: usize,
    /// `<row>-<n>`.
    pub id: String,
    pub title: String,
    pub batch: usize,
    pub state: String,
    pub seat: Option<String>,
    pub tree: Option<String>,
    pub caption: Option<String>,
}

/// One piece for `--json`.
pub fn piece_json(p: &Piece) -> Value {
    json!({
        "n": p.n, "id": p.id, "title": p.title, "batch": p.batch,
        "state": p.state, "seat": p.seat, "tree": p.tree, "caption": p.caption,
    })
}

/// A row's pieces for `--json`.
pub fn pieces_json(pieces: &[Piece]) -> Value {
    Value::Array(pieces.iter().map(piece_json).collect())
}

/// A task in a running list: `{list, group, of}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Running {
    pub list: String,
    pub group: usize,
    pub of: usize,
}

/// One `ls --json` row: the record, whether it is filed away, and its stage.
pub fn ls_row(row: &Row, archived: bool, stage: Option<&Stage>) -> Value {
    let mut v = task_json(row);
    v["archived"] = json!(archived);
    // `wsp-274`: and since when, off the same timeline `show` gives.
    v["stage"] = json!(stage.map(stage_json));
    v
}

/// `ls --json`: the rows, pretty.
pub fn ls(rows: &[Value]) -> String {
    serde_json::to_string_pretty(rows).unwrap_or_default()
}

/// What `show --json` adds to the record, all of it read by the caller.
pub struct Show<'a> {
    pub body: &'a str,
    /// Where it is filed, when it is no longer live.
    pub archived: Option<String>,
    pub claim: Option<Value>,
    pub worked: Option<Value>,
    pub list: Option<Running>,
    /// Each open ask raised about it, already in its own JSON.
    pub raised: Vec<Value>,
    pub verification: Vec<Pass>,
    pub legs: Vec<Leg>,
    pub pieces: Vec<Piece>,
    pub stage: Option<Stage>,
}

/// `show --json`, pretty.
pub fn show(row: &Row, s: &Show) -> String {
    let mut v = task_json(row);
    v["body"] = json!(s.body);
    if let Some(path) = &s.archived {
        v["archived"] = json!(path);
    }
    v["claim"] = s.claim.clone().unwrap_or(Value::Null);
    v["worked"] = s.worked.clone().unwrap_or(Value::Null);
    if let Some(l) = &s.list {
        v["list"] = json!({ "list": l.list, "group": l.group, "of": l.of });
    }
    v["raised"] = json!(s.raised);
    v["verification"] = verification_json(&s.verification);
    v["legs"] = legs_json(&s.legs);
    v["pieces"] = pieces_json(&s.pieces);
    v["stage"] = json!(s.stage.as_ref().map(stage_json));
    v["stages"] = json!(s.stage.as_ref().map(|g| g.steps.iter().map(Step::json).collect::<Vec<_>>()));
    serde_json::to_string_pretty(&v).unwrap_or_default()
}
