//! A spoke's sync with the hub, on the files' side (wsp-176): the record a
//! push sends for a pending row, and what follows a `stale`.
//!
//! A push sends a row's file as a record whose parent is the head the spoke
//! last synced ([`push_record`]); the hub moves its ref only from that head
//! (wsp-459). Where the hub has moved since, the move comes back `stale` with
//! the hub's head, and [`after_stale`] takes that head through the door with
//! [`Merging`]: the merge is written to the file, the hub's head becomes the
//! synced one, and the next [`push_record`] is the merge as a new object whose
//! parent is theirs, pushed as usual.
//!
//! Who runs these is the spoke's sync worker (wsp-464, wsp-469); the lease
//! that lets a spoke move a row's status is wsp-466's, passed in as a [`Lease`].

use std::collections::BTreeMap;

use wsp_objects::{Object, WorkspaceId};
use wsp_store_contract::{At, Entity, Principal, Subject};

use crate::door::{Applied, Door, Heads, Refusal};
use crate::reconcile::{Lease, Merging, Objects};
use crate::store::Store;

/// The record a push sends for `subject`'s row: its file whole, with the
/// synced head as its parent (`None` for a row never synced). `None` where the
/// subject is not a row the spoke has a file for, or the row is private.
pub(crate) fn push_record(store: &Store, subject: &Subject, workspace: WorkspaceId, by: Principal, at: At) -> Option<Object> {
    let Subject::Row(id) = subject else { return None };
    let t = store.task(&id.0)?;
    if t.workspace.is_private() {
        return None;
    }
    let text = std::fs::read_to_string(store.task_path(&id.0)).ok()?;
    let texts = BTreeMap::from([(id.0.clone(), text)]);
    let row = crate::mirror::rows(&[t], &BTreeMap::new(), &[], &texts, &BTreeMap::new()).into_iter().find_map(|e| match e {
        Entity::Row(r) => Some(r),
        _ => None,
    })?;
    let parent = Heads::read(store).head(subject).copied();
    Some(Object::Record { workspace, subject: subject.clone(), entity: Entity::Row(row), parent, by, at })
}

/// A push refused `stale`: the hub's head for the subject, read from
/// `objects` (what the sync fetched), merged into the file by the door.
pub(crate) fn after_stale(
    store: &Store,
    stale: &wsp_client::Stale,
    objects: &dyn Objects,
    lease: &dyn Lease,
    by: &Principal,
) -> Result<Applied, Refusal> {
    let head = stale.head.ok_or_else(|| Refusal::NotYet("a stale with no head on the hub".into()))?;
    let theirs = objects.object(&head).ok_or_else(|| Refusal::NotYet(format!("the hub's head {} was not fetched", head.hex())))?;
    let merging = Merging { objects, lease };
    Door::new(store, &merging).import(&theirs, by)
}
