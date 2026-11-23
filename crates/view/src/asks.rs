//! `flag --person --json` and `yours --json` over `State` (wsp-308).
//!
//! An [`Ask`] made from a message carries its record (`Message::to_json`) in
//! the `record` field, and `src/mirror.rs`'s `ask_facts` writes what the record
//! does not say: whether it is raised, the person's, wanting an answer, a
//! question for the person, its project and that project's chain, and what it
//! unblocks. Two readings that belong to no ask ride on the machine named
//! `local`: the count of rows the run closed unseen and the retro lines.
//! A project's chain on a review row is `project_chain` ([`key::CHAIN`]).

use serde_json::{json, Value};
use wsp_store_contract::{Ask, AskKind, State};

use crate::common::{self, key as row_key};

/// `Ask::fields` keys (and the two machine facts) the mirror writes.
pub mod key {
    /// `"true"` while the message needs attention (`raised`).
    pub const RAISED: &str = "raised";
    /// `"true"` for a raised hand that is the person's and not a reply.
    pub const PERSON: &str = "person";
    /// `"true"` when it wants answering (`yours`'s population, with `person`).
    pub const WANTS: &str = "wants";
    /// `"true"` for a question for the person, open or closed.
    pub const PERSON_Q: &str = "person_q";
    /// The item's project (`yours_scope`), absent for none.
    pub const PROJECT: &str = "item_project";
    /// That project and its ancestors, joined by `,`; on a review row, its own.
    pub const CHAIN: &str = "project_chain";
    /// What it unblocks, absent for nothing.
    pub const UNBLOCKS: &str = "unblocks";
    /// Machine fact: rows the run closed that nobody has looked at, a count.
    pub const UNSEEN: &str = "yours_unseen";
    /// Machine fact: the retro lines, a JSON array of strings.
    pub const RETRO: &str = "yours_retro";
}

/// How long a closed question stays in `yours --json`'s `closed`.
const CLOSED_FOR: i64 = 600;

fn record(a: &Ask) -> Option<Value> {
    if a.kind == AskKind::Flag {
        return None;
    }
    serde_json::from_str(a.fields.get("record")?).ok()
}

fn is(a: &Ask, k: &str) -> bool {
    a.fields.get(k).is_some_and(|v| v == "true")
}

fn at(rec: &Value) -> String {
    rec["at"].as_str().unwrap_or_default().to_string()
}

/// Messages passing `keep`, as `(record, ask)`, oldest first (`message::raised`'s order).
fn raised<'a>(state: &'a State, keep: impl Fn(&Ask) -> bool) -> Vec<(Value, &'a Ask)> {
    let mut out: Vec<(Value, &Ask)> =
        state.asks.values().filter(|a| is(a, key::RAISED) && keep(a)).filter_map(|a| record(a).map(|r| (r, a))).collect();
    out.sort_by(|x, y| at(&x.0).cmp(&at(&y.0)).then_with(|| x.0["id"].as_str().cmp(&y.0["id"].as_str())));
    out
}

/// `wsp flag --person --json`: the person's open set, newest first.
pub fn flags_person(state: &State, _now: i64) -> Value {
    let rows: Vec<Value> = raised(state, |a| is(a, key::PERSON)).into_iter().rev().map(|(r, _)| r).collect();
    Value::Array(rows)
}

fn opt(a: &Ask, k: &str) -> Value {
    a.fields.get(k).filter(|v| !v.is_empty()).map(|v| Value::from(v.as_str())).unwrap_or(Value::Null)
}

fn item(rec: &Value, a: &Ask) -> Value {
    json!({
        "id": rec["id"],
        "asked": rec["text"].as_str().unwrap_or("").lines().next().unwrap_or("").trim(),
        "caption": rec["caption"],
        "text": rec["text"],
        "about": rec["about"],
        "project": opt(a, key::PROJECT),
        "from": from_byline(rec),
        "at": rec["at"],
        "unblocks": opt(a, key::UNBLOCKS),
        "waiting": rec["waiting"],
        "for": rec["for"],
        "unrouted": rec["unrouted"],
        "choices": rec["choices"],
    })
}

fn in_project(a: &Ask, project: Option<&str>) -> bool {
    match project {
        None => true,
        Some(p) => a.fields.get(key::CHAIN).is_some_and(|c| c.split(',').any(|x| x == p)),
    }
}

/// `wsp yours --json`; `project` is the id the CLI resolved, if one was asked.
pub fn yours(state: &State, now: i64, project: Option<&str>) -> Value {
    let items: Vec<Value> = raised(state, |a| is(a, key::PERSON) && is(a, key::WANTS) && in_project(a, project))
        .iter()
        .map(|(r, a)| item(r, a))
        .collect();
    let review = state
        .rows
        .values()
        .filter(|r| common::status(r) == "review")
        .filter(|r| match project {
            None => true,
            Some(p) => r.fields.get(row_key::PROJECT).is_some_and(|x| !x.is_empty())
                && r.fields.get(key::CHAIN).is_some_and(|c| c.split(',').any(|x| x == p)),
        })
        .count();
    let local = state.machines.values().find(|m| m.facts.contains_key(key::UNSEEN));
    let unseen: i64 = local.and_then(|m| m.facts.get(key::UNSEEN)).and_then(|v| v.parse().ok()).unwrap_or(0);
    let retro: Value = local.and_then(|m| m.facts.get(key::RETRO)).and_then(|v| serde_json::from_str(v).ok()).unwrap_or_else(|| json!([]));
    let mut closed: Vec<(Value, Value, &Ask)> = state
        .asks
        .values()
        .filter(|a| is(a, key::PERSON_Q) && in_project(a, project))
        .filter_map(|a| {
            let rec = record(a)?;
            let c = rec.get("closed")?.clone();
            let when = common::epoch_of(c["at"].as_str().unwrap_or_default());
            (when > 0 && common::since(c["at"].as_str().unwrap_or_default(), now) <= CLOSED_FOR).then(|| (rec, c, a))
        })
        .collect();
    closed.sort_by(|a, b| b.1["at"].as_str().cmp(&a.1["at"].as_str()));
    let closed: Vec<Value> = closed
        .into_iter()
        .map(|(r, c, a)| {
            let mut v = item(&r, a);
            v["closed"] = c;
            v
        })
        .collect();
    json!({
        "project": project,
        "items": items,
        "review": review,
        "closed_by_the_run": unseen,
        "retro": retro,
        "closed": closed,
    })
}

/// `Party::byline`, read off the record's `from`.
fn from_byline(rec: &Value) -> String {
    let f = &rec["from"];
    let s = |k: &str| f[k].as_str().unwrap_or_default();
    if !s("pane").is_empty() {
        s("pane").to_string()
    } else if !s("seat").is_empty() {
        format!("{} seat", s("seat"))
    } else if !s("agent").is_empty() {
        s("agent").to_string()
    } else {
        "a person".into()
    }
}
