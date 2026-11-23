//! The offline journal (wsp-469; the design's "Leases and offline sync", d5):
//! every object the spoke makes for a shared workspace, kept in order until the
//! hub has it.
//!
//! **The journal.** `$WSP_STATE/hub/journal/` holds one file per object, named
//! `<seq>-<hex>[-t<token>]`: its place in the order, its id, and the hub lease
//! token the row was held under when it was made (none for a row with no hub
//! lease). The file is the object's canonical bytes. The sync worker
//! ([`crate::hub_sync`]) appends an object here before anything else is done
//! with it, then pushes; an object the hub took leaves the journal. With the hub
//! unreachable the worker still makes the records of rows that moved, so the
//! journal grows while the seats go on (a claim whose lease is in date is not
//! touched), each record following the last one journaled for its subject.
//!
//! **Reconnect.** The worker's next turn that reaches the hub settles what is
//! left, row by row, by the lease each was made under: renewed under the same
//! token, the row fast-forwards; refused, the claim is released as wsp-411
//! releases an expired one, and the lease is taken again: granted (a new
//! token), the row is pushed; held by another user, the objects go to the hub
//! as a **side chain** ([`Side`]): put, no ref moved. The old holder's file
//! then follows the new holder's line, so its verdict and landing never reach
//! the hub, and its code stays on its git branch, which nothing here touches.
//! Then the journal is put in order ([`Journal::replay`]): a put is idempotent
//! by hash, so a push a crash cut short is replayed whole with no object twice.
//!
//! **Telling both users.** Until the hub routes tells between users (wsp-470), a
//! side chain is a local flag on each spoke: `$WSP_STATE/hub/side.json`, a line
//! in `cycle.log` and a tell to the row's governor. The old holder's spoke
//! writes it at once and moves a notice to the hub, an `order.queued` event
//! under the subject `order side:<row>` ([`notice`]); the holder's spoke takes
//! the notice on its next sync and writes its own flag.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{json, Value};
use wsp_api::Blob;
use wsp_objects::{Object, ObjectId, WorkspaceId};
use wsp_store_contract::{At, EventKind, OrderId, Principal, Subject};

use crate::store::Store;

/// One object in the journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) seq: u64,
    pub(crate) id: ObjectId,
    /// The hub lease token the row was held under when the object was made.
    pub(crate) token: Option<u64>,
    name: String,
}

impl Entry {
    fn parse(name: &str) -> Option<Entry> {
        let mut parts = name.split('-');
        let seq = parts.next()?.parse().ok()?;
        let id = id_of_hex(parts.next()?)?;
        let token = match parts.next() {
            Some(t) => Some(t.strip_prefix('t')?.parse().ok()?),
            None => None,
        };
        if parts.next().is_some() {
            return None;
        }
        Some(Entry { seq, id, token, name: name.to_string() })
    }
}

/// An id from its bare hex (a file name, `side.json`).
fn id_of_hex(hex: &str) -> Option<ObjectId> {
    format!("sha256:{hex}").parse().ok()
}

/// `$WSP_STATE/hub/journal/`.
pub(crate) struct Journal(PathBuf);

impl Journal {
    pub(crate) fn of(store: &Store) -> Journal {
        Journal(store.state.join("hub").join("journal"))
    }

    #[cfg(test)]
    pub(crate) fn at(dir: PathBuf) -> Journal {
        Journal(dir)
    }

    /// Every entry, in the order it was appended. A file of another shape (a
    /// write a crash cut short is a dot-file) is no entry.
    pub(crate) fn entries(&self) -> Vec<Entry> {
        let mut out: Vec<Entry> = std::fs::read_dir(&self.0)
            .into_iter()
            .flatten()
            .filter_map(|e| Entry::parse(&e.ok()?.file_name().to_string_lossy()))
            .collect();
        out.sort_by_key(|e| e.seq);
        out
    }

    pub(crate) fn len(&self) -> usize {
        self.entries().len()
    }

    /// Appends `bytes` as `id`, after every entry there; `false` (and nothing
    /// written) where `id` is in the journal already.
    pub(crate) fn append(&self, id: &ObjectId, bytes: &[u8], token: Option<u64>) -> std::io::Result<bool> {
        let entries = self.entries();
        if entries.iter().any(|e| e.id == *id) {
            return Ok(false);
        }
        std::fs::create_dir_all(&self.0)?;
        let seq = entries.last().map_or(1, |e| e.seq + 1);
        let name = match token {
            Some(t) => format!("{seq:012}-{}-t{t}", id.hex()),
            None => format!("{seq:012}-{}", id.hex()),
        };
        let tmp = self.0.join(format!(".{name}.{}.tmp", std::process::id()));
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, self.0.join(name))?;
        Ok(true)
    }

    pub(crate) fn bytes(&self, e: &Entry) -> Option<Vec<u8>> {
        std::fs::read(self.0.join(&e.name)).ok()
    }

    /// The entry leaves the journal: the hub has its object.
    pub(crate) fn remove(&self, e: &Entry) {
        let _ = std::fs::remove_file(self.0.join(&e.name));
    }

    /// The entries of each subject, in order, with the workspace each is of;
    /// one whose bytes are no object is left out.
    pub(crate) fn by_subject(&self) -> BTreeMap<Subject, (WorkspaceId, Vec<Entry>)> {
        let mut out: BTreeMap<Subject, (WorkspaceId, Vec<Entry>)> = BTreeMap::new();
        for e in self.entries() {
            let Some(obj) = self.bytes(&e).and_then(|b| wsp_objects::decode(&b).ok()) else { continue };
            out.entry(obj.subject().clone()).or_insert_with(|| (obj.workspace().clone(), Vec::new())).1.push(e);
        }
        out
    }

    /// The last entry of `subject`, the parent of the next record made for it.
    #[cfg(test)]
    pub(crate) fn tip(&self, subject: &Subject) -> Option<ObjectId> {
        self.by_subject().remove(subject).and_then(|(_, es)| es.last().map(|e| e.id))
    }

    /// The last entry of `subject` and its bytes, where it is a record whose
    /// entity is `made`'s: the object a worker that lost its memory of what it
    /// minted takes again instead of making a twin of it (wsp-484).
    pub(crate) fn tip_of_entity(&self, subject: &Subject, made: &Object) -> Option<(ObjectId, Vec<u8>)> {
        let Object::Record { entity, .. } = made else { return None };
        let (_, entries) = self.by_subject().remove(subject)?;
        let tip = entries.last()?;
        let bytes = self.bytes(tip)?;
        match wsp_objects::decode(&bytes).ok()? {
            Object::Record { entity: held, .. } if held == *entity => Some((tip.id, bytes)),
            _ => None,
        }
    }

    /// Puts `entries` through `put`, in order, in chunks under the client's
    /// put size. The hub keeps an object once whatever is put again, so a
    /// replay after a crash sends what was sent and keeps no duplicate.
    /// How many objects went out.
    pub(crate) fn replay<E>(&self, entries: &[Entry], put: &mut dyn FnMut(Vec<Blob>) -> Result<u32, E>) -> Result<usize, E> {
        let mut chunk: Vec<Blob> = Vec::new();
        let mut size = 0;
        let mut sent = 0;
        for e in entries {
            let Some(bytes) = self.bytes(e) else { continue };
            if size + bytes.len() > wsp_client::PUT_BYTES && !chunk.is_empty() {
                sent += chunk.len();
                put(std::mem::take(&mut chunk))?;
                size = 0;
            }
            size += bytes.len();
            chunk.push(Blob { id: e.id, bytes });
        }
        if !chunk.is_empty() {
            sent += chunk.len();
            put(chunk)?;
        }
        Ok(sent)
    }
}

/// A row whose journal went to the hub as a side chain: another user's spoke
/// held its lease when this one came back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Side {
    pub(crate) workspace: String,
    pub(crate) row: String,
    /// The last object of the side chain, on the hub and under no ref.
    pub(crate) tip: String,
    /// The user whose spoke holds the lease.
    pub(crate) holder: String,
    /// The user whose spoke wrote the side chain while offline.
    pub(crate) old: String,
    /// Whether the old holder is this spoke: its verdict and landing on the
    /// row are refused until it holds the lease again.
    pub(crate) mine: bool,
    /// The lease token the old holder's journal was made under.
    pub(crate) token: Option<u64>,
    pub(crate) at: String,
}

/// `$WSP_STATE/hub/side.json`: the side chains this spoke wrote or was told
/// of, and the notice heads it has taken.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Sides {
    pub(crate) rows: BTreeMap<String, Side>,
    /// Per workspace and row, the head of its notice subject taken.
    pub(crate) notices: BTreeMap<String, BTreeMap<String, String>>,
}

impl Sides {
    fn path(store: &Store) -> PathBuf {
        store.state.join("hub").join("side.json")
    }

    pub(crate) fn read(store: &Store) -> Sides {
        let v: Value = std::fs::read_to_string(Self::path(store)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
        let text = |s: &Value, k: &str| s.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
        let rows = v["rows"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(row, s)| {
                let side = Side {
                    workspace: text(s, "workspace"),
                    row: row.clone(),
                    tip: text(s, "tip"),
                    holder: text(s, "holder"),
                    old: text(s, "old"),
                    mine: s["mine"].as_bool().unwrap_or(false),
                    token: s["token"].as_u64(),
                    at: text(s, "at"),
                };
                (row.clone(), side)
            })
            .collect();
        let notices = v["notices"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(ws, m)| {
                let heads = m.as_object().into_iter().flatten().filter_map(|(r, id)| Some((r.clone(), id.as_str()?.to_string()))).collect();
                (ws.clone(), heads)
            })
            .collect();
        Sides { rows, notices }
    }

    pub(crate) fn write(&self, store: &Store) -> std::io::Result<()> {
        let path = Self::path(store);
        std::fs::create_dir_all(path.parent().expect("under hub/"))?;
        let rows: serde_json::Map<String, Value> = self
            .rows
            .iter()
            .map(|(row, s)| {
                let v = json!({ "workspace": s.workspace, "tip": s.tip, "holder": s.holder, "old": s.old, "mine": s.mine, "token": s.token, "at": s.at });
                (row.clone(), v)
            })
            .collect();
        let v = json!({ "rows": rows, "notices": self.notices });
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        std::fs::write(&tmp, serde_json::to_string_pretty(&v).unwrap_or_default())?;
        std::fs::rename(&tmp, path)
    }

    /// The notice heads taken in `workspace`, as subjects and ids.
    pub(crate) fn notice_heads(&self, workspace: &str) -> Vec<(Subject, ObjectId)> {
        self.notices
            .get(workspace)
            .into_iter()
            .flatten()
            .filter_map(|(row, id)| Some((notice_subject(row), id_of_hex(id)?)))
            .collect()
    }
}

/// `$WSP_STATE/hub/leased.json`: per row, the hub lease token this spoke last
/// held it under. A record is journaled under it even where the claim ended
/// offline (`wsp done`, a hand-off), so the reconnect asks the hub about the
/// lease the work was done under, not about none.
pub(crate) struct Leased;

impl Leased {
    fn path(store: &Store) -> PathBuf {
        store.state.join("hub").join("leased.json")
    }

    pub(crate) fn read(store: &Store) -> BTreeMap<String, u64> {
        std::fs::read_to_string(Self::path(store)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub(crate) fn write(store: &Store, map: &BTreeMap<String, u64>) {
        let path = Self::path(store);
        let _ = std::fs::create_dir_all(path.parent().expect("under hub/"));
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        if std::fs::write(&tmp, serde_json::to_string(map).unwrap_or_default()).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }

    /// The row's token is done with: the lease went back, or another has it.
    pub(crate) fn forget(store: &Store, row: &str) {
        let mut map = Self::read(store);
        if map.remove(row).is_some() {
            Self::write(store, &map);
        }
    }
}

/// The side chain this spoke wrote for `row`, while it does not hold the row's
/// lease again: what refuses its verdict and landing there.
pub(crate) fn superseded(store: &Store, row: &str) -> Option<Side> {
    Sides::read(store).rows.get(row).filter(|s| s.mine).cloned()
}

/// Why `verb` (a verdict: `done`, `review`, `submit`) is refused on `row`: this
/// spoke's work on it went to the hub as a side chain and another user's spoke
/// holds its lease. `None` for any other row.
pub(crate) fn refusal(store: &Store, row: &str, verb: &str) -> Option<String> {
    let s = superseded(store, row)?;
    Some(format!(
        "wsp: {row} was leased by {} while this spoke was offline, so `wsp {verb}` is refused here: the work done offline is on the hub as a side chain ({}) and the code stays on its branch. Lease it again (`wsp claim {row}`) once {} lets go, or hand the branch over.",
        s.holder,
        short(&s.tip),
        s.holder
    ))
}

/// What `wsp show` says of a row with a side chain; `None` for any other.
pub(crate) fn show_line(store: &Store, row: &str) -> Option<String> {
    let s = Sides::read(store).rows.get(row).cloned()?;
    Some(match s.mine {
        true => format!(
            "side chain: {row} was leased by {} while this spoke was offline; its work here is on the hub as a side chain ({}), its verdict and landing refused — the code is on its branch",
            s.holder,
            short(&s.tip)
        ),
        false => format!(
            "side chain: {} worked on {row} offline while you held it; their work is on the hub as a side chain ({}), not on the row",
            s.old,
            short(&s.tip)
        ),
    })
}

fn short(hex: &str) -> &str {
    &hex[..hex.len().min(12)]
}

/// The subject a side chain's notice moves: `order side:<row>`.
pub(crate) fn notice_subject(row: &str) -> Subject {
    Subject::Order(OrderId(format!("side:{row}")))
}

/// The row a notice subject is of; `None` for any other subject.
pub(crate) fn notice_row(subject: &Subject) -> Option<&str> {
    match subject {
        Subject::Order(OrderId(id)) => id.strip_prefix("side:"),
        _ => None,
    }
}

/// The notice of a side chain: who wrote it, who holds the row, and the tip.
pub(crate) fn notice(side: &Side, parent: Option<ObjectId>, by: Principal, at: At) -> Object {
    Object::Event {
        workspace: WorkspaceId(side.workspace.clone()),
        subject: notice_subject(&side.row),
        kind: EventKind::OrderQueued,
        payload: json!({
            "to": [side.holder, side.old],
            "row": side.row,
            "tip": side.tip,
            "holder": side.holder,
            "old": side.old,
            "text": said(side),
        }),
        parent,
        by,
        at,
    }
}

/// The side a notice says, as the spoke of `me` keeps it; `None` for an
/// object that is no notice.
pub(crate) fn side_of_notice(obj: &Object, me: &str) -> Option<Side> {
    let Object::Event { workspace, subject, kind: EventKind::OrderQueued, payload, at, .. } = obj else { return None };
    let row = notice_row(subject)?;
    let field = |k: &str| payload.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let old = field("old")?;
    Some(Side {
        workspace: workspace.0.clone(),
        row: row.to_string(),
        tip: field("tip")?,
        holder: field("holder")?,
        mine: old == me,
        old,
        token: None,
        at: crate::util::iso_at((at.0 / 1000) as i64),
    })
}

/// The words both users are told.
pub(crate) fn said(s: &Side) -> String {
    format!(
        "{row}: {old}'s spoke was offline past its lease and {holder} leased {row} meanwhile; {old}'s work went to the hub as a side chain ({tip}), no ref moved. {old}'s verdict and landing on {row} are refused; the code stays on its git branch.",
        row = s.row,
        old = s.old,
        holder = s.holder,
        tip = short(&s.tip)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use wsp_store_contract::RowId;

    fn row_object(n: u64, parent: Option<ObjectId>) -> (ObjectId, Vec<u8>) {
        wsp_objects::encode(&Object::Event {
            workspace: WorkspaceId("shared:mp".into()),
            subject: Subject::Row(RowId("wsp-7".into())),
            kind: EventKind::RowEdited,
            payload: json!({ "title": format!("Revision {n}") }),
            parent,
            by: Principal::Person("ann".into()),
            at: At(n),
        })
    }

    #[test]
    fn the_journal_keeps_order_and_tokens_and_takes_an_id_once() {
        let dir = std::env::temp_dir().join(format!("wsp-journal-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let j = Journal::at(dir.clone());
        let (a, ab) = row_object(1, None);
        let (b, bb) = row_object(2, Some(a));
        assert!(j.append(&a, &ab, Some(4)).unwrap());
        assert!(j.append(&b, &bb, None).unwrap());
        assert!(!j.append(&a, &ab, Some(4)).unwrap(), "an id already journaled is not appended again");
        // A write a crash cut short is no entry.
        std::fs::write(dir.join(".000000000003-junk.tmp"), b"x").unwrap();
        let es = j.entries();
        assert_eq!(es.iter().map(|e| (e.seq, e.id, e.token)).collect::<Vec<_>>(), vec![(1, a, Some(4)), (2, b, None)]);
        assert_eq!(j.tip(&Subject::Row(RowId("wsp-7".into()))), Some(b));
        assert_eq!(j.bytes(&es[0]).unwrap(), ab);
        j.remove(&es[0]);
        assert_eq!(j.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_crash_mid_push_replays_the_journal_with_no_duplicate_object_on_the_hub() {
        let dir = std::env::temp_dir().join(format!("wsp-journal-crash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let j = Journal::at(dir.join("journal"));
        let hub = wsp_hub::Hub::open(dir.join("hub")).unwrap();
        let mut parent = None;
        for n in 1..=5 {
            let (id, bytes) = row_object(n, parent);
            j.append(&id, &bytes, Some(1)).unwrap();
            parent = Some(id);
        }
        let entries = j.entries();
        // The push gets two objects to the hub, then the process dies: nothing
        // left the journal.
        let first: Vec<Blob> = entries[..2].iter().map(|e| Blob { id: e.id, bytes: j.bytes(e).unwrap() }).collect();
        assert_eq!(hub.put(&first).unwrap(), 2);
        assert_eq!(j.len(), 5, "a cut push leaves the journal whole");

        // The replay puts all five, in order; the hub writes only the three it lacked.
        let mut order = Vec::new();
        let mut written = 0;
        let sent = j
            .replay(&j.entries(), &mut |blobs: Vec<Blob>| {
                order.extend(blobs.iter().map(|b| b.id));
                let n = hub.put(&blobs)?;
                written += n;
                Ok::<u32, wsp_store_contract::Error>(n)
            })
            .unwrap();
        assert_eq!(sent, 5);
        assert_eq!(order, entries.iter().map(|e| e.id).collect::<Vec<_>>(), "the replay is in the journal's order");
        assert_eq!(written, 3, "the two already there were not written again");
        let files = std::fs::read_dir(hub.objects().root()).unwrap().count();
        let held = entries.iter().filter(|e| hub.objects().has(&e.id)).count();
        assert_eq!(held, 5);
        // A second replay (a crash after the put, before the journal let go) adds nothing.
        let again = j.replay(&j.entries(), &mut |blobs: Vec<Blob>| hub.put(&blobs)).unwrap();
        assert_eq!(again, 5);
        assert_eq!(std::fs::read_dir(hub.objects().root()).unwrap().count(), files, "no object is kept twice");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
