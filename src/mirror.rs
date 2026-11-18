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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use wsp_server::SharedStore;
use wsp_store_contract::IdempotencyKey;

use wsp_server::mirror::subject_of;
use wsp_store::caption;
use wsp_store::message::{About, Message, Party, Shape, State};
use wsp_store::model::{self, Machine as MachineFile, Status, Task, TaskHead, Worklist, WorklistStatus};
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
///
/// **And the record itself, verbatim** (wsp-076), so [`task_of`] rebuilds the
/// `Task` `wip` reads with the store's own parser: `body` is the task file's
/// text for every task `texts` holds (those [`wip_reads_in_full`]
/// says `wip` opens), and `head` is the [`TaskHead`] as the store's index keeps
/// it (`TaskHead::to_json`) for the rest, which `wip` reads bodiless.
pub fn rows(
    tasks: &[Task],
    claims: &BTreeMap<String, serde_json::Value>,
    lists: &[Worklist],
    texts: &BTreeMap<String, String>,
) -> Vec<Entity> {
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
            if let Some(claim) = claims.get(&t.id) {
                f.insert(CLAIM.into(), claim.to_string());
            }
            match texts.get(&t.id) {
                Some(text) => {
                    f.insert(BODY.into(), text.clone());
                }
                None => {
                    f.insert(HEAD.into(), TaskHead::of(t).to_json().to_string());
                }
            }
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

/// A row's `fields` key holding the task file's text.
pub const BODY: &str = "body";
/// A row's `fields` key holding the task's head, for a task `wip` reads bodiless.
pub const HEAD: &str = "head";
/// A row's `fields` key holding its `claims.json` record (JSON text), verbatim:
/// an `Entity::Claim` keeps the seat alone, and `wip` joins the record's
/// `agent_id` and `agent_kind` (wsp-076).
pub const CLAIM: &str = "claim";

/// The `claims.json` record a row carries, as `Store::claims` returns it.
pub fn claim_of(row: &c::Row) -> Option<serde_json::Value> {
    serde_json::from_str(row.fields.get(CLAIM)?).ok()
}

/// The `Task` a row was projected from, as `wip`'s own reading makes it: the
/// store's task parser over `body` (what `Store::tasks_read_where` does for a
/// task it reads in full), else `TaskHead::from_json(head).bodiless()` (what it
/// does for the rest). `None` for a row with neither (one no mirror of this
/// build wrote).
pub fn task_of(row: &c::Row) -> Option<Task> {
    if let Some(text) = row.fields.get(BODY) {
        return Some(Task::from_doc(&wsp_base::fm::parse(text), &row.id.0));
    }
    let head: serde_json::Value = serde_json::from_str(row.fields.get(HEAD)?).ok()?;
    TaskHead::from_json(&head).map(|h| h.bodiless())
}

/// The file text of every task `wip` reads in full, by id: the bodies
/// [`rows`] carries.
pub fn texts_of(store: &Store, tasks: &[Task]) -> BTreeMap<String, String> {
    tasks
        .iter()
        .filter(|t| wip_reads_in_full(&TaskHead::of(t)))
        .filter_map(|t| Some((t.id.clone(), std::fs::read_to_string(store.task_path(&t.id)).ok()?)))
        .collect()
}

/// Worklists → `Entity::List`s, each with its groups in order. A group's state
/// is derived from the file alone: below the last verdict is `Passed`; the one
/// at the floor is `Barrier` when every member is done (a member the store no
/// longer holds counts as done), `Held` when the list is held, else `Running`;
/// those after it are `Pending`. A draft's groups are all `Pending`, a done
/// list's all `Passed`. Group `fields`: caption, cap, stop, verdict, agent,
/// kind, weight, together, and where it is `held` or `barrier` `owed` (`time`,
/// `rows`, `check` or `person`), `owed_text`, and `until` or `on` (wsp-095:
/// [`crate::owed`]), and `attention` (`needs-you` or `follow-up`, absent for
/// neither), `attention_text`, and `waiting_on` or `due` ([`crate::classify`],
/// wsp-096), and on a needs-you group only `action`, the runnable act as a
/// JSON string `{verb, args, sentence, buttons}` (wsp-097). List `fields`: title, caption, status, project,
/// prio, after, alone, created, planned, and `body`: the list file's text,
/// verbatim, for each list `texts` holds (wsp-076), so [`list_of`] rebuilds
/// the `Worklist` with the store's own parser — a list's groups and settings
/// are more than its fields say.
pub fn lists(lists: &[Worklist], tasks: &[Task], texts: &BTreeMap<String, String>) -> Vec<Entity> {
    let done: BTreeSet<&str> =
        tasks.iter().filter(|t| Status::parse(&t.status_raw) == Some(Status::Done)).map(|t| t.id.as_str()).collect();
    let known: BTreeSet<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
    lists_with(lists, &|m| done.contains(m) || !known.contains(m), texts, wsp_base::util::epoch_secs())
}

/// The `Worklist` a list was projected from, as `Store::worklists` reads it:
/// the store's parser over `body`. `None` for a list without one.
pub fn list_of(list: &c::List) -> Option<Worklist> {
    let text = list.fields.get(BODY)?;
    Some(Worklist::from_doc(&wsp_base::fm::parse(text), &list.id.0))
}

/// The file text of every list, by id: the bodies [`lists`] carries.
pub fn list_texts_of(store: &Store, lists: &[Worklist]) -> BTreeMap<String, String> {
    lists
        .iter()
        .filter_map(|w| Some((w.id.clone(), std::fs::read_to_string(store.worklist_path(&w.id)).ok()?)))
        .collect()
}

fn lists_with(lists: &[Worklist], settled: &dyn Fn(&str) -> bool, texts: &BTreeMap<String, String>, now: i64) -> Vec<Entity> {
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
                    // `wsp-095`: what a held or ready barrier waits on, so a
                    // reader lists it as needing the person only where `owed`
                    // is `person`, and says the act `owed_text` names.
                    if let Some(owed) = crate::owed::of(w, g, state, settled, now) {
                        put(&mut f, "owed", owed.word());
                        put(&mut f, "owed_text", &owed.text());
                        match &owed {
                            crate::owed::Owed::Until(when) => put(&mut f, "until", when),
                            crate::owed::Owed::Rows(rows) => put(&mut f, "on", &rows.join(" ")),
                            _ => {}
                        }
                        // `wsp-096`: needs-you or follow-up, from the one
                        // classifier every reader asks; absent is neither.
                        if let Some(a) = crate::classify::of(crate::classify::Item::Barrier(&w.id, &owed)) {
                            for (k, v) in a.fields() {
                                put(&mut f, k, &v);
                            }
                        }
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
            if let Some(text) = texts.get(&w.id) {
                f.insert(BODY.into(), text.clone());
            }
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
/// `scope` for a scope the contract has no subject for, `reply_to`, and for an
/// open question for the person `attention` = `needs-you`, `attention_text`
/// ([`crate::classify`], wsp-096) and `action`, the runnable act as a JSON
/// string `{verb, args, sentence, buttons}` (wsp-097).
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
            f.insert(RECORD.into(), m.to_json().to_string());
            let to = match (&m.for_seat, m.for_person) {
                (Some(s), _) => Some(Principal::Seat(SeatId(s.clone()))),
                (None, true) => Some(Principal::Person("person".into())),
                _ => None,
            };
            let answer = m.closing().filter(|h| h.act == wsp_store::message::Act::Answered).map(|h| h.note.clone());
            let mut ask = c::Ask {
                id: AskId(m.id.clone()),
                kind,
                from: principal(&m.from),
                to,
                about: about(&m.about),
                text: m.text.clone(),
                state: ask_state(m.state()),
                answer,
                fields: f,
            };
            // `wsp-096`: an open question for the person needs you.
            if let Some(a) = crate::classify::of(crate::classify::Item::Ask(&ask)) {
                for (k, v) in a.fields() {
                    put(&mut ask.fields, k, &v);
                }
            }
            Entity::Ask(ask)
        })
        .collect();
    out.extend(flags.iter().map(|(task, v)| {
        let mut f = BTreeMap::new();
        for k in ["pane", "workspace", "at", "reason"] {
            put(&mut f, k, v.get(k).and_then(|x| x.as_str()).unwrap_or(""));
        }
        f.insert(RECORD.into(), v.to_string());
        let text = ["text", "message", "reason"]
            .iter()
            .find_map(|k| v.get(k).and_then(|x| x.as_str()))
            .unwrap_or("")
            .to_string();
        Entity::Ask(c::Ask {
            id: AskId(format!("{FLAG_PREFIX}{task}")),
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

/// An ask's `fields` key holding its record: a message as `messages.json`
/// keeps it (`Message::to_json`), a flag as `flags.json` does.
pub const RECORD: &str = "record";
/// The id prefix of a legacy flag's ask.
pub const FLAG_PREFIX: &str = "flag:";

/// The message an ask was projected from, by the store's own reader
/// (`Message::from_json`, as `Store::messages` reads it). `None` for a flag or
/// an ask without a record.
pub fn message_of(ask: &c::Ask) -> Option<Message> {
    if ask.kind == AskKind::Flag {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(ask.fields.get(RECORD)?).ok()?;
    Some(Message::from_json(&ask.id.0, &v))
}

/// The task and `flags.json` record a flag's ask was projected from, as
/// `Store::flags` returns them.
pub fn flag_of(ask: &c::Ask) -> Option<(String, serde_json::Value)> {
    let task = ask.id.0.strip_prefix(FLAG_PREFIX).filter(|_| ask.kind == AskKind::Flag)?;
    Some((task.to_string(), serde_json::from_str(ask.fields.get(RECORD)?).ok()?))
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

// ---------------------------------------------------------------- seats

/// A seat's `fields` key holding its `bindings.json` record (JSON text).
pub const BINDING: &str = "binding";
/// A seat's `fields` key holding its `spawned.json` record (JSON text).
pub const SPAWNED: &str = "spawned";
/// A seat's `fields` keys for what builds a `place::Seated`: the label, the
/// working directory, the agent (`{kind, name, args}` as JSON text), the
/// census state word (`place::State::as_str`) and the session id.
pub const LABEL: &str = "label";
pub const CWD: &str = "cwd";
pub const AGENT: &str = "agent";
pub const CENSUS: &str = "census";
pub const SESSION: &str = "session";

fn set(m: &mut BTreeMap<String, String>, k: &str, v: &str) {
    match v.trim().is_empty() {
        true => {
            m.remove(k);
        }
        false => {
            m.insert(k.to_string(), v.to_string());
        }
    }
}

/// Writes a seat's records onto its `fields`, each replacing what was there
/// and removed where there is none: the whole binding and spawned records,
/// and `seated`'s label, cwd, agent, state word and session — the census row
/// the files path builds (`Compound::seated_as`).
pub fn seat_records(
    fields: &mut BTreeMap<String, String>,
    binding: Option<&serde_json::Value>,
    spawned: Option<&serde_json::Value>,
    seated: &crate::place::Seated,
) {
    set(fields, BINDING, &binding.map(|v| v.to_string()).unwrap_or_default());
    set(fields, SPAWNED, &spawned.map(|v| v.to_string()).unwrap_or_default());
    set(fields, LABEL, &seated.label);
    set(fields, CWD, &seated.cwd);
    let a = &seated.agent;
    let agent = match a.kind.is_empty() && a.name.is_empty() && a.args.is_empty() {
        true => String::new(),
        false => serde_json::json!({ "kind": a.kind, "name": a.name, "args": a.args }).to_string(),
    };
    set(fields, AGENT, &agent);
    set(fields, CENSUS, seated.state.as_str());
    set(fields, SESSION, &seated.session);
}

fn json_field(seat: &c::Seat, k: &str) -> Option<serde_json::Value> {
    serde_json::from_str(seat.fields.get(k)?).ok()
}

/// The seat's `bindings.json` record, as [`seat_records`] carried it.
pub fn binding_of(seat: &c::Seat) -> Option<serde_json::Value> {
    json_field(seat, BINDING)
}

/// The seat's `spawned.json` record, as [`seat_records`] carried it.
pub fn spawned_of(seat: &c::Seat) -> Option<serde_json::Value> {
    json_field(seat, SPAWNED)
}

/// The census row a seat's `fields` carry, built as `Compound::seated` builds
/// it from the seat's record (the agent's `key` is never read there either).
/// A state word this build does not know is `Unknown`.
pub fn seated_of(seat: &c::Seat) -> crate::place::Seated {
    use crate::place::{Agent, Seat, Seated, State};
    let f = |k: &str| seat.fields.get(k).cloned().unwrap_or_default();
    let agent = json_field(seat, AGENT).unwrap_or_default();
    let s = |k: &str| agent.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_string();
    // The seat's own state first where it has ended: a `gone` the fold wrote
    // stands over any word the census row carried before it. A seat with no
    // census word (one mirrored before wsp-076 carried it) reads its own
    // state, never Unknown (wsp-094).
    let state = match (seat.state, f(CENSUS).as_str()) {
        (c::SeatState::Gone | c::SeatState::Ending | c::SeatState::Lost, _) => State::Gone,
        (_, "empty") => State::Empty,
        (_, "starting") => State::Starting,
        (_, "idle") => State::Idle,
        (_, "working") => State::Working,
        (_, "blocked") => State::Blocked,
        (_, "gone") => State::Gone,
        (_, "unreachable") => State::Unreachable,
        (c::SeatState::Idle, "") => State::Idle,
        (c::SeatState::Working, "") => State::Working,
        (c::SeatState::Requested | c::SeatState::Spawning | c::SeatState::Started, "") => State::Starting,
        _ => State::Unknown,
    };
    Seated {
        seat: Seat::new(seat.id.0.clone()),
        label: f(LABEL),
        cwd: f(CWD),
        agent: Agent {
            kind: s("kind"),
            name: s("name"),
            key: None,
            args: agent
                .get("args")
                .and_then(|a| a.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default(),
        },
        state,
        session: f(SESSION),
    }
}

/// Reads the fingerprint half: tasks, claims, worklists, machines.
pub fn read_rows_etc(store: &Store, local: &str) -> Vec<Entity> {
    let tasks = store.tasks();
    let claims_file = store.claims();
    let worklists = store.worklists();
    let texts = texts_of(store, &tasks);
    let mut out = rows(&tasks, &claims_file, &worklists, &texts);
    out.extend(claims(&claims_file));
    out.extend(lists(&worklists, &tasks, &list_texts_of(store, &worklists)));
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

/// `WSP_API=off`: no database, no server, no mirror.
pub(crate) fn api_off() -> bool {
    std::env::var("WSP_API").is_ok_and(|v| v.trim().eq_ignore_ascii_case("off"))
}

/// The port the API listens on: `WSP_API_PORT` (tests and a machine whose 7488
/// is taken), else [`wsp_server::DEFAULT_PORT`].
pub(crate) fn api_port() -> u16 {
    std::env::var("WSP_API_PORT").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(wsp_server::DEFAULT_PORT)
}

/// What `doctor` says of the API: nothing under `WSP_API=off`; a note when
/// there is no `api.json` and no daemon; a problem when `api.json` names an
/// address that does not answer; else the head id.
pub(crate) fn api_health(state: &std::path::Path, running: Option<&[u32]>, problems: &mut Vec<String>, notes: &mut Vec<String>) {
    if api_off() {
        return;
    }
    if !state.join(wsp_api::LISTEN_FILE).exists() {
        if running.is_some_and(|r| r.is_empty()) {
            notes.push("no API: no api.json and no daemon running".into());
        } else {
            problems.push("the daemon runs but wrote no api.json (the API did not start; see cycle.log)".into());
        }
        return;
    }
    match wsp_client::Client::dial(state, wsp_store_contract::Principal::Wsp("doctor".into()), std::time::Duration::from_secs(2)) {
        Ok(c) => notes.push(format!("API answers (head event {})", c.head().0)),
        Err(why) => problems.push(format!("api.json names an address that does not answer: {why}")),
    }
}

/// The machine name `read_rows_etc` marks `Transport::Local`.
pub const LOCAL: &str = "local";

/// What moves a half: the stamp, plus (for the rows half) the mtimes of the two
/// state files neither stamp covers, `claims.json` and `machines.json`.
fn stamp_of(store: &Store, half: Half) -> (u64, [Option<std::time::SystemTime>; 2]) {
    match half {
        Half::Attention => (store.attention_stamp(), [None, None]),
        Half::RowsEtc => {
            let m = |n: &str| std::fs::metadata(store.state_file(n)).ok().and_then(|m| m.modified().ok());
            (store.fingerprint(), [m("claims.json"), m("machines.json")])
        }
    }
}

fn read_half(store: &Store, half: Half) -> Vec<Entity> {
    match half {
        Half::RowsEtc => read_rows_etc(store, LOCAL),
        Half::Attention => read_attention(store),
    }
}

/// What `state` already holds of `half`, keyed by subject.
fn held(state: &c::State, half: Half) -> BTreeMap<Subject, Entity> {
    let mut all: Vec<Entity> = Vec::new();
    match half {
        Half::RowsEtc => {
            all.extend(state.rows.values().cloned().map(Entity::Row));
            all.extend(state.lists.values().cloned().map(Entity::List));
            all.extend(state.claims.values().cloned().map(Entity::Claim));
            // Only machines this half projects: the fold's beats name the local
            // machine by its hostname, and no file does, so they are not held here.
            all.extend(
                state
                    .machines
                    .values()
                    .filter(|m| m.name.0 == LOCAL || !matches!(m.transport, c::Transport::Local))
                    .cloned()
                    .map(Entity::Machine),
            );
        }
        Half::Attention => all.extend(state.asks.values().cloned().map(Entity::Ask)),
    }
    all.into_iter().map(|e| (subject_of(&e), e)).collect()
}

/// The soonest `until` on a group (`wsp-095`), in seconds since the epoch: the
/// moment a held barrier stops waiting on time, which no file write marks.
pub fn next_due(entities: &[Entity]) -> Option<i64> {
    entities
        .iter()
        .filter_map(|e| match e {
            Entity::List(l) => Some(l.groups.iter()),
            _ => None,
        })
        .flatten()
        .filter_map(|g| g.fields.get("until"))
        .map(|u| wsp_base::util::epoch_of(u))
        .filter(|e| *e > 0)
        .min()
}

/// Start the mirror thread: once a second it reads the two stamps, and when
/// one moves (and at start) re-reads that half, diffs against what it last
/// applied for it, and applies one `Command::Mirror` if that is not empty. The
/// first pass diffs against `db.snapshot()`, so a restart re-puts nothing
/// unchanged. A refusal or backend error is one line in `cycle.log`.
pub fn start(store: &Store, db: SharedStore) -> (std::thread::JoinHandle<()>, Arc<AtomicBool>) {
    let (root, state) = (store.root.clone(), store.state.clone());
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let handle = std::thread::Builder::new()
        .name("mirror".into())
        .spawn(move || {
            let store = Store::at(root, state);
            let say = |line: String| wsp_store::store::log_line(&store, &format!("mirror: {line}"));
            let mut seq = 0u64;
            let mut stamps: [Option<(u64, [Option<std::time::SystemTime>; 2])>; 2] = [None, None];
            let mut last: [Option<BTreeMap<Subject, Entity>>; 2] = [None, None];
            // The soonest `until` a held barrier waits for (`wsp-095`): no file
            // moves when it comes, so the rows half is re-read then.
            let mut due: Option<i64> = None;
            while !flag.load(Ordering::SeqCst) {
                if due.is_some_and(|d| wsp_base::util::epoch_secs() >= d) {
                    stamps[0] = None;
                    due = None;
                }
                for (i, half) in [Half::RowsEtc, Half::Attention].into_iter().enumerate() {
                    let now_stamp = stamp_of(&store, half);
                    if stamps[i] == Some(now_stamp) {
                        continue;
                    }
                    let before = match last[i].take() {
                        Some(b) => b,
                        None => match db.snapshot() {
                            Ok((s, _)) => held(&s, half),
                            Err(e) => {
                                say(format!("cannot read the database: {e:?}"));
                                last[i] = None;
                                continue;
                            }
                        },
                    };
                    let now = read_half(&store, half);
                    if half == Half::RowsEtc {
                        due = next_due(&now);
                    }
                    let cmd = diff_half(&before, now.clone(), half);
                    let empty = matches!(&cmd, Command::Mirror { put, gone } if put.is_empty() && gone.is_empty());
                    if !empty {
                        seq += 1;
                        let key = IdempotencyKey(format!(
                            "mirror:{}:{}:{seq}",
                            std::process::id(),
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_nanos())
                                .unwrap_or(0)
                        ));
                        if let Err(e) = db.apply(cmd, key, Principal::Wsp("mirror".into())) {
                            say(format!("{half:?}: not applied: {e:?}"));
                            last[i] = Some(before);
                            continue;
                        }
                    }
                    last[i] = Some(now.iter().map(|e| (subject_of(e), e.clone())).collect());
                    stamps[i] = Some(now_stamp);
                }
                for _ in 0..10 {
                    if flag.load(Ordering::SeqCst) {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        })
        .expect("the mirror thread starts");
    (handle, stop)
}

/// [`wip_tasks`]'s predicate: whether `wip` reads this task's body. Shared with
/// the mirror (`crate::mirror::rows`), which carries the file's text for
/// exactly these.
pub(crate) fn wip_reads_in_full(h: &wsp_store::model::TaskHead) -> bool {
    h.status().is_open()
        || h.has_section(crate::verification::SECTION)
        || h.has_section(crate::legs::SECTION)
        || h.has_section(crate::pieces::SECTION)
        || h.tags.iter().any(|t| t == crate::verification::VERIFY_TAG || t == crate::cycle::BARRIER_TAG)
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

    /// A restart's first pass diffs the files against what the database holds;
    /// the machine the fold's beats put under the hostname is no file's, so it
    /// is not held by this half and is not sent `mirror.gone`.
    #[test]
    fn a_restart_does_not_send_gone_for_the_machine_the_folds_beats_put() {
        let s = seed("mirror-machine");
        let mut state = c::State::default();
        for e in read_rows_etc(&s, LOCAL) {
            if let Entity::Machine(m) = e {
                state.machines.insert(m.name.clone(), m);
            }
        }
        let beat = c::Machine {
            name: MachineName("Jespers-MBP".into()),
            transport: Transport::Local,
            beat: Some(At(1)),
            facts: BTreeMap::new(),
        };
        state.machines.insert(beat.name.clone(), beat);
        let held = held(&state, Half::RowsEtc);
        assert!(!held.contains_key(&Subject::Machine(MachineName("Jespers-MBP".into()))));
        assert!(held.contains_key(&Subject::Machine(MachineName("far".into()))));
        let Command::Mirror { gone, .. } = diff_half(&held, read_rows_etc(&s, LOCAL), Half::RowsEtc) else { panic!() };
        assert!(gone.iter().all(|g| !matches!(g, Subject::Machine(_))), "{gone:?}");
    }

    fn row_of(es: &[Entity], id: &str) -> c::Row {
        match key(es).remove(&Subject::Row(RowId(id.into()))) {
            Some(Entity::Row(r)) => r,
            other => panic!("{id}: {other:?}"),
        }
    }

    /// A row rebuilds the `Task` `wip` reads, for a task it opens (the file's
    /// text through the store's parser) and for one it reads bodiless (the
    /// index's head), and each is what `wip_tasks`'s reading of the files gives.
    /// `wsp-095`: a held barrier carries what it waits on, so "Needs you"
    /// lists it only where `owed` is `person`, and the mirror re-reads when
    /// its time comes.
    #[test]
    fn a_held_group_carries_what_it_waits_on_and_its_time_is_when_the_mirror_reads_again() {
        let mut w = Worklist::new("m1", "m1");
        w.set_status(WorklistStatus::Held);
        w.set_groups(&[wsp_store::model::Group { members: vec!["a-1".into()], agent: "claude".into(), ..Default::default() }]);
        w.held_until = Some(wsp_base::util::iso_at(2_000));
        let fields = |now: i64| match &lists_with(std::slice::from_ref(&w), &|_| true, &BTreeMap::new(), now)[0] {
            Entity::List(l) => l.groups[0].fields.clone(),
            _ => unreachable!(),
        };
        let early = fields(1_000);
        assert_eq!(early.get("owed").map(String::as_str), Some("time"), "{early:?}");
        assert_eq!(early.get("until"), Some(&wsp_base::util::iso_at(2_000)));
        assert!(early["owed_text"].starts_with("waiting until "));
        let late = fields(3_000);
        assert_eq!(late.get("owed").map(String::as_str), Some("person"), "{late:?}");
        assert!(late["owed_text"].contains("wsp worklist go m1"));
        // `wsp-096`: the same two reads, classified: follow-up due then, and
        // needs-you after, with no write between them.
        assert_eq!(early.get("attention").map(String::as_str), Some("follow-up"), "{early:?}");
        assert_eq!(early.get("due"), Some(&wsp_base::util::iso_at(2_000)));
        assert_eq!(early.get("attention_text"), early.get("owed_text"));
        assert_eq!(late.get("attention").map(String::as_str), Some("needs-you"), "{late:?}");
        assert!(late["attention_text"].contains("wsp worklist go m1"));
        assert!(!late.contains_key("due"));
        // `wsp-097`: `action` on the needs-you read only, naming this list.
        assert!(!early.contains_key("action"), "follow-up carries no action: {early:?}");
        let act: serde_json::Value = serde_json::from_str(&late["action"]).unwrap();
        assert_eq!(act["verb"], "worklist");
        assert_eq!(act["args"], serde_json::json!(["go", "m1"]));
        assert_eq!(act["sentence"], "needed");
        let labels: Vec<&str> = act["buttons"].as_array().unwrap().iter().map(|b| b["label"].as_str().unwrap()).collect();
        assert_eq!(labels, vec!["Pass", "Hold"]);
        // Read in the other order, the answers are the same: nothing was kept.
        assert_eq!(fields(3_000).get("attention").map(String::as_str), Some("needs-you"));
        assert_eq!(fields(1_000).get("attention").map(String::as_str), Some("follow-up"));
        let es = lists_with(std::slice::from_ref(&w), &|_| true, &BTreeMap::new(), 1_000);
        assert_eq!(next_due(&es), Some(2_000));
        assert_eq!(next_due(&lists_with(std::slice::from_ref(&w), &|_| true, &BTreeMap::new(), 3_000)), None);
    }

    /// `wsp-096`: a group waiting on rows is follow-up and names them; one
    /// whose check reads it, or a running one, carries no `attention`.
    #[test]
    fn a_group_waiting_on_rows_is_follow_up_and_one_its_check_reads_is_neither() {
        let mut w = Worklist::new("m1", "m1");
        w.set_status(WorklistStatus::Held);
        w.set_groups(&[wsp_store::model::Group { members: vec!["a-1".into()], agent: "claude".into(), ..Default::default() }]);
        w.held_on = vec!["wsp-093".into()];
        let group = |w: &Worklist, settled: &dyn Fn(&str) -> bool| match &lists_with(std::slice::from_ref(w), settled, &BTreeMap::new(), 1_000)[0] {
            Entity::List(l) => l.groups[0].fields.clone(),
            _ => unreachable!(),
        };
        let held = group(&w, &|r| r == "a-1");
        assert_eq!(held.get("attention").map(String::as_str), Some("follow-up"), "{held:?}");
        assert_eq!(held.get("waiting_on").map(String::as_str), Some("wsp-093"));
        assert_eq!(held.get("attention_text").map(String::as_str), Some("waiting on wsp-093"));
        assert!(!held.contains_key("action"), "wsp-097: follow-up carries no action");
        // Running, its members all landed: the run opens the check, so neither.
        w.set_status(WorklistStatus::Running);
        let barrier = group(&w, &|_| true);
        assert_eq!(barrier.get("owed").map(String::as_str), Some("check"), "{barrier:?}");
        assert!(!barrier.contains_key("attention") && !barrier.contains_key("action"), "{barrier:?}");
        let running = group(&w, &|_| false);
        assert!(!running.contains_key("attention") && !running.contains_key("owed"), "{running:?}");
    }

    /// `wsp-096`: an open question for the person needs you; the same question
    /// for a governor, a note, or an answered one carries no `attention`.
    #[test]
    fn an_open_question_for_the_person_carries_needs_you_and_others_carry_nothing() {
        let from = || Party::Pane { pane: "cpd-2".into(), workspace: "w".into() };
        let wait = || wsp_store::message::Waiting::new("cpd-2", "wsp-2");
        let mut msgs = BTreeMap::new();
        let mut person = Message::question(from(), Kind::Direction, "may I?", wait());
        person.id = "m-p".into();
        person.for_person = true;
        let mut governor = Message::question(from(), Kind::Direction, "may I?", wait());
        governor.id = "m-g".into();
        governor.for_person = false;
        governor.for_seat = Some("governor:wsp".into());
        for m in [person, governor] {
            msgs.insert(m.id.clone(), m);
        }
        let es = key(&asks(&msgs, &BTreeMap::new()));
        let fields = |id: &str| match &es[&Subject::Ask(AskId(id.into()))] {
            Entity::Ask(a) => a.fields.clone(),
            _ => unreachable!(),
        };
        let p = fields("m-p");
        assert_eq!(p.get("attention").map(String::as_str), Some("needs-you"), "{p:?}");
        assert!(p["attention_text"].contains("wsp answer m-p"), "{p:?}");
        assert!(!p.contains_key("due") && !p.contains_key("waiting_on"));
        assert!(!fields("m-g").contains_key("attention"), "a governor's to answer");
        // `wsp-097`: the needs-you question carries its action, the governor's none.
        let act: serde_json::Value = serde_json::from_str(&p["action"]).unwrap();
        assert_eq!((act["verb"].as_str(), &act["args"], act["sentence"].as_str()), (Some("answer"), &serde_json::json!(["m-p"]), Some("needed")));
        assert!(!fields("m-g").contains_key("action"), "not needs-you, so no action");
    }

    #[test]
    fn a_task_round_trips_row_to_task_equal_to_the_files() {
        let s = seed("mirror-task");
        let mut t = s.task("wsp-2").unwrap();
        t.body = "## Overview\n\nsome prose\n\n## Log\n\n- 2026-11-18 a line\n".into();
        s.save_task(&t).unwrap();
        let es = read_rows_etc(&s, "here");
        let wip = s.tasks_read_where(wip_reads_in_full);
        for id in ["wsp-1", "wsp-2", "wsp-3", "wsp-4", "wsp-5"] {
            let row = row_of(&es, id);
            let files = wip.iter().find(|t| t.id == id).unwrap();
            let open = id != "wsp-5";
            assert_eq!(row.fields.contains_key(BODY), open, "{id} body");
            assert_eq!(row.fields.contains_key(HEAD), !open, "{id} head");
            let back = task_of(&row).expect("a task");
            assert_eq!(format!("{back:?}"), format!("{files:?}"), "{id}");
        }
        assert!(task_of(&row_of(&es, "wsp-2")).unwrap().body.contains("some prose"));
        let text = std::fs::read_to_string(s.task_path("wsp-2")).unwrap();
        assert_eq!(row_of(&es, "wsp-2").fields[BODY], text, "the file's text, verbatim");
    }

    /// An ask carries its message's record, and the store's reader rebuilds the
    /// same message from it; a flag carries its `flags.json` record.
    #[test]
    fn a_message_and_a_flag_round_trip_through_record() {
        let s = seed("mirror-record");
        let a = key(&read_attention(&s));
        let Entity::Ask(q) = &a[&Subject::Ask(AskId("m-1".into()))] else { panic!() };
        let files = s.messages().remove("m-1").unwrap();
        assert_eq!(message_of(q), Some(files));
        let Entity::Ask(f) = &a[&Subject::Ask(AskId("flag:wsp-4".into()))] else { panic!() };
        assert_eq!(message_of(f), None);
        let want = s.flags().remove("wsp-4").unwrap();
        assert_eq!(flag_of(f), Some(("wsp-4".to_string(), want)));
    }

    /// A seat carries its binding and spawned records whole, and the census row.
    #[test]
    fn a_seat_carries_binding_spawned_and_its_census_row() {
        use crate::place::{Agent, Seat, Seated, State};
        let binding = json!({ "task_id": "wsp-2", "pane_id": "p-1", "host": "here", "epoch": 3 });
        let spawned = json!({ "kind": "claude", "started": { "row": "wsp-2", "attempt": 1, "pid": 42 } });
        let seated = Seated {
            seat: Seat::new("cpd-2"),
            label: "cpd-2".into(),
            cwd: "/tmp/tree".into(),
            agent: Agent { kind: "claude".into(), name: "a-2".into(), key: None, args: vec!["--model".into(), "opus".into()] },
            state: State::Working,
            session: "sess-1".into(),
        };
        let mut seat = c::Seat {
            id: SeatId("cpd-2".into()),
            machine: MachineName("here".into()),
            kind: "claude".into(),
            model: None,
            state: c::SeatState::Working,
            before_lost: None,
            row: None,
            attempt: 0,
            pid: None,
            fields: BTreeMap::from([("task_id".to_string(), "wsp-2".to_string())]),
        };
        seat_records(&mut seat.fields, Some(&binding), Some(&spawned), &seated);
        assert_eq!(binding_of(&seat), Some(binding));
        assert_eq!(spawned_of(&seat), Some(spawned));
        assert_eq!(seat.fields["task_id"], "wsp-2", "what was there stands");
        let back = seated_of(&seat);
        assert_eq!(format!("{back:?}"), format!("{seated:?}"));
        // A binding that ends goes from the fields.
        seat_records(&mut seat.fields, None, None, &Seated { seat: Seat::new("cpd-2"), state: State::Gone, ..Default::default() });
        assert_eq!((binding_of(&seat), spawned_of(&seat)), (None, None));
        assert_eq!(seated_of(&seat).state, State::Gone);
        assert!(!seat.fields.contains_key(LABEL) && !seat.fields.contains_key(AGENT));
    }

    /// wsp-094: a seat the fold ended before the census row was carried (no
    /// `census` field, only its own state `gone`) reads gone, not unknown; and
    /// its own `gone` stands over a census word left from before it ended.
    #[test]
    fn a_gone_seat_with_no_census_word_reads_gone_not_unknown() {
        use crate::place::State;
        let mut seat = c::Seat {
            id: SeatId("cpd-1000".into()),
            machine: MachineName("here".into()),
            kind: "claude".into(),
            model: None,
            state: c::SeatState::Gone,
            before_lost: None,
            row: None,
            attempt: 0,
            pid: Some(19479),
            fields: BTreeMap::from([("epoch".to_string(), "1".to_string()), ("host".to_string(), "here".to_string())]),
        };
        assert_eq!(seated_of(&seat).state, State::Gone);
        seat.fields.insert(CENSUS.into(), "idle".into());
        assert_eq!(seated_of(&seat).state, State::Gone, "its own end stands over an older census word");
        seat.fields.remove(CENSUS);
        seat.state = c::SeatState::Working;
        assert_eq!(seated_of(&seat).state, State::Working, "no word: its own state");
    }
}
