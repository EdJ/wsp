//! The readings (wsp-743): each census read's inputs that the CLI works out in
//! `src/` and no contract entity carries, under a name, as the JSON text of
//! this crate's own input structs. The daemon writes them (`src/readings.rs`)
//! through the contract's `Readings`, the API's `readings` verb serves them,
//! and a reader decodes one with [`of`] and hands it to the projection the CLI
//! uses, so it draws what the CLI prints without working anything out.
//!
//! | name          | shape          | drawn by                                   | beside                 |
//! |---------------|----------------|--------------------------------------------|------------------------|
//! | [`TASKS`]     | [`Tasks`]      | `task::ls_json_row` per id of `order`      | `rows {heads: true}`   |
//! | [`WIP`]       | `wip::Census`  | `wip::census_json`                         |                        |
//! | [`WORKLISTS`] | `[Listing]`    | `worklist::list_json`                      |                        |
//! | [`MACHINES`]  | `machine::Fleet` | `machine::list_json`                     |                        |
//! | [`THROTTLE`]  | `throttle::Throttle` | `throttle::json`                     |                        |
//! | [`SPEND`]     | [`Spend`]      | `spend::report(lines, places, landed, now)` | the reader's clock    |
//!
//! A reading is as fresh as its `at`: the daemon takes each again on its own
//! period, so it may trail the entities by that much.

use std::collections::{BTreeMap, HashMap};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use crate::spend::{Line, Place};
use crate::task::{Leg, Stage};

/// `ls --json --all`'s inputs beside the rows: the order, the stages, the legs.
pub const TASKS: &str = "tasks";
/// `wip --json` whole.
pub const WIP: &str = "wip";
/// `worklist ls --json` (and `list ls --json`).
pub const WORKLISTS: &str = "worklists";
/// `machine ls --json`.
pub const MACHINES: &str = "machines";
/// `throttle --json`, of the machine the store is on.
pub const THROTTLE: &str = "throttle";
/// `spend --json`'s inputs; the clock is the reader's.
pub const SPEND: &str = "spend";

/// Every name the daemon writes.
pub const ALL: [&str; 6] = [TASKS, WIP, WORKLISTS, MACHINES, THROTTLE, SPEND];

/// What `ls --json --all` needs beyond the rows' heads, which carry each
/// task's record (`common::task_json`). Every row of `ls --json --all` is
/// `task::ls_json_row(task_json(row), stages.get(id), legs.get(id))`, in
/// `order`. It lists the live tasks only (the archive is `find --all`'s), as
/// the mirror projects them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tasks {
    /// Every id `ls --json --all` lists, in its order (status, then
    /// priority, then id).
    pub order: Vec<String>,
    /// Each listed task's stage, by id; absent where it has none.
    pub stages: BTreeMap<String, Stage>,
    /// The legs of each task that has a plan of them, by id; absent where
    /// `ls` prints no `legs` key.
    pub legs: BTreeMap<String, Vec<Leg>>,
}

/// `spend::report`'s inputs but the clock.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Spend {
    pub lines: Vec<Line>,
    /// Kept ordered, so the same places are the same text.
    pub places: BTreeMap<String, Place>,
    pub landed: usize,
}

impl Spend {
    /// `places` as `spend::report` takes it.
    pub fn places(&self) -> HashMap<String, Place> {
        self.places.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }
}

/// A reading's text as its shape; `None` where it does not decode.
pub fn of<T: DeserializeOwned>(text: &str) -> Option<T> {
    serde_json::from_str(text).ok()
}

/// A shape as a reading's text.
pub fn text<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_default()
}
