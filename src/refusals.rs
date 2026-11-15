//! The count of a seat's refused deliveries (`wsp-634`), kept apart from
//! `wake` so the reconciler (`repair`) and `wsp wip` can read it without
//! reaching up into the seats layer.

use crate::store::Store;
use crate::util;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Where the scopes whose seat's supervisor keeps answering `failed` are
/// counted: scope → `{ n, since, why }` (wsp-634). A seat that refuses every
/// tick is a governor that is deaf, and nothing else says so.
const REFUSED_FILE: &str = "refused-deliveries.json";

/// How many consecutive ticks a seat may refuse a tell before the daemon ends it
/// so the reconciler seats a successor (wsp-634). Five: a refusal is the
/// supervisor's answer after its own four tries, so five of those is minutes.
pub(crate) const REFUSED_TICKS: u64 = 5;

fn refused_all(store: &Store) -> BTreeMap<String, Value> {
    std::fs::read_to_string(store.state.join(REFUSED_FILE))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn refused_write(store: &Store, all: &BTreeMap<String, Value>) {
    let _ = std::fs::create_dir_all(&store.state);
    let _ = std::fs::write(store.state.join(REFUSED_FILE), json!(all).to_string());
}

/// A refusal on this scope's seat, counted. Returns how many in a row, and the
/// reason the one before it gave (so a retry can say what it is retrying after).
pub(crate) fn refuse(store: &Store, scope: &str, why: &str) -> (u64, Option<String>) {
    let mut all = refused_all(store);
    let was = all.get(scope).cloned().unwrap_or(Value::Null);
    let n = was.get("n").and_then(Value::as_u64).unwrap_or(0) + 1;
    let since = was.get("since").and_then(Value::as_i64).unwrap_or_else(util::epoch_secs);
    let before = was.get("why").and_then(Value::as_str).map(str::to_string);
    all.insert(scope.to_string(), json!({ "n": n, "since": since, "why": why }));
    refused_write(store, &all);
    (n, before)
}

/// The seat took a tell, or nothing is owed it: the count ends.
pub(crate) fn accepted(store: &Store, scope: &str) {
    let mut all = refused_all(store);
    if all.remove(scope).is_some() {
        refused_write(store, &all);
    }
}

/// The scopes whose seat has refused delivery: scope → (ticks in a row, the last
/// reason). Read by `wsp wip` and by the reconciler.
pub(crate) fn refusing(store: &Store) -> BTreeMap<String, (u64, String)> {
    refused_all(store)
        .into_iter()
        .map(|(scope, v)| {
            let n = v.get("n").and_then(Value::as_u64).unwrap_or(0);
            (scope, (n, v.get("why").and_then(Value::as_str).unwrap_or_default().to_string()))
        })
        .filter(|(_, (n, _))| *n > 0)
        .collect()
}
