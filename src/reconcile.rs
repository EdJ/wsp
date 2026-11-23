//! What happens after a `stale` (wsp-176): the spoke's version of a row and the
//! hub's, merged over the version both came from.
//!
//! The version an edit saw is its parent object, so an edit made over a head
//! the hub has since moved is refused `stale` (wsp-459) and arrives here as
//! three records: `base`, the common ancestor (walked through parents,
//! [`common_ancestor`]); `ours`, the spoke's file; and `theirs`, the hub's
//! head. [`reconcile`] merges them field by field:
//!
//! - a field changed on one side only takes that side;
//! - the `## Log` and `## Decisions` sections are dated notes, and are the
//!   union of both sides' entries in time order;
//! - a field changed on both sides to different values keeps the hub's, and
//!   the spoke's value is written into the row's log as a conflict note, with
//!   the row tagged [`FLAG`] for the user to look at;
//! - status is not merged field-wise: the hub's lease (wsp-466) decides who may
//!   move it, and a status change from a spoke without the lease is not applied
//!   but noted as a conflict.
//!
//! [`Merging`] is the door's [`Reconcile`]: it writes the merged task to the
//! file and records the hub's head as the one synced, leaving the file pending,
//! so the merge is pushed as usual as a new object whose parent is theirs
//! ([`crate::hub_sync::push_record`]).

use std::collections::{BTreeMap, BTreeSet};

use wsp_objects::{Object, ObjectId};
use wsp_store_contract::{Entity, RowId};

use crate::cmd_message::Caller;
use crate::cmd_task;
use crate::door::{task_of_record, Applied, Heads, Reconcile, Refusal};
use crate::model::{section_of, set_section_in, Task};
use crate::store::Store;

/// The tag a row carries while a reconcile left a conflict on it for the user.
pub(crate) const FLAG: &str = "conflict";

/// The sections whose entries are dated notes, merged as unions in time order.
const DATED: [&str; 2] = ["Log", "Decisions"];

/// One field both sides changed: the hub's value was kept, the spoke's is here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Conflict {
    /// The field's name (`title`, `status`, `## Overview`, ...).
    pub(crate) field: String,
    /// The spoke's value, which was not applied.
    pub(crate) ours: String,
    /// The hub's value, which was kept.
    pub(crate) theirs: String,
    /// Why ours lost: both sides changed it, or a status moved without the lease.
    pub(crate) why: Why,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Why {
    BothChanged,
    NoLease,
}

/// What [`reconcile`] made of the three versions.
#[derive(Clone, Debug)]
pub(crate) enum Reconciled {
    /// Every change applied; nothing for the user.
    Merged(Task),
    /// The merge, with the hub's value kept where ours was not applied. The
    /// task already carries the conflict notes and the [`FLAG`] tag.
    Conflict(Task, Vec<Conflict>),
}

impl Reconciled {
    pub(crate) fn task(&self) -> &Task {
        match self {
            Reconciled::Merged(t) | Reconciled::Conflict(t, _) => t,
        }
    }
}

/// Whether this spoke holds the hub's lease on a row, which is what lets it
/// move the row's status (wsp-466).
pub(crate) trait Lease {
    fn holds(&self, row: &RowId) -> bool;
}

/// Until wsp-466: no lease is taken from the hub, so none is held, and a
/// spoke's status change over a stale head is never applied.
pub(crate) struct NoLease;

impl Lease for NoLease {
    fn holds(&self, _: &RowId) -> bool {
        false
    }
}

impl Lease for bool {
    fn holds(&self, _: &RowId) -> bool {
        *self
    }
}

/// `ours` and `theirs` merged over `base`, `None` where the two have no common
/// ancestor (a file the spoke never synced): then every field they differ on
/// counts as changed on both sides. `leased` says whether this spoke holds the
/// row's lease.
pub(crate) fn reconcile(base: Option<&Task>, ours: &Task, theirs: &Task, leased: bool) -> Reconciled {
    let mut m = Merge { conflicts: Vec::new() };
    let b = |f: fn(&Task) -> String| base.map(f);
    let mut t = theirs.clone();

    t.title = m.field("title", b(|t| t.title.clone()), ours.title.clone(), theirs.title.clone());
    t.project = m.opt("project", base.map(|t| t.project.clone()), &ours.project, &theirs.project);
    t.tags = m.list("tags", base.map(|t| t.tags.clone()), &ours.tags, &theirs.tags);
    t.priority_raw = m.field("priority", b(|t| t.priority_raw.clone()), ours.priority_raw.clone(), theirs.priority_raw.clone());
    t.parent = m.opt("parent", base.map(|t| t.parent.clone()), &ours.parent, &theirs.parent);
    t.after = m.list("after", base.map(|t| t.after.clone()), &ours.after, &theirs.after);
    t.refs = m.list("refs", base.map(|t| t.refs.clone()), &ours.refs, &theirs.refs);
    t.caption = m.opt("caption", base.map(|t| t.caption.clone()), &ours.caption, &theirs.caption);
    t.for_user = m.opt("for", base.map(|t| t.for_user.clone()), &ours.for_user, &theirs.for_user);
    t.updated = ours.updated.clone().max(theirs.updated.clone());

    // Status: the lease decides, not the fields.
    let ours_moved = match base {
        Some(b) => ours.status_raw != b.status_raw,
        None => true,
    };
    if ours.status_raw != theirs.status_raw && ours_moved {
        if leased {
            t.status_raw = ours.status_raw.clone();
        } else {
            m.conflicts.push(Conflict {
                field: "status".into(),
                ours: ours.status_raw.clone(),
                theirs: theirs.status_raw.clone(),
                why: Why::NoLease,
            });
        }
    }

    t.body = m.body(base.map(|t| t.body.as_str()), &ours.body, &theirs.body);

    if m.conflicts.is_empty() {
        return Reconciled::Merged(t);
    }
    for c in &m.conflicts {
        t.log(&note(c));
    }
    if !t.tags.iter().any(|g| g == FLAG) {
        t.tags.push(FLAG.into());
    }
    Reconciled::Conflict(t, m.conflicts)
}

/// The conflict note written into the row's log: the spoke's value whole, on
/// indented lines where it has more than one, so nothing of it is lost.
fn note(c: &Conflict) -> String {
    let why = match c.why {
        Why::BothChanged => "changed here and on the hub; kept the hub's",
        Why::NoLease => "moved here without the hub's lease; kept the hub's",
    };
    let ours = if c.ours.is_empty() { "(nothing)".to_string() } else { c.ours.clone() };
    if ours.contains('\n') {
        let indented: Vec<String> = ours.lines().map(|l| format!("  {l}")).collect();
        format!("conflict: {} {why} ({:?}); this spoke had:\n{}", c.field, one_line(&c.theirs), indented.join("\n"))
    } else {
        format!("conflict: {} {why} ({:?}); this spoke had {ours:?}", c.field, one_line(&c.theirs))
    }
}

fn one_line(s: &str) -> String {
    let first = s.lines().next().unwrap_or("");
    if s.lines().nth(1).is_some() {
        format!("{first} …")
    } else {
        first.to_string()
    }
}

struct Merge {
    conflicts: Vec<Conflict>,
}

impl Merge {
    /// The three-way rule for one field, shown as `show` in a conflict note.
    fn pick<T: PartialEq + Clone>(&mut self, name: &str, base: Option<T>, ours: T, theirs: T, show: impl Fn(&T) -> String) -> T {
        if ours == theirs || base.as_ref() == Some(&ours) {
            return theirs;
        }
        if base.as_ref() == Some(&theirs) {
            return ours;
        }
        self.conflicts.push(Conflict { field: name.into(), ours: show(&ours), theirs: show(&theirs), why: Why::BothChanged });
        theirs
    }

    fn field(&mut self, name: &str, base: Option<String>, ours: String, theirs: String) -> String {
        self.pick(name, base, ours, theirs, String::clone)
    }

    fn opt(&mut self, name: &str, base: Option<Option<String>>, ours: &Option<String>, theirs: &Option<String>) -> Option<String> {
        self.pick(name, base, ours.clone(), theirs.clone(), |v| v.clone().unwrap_or_default())
    }

    fn list(&mut self, name: &str, base: Option<Vec<String>>, ours: &[String], theirs: &[String]) -> Vec<String> {
        self.pick(name, base, ours.to_vec(), theirs.to_vec(), |v| v.join(", "))
    }

    /// The body section by section: dated sections as unions, the rest by the
    /// three-way rule, in the hub's order with the spoke's new ones after.
    fn body(&mut self, base: Option<&str>, ours: &str, theirs: &str) -> String {
        let (os, ts) = (sections(ours), sections(theirs));
        let mut names: Vec<String> = Vec::new();
        for n in ts.iter().chain(os.iter()) {
            if !names.iter().any(|m| m.eq_ignore_ascii_case(n)) {
                names.push(n.clone());
            }
        }
        let mut out = String::new();
        for name in names {
            let read = |body: &str| match name.is_empty() {
                true => preamble(body),
                false => section_of(body, &name).unwrap_or_default(),
            };
            let (o, t) = (read(ours), read(theirs));
            let text = if DATED.iter().any(|d| d.eq_ignore_ascii_case(&name)) {
                union_in_time_order(&o, &t)
            } else {
                let label = if name.is_empty() { "the text before the first heading".to_string() } else { format!("## {name}") };
                self.field(&label, base.map(read), o, t)
            };
            if !text.trim().is_empty() {
                set_section_in(&mut out, &name, &text);
            }
        }
        out
    }
}

/// A body's section names in order, `""` first for the text before the first
/// heading.
fn sections(body: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    out.extend(body.lines().filter_map(|l| l.strip_prefix("## ")).map(|h| h.trim().to_string()));
    out
}

fn preamble(body: &str) -> String {
    body.lines().take_while(|l| !l.starts_with("## ")).collect::<Vec<_>>().join("\n").trim_end().to_string()
}

/// Both sides' dated entries, each once, in time order (the hub's first where
/// two share a stamp). An entry is a `- <stamp> …` line and the lines under it
/// up to the next; text before the first entry is kept from whichever side
/// has it, the hub's first.
pub(crate) fn union_in_time_order(ours: &str, theirs: &str) -> String {
    let (th, te) = entries(theirs);
    let (oh, oe) = entries(ours);
    let mut all: Vec<(String, String)> = te.clone();
    let have: BTreeSet<&(String, String)> = te.iter().collect();
    all.extend(oe.iter().filter(|e| !have.contains(e)).cloned());
    all.sort_by(|a, b| a.0.cmp(&b.0));
    let head = if th.trim().is_empty() { oh } else { th };
    let mut out: Vec<String> = Vec::new();
    if !head.trim().is_empty() {
        out.push(head);
    }
    out.extend(all.into_iter().map(|(_, text)| text));
    out.join("\n")
}

/// `(text before the first entry, [(stamp, entry text)])`.
fn entries(text: &str) -> (String, Vec<(String, String)>) {
    let stamp = |l: &str| -> Option<String> {
        let (d, _) = l.trim_start().strip_prefix("- ")?.split_once(' ')?;
        crate::util::is_stamp(d).then(|| d.to_string())
    };
    let mut head: Vec<&str> = Vec::new();
    let mut out: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        match (stamp(line), out.last_mut()) {
            (Some(s), _) => out.push((s, line.trim_end().to_string())),
            (None, Some(last)) => {
                last.1.push('\n');
                last.1.push_str(line.trim_end());
            }
            (None, None) => head.push(line),
        }
    }
    for e in &mut out {
        e.1 = e.1.trim_end().to_string();
    }
    (head.join("\n").trim_end().to_string(), out)
}

/// Where the spoke reads the objects behind its heads.
pub(crate) trait Objects {
    fn object(&self, id: &ObjectId) -> Option<Object>;
}

impl Objects for BTreeMap<ObjectId, Object> {
    fn object(&self, id: &ObjectId) -> Option<Object> {
        self.get(id).cloned()
    }
}

impl Objects for BTreeMap<ObjectId, Vec<u8>> {
    fn object(&self, id: &ObjectId) -> Option<Object> {
        wsp_objects::decode(self.get(id)?).ok()
    }
}

/// The nearest object both `ours` and `theirs` descend from (each counting as
/// its own descendant): every ancestor of `ours`, then `theirs`'s line walked
/// back until it meets one. `None` where they never meet within the objects
/// the spoke holds.
pub(crate) fn common_ancestor(objects: &dyn Objects, ours: ObjectId, theirs: ObjectId) -> Option<ObjectId> {
    let mut mine = BTreeSet::new();
    let mut at = Some(ours);
    while let Some(id) = at {
        if !mine.insert(id) {
            break;
        }
        at = objects.object(&id).and_then(|o| o.parent().copied());
    }
    let mut seen = BTreeSet::new();
    let mut at = Some(theirs);
    while let Some(id) = at {
        if mine.contains(&id) {
            return Some(id);
        }
        if !seen.insert(id) {
            break;
        }
        at = objects.object(&id).and_then(|o| o.parent().copied());
    }
    None
}

/// The door's reconcile: the merge written to the file, the hub's head
/// recorded as synced, and the file left pending so the merge is pushed as a
/// new object whose parent is theirs.
pub(crate) struct Merging<'a> {
    pub(crate) objects: &'a dyn Objects,
    pub(crate) lease: &'a dyn Lease,
}

impl Reconcile for Merging<'_> {
    fn reconcile(&self, store: &Store, obj: &Object, id: ObjectId, local: Option<ObjectId>) -> Result<Applied, Refusal> {
        let Object::Record { subject, entity: Entity::Row(row), .. } = obj else {
            return Err(Refusal::NotYet("an event or a record other than a row".into()));
        };
        let theirs = task_of_record(row).ok_or_else(|| Refusal::Unreadable(row.id.clone()))?;
        let ours = store.task(&row.id.0).ok_or_else(|| Refusal::Unreadable(row.id.clone()))?;
        let base = local
            .and_then(|l| common_ancestor(self.objects, l, id))
            .and_then(|b| self.objects.object(&b))
            .and_then(|o| match o {
                Object::Record { entity: Entity::Row(r), .. } => task_of_record(&r),
                _ => None,
            });
        let merged = reconcile(base.as_ref(), &ours, &theirs, self.lease.holds(&row.id));
        let mut t = merged.task().clone();
        let caller = Caller::here(store);
        t.log(&format!("merged with the hub's {} {}", &id.hex()[..12], caller.byline()));
        cmd_task::write_task(store, &t, "reconcile", &caller).map_err(|e| Refusal::Write(e.to_string()))?;
        let mut heads = Heads::read(store);
        // The hub's head is the one synced; no file digest, so the merge is pending.
        heads.set(subject, id, None);
        heads.write(store).map_err(|e| Refusal::Write(e.to_string()))?;
        Ok(Applied::Reconciled(match merged {
            Reconciled::Merged(_) => format!("{}: merged", row.id.0),
            Reconciled::Conflict(_, cs) => {
                let fields: Vec<&str> = cs.iter().map(|c| c.field.as_str()).collect();
                format!("{}: merged, the hub's kept on {}; flagged `{FLAG}`", row.id.0, fields.join(", "))
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::door::{pending, Door, Unreconciled};
    use wsp_objects::WorkspaceId as ObjWorkspace;
    use wsp_store_contract::{At, Principal, SharedName, Subject, WorkspaceId};

    const BASE_LOG: &str = "- 2026-11-20T08:00:00Z added\n";

    fn task(title: &str, status: &str, prio: &str, overview: &str, log: &str) -> Task {
        let mut t = Task::new(title, "wsp-950");
        t.status_raw = status.into();
        t.priority_raw = prio.into();
        t.body = format!("## Overview\n{overview}\n\n## Log\n{log}");
        t
    }

    fn base() -> Task {
        task("a row", "todo", "normal", "what it is", BASE_LOG)
    }

    fn log_of(t: &Task) -> Vec<String> {
        t.section("Log").unwrap_or_default().lines().map(str::to_string).collect()
    }

    /// One case: what each side did to the base, whether the spoke holds the
    /// lease, and what must hold of the result.
    struct Case {
        name: &'static str,
        ours: fn(&mut Task),
        theirs: fn(&mut Task),
        leased: bool,
        check: fn(&Reconciled),
    }

    #[test]
    fn reconcile_table() {
        let cases = [
            Case {
                name: "disjoint fields merge",
                ours: |t| t.title = "renamed here".into(),
                theirs: |t| {
                    t.priority_raw = "high".into();
                    t.body = t.body.replace("what it is", "what it is, said better");
                },
                leased: false,
                check: |r| {
                    let Reconciled::Merged(t) = r else { panic!("a conflict: {r:?}") };
                    assert_eq!(t.title, "renamed here");
                    assert_eq!(t.priority_raw, "high");
                    assert_eq!(t.section("Overview").as_deref(), Some("what it is, said better"));
                    assert!(!t.tags.iter().any(|g| g == FLAG));
                },
            },
            Case {
                name: "both sides adding log lines keep both in time order",
                ours: |t| t.body.push_str("- 2026-11-21T10:00:00Z ours, between\n"),
                theirs: |t| t.body.push_str("- 2026-11-21T09:00:00Z theirs, first\n- 2026-11-21T11:00:00Z theirs, last\n  and a line under it\n"),
                leased: false,
                check: |r| {
                    let Reconciled::Merged(t) = r else { panic!("a conflict: {r:?}") };
                    assert_eq!(
                        log_of(t),
                        vec![
                            "- 2026-11-20T08:00:00Z added",
                            "- 2026-11-21T09:00:00Z theirs, first",
                            "- 2026-11-21T10:00:00Z ours, between",
                            "- 2026-11-21T11:00:00Z theirs, last",
                            "  and a line under it",
                        ]
                    );
                },
            },
            Case {
                name: "the same field both sides keeps theirs, notes ours and flags",
                ours: |t| t.title = "ours".into(),
                theirs: |t| t.title = "theirs".into(),
                leased: false,
                check: |r| {
                    let Reconciled::Conflict(t, cs) = r else { panic!("merged: {r:?}") };
                    assert_eq!(t.title, "theirs");
                    assert_eq!(cs, &vec![Conflict { field: "title".into(), ours: "ours".into(), theirs: "theirs".into(), why: Why::BothChanged }]);
                    assert!(t.tags.iter().any(|g| g == FLAG), "{:?}", t.tags);
                    let log = log_of(t).join("\n");
                    assert!(log.contains("conflict: title") && log.contains("this spoke had \"ours\""), "{log}");
                },
            },
            Case {
                name: "a section both sides rewrote keeps theirs and notes ours whole",
                ours: |t| t.body = t.body.replace("what it is", "ours\nsecond line"),
                theirs: |t| t.body = t.body.replace("what it is", "theirs"),
                leased: false,
                check: |r| {
                    let Reconciled::Conflict(t, cs) = r else { panic!("merged: {r:?}") };
                    assert_eq!(t.section("Overview").as_deref(), Some("theirs"));
                    assert_eq!(cs[0].field, "## Overview");
                    let log = t.section("Log").unwrap();
                    assert!(log.contains("this spoke had:\n  ours\n  second line"), "{log}");
                },
            },
            Case {
                name: "a status change without the lease is not applied",
                ours: |t| t.status_raw = "doing".into(),
                theirs: |t| t.title = "renamed there".into(),
                leased: false,
                check: |r| {
                    let Reconciled::Conflict(t, cs) = r else { panic!("merged: {r:?}") };
                    assert_eq!(t.status_raw, "todo");
                    assert_eq!(t.title, "renamed there");
                    assert_eq!(cs, &vec![Conflict { field: "status".into(), ours: "doing".into(), theirs: "todo".into(), why: Why::NoLease }]);
                    assert!(t.tags.iter().any(|g| g == FLAG));
                    assert!(t.section("Log").unwrap().contains("without the hub's lease"));
                },
            },
            Case {
                name: "a status change with the lease is applied over the hub's",
                ours: |t| t.status_raw = "review".into(),
                theirs: |t| t.status_raw = "doing".into(),
                leased: true,
                check: |r| {
                    let Reconciled::Merged(t) = r else { panic!("a conflict: {r:?}") };
                    assert_eq!(t.status_raw, "review");
                },
            },
            Case {
                name: "the hub's status change stands where the spoke did not move it",
                ours: |t| t.title = "renamed here".into(),
                theirs: |t| t.status_raw = "doing".into(),
                leased: false,
                check: |r| {
                    let Reconciled::Merged(t) = r else { panic!("a conflict: {r:?}") };
                    assert_eq!((t.status_raw.as_str(), t.title.as_str()), ("doing", "renamed here"));
                },
            },
        ];
        for c in cases {
            let (mut ours, mut theirs) = (base(), base());
            (c.ours)(&mut ours);
            (c.theirs)(&mut theirs);
            let r = reconcile(Some(&base()), &ours, &theirs, c.leased);
            eprintln!("case: {}", c.name);
            (c.check)(&r);
        }
    }

    #[test]
    fn reconcile_without_a_base_keeps_theirs_where_they_differ() {
        let ours = task("mine", "todo", "normal", "same", BASE_LOG);
        let theirs = task("theirs", "todo", "normal", "same", BASE_LOG);
        let Reconciled::Conflict(t, cs) = reconcile(None, &ours, &theirs, false) else { panic!("merged") };
        assert_eq!(t.title, "theirs");
        assert_eq!(cs.len(), 1, "{cs:?}");
    }

    fn record(t: &Task, parent: Option<ObjectId>) -> Object {
        let mut t = t.clone();
        t.workspace = WorkspaceId::Shared(SharedName("team".into()));
        t.project = Some("wsp".into());
        let row = crate::mirror::rows(&[t], &BTreeMap::new(), &[], &BTreeMap::new(), &BTreeMap::new())
            .into_iter()
            .find_map(|e| match e {
                Entity::Row(r) => Some(r),
                _ => None,
            })
            .unwrap();
        Object::Record {
            workspace: ObjWorkspace("shared:team".into()),
            subject: Subject::Row(RowId("wsp-950".into())),
            entity: Entity::Row(row),
            parent,
            by: Principal::Person("ann".into()),
            at: At(1),
        }
    }

    #[test]
    fn reconcile_finds_the_common_ancestor_through_parents() {
        let a = record(&base(), None);
        let ida = wsp_objects::encode(&a).0;
        let b = record(&task("b", "todo", "normal", "x", BASE_LOG), Some(ida));
        let idb = wsp_objects::encode(&b).0;
        let c = record(&task("c", "todo", "normal", "x", BASE_LOG), Some(idb));
        let idc = wsp_objects::encode(&c).0;
        let d = record(&task("d", "todo", "normal", "x", BASE_LOG), Some(ida));
        let idd = wsp_objects::encode(&d).0;
        let objects: BTreeMap<ObjectId, Object> = [(ida, a), (idb, b), (idc, c), (idd, d)].into();
        assert_eq!(common_ancestor(&objects, idd, idc), Some(ida));
        assert_eq!(common_ancestor(&objects, idb, idc), Some(idb), "ours is theirs's ancestor");
        let stray = wsp_objects::encode(&record(&task("e", "todo", "normal", "y", BASE_LOG), None)).0;
        assert_eq!(common_ancestor(&objects, stray, idc), None);
    }

    #[test]
    fn reconcile_after_a_stale_writes_the_merge_and_pushes_it_as_a_child_of_theirs() {
        let store = crate::testkit::store("reconcile-stale");
        let ann = Principal::Person("ann".into());
        let first = record(&base(), None);
        let id1 = wsp_objects::encode(&first).0;
        Door::new(&store, &Unreconciled).import(&first, &ann).unwrap();

        // The spoke renames it and moves it, unpushed; the hub took another edit.
        let mut t = store.task("wsp-950").unwrap();
        t.title = "renamed here".into();
        t.status_raw = "doing".into();
        store.save_task(&t).unwrap();
        let mut hub = base();
        hub.priority_raw = "high".into();
        hub.body.push_str("- 2026-11-21T09:00:00Z edited on the hub\n");
        let theirs = record(&hub, Some(id1));
        let id2 = wsp_objects::encode(&theirs).0;
        let objects: BTreeMap<ObjectId, Object> = [(id1, first), (id2, theirs)].into();

        let stale = wsp_client::Stale { subject: Subject::Row(RowId("wsp-950".into())), from: Some(id1), head: Some(id2) };
        let got = crate::hub_sync::after_stale(&store, &stale, &objects, &NoLease, &ann).unwrap();
        assert!(matches!(&got, Applied::Reconciled(s) if s.contains("status")), "{got:?}");

        let t = store.task("wsp-950").unwrap();
        assert_eq!(t.title, "renamed here", "ours, changed on one side");
        assert_eq!(t.priority_raw, "high", "theirs, changed on one side");
        assert_eq!(t.status_raw, "todo", "no lease: the spoke's move is not applied");
        assert!(t.tags.iter().any(|g| g == FLAG));
        assert!(t.body.contains("edited on the hub"), "{}", t.body);

        // The hub's head is the synced one, and the merge is pending: its push is a child of theirs.
        let subject = Subject::Row(RowId("wsp-950".into()));
        assert_eq!(Heads::read(&store).head(&subject), Some(&id2));
        assert_eq!(pending(&store), vec![subject.clone()]);
        let push = crate::hub_sync::push_record(&store, &subject, ObjWorkspace("shared:team".into()), ann, At(2)).unwrap();
        assert_eq!(push.parent(), Some(&id2));
        let Object::Record { entity: Entity::Row(row), .. } = &push else { panic!() };
        assert_eq!(task_of_record(row).unwrap().title, "renamed here");
    }
}
