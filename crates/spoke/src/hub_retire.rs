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
//! - stale with another head (someone changed X since): that head is brought
//!   into Y as below, and X is retired from it at the next turn;
//! - `held` (another user, or this user on another machine, holds X's lease):
//!   kept waiting, said once in the log, retried each turn;
//! - `invalid` (Y has no head on the hub yet): retried once Y is pushed;
//! - `forbidden`, `too_large`: said once and not asked again
//!   (`$WSP_STATE/hub/retire.json`'s `refused`).
//!
//! **Someone else's change to X while it was being moved** ([`bring_over`]).
//! Before X is retired, the hub's head of X must be one Y already carries: X's
//! synced head here records the last of X's heads taken into Y (at first the
//! one the move was made from). Where the hub has moved X past it, or answers
//! the retire `stale` with a head past it, the other side's change is merged
//! into Y as any divergence of one row is ([`crate::reconcile`]): the hub's
//! head of X, read as Y's record, is `theirs`, Y's file `ours`, and the base
//! their common ancestor (X's synced head on X's line); the door's
//! [`Merging`] writes the merge into Y's file, a field both sides changed
//! keeping the hub's value with Y's in a conflict note and the `conflict` tag.
//! Y's synced head stays what the hub has of Y, so the merge is pushed as
//! Y's next record; X's synced head becomes the head taken, and X is retired
//! from it at the next turn, once Y has the change on the hub.
//!
//! **Everyone's side** ([`follow_retired`]): every `retired` entry of each
//! `sync.heads` reply (the durable record, past event retention) is applied:
//! a stub of X goes (file, cache entry, synced head), a whole X with nothing
//! unsent goes, X → Y is recorded so `wsp show X` finds Y, and X's journal is
//! dropped. A whole X with edits not yet on the hub is moved to Y where Y is
//! not here (its edits then reach the hub as Y's). Where Y is here too (whole
//! with nothing unsent, or a stub), X's edits are merged into Y as a stale
//! push's are ([`crate::hub_sync::after_stale`]): X's file becomes Y's, with
//! X's synced head as Y's, and the hub's head of Y comes in through the door
//! with [`Merging`] over their common ancestor, so Y keeps both sides and the
//! merge is pushed as Y's. Where Y has edits of its own not yet sent, or the
//! merge cannot be made, X is kept, unpushed and said. A `ref.retired` event
//! on the stream wakes a turn.
//!
//! **A shared row removed** (`wsp rm`, which asks `--force` for one the hub
//! has, [`removing`]). The row is archived here as any; a row X is then
//! *removed away* ([`removed_away`]) when the hub has a head for it, this
//! spoke has synced it, it has no file here, no move records it and the
//! archive holds it. Like a move, nothing else records it: read again every
//! turn. It is never kept as a head meanwhile, and after the push it is
//! retired with no successor ([`retire_removed`]): `retired` forgets it here;
//! stale is tried again next turn; `held` waits, said; any other refusal is
//! said once and not asked again. Every spoke follows a retirement with no
//! successor: a stub goes, a synced copy is archived as `rm` archives (an
//! edit never sent goes into the archive with it, said in one line), and a
//! copy the hub never had from here (one recovered from the archive by hand)
//! stays, this machine's alone, never pushed under its id, `show` saying so
//! ([`show_line`], from `retire.json`'s `gone`).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use wsp_engine::door::Heads;
use wsp_engine::store::Store;
use wsp_objects::{ObjectId, Refs, WorkspaceId};
use wsp_store_contract::{RowId, Subject};

use wsp_engine::door::{Applied, Reconcile};
use wsp_objects::Object;
use wsp_store_contract::{Entity, Principal};

use crate::hub_client::{Client, Error as HubError};
use crate::hub_sync::ObjectDir;
use crate::reconcile::{Merging, NoLease, Objects};

/// `$WSP_STATE/hub/retire.json`: retires the hub refused for good (X → why),
/// not asked again.
fn path(store: &Store) -> std::path::PathBuf {
    store.state.join("hub").join("retire.json")
}

fn refused(store: &Store) -> BTreeMap<String, String> {
    let v: Value = std::fs::read_to_string(path(store)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
    v["refused"].as_object().into_iter().flatten().filter_map(|(k, w)| Some((k.clone(), w.as_str()?.to_string()))).collect()
}

fn read_doc(store: &Store) -> Value {
    std::fs::read_to_string(path(store)).ok().and_then(|t| serde_json::from_str(&t).ok()).filter(Value::is_object).unwrap_or_else(|| json!({}))
}

fn write_doc(store: &Store, doc: &Value) {
    let p = path(store);
    let _ = std::fs::create_dir_all(p.parent().expect("under hub/"));
    let tmp = p.with_extension(format!("json.{}.tmp", std::process::id()));
    if std::fs::write(&tmp, doc.to_string()).and_then(|_| std::fs::rename(&tmp, &p)).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

fn refuse(store: &Store, x: &str, why: &str) {
    let mut doc = read_doc(store);
    doc["refused"][x] = json!(why);
    write_doc(store, &doc);
}

/// The rows retired on the hub with no successor (removed) that this spoke
/// has followed: `retire.json`'s `gone`, for `show` to say a copy here is
/// this machine's alone.
pub fn gone(store: &Store) -> BTreeSet<String> {
    read_doc(store)["gone"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect()
}

fn mark_gone(store: &Store, x: &str) {
    let mut all = gone(store);
    if all.insert(x.to_string()) {
        let mut doc = read_doc(store);
        doc["gone"] = json!(all);
        write_doc(store, &doc);
    }
}

/// What `wsp rm` of `row` does on the hub, said before it is done: `None`
/// for a row the hub has never had (private, provisional, never synced).
pub fn removing(store: &Store, row: &str) -> Option<wsp_engine::addon::Removal> {
    let t = store.task(row)?;
    if t.workspace.is_private() || crate::hub_ids::is_provisional(store, row) || Heads::read(store).head(&self::row(row)).is_none() {
        return None;
    }
    let ws = t.workspace.to_string();
    Some(wsp_engine::addon::Removal {
        warning: format!("{row} is shared in {ws}: removing it retires it on the hub for everyone in {ws}, and it cannot be brought back there; your archived copy stays on this machine"),
        done: format!("{row} is retired on the hub for everyone in {ws} at the next sync; it cannot be brought back there"),
    })
}

/// The line `wsp show` prints for a row the hub retired with no successor.
pub fn show_line(store: &Store, row: &str) -> Option<String> {
    gone(store).contains(row).then(|| {
        format!("retired on the hub (removed for everyone in the workspace): this copy is this machine's alone, and is never pushed under {row}")
    })
}

/// The rows of `hub` this spoke removed (`wsp rm`, an archive): `(X, X's hub
/// head)` where the hub has a head for X, this spoke has synced X, X has no
/// file here, no move records it, and the archive holds X of this workspace.
/// Read again at every turn, like a move, so it outlasts an outage and a crash.
pub fn removed_away(store: &Store, ws: &str, hub: &Refs) -> Vec<(String, ObjectId)> {
    let heads = Heads::read(store);
    let renamed = store.renamed_ids();
    let candidates: Vec<(String, ObjectId)> = hub
        .heads
        .iter()
        .filter_map(|(s, h)| match s {
            Subject::Row(RowId(x)) => Some((x.clone(), *h)),
            _ => None,
        })
        .filter(|(x, _)| !store.task_path(x).exists() && heads.head(&row(x)).is_some() && !renamed.contains_key(x))
        .collect();
    if candidates.is_empty() {
        return Vec::new();
    }
    let archived: BTreeSet<String> = store.archived_tasks().into_iter().filter(|t| t.workspace.to_string() == ws).map(|t| t.id).collect();
    candidates.into_iter().filter(|(x, _)| archived.contains(x)).collect()
}

/// After `ws`'s push: retire each row [`removed_away`] names in `hub`, with no
/// successor, from the hub's head. `retired` (or stale with no head) forgets
/// it here; stale with another head is tried again next turn; `held` waits,
/// said; any other refusal is said once and not asked again. What was retired.
pub fn retire_removed(store: &Store, client: &mut Client, ws: &str, hub: &Refs, say: &mut dyn FnMut(String)) -> Result<Vec<String>, HubError> {
    let wid = WorkspaceId(ws.to_string());
    let refused_before = refused(store);
    let mut done = Vec::new();
    for (x, head) in removed_away(store, ws, hub) {
        if refused_before.contains_key(&x) {
            continue;
        }
        match client.refs_retire(&wid, row(&x), head, None) {
            Ok(Ok(_)) | Ok(Err(None)) => {
                forget(store, &x);
                mark_gone(store, &x);
                done.push(x);
            }
            Ok(Err(Some(_))) => {}
            Err(e) if crate::hub_sync::unreachable(&e) => return Err(e),
            Err(HubError::Refused(wsp_store_contract::Refusal::Held { holder, .. }, text)) => {
                let who = match crate::hub_lease::other_machine(&text) {
                    Some(m) if Some(&holder) == Some(client.by()) => format!("you on your other machine {m}"),
                    _ => wsp_engine::door::user_of(&holder),
                };
                say(format!("hub-sync: {x} is removed here, but {x} is leased by {who}: it is retired on the hub once they let go, and is tried again each turn"));
            }
            Err(e) => {
                let why = e.to_string();
                say(format!("hub-sync: {x} (removed here) was not retired on the hub, and is not asked again: {why}"));
                refuse(store, &x, &why);
            }
        }
    }
    Ok(done)
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

/// The subjects [`moved_away`] and [`removed_away`] name, for the
/// classification to pass over.
pub fn skip(store: &Store, ws: &str, hub: &Refs) -> BTreeSet<Subject> {
    let moved = moved_away(store, ws, hub).into_iter().map(|(x, _, _)| row(&x));
    moved.chain(removed_away(store, ws, hub).into_iter().map(|(x, _)| row(&x))).collect()
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
        // The hub has X past the last head Y took: that change into Y first,
        // and X retired from it once Y has it on the hub.
        if Heads::read(store).head(&row(&x)) != Some(&head) {
            say(bring_over(store, client, &x, head, &y)?);
            continue;
        }
        match client.refs_retire(&wid, row(&x), head, Some(row(&y))) {
            Ok(Ok(_)) | Ok(Err(None)) => {
                forget(store, &x);
                done.push((x, y));
            }
            Ok(Err(Some(theirs))) => say(bring_over(store, client, &x, theirs, &y)?),
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

/// The objects of the hub's line from `head` back to one held here, fetched
/// and kept: what a merge over the two lines' common ancestor reads.
fn fetch_line(client: &mut Client, objects: &ObjectDir, head: ObjectId) -> Result<(), HubError> {
    let mut at = Some(head);
    while let Some(id) = at.filter(|id| !objects.has(id)) {
        let Some(blob) = client.sync_get(vec![id])?.into_iter().find(|b| b.id == id) else { break };
        if wsp_objects::verify(&blob.id, &blob.bytes).is_err() || objects.put(&blob.id, &blob.bytes).is_err() {
            break;
        }
        at = Objects::object(objects, &id).and_then(|o| o.parent().copied());
    }
    Ok(())
}

/// `obj`, a row's record, as the record of row `to`: what a merge into `to` reads.
fn as_row(obj: Object, to: &str) -> Option<Object> {
    match obj {
        Object::Record { workspace, entity: Entity::Row(mut r), parent, by, at, .. } => {
            r.id = RowId(to.to_string());
            Some(Object::Record { workspace, subject: row(to), entity: Entity::Row(r), parent, by, at })
        }
        _ => None,
    }
}

/// X's head `theirs` on the hub, past the last one Y took, merged into Y (moved
/// here from X) as a divergence is (see the module's doc); what the log is told.
/// `Err` is the hub not answering.
pub fn bring_over(store: &Store, client: &mut Client, x: &str, theirs: ObjectId, y: &str) -> Result<String, HubError> {
    let objects = ObjectDir::of(store);
    let mut heads = Heads::read(store);
    let not_yet = |why: String| {
        format!("hub-sync: {x} changed on the hub after it was moved here to {y}, and the change is not in {y} yet ({why}); {x} is not retired until it is, and this is tried again each turn")
    };
    let Some(y_head) = heads.head(&row(y)).copied() else {
        return Ok(not_yet(format!("{y} is not on the hub yet")));
    };
    match fetch_line(client, &objects, theirs) {
        Err(e) if crate::hub_sync::unreachable(&e) => return Err(e),
        Err(e) => return Ok(not_yet(e.to_string())),
        Ok(()) => {}
    }
    let Some(record) = Objects::object(&objects, &theirs).and_then(|o| as_row(o, y)) else {
        return Ok(not_yet(format!("the hub's head {} of {x} was not fetched as a row", &theirs.hex()[..12])));
    };
    // The base is X's synced head: the last of X's line Y took.
    let taken = heads.head(&row(x)).copied();
    let merging = Merging { objects: &objects, lease: &NoLease };
    let said = match merging.reconcile(store, &record, theirs, taken) {
        Ok(Applied::Reconciled(text)) => text,
        Ok(other) => format!("{other:?}"),
        Err(r) => return Ok(not_yet(format!("{r:?}"))),
    };
    // Y's synced head stays the hub's, so the merge is pushed as Y's next
    // record; X's is the head now taken into Y, which X is retired from.
    heads = Heads::read(store);
    heads.set(&row(y), y_head, None);
    heads.set(&row(x), theirs, None);
    if let Err(e) = heads.write(store) {
        return Ok(not_yet(e.to_string()));
    }
    Ok(format!("hub-sync: {x} changed on the hub after it was moved here to {y}; the change is merged into {y} ({said}), and {x} is retired once {y} has it on the hub"))
}

/// X's edits here, not yet sent, into Y, which is here too (whole with nothing
/// unsent, or a stub), merged with the hub's head of Y as a stale push is
/// (see the module's doc): what the merge did, or why it was not made (Y and
/// X then as they were).
fn carry_into(store: &Store, client: &mut Client, hub: &Refs, x: &str, y: &str, by: &Principal) -> Result<Result<String, String>, HubError> {
    let objects = ObjectDir::of(store);
    let heads = Heads::read(store);
    let Some(synced) = heads.head(&row(x)).copied() else { return Ok(Err(format!("{x} was never synced here"))) };
    let Some(theirs) = hub.heads.get(&row(y)).copied() else { return Ok(Err(format!("{y} has no head on the hub"))) };
    let y_file = store.task_path(y);
    let y_bytes = std::fs::read(&y_file).ok();
    let y_stub = crate::hub_cache::is_head(store, y);
    if !y_stub && heads.file(&row(y)) != y_bytes.as_ref().map(|b| ObjectId::of(b)) {
        return Ok(Err(format!("{y} has edits here not yet sent too")));
    }
    match fetch_line(client, &objects, theirs) {
        Err(e) if crate::hub_sync::unreachable(&e) => return Err(e),
        Err(e) => return Ok(Err(e.to_string())),
        Ok(()) => {}
    }
    let Some(mut t) = store.task(x) else { return Ok(Err(format!("{x} does not read"))) };
    let y_was = heads.head(&row(y)).map(|h| (*h, heads.file(&row(y))));
    let put_back = |store: &Store| {
        if let Some(b) = &y_bytes {
            let _ = std::fs::write(&y_file, b);
            store.wrote(y_file.clone());
        }
        let mut h = Heads::read(store);
        match y_was {
            Some((head, file)) => h.set(&row(y), head, file),
            None => h.remove(&row(y)),
        }
        let _ = h.write(store);
    };
    // Y's file is X's, as of X's synced head: what a stale push of it meets.
    t.id = y.to_string();
    if let Err(e) = store.save_task(&t) {
        put_back(store);
        return Ok(Err(e.to_string()));
    }
    let mut h = Heads::read(store);
    h.set(&row(y), synced, None);
    if let Err(e) = h.write(store) {
        put_back(store);
        return Ok(Err(e.to_string()));
    }
    let stale = crate::hub_client::Stale { subject: row(y), from: Some(synced), head: Some(theirs) };
    match crate::hub_sync::after_stale(store, &stale, &objects, &NoLease, by) {
        Ok(applied) => {
            crate::hub_cache::drop_head(store, y);
            let file = store.task_path(x);
            let _ = std::fs::remove_file(&file);
            store.wrote(file);
            Ok(Ok(match applied {
                Applied::Reconciled(text) => text,
                other => format!("{other:?}"),
            }))
        }
        Err(r) => {
            put_back(store);
            Ok(Err(format!("{r:?}")))
        }
    }
}

/// The retirements `hub` lists, applied here (see the module's doc); what was
/// done, in words for the log. `Err` is the hub not answering.
pub fn follow_retired(store: &Store, client: &mut Client, hub: &Refs, by: &Principal) -> Result<Vec<String>, HubError> {
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
        if y.is_none() {
            // Removed on the hub, for everyone: a stub goes, a synced copy is
            // archived as `wsp rm` archives (unsent edits and all), and a copy
            // the hub never had from here (one recovered from the archive)
            // stays, this machine's alone, never pushed under X.
            if file.exists() {
                let heads = Heads::read(store);
                let digest = std::fs::read(&file).ok().map(|b| ObjectId::of(&b));
                let ws = hub.workspace.0.clone();
                if crate::hub_cache::is_head(store, x) {
                    let _ = std::fs::remove_file(&file);
                    store.wrote(file.clone());
                } else if heads.head(subject).is_some() {
                    let unsent = heads.file(subject) != digest;
                    match store.task(x).map(|t| store.archive_task(&t)) {
                        Some(Ok(_)) if unsent => said.push(format!(
                            "hub-sync: {x} was removed on the hub, for everyone in {ws}; this copy had edits never sent, and they are kept in the archive here (wsp show {x})"
                        )),
                        Some(Ok(_)) => said.push(format!("hub-sync: {x} was removed on the hub, for everyone in {ws}; this copy is archived here (wsp show {x})")),
                        _ => {
                            keep_x = true;
                            said.push(format!("hub-sync: {x} was removed on the hub, for everyone in {ws}, but this copy could not be archived: it is kept in {x}, which is no longer pushed"));
                        }
                    }
                } else {
                    keep_x = true;
                    said.push(format!(
                        "hub-sync: {x} is retired on the hub (removed for everyone in {ws}); the copy here is kept on this machine and never pushed under {x}"
                    ));
                }
            }
            if keep_x {
                crate::hub_cache::drop_head(store, x);
            } else {
                forget(store, x);
            }
            mark_gone(store, x);
            continue;
        }
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
                (true, Some(y)) => match carry_into(store, client, hub, x, y, by)? {
                    Ok(merged) => said.push(format!("hub-sync: {x} was moved on the hub to {y}; its edits here not yet sent are merged into {y} ({merged})")),
                    Err(why) => {
                        keep_x = true;
                        said.push(format!(
                            "hub-sync: {x} was retired on the hub (it continues as {y}), but its edits here were never sent and could not be merged into {y} ({why}): they are kept in {x}, which is no longer pushed; bring them over by hand"
                        ));
                    }
                },
                (true, None) => {
                    keep_x = true;
                    said.push(format!(
                        "hub-sync: {x} was retired on the hub, but its edits here were never sent: they are kept in {x}, which is no longer pushed; bring them over by hand"
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
    Ok(said)
}

/// The subjects `hub` lists as retired: never pushed nor synced.
pub fn retired(hub: &Refs) -> BTreeSet<Subject> {
    hub.retired.keys().cloned().collect()
}
