//! A shared row moved to a new id leaves no ghost: its old subject is retired
//! on the hub (`refs.retire`, the hub's 19317ac and `docs/hub-deploy.md`
//! "Retiring a subject"), and a retirement another spoke made is followed here.
//!
//! **The mover's side.** `wsp mv` of a shared row X to Y renames the file and
//! records X → Y in `ids.json`; the hub still has X's head. A row X is *moved
//! away* here ([`moved_away`]) when the hub has a head for X, this spoke has
//! synced X before (its synced heads have X), X has no file here and
//! `ids.json` maps it to a row Y of the same workspace that is not
//! provisional. Nothing else records the pending retire: it is read again from
//! `ids.json` and the hub's heads at every turn, so it outlasts an outage and a
//! crash. Such an X is never kept as a head again ([`crate::hub_cache::classify`]
//! is given it to pass over), Y's first record names X's head as its parent
//! (so X's history is reached through Y), and once Y is pushed the turn
//! retires X from the head it saw, continuing as Y ([`retire_moved`]):
//!
//! - `retired`, or stale with no head (retired already): X's synced head,
//!   cache entry and journal are forgotten here;
//! - stale with another head (someone changed X since): retried from the
//!   hub's head at the next turn (their change to X is not merged into Y: see
//!   the report of this change);
//! - `held` (another user, or this user on another machine, holds X's lease):
//!   kept waiting, said once in the log, retried each turn;
//! - `invalid` (Y has no head on the hub yet): retried once Y is pushed;
//! - `forbidden`, `too_large`: said once and not asked again
//!   (`$WSP_STATE/hub/retire.json`'s `refused`).
//!
//! **Everyone's side** ([`follow_retired`]): every `retired` entry of each
//! `sync.heads` reply (the durable record, past event retention) is applied:
//! a stub of X goes (file, cache entry, synced head), a whole X with nothing
//! unsent goes, X → Y is recorded so `wsp show X` finds Y, and X's journal is
//! dropped. A whole X with edits not yet on the hub is moved to Y where Y is
//! not here (its edits then reach the hub as Y's), and is kept, unpushed and
//! said, where Y is here too. A `ref.retired` event on the stream wakes a turn.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use wsp_engine::door::Heads;
use wsp_engine::store::Store;
use wsp_objects::{ObjectId, Refs, WorkspaceId};
use wsp_store_contract::{RowId, Subject};

use crate::hub_client::{Client, Error as HubError};

/// `$WSP_STATE/hub/retire.json`: retires the hub refused for good (X → why),
/// not asked again.
fn path(store: &Store) -> std::path::PathBuf {
    store.state.join("hub").join("retire.json")
}

fn refused(store: &Store) -> BTreeMap<String, String> {
    let v: Value = std::fs::read_to_string(path(store)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
    v["refused"].as_object().into_iter().flatten().filter_map(|(k, w)| Some((k.clone(), w.as_str()?.to_string()))).collect()
}

fn refuse(store: &Store, x: &str, why: &str) {
    let mut r = refused(store);
    r.insert(x.to_string(), why.to_string());
    let p = path(store);
    let _ = std::fs::create_dir_all(p.parent().expect("under hub/"));
    let _ = std::fs::write(&p, json!({ "refused": r }).to_string());
}

fn row(id: &str) -> Subject {
    Subject::Row(RowId(id.to_string()))
}

/// The rows of `hub` this spoke moved away: `(X, X's hub head, Y)`.
pub fn moved_away(store: &Store, ws: &str, hub: &Refs) -> Vec<(String, ObjectId, String)> {
    let renamed = store.renamed_ids();
    if renamed.is_empty() {
        return Vec::new();
    }
    let heads = Heads::read(store);
    let provisional = crate::hub_ids::Book::read(store).rows;
    let mut out = Vec::new();
    for (subject, head) in &hub.heads {
        let Subject::Row(RowId(x)) = subject else { continue };
        let Some(y) = renamed.get(x) else { continue };
        if store.task_path(x).exists() || heads.head(subject).is_none() || provisional.contains_key(y) {
            continue;
        }
        if store.task(y).is_some_and(|t| t.workspace.to_string() == ws) {
            out.push((x.clone(), *head, y.clone()));
        }
    }
    out
}

/// The subjects [`moved_away`] names, for the classification to pass over.
pub fn skip(store: &Store, ws: &str, hub: &Refs) -> BTreeSet<Subject> {
    moved_away(store, ws, hub).into_iter().map(|(x, _, _)| row(&x)).collect()
}

/// The parent Y's first record takes: the head X had, where Y is a row this
/// spoke moved X to and X's head is one it has synced and holds.
pub fn first_parent(store: &Store, y: &str) -> Option<ObjectId> {
    let x = store.renamed_ids().into_iter().find(|(_, to)| to == y).map(|(from, _)| from)?;
    Heads::read(store).head(&row(&x)).copied()
}

/// X is gone from the hub: its synced head, cache entry and journal here go.
fn forget(store: &Store, x: &str) {
    let subject = row(x);
    let mut heads = Heads::read(store);
    heads.remove(&subject);
    let _ = heads.write(store);
    crate::hub_cache::drop_head(store, x);
    let journal = crate::hub_journal::Journal::of(store);
    if let Some((_, entries)) = journal.by_subject().remove(&subject) {
        for e in &entries {
            journal.remove(e);
        }
    }
}

/// After `ws`'s push: retire each row [`moved_away`] names in `hub` (the heads
/// before the push), continuing as its new id. `Err` is the hub not
/// answering; each other answer is handled as the module's doc says, `say`
/// given what the log is told (once per text, by the caller).
pub fn retire_moved(store: &Store, client: &mut Client, ws: &str, hub: &Refs, say: &mut dyn FnMut(String)) -> Result<Vec<(String, String)>, HubError> {
    let wid = WorkspaceId(ws.to_string());
    let refused_before = refused(store);
    let mut done = Vec::new();
    for (x, head, y) in moved_away(store, ws, hub) {
        if refused_before.contains_key(&x) {
            continue;
        }
        match client.refs_retire(&wid, row(&x), head, Some(row(&y))) {
            Ok(Ok(_)) | Ok(Err(None)) => {
                forget(store, &x);
                done.push((x, y));
            }
            Ok(Err(Some(_))) => say(format!("hub-sync: {x} (moved here to {y}) changed on the hub since; it is retired from its new head at the next turn")),
            Err(e) if crate::hub_sync::unreachable(&e) => return Err(e),
            Err(HubError::Refused(wsp_store_contract::Refusal::Held { holder, .. }, text)) => {
                let who = match crate::hub_lease::other_machine(&text) {
                    Some(m) if Some(&holder) == Some(client.by()) => format!("you on your other machine {m}"),
                    _ => wsp_engine::door::user_of(&holder),
                };
                say(format!("hub-sync: {x} is moved here to {y}, but {x} is leased by {who}: it is retired on the hub once they let go, and is tried again each turn"));
            }
            Err(HubError::Failed(f)) if f.code == wsp_hub_protocol::Code::Invalid => {
                say(format!("hub-sync: {x} (moved here to {y}) waits for {y} to reach the hub before it is retired: {}", f.text));
            }
            Err(e) => {
                let why = e.to_string();
                say(format!("hub-sync: {x} (moved here to {y}) was not retired on the hub, and is not asked again: {why}"));
                refuse(store, &x, &why);
            }
        }
    }
    Ok(done)
}

/// The retirements `hub` lists, applied here (see the module's doc); what was
/// done, in words for the log.
pub fn follow_retired(store: &Store, hub: &Refs) -> Vec<String> {
    let mut said = Vec::new();
    let renamed = store.renamed_ids();
    for (subject, successor) in &hub.retired {
        let Subject::Row(RowId(x)) = subject else { continue };
        let y = match successor {
            Some(Subject::Row(RowId(y))) => Some(y.clone()),
            _ => None,
        };
        let file = store.task_path(x);
        let mut keep_x = false;
        if file.exists() {
            let heads = Heads::read(store);
            let digest = std::fs::read(&file).ok().map(|b| ObjectId::of(&b));
            let head_only = crate::hub_cache::is_head(store, x);
            let unsent = !head_only && heads.file(subject) != digest;
            match (unsent, &y) {
                (false, _) => {
                    let _ = std::fs::remove_file(&file);
                    store.wrote(file.clone());
                }
                (true, Some(y)) if !store.task_path(y).exists() => {
                    // Its edits go on as Y's: the rename moves the file and records X → Y.
                    let _ = store.rename_tasks(&BTreeMap::from([(x.clone(), y.clone())]));
                    said.push(format!("hub-sync: {x} was moved on the hub to {y}; its edits here not yet sent go on as {y}'s"));
                }
                (true, _) => {
                    keep_x = true;
                    said.push(format!(
                        "hub-sync: {x} was retired on the hub{}, but its edits here were never sent: they are kept in {x}, which is no longer pushed; bring them over by hand",
                        y.as_ref().map(|y| format!(" (it continues as {y})")).unwrap_or_default()
                    ));
                }
            }
        }
        if !keep_x {
            forget(store, x);
        } else {
            crate::hub_cache::drop_head(store, x);
        }
        if let Some(y) = &y {
            if !renamed.contains_key(x) && !store.task_path(x).exists() {
                let _ = store.rename_tasks(&BTreeMap::from([(x.clone(), y.clone())]));
            }
        }
    }
    said
}

/// The subjects `hub` lists as retired: never pushed nor synced.
pub fn retired(hub: &Refs) -> BTreeSet<Subject> {
    hub.retired.keys().cloned().collect()
}
