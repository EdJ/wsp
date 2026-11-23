//! `worklist ls --json` (and the object every worklist verb emits): pure
//! functions over plain values the CLI has already read.
//!
//! A worklist's phase, position and queue come from the store's files and the
//! machine's seats, so the caller works them out and hands the answers in as
//! [`Head`], [`Listing`] and [`Queue`]; this module only draws them.

use serde_json::{json, Value};

use wsp_store_contract::{List, State};

use crate::common::iso_at;

/// The keys `mirror::lists` writes into [`List::fields`] beside `title`,
/// `caption`, `status`, `prio`, `after`, `created` and `planned` (wsp-308).
pub mod key {
    /// `caption::of_worklist`'s `from`.
    pub const CAPTION_FROM: &str = "caption_from";
    /// The status word `ls` prints (`executing`, `planned`, ...).
    pub const PHASE: &str = "phase";
    pub const HELD: &str = "held";
    pub const WAITING_ON_ED: &str = "waiting_on_ed";
    pub const PARKED: &str = "parked";
    /// The record's own word, trimmed.
    pub const STORED: &str = "stored";
    /// How many groups there are.
    pub const GROUPS: &str = "group_count";
    /// The group the run is at, `""` for none.
    pub const AT: &str = "at";
    /// JSON array of `{name, kind, file, present}`.
    pub const OUTPUTS: &str = "outputs";
    pub const OUTPUTS_DIR: &str = "outputs_dir";
    pub const SEGMENT: &str = "segment";
    /// The place `ls` draws it at (segment, then newest activity, then id).
    pub const ORDER: &str = "order";
    pub const OPEN: &str = "open";
    /// JSON array of member ids.
    pub const GONE: &str = "gone";
    /// JSON array of group ordinals.
    pub const UNWRITTEN: &str = "unwritten";
    /// Epoch seconds.
    pub const ACTIVITY: &str = "activity";
    /// `waits::Prio::word`.
    pub const PRIO_WORD: &str = "prio_word";
    /// One-based queue place, present for a planned list only.
    pub const QUEUE: &str = "queue";
    /// What it waits on, `""` where it may start; with `queue`.
    pub const WAITS: &str = "waits";
}

/// One declared output of a worklist and whether its file is there.
#[derive(Clone, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Output {
    pub name: String,
    pub kind: String,
    pub file: String,
    pub present: bool,
}

/// The object every worklist verb emits, before `project` is added.
#[derive(Clone, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Head {
    pub id: String,
    pub title: String,
    pub caption_text: String,
    pub caption_from: String,
    /// The status word (`executing`, ...).
    pub status: String,
    pub held: bool,
    pub waiting_on_ed: bool,
    pub parked: bool,
    /// The word on the record, trimmed.
    pub stored: String,
    pub planned: Option<String>,
    pub created: String,
    /// How many groups there are.
    pub groups: usize,
    /// The group the run is at, if any.
    pub at: Option<usize>,
    pub outputs: Vec<Output>,
    pub outputs_dir: String,
}

/// Where a planned list queues and what it waits on.
#[derive(Clone, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Queue {
    /// One-based place in the queue.
    pub n: usize,
    /// The text of what it waits on; `None` may start.
    pub waits: Option<String>,
}

/// One row of `worklist ls --json`: the head and the facts the listing adds.
#[derive(Clone, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Listing {
    pub head: Head,
    pub segment: String,
    pub open: usize,
    pub gone: Vec<String>,
    pub unwritten: Vec<usize>,
    /// When anything last happened, epoch seconds.
    pub activity: i64,
    pub prio: String,
    pub after: Option<String>,
    /// Present for a planned list only.
    pub queue: Option<Queue>,
}

/// The worklist object where the position is already in hand.
pub fn head_json(h: &Head) -> Value {
    json!({
        "id": h.id,
        "title": h.title,
        "caption": json!({ "text": h.caption_text, "from": h.caption_from }),
        "status": h.status,
        "flags": json!({ "held": h.held, "waiting_on_ed": h.waiting_on_ed, "parked": h.parked }),
        "stored": h.stored,
        "planned": h.planned,
        "created": h.created,
        "groups": h.groups,
        "at": h.at,
        "outputs": h.outputs.iter().map(|o| json!({ "name": o.name, "kind": o.kind, "file": o.file, "present": o.present })).collect::<Vec<_>>(),
        "outputs_dir": h.outputs_dir,
    })
}

/// One row for a parser: the head and the facts a listing adds to it.
pub fn listed_json(l: &Listing) -> Value {
    let mut out = head_json(&l.head);
    if let Some(o) = out.as_object_mut() {
        o.insert("segment".into(), json!(l.segment));
        o.insert("open".into(), json!(l.open));
        o.insert("gone".into(), json!(l.gone));
        o.insert("unwritten".into(), json!(l.unwritten));
        o.insert("activity".into(), json!(iso_at(l.activity)));
        o.insert("prio".into(), json!(l.prio));
        o.insert("after".into(), json!(l.after));
        // A planned list says where it queues and what it waits on; the rest
        // carry neither key (`wsp-383`).
        if let Some(q) = &l.queue {
            o.insert("queue".into(), json!(q.n));
            o.insert("may_start".into(), json!(q.waits.is_none()));
            o.insert("waits".into(), json!(q.waits));
        }
    }
    out
}

/// `worklist ls --json`: every list, no collapse and no `--all`.
pub fn list_json(listed: &[Listing]) -> Value {
    Value::Array(listed.iter().map(listed_json).collect())
}

fn f<'a>(l: &'a List, k: &str) -> &'a str {
    l.fields.get(k).map(String::as_str).unwrap_or("")
}

fn flag(l: &List, k: &str) -> bool {
    f(l, k) == "true"
}

fn json_of(l: &List, k: &str) -> Vec<Value> {
    serde_json::from_str(f(l, k)).unwrap_or_default()
}

/// The [`Head`] a list's fields say.
pub fn head_of_list(l: &List) -> Head {
    let outputs = json_of(l, key::OUTPUTS);
    let s = |v: &Value, k: &str| v[k].as_str().unwrap_or("").to_string();
    Head {
        id: l.id.0.clone(),
        title: f(l, "title").into(),
        caption_text: f(l, "caption").into(),
        caption_from: f(l, key::CAPTION_FROM).into(),
        status: f(l, key::PHASE).into(),
        held: flag(l, key::HELD),
        waiting_on_ed: flag(l, key::WAITING_ON_ED),
        parked: flag(l, key::PARKED),
        stored: f(l, key::STORED).into(),
        planned: Some(f(l, "planned")).filter(|s| !s.is_empty()).map(str::to_string),
        created: f(l, "created").into(),
        groups: f(l, key::GROUPS).parse().unwrap_or(0),
        at: f(l, key::AT).parse().ok(),
        outputs: outputs
            .iter()
            .map(|o| Output { name: s(o, "name"), kind: s(o, "kind"), file: s(o, "file"), present: o["present"].as_bool().unwrap_or(false) })
            .collect(),
        outputs_dir: f(l, key::OUTPUTS_DIR).into(),
    }
}

/// The [`Listing`] a list's fields say.
pub fn listing_of_list(l: &List) -> Listing {
    Listing {
        head: head_of_list(l),
        segment: f(l, key::SEGMENT).into(),
        open: f(l, key::OPEN).parse().unwrap_or(0),
        gone: json_of(l, key::GONE).iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
        unwritten: json_of(l, key::UNWRITTEN).iter().filter_map(|v| v.as_u64().map(|n| n as usize)).collect(),
        activity: f(l, key::ACTIVITY).parse().unwrap_or(0),
        prio: f(l, key::PRIO_WORD).into(),
        after: Some(f(l, "after")).filter(|s| !s.is_empty()).map(str::to_string),
        queue: f(l, key::QUEUE).parse().ok().map(|n| Queue {
            n,
            waits: Some(f(l, key::WAITS)).filter(|s| !s.is_empty()).map(str::to_string),
        }),
    }
}

/// `worklist ls --json` over the State: every list in the order `ls` draws.
pub fn ls_of_state(state: &State, _now: i64) -> Value {
    let mut lists: Vec<&List> = state.lists.values().collect();
    lists.sort_by_key(|l| (f(l, key::ORDER).parse::<usize>().unwrap_or(usize::MAX), l.id.0.clone()));
    let listed: Vec<Listing> = lists.into_iter().map(listing_of_list).collect();
    list_json(&listed)
}

/// The object `worklist show <id> --json` carries under `"worklist"`, less
/// `project`: the head alone, from the State.
pub fn head_of_state(state: &State, id: &str) -> Option<Value> {
    state.lists.values().find(|l| l.id.0 == id).map(|l| head_json(&head_of_list(l)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head() -> Head {
        Head {
            id: "w".into(),
            title: "W".into(),
            caption_text: "W".into(),
            caption_from: "title".into(),
            status: "executing".into(),
            held: false,
            waiting_on_ed: false,
            parked: false,
            stored: "running".into(),
            planned: None,
            created: "2026-01-01T00:00:00Z".into(),
            groups: 2,
            at: Some(1),
            outputs: vec![],
            outputs_dir: "/x".into(),
        }
    }

    #[test]
    fn a_queued_list_carries_queue_may_start_and_waits_and_the_rest_do_not() {
        let mut l = Listing {
            head: head(),
            segment: "running".into(),
            open: 1,
            gone: vec![],
            unwritten: vec![],
            activity: 0,
            prio: "normal".into(),
            after: None,
            queue: None,
        };
        assert!(listed_json(&l).get("queue").is_none());
        l.queue = Some(Queue { n: 2, waits: Some("room: 1 slot short".into()) });
        let v = listed_json(&l);
        assert_eq!((v["queue"].clone(), v["may_start"].clone()), (json!(2), json!(false)));
        assert_eq!(v["waits"], "room: 1 slot short");
        assert_eq!(v["activity"], "1970-01-01T00:00:00Z");
    }
}
