//! The import door (wsp-175): an object from the hub, reaching the files.
//!
//! A CLI on a person's machine writes to its own local wsp; what arrives over
//! the wire is objects the hub holds, and until state-core steps 3 and 4 the
//! files own rows, so an imported record has to become a file write. The door
//! makes it through the same writes the verbs end in ([`cmd_task::file_new`],
//! [`cmd_task::write_task`], [`cmd_task::move_status`]), with an explicit
//! [`Caller`] naming the hub's user where a verb reads the pane.
//!
//! **Which write.** A record for a subject the spoke has no file for creates
//! it. One whose `parent` is the subject's local head, on a file that is still
//! the one that head left, updates it. Any other (a file the spoke never
//! synced, a parent that is not the head, or a local write since the head not
//! yet pushed) is the two sides having moved apart, and goes to [`Reconcile`]
//! (wsp-176), which writes what it decides; the door writes nothing of its own
//! for it. [`crate::reconcile::Merging`] is the one that merges.
//!
//! **Not pushed back.** The spoke keeps, per subject, the head it last synced
//! and the digest of the file that head left ([`Heads`], in the state
//! directory). An import records both after its write, so [`pending`], what a
//! sync worker pushes, compares the file against the imported head and finds
//! nothing to send; a verb's write after it changes the file, and is pending.
//!
//! Rows only for now: a list, seat, ask or any other entity is refused
//! [`Refusal::NotYet`], as is an event object.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use wsp_objects::{Object, ObjectId};
use wsp_store_contract::{Entity, Principal, RowId, Subject};

use crate::cmd_message::Caller;
use crate::cmd_task;
use crate::model::Task;
use crate::store::Store;

/// What an import did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Applied {
    /// The spoke had no file for the row; now it has.
    Created(RowId),
    /// The record followed the local head; the file is now that record.
    Updated(RowId),
    /// [`Reconcile`] took it, and says what it did.
    Reconciled(String),
}

/// Why an object was not imported. Nothing is written for any of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// An event, or a record of an entity the files do not take from the hub yet.
    NotYet(String),
    /// The record's subject is not the entity it carries.
    Mismatch { subject: String, entity: String },
    /// A private record: the private workspace never leaves its spoke.
    Private(RowId),
    /// The row carries neither its file's text nor its head.
    Unreadable(RowId),
    /// The two sides moved apart and nothing reconciles them yet.
    Diverged { row: RowId, local: Option<ObjectId>, parent: Option<ObjectId> },
    /// The file write failed.
    Write(String),
}

/// What decides an object whose parent is not the local head (wsp-176).
pub(crate) trait Reconcile {
    /// `obj` (whose id is `id`) against the spoke's `local` head for its
    /// subject, `None` where the spoke has a file and never synced it.
    fn reconcile(&self, store: &Store, obj: &Object, id: ObjectId, local: Option<ObjectId>) -> Result<Applied, Refusal>;
}

/// Every divergence refused, and nothing written: for a door that must not
/// merge (a read-only spoke, a test of the door alone).
pub(crate) struct Unreconciled;

impl Reconcile for Unreconciled {
    fn reconcile(&self, _: &Store, obj: &Object, _: ObjectId, local: Option<ObjectId>) -> Result<Applied, Refusal> {
        let row = match obj.subject() {
            Subject::Row(r) => r.clone(),
            other => RowId(format!("{other:?}")),
        };
        Err(Refusal::Diverged { row, local, parent: obj.parent().copied() })
    }
}

/// The door: one spoke's store, and what reconciles for it.
pub(crate) struct Door<'a> {
    store: &'a Store,
    reconcile: &'a dyn Reconcile,
}

impl<'a> Door<'a> {
    pub(crate) fn new(store: &'a Store, reconcile: &'a dyn Reconcile) -> Door<'a> {
        Door { store, reconcile }
    }

    /// `obj`, delivered by the hub on `by`'s behalf, into the files.
    pub(crate) fn import(&self, obj: &Object, by: &Principal) -> Result<Applied, Refusal> {
        let Object::Record { subject, entity, parent, .. } = obj else {
            return Err(Refusal::NotYet("an event".into()));
        };
        let row = match entity {
            Entity::Row(r) => r,
            other => return Err(Refusal::NotYet(format!("a {} record", entity_kind(other)))),
        };
        if *subject != Subject::Row(row.id.clone()) {
            return Err(Refusal::Mismatch { subject: format!("{subject:?}"), entity: row.id.0.clone() });
        }
        if row.workspace.is_private() {
            return Err(Refusal::Private(row.id.clone()));
        }
        let (id, _) = wsp_objects::encode(obj);
        let caller = Caller::hub(&user_of(by));
        let mut heads = Heads::read(self.store);
        let local = self.store.task(&row.id.0);
        let mut t = task_of_record(row).ok_or_else(|| Refusal::Unreadable(row.id.clone()))?;
        // The file is still the one the synced head left: no local write since.
        let unmoved = |heads: &Heads, subject: &Subject| heads.file(subject) == file_digest(self.store, &row.id.0);
        let applied = match local {
            None => {
                cmd_task::file_new(self.store, &mut t, &caller).map_err(|e| Refusal::Write(e.to_string()))?;
                Applied::Created(row.id.clone())
            }
            Some(was) if parent.is_some() && heads.head(subject) == parent.as_ref() && unmoved(&heads, subject) => {
                let want = t.status();
                t.status_raw = was.status_raw.clone();
                let verb = if want != was.status() {
                    cmd_task::move_status(&mut t, want, &caller);
                    want.as_str().to_string()
                } else {
                    t.log(&format!("edited {}", caller.byline()));
                    "edit".to_string()
                };
                cmd_task::write_task(self.store, &t, &verb, &caller).map_err(|e| Refusal::Write(e.to_string()))?;
                Applied::Updated(row.id.clone())
            }
            Some(_) => {
                let local = heads.head(subject).copied();
                return self.reconcile.reconcile(self.store, obj, id, local);
            }
        };
        heads.set(subject, id, file_digest(self.store, &row.id.0));
        heads.write(self.store).map_err(|e| Refusal::Write(e.to_string()))?;
        Ok(applied)
    }
}

/// The task a row record is: its file's text where it carries one (as the
/// mirror writes `body`), else its head with `prose` as the body; under the
/// record's id, title and workspace whichever it was.
pub(crate) fn task_of_record(row: &wsp_store_contract::Row) -> Option<Task> {
    let mut t = crate::mirror::task_of(row)?;
    if !row.fields.contains_key(crate::mirror::BODY) {
        t.body = row.fields.get(wsp_view::common::key::PROSE).cloned().unwrap_or_default();
    }
    t.id = row.id.0.clone();
    t.title = row.title.clone();
    t.workspace = row.workspace.clone();
    Some(t)
}

/// The name a principal's write is logged under.
fn user_of(by: &Principal) -> String {
    match by {
        Principal::Person(n) => n.clone(),
        Principal::Seat(s) => s.0.clone(),
        Principal::Manager(m) | Principal::Executor(m) => m.0.clone(),
        Principal::Wsp(i) => format!("wsp:{i}"),
    }
}

fn entity_kind(e: &Entity) -> &'static str {
    match e {
        Entity::Row(_) => "row",
        Entity::List(_) => "list",
        Entity::Seat(_) => "seat",
        Entity::Claim(_) => "claim",
        Entity::Order(_) => "order",
        Entity::Ask(_) => "ask",
        Entity::Machine(_) => "machine",
    }
}

/// The SHA-256 of a row's file as it is now; `None` for no file.
fn file_digest(store: &Store, id: &str) -> Option<ObjectId> {
    std::fs::read(store.task_path(id)).ok().map(|b| ObjectId::of(&b))
}

/// Per subject, the head the spoke last synced and the digest of the file it
/// left: `sync-heads.json` in the state directory, a list since a subject
/// cannot key a JSON map.
#[derive(Debug, Default)]
pub(crate) struct Heads {
    by_subject: BTreeMap<String, (Subject, ObjectId, Option<ObjectId>)>,
}

impl Heads {
    const FILE: &'static str = "sync-heads.json";

    pub(crate) fn read(store: &Store) -> Heads {
        let text = std::fs::read_to_string(store.state_file(Self::FILE)).unwrap_or_default();
        let list: Vec<Value> = serde_json::from_str(&text).unwrap_or_default();
        let mut by_subject = BTreeMap::new();
        for v in list {
            let subject: Option<Subject> = v.get("subject").and_then(|s| serde_json::from_value(s.clone()).ok());
            let head = v.get("head").and_then(Value::as_str).and_then(|h| h.parse().ok());
            let file = v.get("file").and_then(Value::as_str).and_then(|h| h.parse().ok());
            if let (Some(subject), Some(head)) = (subject, head) {
                by_subject.insert(key(&subject), (subject, head, file));
            }
        }
        Heads { by_subject }
    }

    pub(crate) fn head(&self, subject: &Subject) -> Option<&ObjectId> {
        self.by_subject.get(&key(subject)).map(|(_, h, _)| h)
    }

    pub(crate) fn file(&self, subject: &Subject) -> Option<ObjectId> {
        self.by_subject.get(&key(subject)).and_then(|(_, _, f)| *f)
    }

    pub(crate) fn set(&mut self, subject: &Subject, head: ObjectId, file: Option<ObjectId>) {
        self.by_subject.insert(key(subject), (subject.clone(), head, file));
    }

    pub(crate) fn write(&self, store: &Store) -> std::io::Result<()> {
        let list: Vec<Value> = self
            .by_subject
            .values()
            .map(|(s, h, f)| json!({ "subject": s, "head": h.to_string(), "file": f.map(|f| f.to_string()) }))
            .collect();
        std::fs::create_dir_all(&store.state)?;
        std::fs::write(store.state_file(Self::FILE), serde_json::to_string_pretty(&list).unwrap_or_default())
    }
}

fn key(subject: &Subject) -> String {
    serde_json::to_string(subject).unwrap_or_default()
}

/// The shared rows a sync worker would push: each whose file is not the one
/// its synced head left, or that has no head. An import leaves none behind.
pub(crate) fn pending(store: &Store) -> Vec<Subject> {
    let heads = Heads::read(store);
    let Ok(dir) = std::fs::read_dir(store.tasks_dir()) else { return Vec::new() };
    let mut ids: Vec<String> = dir
        .filter_map(|e| e.ok()?.path().file_name()?.to_str()?.strip_suffix(".md").map(str::to_string))
        .collect();
    ids.sort();
    ids.into_iter()
        .filter_map(|id| {
            let t = store.task(&id)?;
            let subject = Subject::Row(RowId(id.clone()));
            let moved = heads.head(&subject).is_none() || heads.file(&subject) != file_digest(store, &id);
            (!t.workspace.is_private() && moved).then_some(subject)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use wsp_objects::WorkspaceId as ObjWorkspace;
    use wsp_store_contract::{At, SharedName, WorkspaceId};

    /// A reconcile that records what it was handed and writes nothing.
    #[derive(Default)]
    struct Recording(RefCell<Vec<(ObjectId, Option<ObjectId>)>>);

    impl Reconcile for Recording {
        fn reconcile(&self, _: &Store, _: &Object, id: ObjectId, local: Option<ObjectId>) -> Result<Applied, Refusal> {
            self.0.borrow_mut().push((id, local));
            Ok(Applied::Reconciled("recorded".into()))
        }
    }

    fn shared() -> WorkspaceId {
        WorkspaceId::Shared(SharedName("team".into()))
    }

    /// A record as a spoke would push it: a task projected the way the mirror
    /// projects one, its head and its prose in the row's fields.
    fn record(id: &str, title: &str, status: &str, prose: &str, parent: Option<ObjectId>) -> Object {
        let mut t = Task::new(title, id);
        t.project = Some("wsp".into());
        t.status_raw = status.into();
        t.workspace = shared();
        t.body = prose.into();
        let row = crate::mirror::rows(&[t], &BTreeMap::new(), &[], &BTreeMap::new(), &BTreeMap::new())
            .into_iter()
            .find_map(|e| match e {
                Entity::Row(r) => Some(r),
                _ => None,
            })
            .unwrap();
        Object::Record {
            workspace: ObjWorkspace("shared:team".into()),
            subject: Subject::Row(RowId(id.into())),
            entity: Entity::Row(row),
            parent,
            by: Principal::Person("ann".into()),
            at: At(1),
        }
    }

    fn ann() -> Principal {
        Principal::Person("ann".into())
    }

    #[test]
    fn a_new_shared_row_writes_its_file_and_is_not_pushed_back() {
        let store = crate::testkit::store("door-new");
        let first = record("wsp-900", "a shared row", "todo", "## Overview\nwhat it is\n", None);
        assert_eq!(Door::new(&store, &Unreconciled).import(&first, &ann()), Ok(Applied::Created(RowId("wsp-900".into()))));
        let t = store.task("wsp-900").expect("the file is written");
        assert_eq!(t.title, "a shared row");
        assert_eq!(t.workspace, shared());
        assert!(t.body.contains("what it is"), "{}", t.body);
        assert!(t.body.contains("added by ann via the hub"), "{}", t.body);
        assert_eq!(Heads::read(&store).head(first.subject()), Some(&wsp_objects::encode(&first).0));
        assert!(pending(&store).is_empty(), "an import leaves nothing to push: {:?}", pending(&store));
    }

    #[test]
    fn its_child_version_updates_the_file_and_is_not_pushed_back() {
        let store = crate::testkit::store("door-child");
        let door = Door::new(&store, &Unreconciled);
        let first = record("wsp-901", "a shared row", "todo", "## Overview\nfirst\n", None);
        door.import(&first, &ann()).unwrap();
        let child = record("wsp-901", "a shared row, renamed", "doing", "## Overview\nsecond\n", Some(wsp_objects::encode(&first).0));
        assert_eq!(door.import(&child, &ann()), Ok(Applied::Updated(RowId("wsp-901".into()))));
        let t = store.task("wsp-901").unwrap();
        assert_eq!(t.title, "a shared row, renamed");
        assert_eq!(t.status_raw, "doing");
        assert!(t.body.contains("second") && !t.body.contains("first"), "{}", t.body);
        assert!(t.body.contains("→ doing · by ann via the hub"), "{}", t.body);
        assert_eq!(Heads::read(&store).head(child.subject()), Some(&wsp_objects::encode(&child).0));
        assert!(pending(&store).is_empty(), "{:?}", pending(&store));

        // A verb's write after it is the spoke's own, and is what a push sends.
        let mut t = store.task("wsp-901").unwrap();
        cmd_task::move_status(&mut t, crate::model::Status::Review, &Caller::hub("bob"));
        store.save_task(&t).unwrap();
        assert_eq!(pending(&store), vec![Subject::Row(RowId("wsp-901".into()))]);
    }

    #[test]
    fn a_parent_that_is_not_the_local_head_goes_to_reconcile_and_writes_nothing() {
        let store = crate::testkit::store("door-diverged");
        let first = record("wsp-902", "a shared row", "todo", "## Overview\nfirst\n", None);
        Door::new(&store, &Unreconciled).import(&first, &ann()).unwrap();
        let before = std::fs::read_to_string(store.task_path("wsp-902")).unwrap();
        let heads_before = std::fs::read_to_string(store.state_file(Heads::FILE)).unwrap();

        let elsewhere = record("wsp-902", "another line of it", "todo", "## Overview\nother\n", None);
        let stranger = record("wsp-902", "a version of that", "doing", "## Overview\nthird\n", Some(wsp_objects::encode(&elsewhere).0));
        let rec = Recording::default();
        assert_eq!(Door::new(&store, &rec).import(&stranger, &ann()), Ok(Applied::Reconciled("recorded".into())));
        assert_eq!(*rec.0.borrow(), vec![(wsp_objects::encode(&stranger).0, Some(wsp_objects::encode(&first).0))]);
        assert_eq!(std::fs::read_to_string(store.task_path("wsp-902")).unwrap(), before, "the door wrote nothing");
        assert_eq!(std::fs::read_to_string(store.state_file(Heads::FILE)).unwrap(), heads_before, "nor moved the head");

        // And with nothing to reconcile it yet, it is refused, still unwritten.
        assert!(matches!(Door::new(&store, &Unreconciled).import(&stranger, &ann()), Err(Refusal::Diverged { .. })));
        assert_eq!(std::fs::read_to_string(store.task_path("wsp-902")).unwrap(), before);
    }

    #[test]
    fn a_child_of_the_head_over_a_local_edit_not_yet_pushed_goes_to_reconcile() {
        let store = crate::testkit::store("door-pending");
        let first = record("wsp-905", "a shared row", "todo", "## Overview\nfirst\n", None);
        Door::new(&store, &Unreconciled).import(&first, &ann()).unwrap();

        // The person edits it on the spoke through a verb's write: it is pending.
        let mut t = store.task("wsp-905").unwrap();
        t.title = "edited here".into();
        t.log(&format!("edited {}", Caller::hub("bob").byline()));
        cmd_task::write_task(&store, &t, "edit", &Caller::hub("bob")).unwrap();
        assert_eq!(pending(&store), vec![Subject::Row(RowId("wsp-905".into()))]);
        let before = std::fs::read_to_string(store.task_path("wsp-905")).unwrap();
        let heads_before = std::fs::read_to_string(store.state_file(Heads::FILE)).unwrap();

        let head = wsp_objects::encode(&first).0;
        let child = record("wsp-905", "edited there", "doing", "## Overview\nsecond\n", Some(head));
        let rec = Recording::default();
        assert_eq!(Door::new(&store, &rec).import(&child, &ann()), Ok(Applied::Reconciled("recorded".into())));
        assert_eq!(*rec.0.borrow(), vec![(wsp_objects::encode(&child).0, Some(head))]);
        assert_eq!(std::fs::read_to_string(store.task_path("wsp-905")).unwrap(), before, "the local edit stands");
        assert_eq!(std::fs::read_to_string(store.state_file(Heads::FILE)).unwrap(), heads_before, "nor moved the head");
        assert_eq!(pending(&store), vec![Subject::Row(RowId("wsp-905".into()))], "still the spoke's to push");
    }

    #[test]
    fn a_file_the_spoke_never_synced_goes_to_reconcile() {
        let store = crate::testkit::store("door-unsynced");
        let mut t = Task::new("mine", "wsp-903");
        t.workspace = shared();
        store.save_task(&t).unwrap();
        let rec = Recording::default();
        let theirs = record("wsp-903", "theirs", "todo", "", None);
        assert_eq!(Door::new(&store, &rec).import(&theirs, &ann()), Ok(Applied::Reconciled("recorded".into())));
        assert_eq!(rec.0.borrow()[0].1, None);
        assert_eq!(store.task("wsp-903").unwrap().title, "mine");
    }

    #[test]
    fn what_the_files_do_not_take_yet_is_refused() {
        let store = crate::testkit::store("door-refused");
        let door = Door::new(&store, &Unreconciled);
        let mut private = record("wsp-904", "private", "todo", "", None);
        if let Object::Record { entity: Entity::Row(r), .. } = &mut private {
            r.workspace = WorkspaceId::Private;
        }
        assert_eq!(door.import(&private, &ann()), Err(Refusal::Private(RowId("wsp-904".into()))));
        let event = Object::Event {
            workspace: ObjWorkspace("shared:team".into()),
            subject: Subject::Row(RowId("wsp-904".into())),
            kind: wsp_store_contract::EventKind::RowAdded,
            payload: Value::Null,
            parent: None,
            by: ann(),
            at: At(1),
        };
        assert!(matches!(door.import(&event, &ann()), Err(Refusal::NotYet(_))));
        assert!(store.task("wsp-904").is_none());
    }
}
