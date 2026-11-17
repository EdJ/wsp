//! The mirror's projections (wsp-075, `docs/state-core.md` section 7's step 1):
//! today's files → contract [`Entity`]s, one pure function per kind, and
//! [`diff`], which turns what a re-read found into one `Command::Mirror`.
//!
//! Nothing here starts a thread or opens SQLite; the serve leg does. The
//! functions take what the store's readers return (no IO inside), and the
//! `read_*` fns are the thin readers.
//!
//! **Two halves**, split by what each stamp covers in `crates/store/src/store.rs`:
//! - [`Half::RowsEtc`] is what `Store::fingerprint` covers, `live_record_dirs`
//!   (`projects/`, `tasks/`, `machines/`, `worklists/`): rows, lists and their
//!   groups, machines. Claims ride with it because a row's `seat` is read off
//!   them. **`claims.json` and the machines' live facts are state files, and
//!   neither `fingerprint` nor `attention_stamp` covers them**: a claim that
//!   changes alone does not move the fingerprint. The serve leg must either
//!   re-read this half on a stat of `claims.json` and `machines-live` too, or
//!   accept that a claim shows at the next task or list write.
//! - [`Half::Attention`] is what `attention_stamp` covers, `Store::attention_files`
//!   (`flags.json`, `messages.json`): asks, notes and flags.
//!
//! Seats and orders are the managers' (the fold leg), so neither half owns them
//! and [`diff`] never names one in `gone`.

use std::collections::{BTreeMap, BTreeSet};

use wsp_server::mirror::subject_of;
use wsp_store::caption;
use wsp_store::message::{About, Message, Party, Shape, State};
use wsp_store::model::{self, Machine as MachineFile, Status, Task, Worklist, WorklistStatus};
use wsp_store::store::{MachineLive, Store};
use wsp_store_contract as c;
use wsp_store_contract::{
    AskId, AskKind, AskState, At, Command, Entity, GroupId, ListId, ListState, MachineName, Principal, ProjectId,
    RowId, RowState, SeatId, Subject, Token, Transport,
};

/// The two halves a re-read covers, each with its own stamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Half {
    /// `Store::fingerprint`'s: rows, claims, lists and groups, machines.
    RowsEtc,
    /// `Store::attention_stamp`'s: asks, notes and flags.
    Attention,
}

impl Half {
    /// Whether this half's re-read decides `subject` is gone when it is absent.
    pub fn owns(self, subject: &Subject) -> bool {
        match self {
            Half::RowsEtc => matches!(
                subject,
                Subject::Row(_) | Subject::List(_) | Subject::Claim(_) | Subject::Machine(_)
            ),
            Half::Attention => matches!(subject, Subject::Ask(_)),
        }
    }
}

/// What one re-read of the files gives: the entities of each half.
#[derive(Clone, Debug, Default)]
pub struct Halves {
    pub rows_etc: Vec<Entity>,
    pub attention: Vec<Entity>,
}

/// A row's state, from a task's status. `inbox` and `todo` are `Todo` (the word
/// is kept in `fields["status"]`); the rest are one to one. `SentBack` is
/// the verifier's and no file says it, so no row maps to it here.
pub fn row_state(s: Status) -> RowState {
    match s {
        Status::Inbox | Status::Todo => RowState::Todo,
        Status::Doing => RowState::Doing,
        Status::Blocked => RowState::Blocked,
        Status::Parked => RowState::Parked,
        Status::Review => RowState::Review,
        Status::Done => RowState::Done,
    }
}

/// A list's state, from a worklist's status: `draft` is `Planned`; `running`,
/// `held` and `parked` are `Running` (the list has begun; the word is kept in
/// `fields["status"]`, as a list has no `held` state); `done` is `Finished`.
pub fn list_state(s: WorklistStatus) -> ListState {
    match s {
        WorklistStatus::Draft => ListState::Planned,
        WorklistStatus::Running | WorklistStatus::Held | WorklistStatus::Parked => ListState::Running,
        WorklistStatus::Done => ListState::Finished,
    }
}

/// A message's ask state: `open` is `Raised`, `acknowledged` is `Acked`,
/// `answered` is `Answered`, and `abandoned` is `Answered` too (the question
/// is closed; `fields["state"]` keeps the word). A flag is always `Raised`.
pub fn ask_state(s: Option<State>) -> AskState {
    match s {
        Some(State::Open) | None => AskState::Raised,
        Some(State::Acknowledged) => AskState::Acked,
        Some(State::Answered | State::Abandoned) => AskState::Answered,
    }
}

fn put(m: &mut BTreeMap<String, String>, k: &str, v: &str) {
    if !v.trim().is_empty() {
        m.insert(k.to_string(), v.to_string());
    }
}

/// The seat a claim record names: the workspace's label, else its id, else the
/// agent's. The files carry no token or expiry, so a claim's are 0.
fn claim_seat(v: &serde_json::Value) -> Option<String> {
    ["workspace_label", "workspace_id", "workspace", "agent_id"]
        .iter()
        .find_map(|k| v.get(k).and_then(|x| x.as_str()).filter(|s| !s.is_empty()))
        .map(str::to_string)
}

/// `claims.json` → `Entity::Claim`s: the row, the seat the record names, token
/// and expiry 0 (the files have neither).
pub fn claims(claims: &BTreeMap<String, serde_json::Value>) -> Vec<Entity> {
    claims
        .iter()
        .filter_map(|(row, v)| {
            Some(Entity::Claim(c::Claim {
                row: RowId(row.clone()),
                seat: SeatId(claim_seat(v)?),
                token: Token(0),
                expires: At(0),
            }))
        })
        .collect()
}

/// Which group of which list holds each member: a running list's first, else
/// any list's first (lists in id order).
fn placements(lists: &[Worklist]) -> BTreeMap<String, GroupId> {
    let mut out: BTreeMap<String, (bool, GroupId)> = BTreeMap::new();
    for w in lists {
        let running = w.status() == WorklistStatus::Running;
        for (i, g) in w.groups().iter().enumerate() {
            for m in &g.members {
                let gid = GroupId { list: ListId(w.id.clone()), n: i as u32 + 1 };
                match out.get(m) {
                    Some((true, _)) => {}
                    Some((false, _)) if !running => {}
                    _ => {
                        out.insert(m.clone(), (running, gid));
                    }
                }
            }
        }
    }
    out.into_iter().map(|(k, (_, g))| (k, g)).collect()
}

/// Tasks → `Entity::Row`s. `fields` carry what `wip --json` shows of a task:
/// `status` (the file's word), `prio`, `tags`, `caption`, `parent`, `after`,
/// `refs`, `created`, `updated`. The seat is the claim's; the group is the
/// list's placement. `before_block` and `commit` are not in the files (None).
pub fn rows(tasks: &[Task], claims: &BTreeMap<String, serde_json::Value>, lists: &[Worklist]) -> Vec<Entity> {
    let place = placements(lists);
    tasks
        .iter()
        .map(|t| {
            let status = Status::parse(&t.status_raw).unwrap_or(Status::Todo);
            let mut f = BTreeMap::new();
            put(&mut f, "status", status.as_str());
            put(&mut f, "prio", model::Priority::parse(&t.priority_raw).unwrap_or(model::Priority::Normal).as_str());
            put(&mut f, "tags", &t.tags.join(","));
            put(&mut f, "caption", &caption::of_task(t).text);
            put(&mut f, "parent", t.parent.as_deref().unwrap_or(""));
            put(&mut f, "after", &t.after.join(","));
            put(&mut f, "refs", &t.refs.join(","));
            put(&mut f, "created", &t.created);
            put(&mut f, "updated", &t.updated);
            Entity::Row(c::Row {
                id: RowId(t.id.clone()),
                project: ProjectId(model::project_of(t).unwrap_or_default()),
                title: t.title.clone(),
                state: row_state(status),
                before_block: None,
                seat: claims.get(&t.id).and_then(claim_seat).map(SeatId),
                group: place.get(&t.id).cloned(),
                commit: None,
                fields: f,
            })
        })
        .collect()
}

/// Worklists → `Entity::List`s, each with its groups in order. A group's state
/// is derived from the file alone: below the last verdict is `Passed`; the one
/// at the floor is `Barrier` when every member is done (a member the store no
/// longer holds counts as done), `Held` when the list is held, else `Running`;
/// those after it are `Pending`. A draft's groups are all `Pending`, a done
/// list's all `Passed`. Group `fields`: caption, cap, stop, verdict, agent,
/// kind, weight, together. List `fields`: title, caption, status, project,
/// prio, after, alone, created, planned.
pub fn lists(lists: &[Worklist], tasks: &[Task]) -> Vec<Entity> {
    let done: BTreeSet<&str> =
        tasks.iter().filter(|t| Status::parse(&t.status_raw) == Some(Status::Done)).map(|t| t.id.as_str()).collect();
    let known: BTreeSet<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
    lists_with(lists, &|m| done.contains(m) || !known.contains(m))
}

fn lists_with(lists: &[Worklist], settled: &dyn Fn(&str) -> bool) -> Vec<Entity> {
    lists
        .iter()
        .map(|w| {
            let status = w.status();
            let gs = w.groups();
            let floor = model::crossed(&gs);
            let groups = gs
                .iter()
                .enumerate()
                .map(|(i, g)| {
                    let ord = i + 1;
                    let state = match status {
                        WorklistStatus::Draft => ListState::Pending,
                        WorklistStatus::Done => ListState::Passed,
                        _ if ord <= floor => ListState::Passed,
                        _ if ord > floor + 1 => ListState::Pending,
                        WorklistStatus::Held => ListState::Held,
                        _ if g.members.iter().all(|m| settled(m)) => ListState::Barrier,
                        _ => ListState::Running,
                    };
                    let mut f = BTreeMap::new();
                    put(&mut f, "caption", &g.caption);
                    put(&mut f, "cap", &g.cap.map(|n| n.to_string()).unwrap_or_default());
                    put(&mut f, "stop", &g.stop);
                    put(&mut f, "verdict", &g.verdict);
                    put(&mut f, "agent", &g.agent);
                    put(&mut f, "kind", &g.kind);
                    put(&mut f, "weight", &g.weight);
                    if g.together {
                        f.insert("together".into(), "true".into());
                    }
                    c::Group {
                        id: GroupId { list: ListId(w.id.clone()), n: ord as u32 },
                        state,
                        members: g.members.iter().cloned().map(RowId).collect(),
                        attempt: 0,
                        fields: f,
                    }
                })
                .collect();
            let mut f = BTreeMap::new();
            put(&mut f, "title", &w.title);
            put(&mut f, "caption", &caption::of_worklist(w).text);
            put(&mut f, "status", status.as_str());
            put(&mut f, "project", w.project.as_deref().unwrap_or(""));
            put(&mut f, "prio", w.prio.as_deref().unwrap_or(""));
            put(&mut f, "after", w.after.as_deref().unwrap_or(""));
            put(&mut f, "alone", &w.alone.map(|a| a.to_string()).unwrap_or_default());
            put(&mut f, "created", &w.created);
            put(&mut f, "planned", w.planned.as_deref().unwrap_or(""));
            Entity::List(c::List { id: ListId(w.id.clone()), state: list_state(status), groups, fields: f })
        })
        .collect()
}

fn principal(p: &Party) -> Principal {
    match p {
        Party::Pane { pane, .. } => Principal::Seat(SeatId(pane.clone())),
        Party::Seat(scope) => Principal::Seat(SeatId(scope.clone())),
        Party::Agent(h) if h == "wsp" => Principal::Wsp("wsp".into()),
        Party::Agent(h) => Principal::Seat(SeatId(h.clone())),
        Party::Human => Principal::Person("person".into()),
    }
}

fn about(a: &About) -> Option<Subject> {
    match a {
        About::Task(t) => Some(Subject::Row(RowId(t.clone()))),
        About::Group { scope, group } => Some(Subject::Group(GroupId { list: ListId(scope.clone()), n: *group as u32 })),
        About::Nothing | About::Scope(_) => None,
    }
}

/// Messages and legacy flags → `Entity::Ask`. A question is `AskKind::Ask`, a
/// notification or signal is `Note`, and a flag (`flags.json`, keyed by task,
/// id `flag:<task>`) is `Flag`. States by [`ask_state`]. `fields`: `shape`,
/// `message_kind` (stop, direction, note, fyi), `state` (the file's word), `at`,
/// `scope` for a scope the contract has no subject for, `reply_to`.
pub fn asks(messages: &BTreeMap<String, Message>, flags: &BTreeMap<String, serde_json::Value>) -> Vec<Entity> {
    let mut out: Vec<Entity> = messages
        .values()
        .map(|m| {
            let kind = match m.shape() {
                Some(Shape::Question) => AskKind::Ask,
                _ => AskKind::Note,
            };
            let mut f = BTreeMap::new();
            put(&mut f, "shape", &m.shape_raw);
            put(&mut f, "message_kind", &m.kind_raw);
            put(&mut f, "state", &m.state_raw);
            put(&mut f, "at", &m.at);
            put(&mut f, "scope", m.about.scope().unwrap_or(""));
            put(&mut f, "reply_to", m.reply_to.as_deref().unwrap_or(""));
            let to = match (&m.for_seat, m.for_person) {
                (Some(s), _) => Some(Principal::Seat(SeatId(s.clone()))),
                (None, true) => Some(Principal::Person("person".into())),
                _ => None,
            };
            let answer = m.closing().filter(|h| h.act == wsp_store::message::Act::Answered).map(|h| h.note.clone());
            Entity::Ask(c::Ask {
                id: AskId(m.id.clone()),
                kind,
                from: principal(&m.from),
                to,
                about: about(&m.about),
                text: m.text.clone(),
                state: ask_state(m.state()),
                answer,
                fields: f,
            })
        })
        .collect();
    out.extend(flags.iter().map(|(task, v)| {
        let mut f = BTreeMap::new();
        for k in ["pane", "workspace", "at", "reason"] {
            put(&mut f, k, v.get(k).and_then(|x| x.as_str()).unwrap_or(""));
        }
        let text = ["text", "message", "reason"]
            .iter()
            .find_map(|k| v.get(k).and_then(|x| x.as_str()))
            .unwrap_or("")
            .to_string();
        Entity::Ask(c::Ask {
            id: AskId(format!("flag:{task}")),
            kind: AskKind::Flag,
            from: Principal::Seat(SeatId(
                v.get("pane").and_then(|x| x.as_str()).filter(|s| !s.is_empty()).unwrap_or(task).to_string(),
            )),
            to: None,
            about: Some(Subject::Row(RowId(task.clone()))),
            text,
            state: AskState::Raised,
            answer: None,
            fields: f,
        })
    }));
    out
}

/// Machine records and their live facts → `Entity::Machine`. The transport is
/// `Local` for the machine named `local`, else `Tcp` at the record's `ssh`
/// alias (no port: the record has none). `beat` is the live `beat_at`; `facts`
/// are the record's os, arch, status, agents, lease, exec_dir and the live
/// reachable, ready, tick_secs, last_seen, error where there is one.
pub fn machines(files: &[MachineFile], live: &BTreeMap<String, MachineLive>, local: &str) -> Vec<Entity> {
    files
        .iter()
        .map(|m| {
            let mut f = BTreeMap::new();
            put(&mut f, "os", &m.os);
            put(&mut f, "arch", &m.arch);
            put(&mut f, "status", &m.status);
            put(&mut f, "agents", &m.agents.map(|n| n.to_string()).unwrap_or_default());
            put(&mut f, "lease", &m.lease.map(|n| n.to_string()).unwrap_or_default());
            put(&mut f, "exec_dir", &m.exec_dir);
            let mut beat = None;
            if let Some(l) = live.get(&m.name) {
                f.insert("reachable".into(), l.reachable.to_string());
                f.insert("ready".into(), l.ready.to_string());
                if l.tick_secs != 0 {
                    f.insert("tick_secs".into(), l.tick_secs.to_string());
                }
                put(&mut f, "last_seen", &l.last_seen);
                put(&mut f, "error", &l.error);
                let secs = wsp_base::util::epoch_of(&l.beat_at);
                if secs > 0 {
                    beat = Some(At(secs as u64 * 1000));
                }
            }
            Entity::Machine(c::Machine {
                name: MachineName(m.name.clone()),
                transport: if m.name == local { Transport::Local } else { Transport::Tcp { addr: m.ssh.clone() } },
                beat,
                facts: f,
            })
        })
        .collect()
}

/// Reads the fingerprint half: tasks, claims, worklists, machines.
pub fn read_rows_etc(store: &Store, local: &str) -> Vec<Entity> {
    let tasks = store.tasks();
    let claims_file = store.claims();
    let worklists = store.worklists();
    let mut out = rows(&tasks, &claims_file, &worklists);
    out.extend(claims(&claims_file));
    out.extend(lists(&worklists, &tasks));
    out.extend(machines(&store.machines(), &store.machines_live(), local));
    out
}

/// Reads the attention half: messages and flags.
pub fn read_attention(store: &Store) -> Vec<Entity> {
    asks(&store.messages(), &store.flags())
}

/// Both halves.
pub fn read_halves(store: &Store, local: &str) -> Halves {
    Halves { rows_etc: read_rows_etc(store, local), attention: read_attention(store) }
}

/// What a re-read changes: `put` is every entity that is new or differs from
/// `before`, and `gone` every subject of `before` that `now` no longer has.
/// `gone` is every kind the mirror projects; use [`diff_half`] when only one
/// half was re-read.
pub fn diff(before: &BTreeMap<Subject, Entity>, now: Vec<Entity>) -> Command {
    diff_where(before, now, |s| Half::RowsEtc.owns(s) || Half::Attention.owns(s))
}

/// [`diff`] for one re-read half: `gone` names only subjects that half owns.
pub fn diff_half(before: &BTreeMap<Subject, Entity>, now: Vec<Entity>, half: Half) -> Command {
    diff_where(before, now, |s| half.owns(s))
}

fn diff_where(before: &BTreeMap<Subject, Entity>, now: Vec<Entity>, owns: impl Fn(&Subject) -> bool) -> Command {
    let mut seen = BTreeSet::new();
    let mut put = Vec::new();
    for e in now {
        let s = subject_of(&e);
        if before.get(&s) != Some(&e) {
            put.push(e);
        }
        seen.insert(s);
    }
    let gone = before.keys().filter(|s| owns(s) && !seen.contains(*s)).cloned().collect();
    Command::Mirror { put, gone }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::store;
    use serde_json::json;
    use wsp_store::message::{Kind, Message};

    fn key(es: &[Entity]) -> BTreeMap<Subject, Entity> {
        es.iter().map(|e| (subject_of(e), e.clone())).collect()
    }

    fn seed(tag: &str) -> Store {
        let s = store(tag);
        for (id, status, tags) in [("wsp-1", "todo", "a"), ("wsp-2", "doing", "a,b"), ("wsp-3", "review", ""), ("wsp-4", "blocked", ""), ("wsp-5", "done", "")] {
            let mut t = Task::new(&format!("title of {id}"), id);
            t.project = Some("wsp".into());
            t.status_raw = status.into();
            t.tags = tags.split(',').filter(|x| !x.is_empty()).map(String::from).collect();
            t.caption = Some(format!("cap {id}"));
            s.save_task(&t).unwrap();
        }
        s.set_claim("wsp-2", json!({ "workspace_label": "cpd-2", "agent_id": "a-2" }));
        let mut w = Worklist::new("list-a", "A list");
        w.set_status(WorklistStatus::Running);
        let mut g1 = model::Group::default();
        g1.members = vec!["wsp-5".into()];
        g1.verdict = "holds".into();
        let mut g2 = model::Group::default();
        g2.members = vec!["wsp-2".into(), "wsp-3".into()];
        w.set_groups(&[g1, g2]);
        s.save_worklist(&w).unwrap();
        let mut m = Message::question(Party::Pane { pane: "cpd-2".into(), workspace: "w".into() }, Kind::Direction, "may I?", wsp_store::message::Waiting::new("cpd-2", "wsp-2"));
        m.id = "m-1".into();
        s.save_message(&m).unwrap();
        s.set_legacy_flag("wsp-4", json!({ "pane": "cpd-4", "reason": "stuck" }));
        let mut mf = MachineFile::default();
        mf.name = "far".into();
        mf.ssh = "far-alias".into();
        mf.os = "linux".into();
        mf.status = "active".into();
        s.save_machine(&mf).unwrap();
        s
    }

    #[test]
    fn rows_key_by_key() {
        let s = seed("mirror-rows");
        let r = key(&read_rows_etc(&s, "here"));
        let Entity::Row(r2) = &r[&Subject::Row(RowId("wsp-2".into()))] else { panic!() };
        assert_eq!((r2.state, r2.project.0.as_str(), r2.title.as_str()), (RowState::Doing, "wsp", "title of wsp-2"));
        assert_eq!(r2.seat, Some(SeatId("cpd-2".into())));
        assert_eq!(r2.group, Some(GroupId { list: ListId("list-a".into()), n: 2 }));
        assert_eq!(r2.fields["tags"], "a,b");
        assert_eq!(r2.fields["caption"], "cap wsp-2");
        assert_eq!(r2.fields["status"], "doing");
        for (id, st) in [("wsp-1", RowState::Todo), ("wsp-3", RowState::Review), ("wsp-4", RowState::Blocked), ("wsp-5", RowState::Done)] {
            let Entity::Row(x) = &r[&Subject::Row(RowId(id.into()))] else { panic!() };
            assert_eq!(x.state, st, "{id}");
        }
        let Entity::Row(r1) = &r[&Subject::Row(RowId("wsp-1".into()))] else { panic!() };
        assert_eq!((r1.seat.clone(), r1.group.clone()), (None, None));
    }

    #[test]
    fn claim_list_ask_flag_machine_key_by_key() {
        let s = seed("mirror-rest");
        let r = key(&read_halves(&s, "here").rows_etc);
        let Entity::Claim(cl) = &r[&Subject::Claim(RowId("wsp-2".into()))] else { panic!() };
        assert_eq!(cl.seat, SeatId("cpd-2".into()));
        let Entity::List(l) = &r[&Subject::List(ListId("list-a".into()))] else { panic!() };
        assert_eq!(l.state, ListState::Running);
        assert_eq!(l.groups.len(), 2);
        assert_eq!((l.groups[0].state, l.groups[0].fields["verdict"].as_str()), (ListState::Passed, "holds"));
        assert_eq!(l.groups[0].members, vec![RowId("wsp-5".into())]);
        assert_eq!(l.groups[1].state, ListState::Running);
        assert_eq!(l.groups[1].members, vec![RowId("wsp-2".into()), RowId("wsp-3".into())]);
        assert_eq!(l.fields["status"], "running");
        let Entity::Machine(m) = &r[&Subject::Machine(MachineName("far".into()))] else { panic!() };
        assert_eq!(m.transport, Transport::Tcp { addr: "far-alias".into() });
        assert_eq!(m.facts["os"], "linux");
        let a = key(&read_attention(&s));
        let Entity::Ask(q) = &a[&Subject::Ask(AskId("m-1".into()))] else { panic!() };
        assert_eq!((q.kind, q.state, q.text.as_str()), (AskKind::Ask, AskState::Raised, "may I?"));
        assert_eq!(q.from, Principal::Seat(SeatId("cpd-2".into())));
        let Entity::Ask(f) = &a[&Subject::Ask(AskId("flag:wsp-4".into()))] else { panic!() };
        assert_eq!((f.kind, f.about.clone()), (AskKind::Flag, Some(Subject::Row(RowId("wsp-4".into())))));
        assert_eq!(f.fields["reason"], "stuck");
        assert!(!r.keys().any(|k| matches!(k, Subject::Ask(_))) && !a.keys().any(|k| matches!(k, Subject::Row(_))));
    }

    #[test]
    fn diff_edit_removal_and_quiet() {
        let s = seed("mirror-diff");
        let before = key(&read_rows_etc(&s, "here"));
        let quiet = diff_half(&before, read_rows_etc(&s, "here"), Half::RowsEtc);
        assert_eq!(quiet, Command::Mirror { put: vec![], gone: vec![] });
        let mut t = s.task("wsp-1").unwrap();
        t.status_raw = "doing".into();
        s.save_task(&t).unwrap();
        let Command::Mirror { put, gone } = diff_half(&before, read_rows_etc(&s, "here"), Half::RowsEtc) else { panic!() };
        assert_eq!((put.len(), gone.len()), (1, 0));
        assert_eq!(subject_of(&put[0]), Subject::Row(RowId("wsp-1".into())));
        s.clear_claim("wsp-2");
        let Command::Mirror { put, gone } = diff_half(&before, read_rows_etc(&s, "here"), Half::RowsEtc) else { panic!() };
        assert_eq!(gone, vec![Subject::Claim(RowId("wsp-2".into()))]);
        assert_eq!(put.len(), 2, "wsp-1 (edited above) and wsp-2, which loses its seat");
        // A half that was not re-read names nothing of the other's in `gone`.
        let mut both = before.clone();
        both.extend(key(&read_attention(&s)));
        let Command::Mirror { gone, .. } = diff_half(&both, read_rows_etc(&s, "here"), Half::RowsEtc) else { panic!() };
        assert!(!gone.iter().any(|g| matches!(g, Subject::Ask(_))));
    }
}
