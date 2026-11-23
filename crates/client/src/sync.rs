//! A spoke's side of the sync verbs (wsp-459; the design's "Hash-based sync,
//! and why it scales"): [`Client::sync`] syncs one workspace with the hub as
//! git fetches and pushes, and the five verbs are here one by one for whoever
//! needs only one ([`Client::sync_heads`], [`Client::sync_have`],
//! [`Client::sync_put`], [`Client::sync_get`], [`Client::refs_move`]), with
//! [`Client::ids_alloc`], the hub's row ids for a publish (wsp-185).
//!
//! The client stores nothing. It reads the spoke's objects through an
//! [`ObjectSource`] and its heads from the [`Refs`] it is handed, and returns
//! what it fetched and where the heads now stand in a [`SyncReport`], for the
//! spoke's store to keep (wsp-464).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::Value;
use wsp_api::{
    Answer, Blob, Call, IdsAlloc, LeaseHold, LeaseTake, Move, ObjectId, Outcome, RefMove, Refs, SyncCall, WorkspaceId,
};
use wsp_store_contract::{At, RowId, Subject, Token};

use crate::{orders, Client, Error};

/// The most ids one `sync.have` or `sync.get` asks about; more go in several.
pub const IDS_PER_REQUEST: usize = 4096;

/// The most object bytes one `sync.put` carries (before base64), so a put stays
/// well under `MAX_LINE`; more go in several.
pub const PUT_BYTES: usize = 4 * 1024 * 1024;

/// Where the spoke's objects are read: the canonical bytes of an id it holds.
/// A spoke holds every object behind each of its heads, back to the first.
pub trait ObjectSource {
    fn object(&self, id: &ObjectId) -> Option<Vec<u8>>;
}

impl ObjectSource for BTreeMap<ObjectId, Vec<u8>> {
    fn object(&self, id: &ObjectId) -> Option<Vec<u8>> {
        self.get(id).cloned()
    }
}

impl ObjectSource for HashMap<ObjectId, Vec<u8>> {
    fn object(&self, id: &ObjectId) -> Option<Vec<u8>> {
        self.get(id).cloned()
    }
}

/// A `refs.move` the hub refused: the subject's head there was not the one this
/// sync saw, but `head`. Returned to the caller to reconcile (wsp-176), never
/// retried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stale {
    pub subject: Subject,
    /// The head this sync saw and moved from.
    pub from: Option<ObjectId>,
    /// The head the hub has.
    pub head: Option<ObjectId>,
}

/// What one [`Client::sync`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncReport {
    /// Objects sent to the hub: those it said it lacked.
    pub sent: usize,
    /// Objects fetched from the hub, each held to its id, parents before
    /// children. The spoke's store keeps them.
    pub received: Vec<Blob>,
    /// The spoke's heads after the sync: its own where the hub took them or a
    /// move was refused, the hub's where the hub was ahead.
    pub refs: Refs,
    /// Subjects whose head on the hub this sync moved to the spoke's.
    pub moved: Vec<Subject>,
    /// Subjects whose head on the spoke moves to the hub's (in `refs`).
    pub adopted: Vec<Subject>,
    /// Moves the hub refused, with the head it has.
    pub stale: Vec<Stale>,
    /// Subjects where neither head follows from the other: both sides wrote.
    /// The hub's objects are in `received`; the merge is the caller's (wsp-176).
    pub diverged: Vec<Subject>,
}

/// Why a sync stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncError {
    /// A request got no answer, or an answer other than its verb's.
    Request(Error),
    /// The refs handed in are of another workspace than the one asked for.
    Workspace { asked: WorkspaceId, refs: WorkspaceId },
    /// The hub's heads are of another workspace than the one asked for.
    HubWorkspace(WorkspaceId),
    /// The spoke's source lacks an object one of its heads leads back to.
    LocalMissing(ObjectId),
    /// The hub's refs name an object it did not hand over.
    HubMissing(ObjectId),
    /// Bytes that are no object, or that do not hash to their id; `local` says
    /// whose.
    Corrupt { id: ObjectId, local: bool, why: String },
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncError::Request(e) => write!(f, "{e}"),
            SyncError::Workspace { asked, refs } => {
                write!(f, "asked to sync {:?} with the refs of {:?}", asked.0, refs.0)
            }
            SyncError::HubWorkspace(w) => write!(f, "the hub answered with the refs of {:?}", w.0),
            SyncError::LocalMissing(id) => write!(f, "this spoke lacks {id}, which one of its heads leads back to"),
            SyncError::HubMissing(id) => write!(f, "the hub names {id} and did not hand it over"),
            SyncError::Corrupt { id, local, why } => {
                write!(f, "{} object {id} is unusable: {why}", if *local { "this spoke's" } else { "the hub's" })
            }
        }
    }
}

impl std::error::Error for SyncError {}

impl From<Error> for SyncError {
    fn from(e: Error) -> SyncError {
        SyncError::Request(e)
    }
}

/// The `parent` an object's bytes name, read schema-free (any kind of object,
/// one tag around a map with a `parent`), so an object of a kind this build
/// does not know still leads back.
fn parent_in(id: &ObjectId, bytes: &[u8], local: bool) -> Result<Option<ObjectId>, SyncError> {
    let corrupt = |why: String| SyncError::Corrupt { id: *id, local, why };
    let v: Value = serde_json::from_slice(bytes).map_err(|e| corrupt(e.to_string()))?;
    let body = v
        .as_object()
        .filter(|m| m.len() == 1)
        .and_then(|m| m.values().next())
        .ok_or_else(|| corrupt("not one tagged object".into()))?;
    match body.get("parent") {
        None | Some(Value::Null) => Ok(None),
        Some(p) => serde_json::from_value(p.clone()).map(Some).map_err(|e| corrupt(format!("parent: {e}"))),
    }
}

/// The spoke's objects and those fetched in this sync, read together.
struct Held<'a, S: ObjectSource> {
    source: &'a S,
    fetched: BTreeMap<ObjectId, Vec<u8>>,
}

impl<S: ObjectSource> Held<'_, S> {
    fn has(&self, id: &ObjectId) -> bool {
        self.fetched.contains_key(id) || self.source.object(id).is_some()
    }

    fn parent(&self, id: &ObjectId) -> Result<Option<ObjectId>, SyncError> {
        match self.fetched.get(id) {
            Some(bytes) => parent_in(id, bytes, false),
            None => {
                let bytes = self.source.object(id).ok_or(SyncError::LocalMissing(*id))?;
                parent_in(id, &bytes, true)
            }
        }
    }

    /// Whether `ancestor` is `head` or one of the objects it follows.
    fn descends(&self, head: &ObjectId, ancestor: &ObjectId) -> Result<bool, SyncError> {
        let mut at = Some(*head);
        while let Some(id) = at {
            if id == *ancestor {
                return Ok(true);
            }
            at = self.parent(&id)?;
        }
        Ok(false)
    }
}

impl Client {
    /// `sync.heads`: each subject's head in `workspace` on the hub.
    pub fn sync_heads(&mut self, workspace: &WorkspaceId) -> Result<Refs, Error> {
        match self.sync_call(SyncCall::Heads { workspace: workspace.clone() })? {
            Answer::Heads(refs) => Ok(refs),
            other => Err(Error::Unexpected(format!("sync.heads was answered {other:?}"))),
        }
    }

    /// `sync.have`: those of `ids` the hub lacks, in the order asked, in as
    /// many requests as [`IDS_PER_REQUEST`] takes.
    pub fn sync_have(&mut self, ids: Vec<ObjectId>) -> Result<Vec<ObjectId>, Error> {
        let mut missing = Vec::new();
        for chunk in ids.chunks(IDS_PER_REQUEST) {
            match self.sync_call(SyncCall::Have { ids: chunk.to_vec() })? {
                Answer::Missing(m) => missing.extend(m),
                other => return Err(Error::Unexpected(format!("sync.have was answered {other:?}"))),
            }
        }
        Ok(missing)
    }

    /// `sync.put`: hand the hub `objects`, in as many requests as
    /// [`PUT_BYTES`] takes; how many it wrote (one it held already is not).
    pub fn sync_put(&mut self, objects: Vec<Blob>) -> Result<u32, Error> {
        let mut stored = 0;
        let mut batch: Vec<Blob> = Vec::new();
        let mut size = 0;
        let mut objects = objects.into_iter().peekable();
        while let Some(blob) = objects.next() {
            size += blob.bytes.len();
            batch.push(blob);
            let next = objects.peek().map_or(0, |b| b.bytes.len());
            if objects.peek().is_none() || size + next > PUT_BYTES {
                match self.sync_call(SyncCall::Put { objects: std::mem::take(&mut batch) })? {
                    Answer::Stored { stored: n } => stored += n,
                    other => return Err(Error::Unexpected(format!("sync.put was answered {other:?}"))),
                }
                size = 0;
            }
        }
        Ok(stored)
    }

    /// `sync.get`: those of `ids` the hub holds, as it sent them (the caller
    /// holds each to its id), in as many requests as [`IDS_PER_REQUEST`] takes.
    pub fn sync_get(&mut self, ids: Vec<ObjectId>) -> Result<Vec<Blob>, Error> {
        let mut out = Vec::new();
        for chunk in ids.chunks(IDS_PER_REQUEST) {
            match self.sync_call(SyncCall::Get { ids: chunk.to_vec() })? {
                Answer::Objects(blobs) => out.extend(blobs),
                other => return Err(Error::Unexpected(format!("sync.get was answered {other:?}"))),
            }
        }
        Ok(out)
    }

    /// `refs.move`: moved, or [`Move::Stale`] with the head the hub has.
    pub fn refs_move(&mut self, mv: RefMove) -> Result<Move, Error> {
        match self.sync_call(SyncCall::Move(mv))? {
            Answer::Move(m) => Ok(m),
            other => Err(Error::Unexpected(format!("refs.move was answered {other:?}"))),
        }
    }

    /// `ids.alloc`: `n` row ids under `project` from the hub's counter, each
    /// above `past`, for a publish into `workspace` (which the caller must be
    /// a member of). In increasing order; never one handed out before.
    pub fn ids_alloc(&mut self, workspace: &WorkspaceId, project: &str, n: u32, past: Option<u64>) -> Result<Vec<String>, Error> {
        let ask = IdsAlloc { workspace: workspace.clone(), project: project.to_string(), n, past };
        match self.sync_call(SyncCall::Alloc(ask))? {
            Answer::Ids(ids) => Ok(ids),
            other => Err(Error::Unexpected(format!("ids.alloc was answered {other:?}"))),
        }
    }

    /// `lease.take` (wsp-466): the hub's lease on `row` in `workspace`, its
    /// fencing token and when it expires. Refused `held` (naming the holder)
    /// while another user holds it, `forbidden` when it is allocated to
    /// another user or the caller is no member of `workspace`.
    pub fn lease_take(&mut self, workspace: &WorkspaceId, row: &RowId) -> Result<(Token, At), Error> {
        let ask = LeaseTake { workspace: workspace.clone(), row: row.clone() };
        match self.sync_call(SyncCall::LeaseTake(ask))? {
            Answer::Lease { token, expires } => Ok((token, expires)),
            other => Err(Error::Unexpected(format!("lease.take was answered {other:?}"))),
        }
    }

    /// `lease.renew`: the caller's lease on `row` under `token` runs the TTL
    /// again; refused `stale` once it has expired.
    pub fn lease_renew(&mut self, row: &RowId, token: Token) -> Result<(Token, At), Error> {
        match self.sync_call(SyncCall::LeaseRenew(LeaseHold { row: row.clone(), token }))? {
            Answer::Lease { token, expires } => Ok((token, expires)),
            other => Err(Error::Unexpected(format!("lease.renew was answered {other:?}"))),
        }
    }

    /// `lease.release`: lets the caller's lease on `row` under `token` go.
    pub fn lease_release(&mut self, row: &RowId, token: Token) -> Result<(), Error> {
        match self.sync_call(SyncCall::LeaseRelease(LeaseHold { row: row.clone(), token }))? {
            Answer::Released => Ok(()),
            other => Err(Error::Unexpected(format!("lease.release was answered {other:?}"))),
        }
    }

    fn sync_call(&mut self, call: SyncCall) -> Result<Answer, Error> {
        match self.send_call(Call::Sync(call), None)?.outcome {
            Outcome::Ok(a) => Ok(a),
            Outcome::Err(f) => Err(orders::refused(f)),
        }
    }

    /// Sync `workspace` with the hub, from the spoke's heads `local` and its
    /// objects in `source`:
    ///
    /// 1. `sync.heads`, and [`wsp_objects::diff`] against `local`.
    /// 2. Fetch: every object behind a hub head the spoke lacks, back to one it
    ///    holds (`sync.get`, a round per generation), each held to its id.
    /// 3. Decide each subject that differs: the spoke's head follows the hub's
    ///    (or the hub has none): push it; the hub's follows the spoke's: adopt
    ///    it; neither: diverged, left to the caller.
    /// 4. Push: the objects behind each pushed head back to the hub's, asked
    ///    with one `sync.have` and the missing ones sent with `sync.put`,
    ///    parents first.
    /// 5. One `refs.move` per pushed subject, from the head step 1 saw. A
    ///    stale one is reported in [`SyncReport::stale`] and not retried.
    ///
    /// Two sides that agree send nothing but `sync.heads`.
    pub fn sync(
        &mut self,
        workspace: &WorkspaceId,
        local: &Refs,
        source: &impl ObjectSource,
    ) -> Result<SyncReport, SyncError> {
        self.sync_skipping(workspace, local, source, &|_| false)
    }

    /// [`Client::sync`] over the subjects `skip` does not name: one it names is
    /// left out of the diff, so nothing of it is fetched, adopted or pushed
    /// (the spoke's synced cache keeps only the head of a row that is not its
    /// own, wsp-465).
    pub fn sync_skipping(
        &mut self,
        workspace: &WorkspaceId,
        local: &Refs,
        source: &impl ObjectSource,
        skip: &dyn Fn(&Subject) -> bool,
    ) -> Result<SyncReport, SyncError> {
        if local.workspace != *workspace {
            return Err(SyncError::Workspace { asked: workspace.clone(), refs: local.workspace.clone() });
        }
        let theirs = self.sync_heads(workspace)?;
        let mut diff = wsp_objects::diff(local, &theirs).map_err(|_| SyncError::HubWorkspace(theirs.workspace.clone()))?;
        diff.we_lack.retain(|s| !skip(s));
        diff.they_lack.retain(|s| !skip(s));
        diff.differ.retain(|s| !skip(s));
        let mut held = Held { source, fetched: BTreeMap::new() };

        // 2. Fetch, generation by generation, children before parents.
        let mut order: Vec<ObjectId> = Vec::new();
        let mut want: BTreeSet<ObjectId> =
            diff.we_lack.iter().chain(&diff.differ).map(|s| theirs.heads[s]).filter(|id| !held.has(id)).collect();
        while !want.is_empty() {
            let blobs = self.sync_get(want.iter().copied().collect())?;
            let mut next = BTreeSet::new();
            for blob in blobs {
                if !want.remove(&blob.id) {
                    continue;
                }
                wsp_objects::verify(&blob.id, &blob.bytes).map_err(|e| SyncError::Corrupt {
                    id: blob.id,
                    local: false,
                    why: e.to_string(),
                })?;
                if let Some(parent) = parent_in(&blob.id, &blob.bytes, false)? {
                    if !held.has(&parent) {
                        next.insert(parent);
                    }
                }
                order.push(blob.id);
                held.fetched.insert(blob.id, blob.bytes);
            }
            if let Some(id) = want.into_iter().next() {
                return Err(SyncError::HubMissing(id));
            }
            next.retain(|id| !held.fetched.contains_key(id));
            want = next;
        }

        // 3. Decide.
        let mut refs = local.clone();
        let mut push: Vec<(Subject, Option<ObjectId>, ObjectId)> = Vec::new();
        let mut adopted = Vec::new();
        let mut diverged = Vec::new();
        for s in &diff.they_lack {
            push.push((s.clone(), None, local.heads[s]));
        }
        for s in &diff.we_lack {
            refs.set(s.clone(), theirs.heads[s]);
            adopted.push(s.clone());
        }
        for s in &diff.differ {
            let (ours, hubs) = (local.heads[s], theirs.heads[s]);
            if held.descends(&ours, &hubs)? {
                push.push((s.clone(), Some(hubs), ours));
            } else if held.descends(&hubs, &ours)? {
                refs.set(s.clone(), hubs);
                adopted.push(s.clone());
            } else {
                diverged.push(s.clone());
            }
        }

        // 4. Push: the spoke's objects behind each pushed head, back to the
        // hub's head (or to the first), children before parents.
        let mut candidates: Vec<ObjectId> = Vec::new();
        let mut seen = BTreeSet::new();
        for (_, from, to) in &push {
            let mut at = Some(*to);
            while let Some(id) = at {
                if Some(id) == *from || !seen.insert(id) {
                    break;
                }
                candidates.push(id);
                at = held.parent(&id)?;
            }
        }
        let missing: BTreeSet<ObjectId> = if candidates.is_empty() {
            BTreeSet::new()
        } else {
            self.sync_have(candidates.clone())?.into_iter().collect()
        };
        let mut blobs = Vec::with_capacity(missing.len());
        for id in candidates.iter().rev().filter(|id| missing.contains(id)) {
            let bytes = source.object(id).ok_or(SyncError::LocalMissing(*id))?;
            blobs.push(Blob { id: *id, bytes });
        }
        let sent = blobs.len();
        if !blobs.is_empty() {
            self.sync_put(blobs)?;
        }

        // 5. Move the hub's heads, each from the one step 1 saw.
        let mut moved = Vec::new();
        let mut stale = Vec::new();
        for (subject, from, to) in push {
            let mv = RefMove { workspace: workspace.clone(), subject: subject.clone(), from, to };
            match self.refs_move(mv)? {
                Move::Moved { .. } => moved.push(subject),
                Move::Stale { head } => stale.push(Stale { subject, from, head }),
            }
        }

        let received = order
            .into_iter()
            .rev()
            .map(|id| Blob { id, bytes: held.fetched.remove(&id).expect("fetched above") })
            .collect();
        Ok(SyncReport { sent, received, refs, moved, adopted, stale, diverged })
    }
}
