//! The sync protocol against the in-memory reference hub (wsp-459): two spokes
//! with disjoint changes converge and a second sync sends nothing; one change
//! among a thousand records sends one object; a move from a stale head is
//! refused with the head there is, reported, and not retried.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use wsp_client::testing::{FakeHub, MemHub};
use wsp_client::{Client, ObjectSource, Stale, SyncReport};
use wsp_api::{Blob, Move, ObjectId, RefMove, Refs, WorkspaceId};
use wsp_objects::{encode, Object};
use wsp_store_contract::{At, Entity, Principal, ProjectId, Row, RowId, RowState, Subject};

fn ws() -> WorkspaceId {
    WorkspaceId("shared:mp".into())
}

fn person(name: &str) -> Principal {
    Principal::Person(name.into())
}

fn subject(row: &str) -> Subject {
    Subject::Row(RowId(row.into()))
}

/// One spoke: its heads and every object behind them, as its store would hold
/// them (wsp-464), and the clock it stamps its writes with.
struct Spoke {
    by: Principal,
    refs: Refs,
    objects: BTreeMap<ObjectId, Vec<u8>>,
    clock: u64,
}

impl Spoke {
    fn new(name: &str) -> Spoke {
        Spoke { by: person(name), refs: Refs::new(ws()), objects: BTreeMap::new(), clock: 1_795_400_000_000 }
    }

    /// A new version of `row`, titled `title`, following the head it has.
    fn write(&mut self, row: &str, title: &str) -> ObjectId {
        self.clock += 1;
        let object = Object::Record {
            workspace: ws(),
            subject: subject(row),
            entity: Entity::Row(Row {
                workspace: Default::default(),
                id: RowId(row.into()),
                project: ProjectId("wsp".into()),
                title: title.into(),
                state: RowState::Todo,
                before_block: None,
                seat: None,
                group: None,
                commit: None,
                fields: BTreeMap::new(),
            }),
            parent: self.refs.get(&subject(row)).copied(),
            by: self.by.clone(),
            at: At(self.clock),
        };
        let (id, bytes) = encode(&object);
        self.objects.insert(id, bytes);
        self.refs.set(subject(row), id);
        id
    }

    /// What a spoke's store does with a report: keep what came, take the heads.
    fn keep(&mut self, report: &SyncReport) {
        for Blob { id, bytes } in &report.received {
            self.objects.insert(*id, bytes.clone());
        }
        self.refs = report.refs.clone();
    }

    fn sync(&mut self, client: &mut Client) -> SyncReport {
        let report = client.sync(&ws(), &self.refs, &self.objects).expect("the sync runs");
        self.keep(&report);
        report
    }
}

#[test]
fn two_spokes_with_disjoint_changes_converge_in_one_sync_each_and_a_second_sync_sends_nothing() {
    let hub = FakeHub::start();
    let (mut a, mut b) = (Spoke::new("ann"), Spoke::new("bo"));
    a.write("wsp-1", "first");
    let a2v1 = a.write("wsp-2", "second");
    let a2v2 = a.write("wsp-2", "second, edited");
    b.write("wsp-3", "third");
    b.write("wsp-4", "fourth");
    let (mut ca, mut cb) = (hub.client(a.by.clone()), hub.client(b.by.clone()));

    let ra = a.sync(&mut ca);
    assert_eq!((ra.sent, ra.received.len()), (3, 0), "ann sends her three objects: {ra:?}");
    assert_eq!(ra.moved, [subject("wsp-1"), subject("wsp-2")]);

    let rb = b.sync(&mut cb);
    assert_eq!((rb.sent, rb.received.len()), (2, 3), "bo sends his two and fetches ann's three: {rb:?}");
    assert_eq!(rb.moved, [subject("wsp-3"), subject("wsp-4")]);
    assert_eq!(rb.adopted, [subject("wsp-1"), subject("wsp-2")]);
    let order: Vec<ObjectId> = rb.received.iter().map(|o| o.id).collect();
    let at = |id: ObjectId| order.iter().position(|x| *x == id).unwrap();
    assert!(at(a2v1) < at(a2v2), "a parent comes before its child");
    assert!(rb.stale.is_empty() && rb.diverged.is_empty());

    // One sync each: the hub and bo hold everything; ann's own is all there.
    assert_eq!(b.refs, hub.hub().refs(&ws()));
    assert_eq!(b.refs.heads.len(), 4);
    assert_eq!(hub.hub().len(), 5);

    // A second sync sends nothing: ann only fetches bo's, bo nothing at all.
    let moves = hub.hub().moves();
    let ra2 = a.sync(&mut ca);
    assert_eq!((ra2.sent, ra2.received.len()), (0, 2), "{ra2:?}");
    assert!(ra2.moved.is_empty());
    let rb2 = b.sync(&mut cb);
    assert_eq!((rb2.sent, rb2.received.len()), (0, 0), "{rb2:?}");
    assert!(rb2.moved.is_empty() && rb2.adopted.is_empty());
    assert_eq!(hub.hub().moves(), moves, "nothing left to move");
    assert_eq!(a.refs, b.refs);
    assert_eq!(a.objects, b.objects);
    assert_eq!(hub.hub().writes(), 5, "every object was written to the hub once");
}

#[test]
fn in_a_workspace_of_a_thousand_records_one_change_sends_one_object() {
    let hub = FakeHub::start();
    let (mut a, mut b) = (Spoke::new("ann"), Spoke::new("bo"));
    for i in 0..1000 {
        a.write(&format!("wsp-{i}"), "a record");
    }
    let (mut ca, mut cb) = (hub.client(a.by.clone()), hub.client(b.by.clone()));
    assert_eq!(a.sync(&mut ca).sent, 1000);
    assert_eq!(b.sync(&mut cb).received.len(), 1000);

    let changed = a.write("wsp-617", "a record, edited");
    let writes = hub.hub().writes();
    let r = a.sync(&mut ca);
    assert_eq!(r.sent, 1, "one change sends one object: {:?}", r.moved);
    assert_eq!(r.moved, [subject("wsp-617")]);
    assert_eq!(hub.hub().writes(), writes + 1);
    assert_eq!(hub.hub().refs(&ws()).get(&subject("wsp-617")), Some(&changed));

    // And the other way: the other spoke fetches that one object.
    let r = b.sync(&mut cb);
    assert_eq!((r.sent, r.received.len()), (0, 1));
    assert_eq!(r.received[0].id, changed);
    assert_eq!(r.adopted, [subject("wsp-617")]);
    assert_eq!(a.refs, b.refs);
}

/// A spoke's objects that, on the first read, let another writer move one head
/// on the hub: the race between `sync.heads` and `refs.move`.
struct Racing<'a> {
    objects: &'a BTreeMap<ObjectId, Vec<u8>>,
    hub: Arc<Mutex<MemHub>>,
    move_to: Cell<Option<(Subject, ObjectId)>>,
}

impl ObjectSource for Racing<'_> {
    fn object(&self, id: &ObjectId) -> Option<Vec<u8>> {
        if let Some((subject, head)) = self.move_to.take() {
            self.hub.lock().unwrap().set_head(&ws(), subject, head);
        }
        self.objects.object(id)
    }
}

#[test]
fn a_move_from_a_stale_head_is_refused_with_the_head_there_is_and_the_client_reports_it() {
    let hub = FakeHub::start();
    let (mut a, mut b) = (Spoke::new("ann"), Spoke::new("bo"));
    let seen = a.write("wsp-1", "first");
    let mut ca = hub.client(a.by.clone());
    a.sync(&mut ca);
    let mut cb = hub.client(b.by.clone());
    b.sync(&mut cb);

    // The verb: from a head the hub no longer has, refused with the one it has.
    let theirs = b.write("wsp-1", "bo's edit");
    let ours = a.write("wsp-1", "ann's edit");
    b.sync(&mut cb);
    let mv = RefMove { workspace: ws(), subject: subject("wsp-1"), from: Some(seen), to: ours };
    assert_eq!(ca.refs_move(mv).unwrap(), Move::Stale { head: Some(theirs) });
    let mv = RefMove { workspace: ws(), subject: subject("wsp-9"), from: Some(seen), to: ours };
    assert_eq!(ca.refs_move(mv).unwrap(), Move::Stale { head: None }, "no head is not the head seen");
    assert_eq!(hub.hub().refs(&ws()).get(&subject("wsp-1")), Some(&theirs), "left as it was");

    // The client: another writer moves the head between `sync.heads` and
    // `refs.move`. The move is refused, reported, and not sent again.
    let hub2 = FakeHub::start();
    let (mut c, mut d) = (Spoke::new("cy"), Spoke::new("di"));
    let base = c.write("wsp-1", "first");
    let mut cc = hub2.client(c.by.clone());
    c.sync(&mut cc);
    let mut cd = hub2.client(d.by.clone());
    d.sync(&mut cd);
    let other = d.write("wsp-1", "di's edit");
    hub2.hub().insert(other, d.objects[&other].clone());
    let mine = c.write("wsp-1", "cy's edit");

    let racing = Racing { objects: &c.objects, hub: hub2.shared(), move_to: Cell::new(Some((subject("wsp-1"), other))) };
    let moves = hub2.hub().moves();
    let r = cc.sync(&ws(), &c.refs, &racing).expect("a stale move is a report, not an error");
    assert_eq!(r.stale, [Stale { subject: subject("wsp-1"), from: Some(base), head: Some(other) }]);
    assert!(r.moved.is_empty());
    assert_eq!(r.sent, 1, "the object went; the head did not");
    assert_eq!(r.refs.get(&subject("wsp-1")), Some(&mine), "the spoke keeps its own head to reconcile from");
    assert_eq!(hub2.hub().moves(), moves + 1, "one refs.move, never retried");
    assert_eq!(hub2.hub().refs(&ws()).get(&subject("wsp-1")), Some(&other));
}

#[test]
fn a_put_is_held_to_its_hash_and_a_second_put_of_one_id_writes_nothing() {
    let hub = FakeHub::start();
    let mut a = Spoke::new("ann");
    let id = a.write("wsp-1", "first");
    let blob = Blob { id, bytes: a.objects[&id].clone() };
    let mut c = hub.client(a.by.clone());
    assert_eq!(c.sync_have(vec![id]).unwrap(), [id]);
    assert_eq!(c.sync_put(vec![blob.clone()]).unwrap(), 1);
    assert_eq!(c.sync_put(vec![blob.clone()]).unwrap(), 0, "already held");
    assert_eq!(hub.hub().writes(), 1);
    assert!(c.sync_have(vec![id]).unwrap().is_empty());
    assert_eq!(c.sync_get(vec![id]).unwrap(), [blob]);

    // Bytes that do not hash to their id refuse the whole put, the good one too.
    let other = a.write("wsp-2", "second");
    let good = Blob { id: other, bytes: a.objects[&other].clone() };
    let bad = Blob { id, bytes: b"{\"record\":{}}".to_vec() };
    match c.sync_put(vec![good, bad]) {
        Err(wsp_client::Error::Failed(f)) => assert_eq!(f.code, wsp_api::Code::Invalid, "{}", f.text),
        other => panic!("a mismatched object is refused: {other:?}"),
    }
    assert!(!hub.hub().holds(&other), "nothing of a refused put is kept");
}
