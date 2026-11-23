//! `flag --person --json` and `yours --json` (wsp-745): what host-core's
//! `fetch_flags` reads, drawn from [`Flags`].
//!
//! The inputs are the records and the few values the CLI works out beside
//! them: each hand's record is `Message::to_json` (its derived keys, `caption`,
//! `for`, `closed`, `unrouted`, already in it, as `messages.json` and
//! `events.jsonl` carry them), and an item adds its headline, the asker's
//! byline, its project and what answering it unblocks. Those four read the
//! rows, the lists and the running lists, which this crate does not, so the
//! caller works them out (`src/cmd_message.rs`'s `yours_reading`).

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// One of the person's questions as `yours` lists it: the record and what
/// the CLI works out beside it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Item {
    /// The message's record, `Message::to_json`.
    pub record: Value,
    /// Its headline, `Message::title`.
    pub asked: String,
    /// Who asked, `Party::byline`.
    pub from: String,
    /// The project of what it is about; none for a list with no live member.
    pub project: Option<String>,
    /// What answering it unblocks: a list's group, a list, or the row.
    pub unblocks: Option<String>,
}

/// A question of the person's closed lately, with how it ended
/// (`Message::closed_json`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Closed {
    pub item: Item,
    pub closed: Value,
}

/// Both reads' inputs. `person` is `flag --person`'s; the rest are
/// `yours`'s, for `project` (none is every project).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Flags {
    /// Every hand raised to the person, replies left out, oldest first, each
    /// its record.
    pub person: Vec<Value>,
    pub project: Option<String>,
    /// The open questions only the person may close, oldest first.
    pub items: Vec<Item>,
    /// How many rows wait at `review`.
    pub review: usize,
    /// How many rows the run closed that the person has not looked at.
    pub closed_by_the_run: usize,
    /// The retro lines (`retro_yours::yours_lines`).
    pub retro: Vec<String>,
    /// The person's questions closed in the last ten minutes, newest first.
    pub closed: Vec<Closed>,
}

/// `flag --person --json`: the hands, newest first.
pub fn person_json(person: &[Value]) -> Value {
    Value::Array(person.iter().rev().cloned().collect())
}

/// An item as `yours --json` gives it.
pub fn item_json(i: &Item) -> Value {
    let r = &i.record;
    json!({
        "id": r["id"],
        "asked": i.asked,
        "caption": r["caption"],
        "text": r["text"],
        "about": r["about"],
        "project": i.project,
        "from": i.from,
        "at": r["at"],
        "unblocks": i.unblocks,
        "waiting": r["waiting"],
        "for": r["for"],
        "unrouted": r["unrouted"],
        // Null for a question with none, which is today's.
        "choices": r["choices"],
    })
}

/// A closed question as `yours --json`'s `closed` lists it: the item, and how
/// it ended.
pub fn closed_json(c: &Closed) -> Value {
    let mut v = item_json(&c.item);
    v["closed"] = c.closed.clone();
    v
}

/// `yours --json`.
pub fn yours_json(f: &Flags) -> Value {
    json!({
        "project": f.project,
        "items": f.items.iter().map(item_json).collect::<Vec<_>>(),
        "review": f.review,
        "closed_by_the_run": f.closed_by_the_run,
        "retro": f.retro,
        // The person's questions closed in the last ten minutes, so a
        // reader can tell *answered in the pane* from *gone*.
        "closed": f.closed.iter().map(closed_json).collect::<Vec<_>>(),
    })
}
