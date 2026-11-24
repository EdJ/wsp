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
use wsp_view::common::key;
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
/// agent's. The files carry no token, so a claim's is 0.
fn claim_seat(v: &serde_json::Value) -> Option<String> {
    ["workspace_label", "workspace_id", "workspace", "agent_id"]
        .iter()
        .find_map(|k| v.get(k).and_then(|x| x.as_str()).filter(|s| !s.is_empty()))
        .map(str::to_string)
}

/// `claims.json` → `Entity::Claim`s: the row, the seat the record names, token
/// 0 (the files have none) and the expiry the record carries (`wsp-409`) in
/// milliseconds, as the contract's `At` is; a record without one, which never
/// expires, is `At(0)`.
pub fn claims(claims: &BTreeMap<String, serde_json::Value>) -> Vec<Entity> {
    claims
        .iter()
        .filter_map(|(row, v)| {
            Some(Entity::Claim(c::Claim {
                row: RowId(row.clone()),
                seat: SeatId(claim_seat(v)?),
                token: Token(0),
                expires: At(wsp_store::store::record_expires(v).map_or(0, |s| s as u64 * 1000)),
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
/// `refs`, `created`, `updated`; and what `ls` and `show` print beside it
/// (wsp-305): `project`, the file's own (absent for an inbox row, whose
/// `Row::project` is named after its id), and `caption_from`. The seat is the
/// claim's; the group is the list's placement. `before_block` and `commit` are
/// not in the files (None). `wsp_view::common` reads every key back, and its
/// table is the contract.
///
/// **And the record itself, verbatim** (wsp-076), so [`task_of`] rebuilds the
/// `Task` `wip` reads with the store's own parser: `body` is the task file's
/// text for every task `texts` holds (those [`wip_reads_in_full`]
/// says `wip` opens), and `head` is the [`TaskHead`] as the store's index keeps
/// it (`TaskHead::to_json`) on every row (wsp-695), so a read that leaves a
/// done row's `body` out still has the row; where there is no `body`, `prose`
/// is the task's body after its front matter (wsp-305), so `ls --all` and
/// `show` read every row's legs, log and passes.
///
/// **And its stage's file half** (wsp-308): `stage`, the caption
/// `stage::of_member` reads with no agent looked at; `stage_steps`, its
/// `history` before the live word settles it; and on a `doing` row whose word
/// its seat decides, `stage_live`, what its agent last said (`said`, from
/// `stage::sayings`). `wsp_view::stage::stage_of` overlays the seat's half.
/// And on a staged row `legs`, `legs::json`'s text, which `ls --json` prints.
///
/// A row at review that is finished (`wsp-105`: verified, not reverted, no
/// held barrier still judging it) carries `attention` = `needs-you`,
/// `attention_text`, and `action` (`wsp done <id>`, Mark done), as groups and
/// asks do. Where several are, each also carries [`ATTENTION_GROUP`], and
/// the first by id alone `group_text` and `group_action` (wsp-755: a count
/// that moves re-puts one row, not every member): the one item
/// [`crate::classify::finished`] makes of them, Mark all done and its items.
pub fn rows(
    tasks: &[Task],
    claims: &BTreeMap<String, serde_json::Value>,
    lists: &[Worklist],
    texts: &BTreeMap<String, String>,
    said: &BTreeMap<String, String>,
) -> Vec<Entity> {
    let place = placements(lists);
    let mut staged = crate::stage::filed(tasks, lists, claims, said);
    let finished = finished_rows(tasks, lists);
    let group = (finished.len() > 1).then(|| finished_item(&finished)).flatten();
    tasks
        .iter()
        .map(|t| {
            let status = Status::parse(&t.status_raw).unwrap_or(Status::Todo);
            let mut f = BTreeMap::new();
            put(&mut f, "status", status.as_str());
            put(&mut f, "prio", model::Priority::parse(&t.priority_raw).unwrap_or(model::Priority::Normal).as_str());
            put(&mut f, "tags", &t.tags.join(","));
            let cap = caption::of_task(t);
            put(&mut f, "caption", &cap.text);
            put(&mut f, key::CAPTION_FROM, cap.from);
            put(&mut f, key::PROJECT, t.project.as_deref().unwrap_or(""));
            put(&mut f, "parent", t.parent.as_deref().unwrap_or(""));
            put(&mut f, "after", &t.after.join(","));
            put(&mut f, "refs", &t.refs.join(","));
            put(&mut f, "created", &t.created);
            put(&mut f, "updated", &t.updated);
            if let Some(claim) = claims.get(&t.id) {
                f.insert(CLAIM.into(), claim.to_string());
            }
            if let Some(a) = finished.get(&t.id) {
                for (k, v) in a.fields() {
                    put(&mut f, k, &v);
                }
                if let Some(g) = &group {
                    put(&mut f, ATTENTION_GROUP, "finished");
                    // `wsp-755`: the group's text and act change with its
                    // count, so they ride on one row; on every member a new
                    // finished row re-put each of them.
                    if finished.keys().next() == Some(&t.id) {
                        put(&mut f, "group_text", &g.text);
                        if let Some(act) = &g.action {
                            put(&mut f, "group_action", &act.json().to_string());
                        }
                    }
                }
            }
            f.insert(HEAD.into(), TaskHead::of(t).to_json().to_string());
            // The allocation travels as a field, where the hub's lease guard reads it (wsp-466).
            if let Some(u) = t.for_user.as_deref().filter(|u| !u.is_empty()) {
                f.insert("for".into(), u.to_string());
            }
            if let Some(st) = staged.remove(&t.id) {
                f.insert(key::STAGE.into(), crate::stage::told_view(&st.told).json().to_string());
                f.insert(key::STAGE_STEPS.into(), serde_json::Value::Array(st.steps.iter().map(|s| s.json()).collect()).to_string());
                if let Some(said) = st.live {
                    f.insert(key::STAGE_LIVE.into(), serde_json::json!({ "said": said }).to_string());
                }
            }
            // The objects `show --json` has, and absent on an unstaged row.
            if t.body.contains(&format!("## {}", crate::legs::SECTION)) && !crate::legs::legs(t).is_empty() {
                f.insert(key::LEGS.into(), crate::legs::json(t).to_string());
            }
            match texts.get(&t.id) {
                Some(text) => drop(f.insert(BODY.into(), text.clone())),
                None => drop(f.insert(key::PROSE.into(), t.body.clone())),
            }
            Entity::Row(c::Row {
                workspace: t.workspace.clone(),
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

/// The tasks of each shared workspace, by workspace, ids in order: what the
/// hub's sync worker projects and pushes (wsp-464). A private task is in none,
/// so nothing private is ever projected for the hub.
pub fn by_shared_workspace(tasks: Vec<Task>) -> BTreeMap<wsp_store_contract::WorkspaceId, Vec<Task>> {
    let mut out: BTreeMap<_, Vec<Task>> = BTreeMap::new();
    for t in tasks.into_iter().filter(|t| !t.workspace.is_private()) {
        out.entry(t.workspace.clone()).or_default().push(t);
    }
    for ts in out.values_mut() {
        ts.sort_by(|a, b| a.id.cmp(&b.id));
    }
    out
}

/// One shared task as the row the hub is sent: [`rows`] over it alone, with its
/// file's `text` as the body, so the row is the file and nothing of this
/// spoke's claims, lists or seats rides along. `None` for a private task.
pub fn shared_row(t: &Task, text: &str) -> Option<c::Row> {
    if t.workspace.is_private() {
        return None;
    }
    let texts = BTreeMap::from([(t.id.clone(), text.to_string())]);
    rows(std::slice::from_ref(t), &BTreeMap::new(), &[], &texts, &BTreeMap::new()).into_iter().find_map(|e| match e {
        Entity::Row(r) => Some(r),
        _ => None,
    })
}

/// A row's `fields` key holding the task file's text.
pub const BODY: &str = key::BODY;
/// A row's `fields` key holding the task's head, on every row: what `wip`
/// reads of a task it reads bodiless, and what a done row keeps in
/// `wip_snapshot` (wsp-695).
pub const HEAD: &str = key::HEAD;
/// A row's `fields` key holding its `claims.json` record (JSON text), verbatim:
/// an `Entity::Claim` keeps the seat alone, and `wip` joins the record's
/// `agent_id` and `agent_kind` (wsp-076).
pub const CLAIM: &str = key::CLAIM;

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
///
/// A done list that still has a seat on it — a check whose entry names a pane
/// and has not ended ([`crate::barrier::seats`] reads every list for it), or a
/// governor's post `governors` still has filled — also carries
/// [`c::WIP_SEATED`], so `wip_snapshot` keeps its `body` (wsp-695).
pub fn lists(lists: &[Worklist], tasks: &[Task], texts: &BTreeMap<String, String>, governors: &BTreeMap<String, serde_json::Value>) -> Vec<Entity> {
    lists_with(lists, &settled_in(tasks), texts, &|id| post_filled(governors, id), wsp_base::util::epoch_secs())
}

/// Whether a governor's post is filled: its record names a workspace, as a
/// seated one does from any machine and a vacated one (`{vacated, last}`)
/// does not.
fn post_filled(governors: &BTreeMap<String, serde_json::Value>, scope: &str) -> bool {
    governors.get(scope).and_then(|r| r.get("workspace")).and_then(serde_json::Value::as_str).is_some_and(|w| !w.is_empty())
}

/// Whether a done list still has a seat on it: a standing check, or its
/// governor's post filled (`governed`).
fn seated(w: &Worklist, governed: &dyn Fn(&str) -> bool) -> bool {
    w.groups().iter().flat_map(|g| g.checks.iter()).any(|c| c.standing().is_some()) || governed(&w.id)
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

/// A group's state from its list's file alone (what [`lists`] says of each).
fn group_state(status: WorklistStatus, ord: usize, floor: usize, g: &model::Group, settled: &dyn Fn(&str) -> bool) -> ListState {
    match status {
        WorklistStatus::Draft => ListState::Pending,
        WorklistStatus::Done => ListState::Passed,
        _ if ord <= floor => ListState::Passed,
        _ if ord > floor + 1 => ListState::Pending,
        WorklistStatus::Held => ListState::Held,
        _ if g.members.iter().all(|m| settled(m)) => ListState::Barrier,
        _ => ListState::Running,
    }
}

/// Whether a member counts as done for a barrier: done, or no longer held.
fn settled_in(tasks: &[Task]) -> impl Fn(&str) -> bool + '_ {
    let done: BTreeSet<&str> =
        tasks.iter().filter(|t| Status::parse(&t.status_raw) == Some(Status::Done)).map(|t| t.id.as_str()).collect();
    let known: BTreeSet<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
    move |m| done.contains(m) || !known.contains(m)
}

/// Whether a row's log says its change was reverted (`wsp-105`): an entry
/// (or a `note:`) that starts "reverted", as `wsp log <id> "reverted in
/// <sha>"` writes one. The mirror runs no git, so a revert commit nobody
/// noted on the row is not seen here.
fn reverted(t: &Task) -> bool {
    let Some(log) = t.section("Log") else { return false };
    log.lines().any(|l| {
        let Some((_, said)) = l.strip_prefix("- ").and_then(|r| r.split_once(' ')) else { return false };
        let said = said.strip_prefix("note: ").unwrap_or(said);
        said.to_ascii_lowercase().starts_with("reverted")
    })
}

/// `wsp-105`: each row at review that is finished as [`crate::classify`]
/// sorts it, by id: its newest verification holds, its change was not
/// reverted, and no held or ready barrier's stop still judges it (a member of
/// a group at `held` or `barrier`).
pub(crate) fn finished_rows(tasks: &[Task], lists: &[Worklist]) -> BTreeMap<String, crate::classify::Attention> {
    let settled = settled_in(tasks);
    let mut judged: BTreeSet<String> = BTreeSet::new();
    for w in lists {
        let gs = w.groups();
        let floor = model::crossed(&gs);
        for (i, g) in gs.iter().enumerate() {
            if matches!(group_state(w.status(), i + 1, floor, g, &settled), ListState::Held | ListState::Barrier) {
                judged.extend(g.members.iter().cloned());
            }
        }
    }
    tasks
        .iter()
        .filter(|t| Status::parse(&t.status_raw) == Some(Status::Review))
        .filter_map(|t| {
            let row = crate::classify::Row {
                id: &t.id,
                review: true,
                verified: crate::verification::verified(tasks, &t.id),
                owes: reverted(t) || judged.contains(&t.id),
            };
            Some((t.id.clone(), crate::classify::of(crate::classify::Item::Row(row))?))
        })
        .collect()
}

/// The finished rows as the one item the person reads, built by
/// [`crate::classify::finished`] alone: the mirror's rows and `wip --json`
/// both print this, so the census and the CLI cannot disagree (`wsp-105`).
pub(crate) fn finished_item(finished: &BTreeMap<String, crate::classify::Attention>) -> Option<crate::classify::Attention> {
    crate::classify::finished(finished.iter().map(|(id, a)| (id.clone(), a.clone())).collect())
}

/// A finished row's key for the item it folds into, where there are several:
/// `finished`. Beside it `group_text` ("N finished rows to close") and
/// `group_action`, the group's act as JSON, both on **one** row only: the lowest id
/// among the rows in the group, which is after both exclusions (a reverted
/// row or one a held barrier judges is never it), not among all at review
/// (`wsp-105`).
pub const ATTENTION_GROUP: &str = "attention_group";

fn lists_with(
    lists: &[Worklist],
    settled: &dyn Fn(&str) -> bool,
    texts: &BTreeMap<String, String>,
    governed: &dyn Fn(&str) -> bool,
    now: i64,
) -> Vec<Entity> {
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
                    let state = group_state(status, ord, floor, g, settled);
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
            if let Some(u) = w.for_user.as_deref().filter(|u| !u.is_empty()) {
                f.insert("for".into(), u.to_string());
            }
            if status == WorklistStatus::Done && seated(w, governed) {
                f.insert(c::WIP_SEATED.into(), "true".into());
            }
            Entity::List(c::List { workspace: w.workspace.clone(), id: ListId(w.id.clone()), state: list_state(status), groups, fields: f })
        })
        .collect()
}

fn principal(p: &Party) -> Principal {
    match p {
        Party::Pane { pane, .. } => Principal::Seat(SeatId(pane.clone())),
        Party::Seat(scope) => Principal::Seat(SeatId(scope.clone())),
        Party::Agent(h) if h == "wsp" => Principal::Wsp("wsp".into()),
        Party::Agent(h) => Principal::Seat(SeatId(h.clone())),
        Party::Human(name) => Principal::Person(name.clone().unwrap_or_else(|| "person".into())),
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
            for (k, v) in [("of", &m.of), ("then", &m.then), ("answered_by", &m.answered_by), ("relayed_by", &m.relayed_by)] {
                put(&mut f, k, v.as_deref().unwrap_or(""));
            }
            if m.step_done_at.is_some() {
                put(&mut f, "step_done", "true");
            }
            f.insert(RECORD.into(), m.to_json().to_string());
            let to = match (&m.for_seat, m.for_person) {
                (Some(s), _) => Some(Principal::Seat(SeatId(s.clone()))),
                (None, true) => Some(Principal::Person("person".into())),
                _ => None,
            };
            let answer = m.closing().filter(|h| h.act == wsp_store::message::Act::Answered).map(|h| h.note.clone());
            let mut ask = c::Ask {
                workspace: Default::default(),
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
            workspace: Default::default(),
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
            use wsp_view::machine::key as k;
            let mut f = BTreeMap::new();
            // The record, as `machine ls --json` draws it (wsp-308). `ssh` is
            // always there: it marks a fact set that is a record.
            f.insert(k::SSH.into(), m.ssh.clone());
            put(&mut f, k::OS, &m.os);
            put(&mut f, k::ARCH, &m.arch);
            put(&mut f, k::BACKEND_AT, &m.backend_at);
            put(&mut f, k::SUP_AT, &m.sup_at);
            put(&mut f, k::ADDED, &m.added);
            put(&mut f, k::STATUS, &m.status);
            put(&mut f, k::AGENTS, &m.agents.map(|n| n.to_string()).unwrap_or_default());
            put(&mut f, k::LEASE, &m.lease.map(|n| n.to_string()).unwrap_or_default());
            put(&mut f, k::EXEC_DIR, &m.exec_dir);
            let mut beat = None;
            if let Some(l) = live.get(&m.name) {
                f.insert(k::REACHABLE.into(), l.reachable.to_string());
                f.insert(k::READY.into(), l.ready.to_string());
                if l.tick_secs != 0 {
                    f.insert(k::TICK_SECS.into(), l.tick_secs.to_string());
                }
                put(&mut f, k::LAST_SEEN, &l.last_seen);
                put(&mut f, k::ERROR, &l.error);
                put(&mut f, k::HEARD, &l.heard);
                put(&mut f, k::BEAT_AT, &l.beat_at);
                put(&mut f, k::TUNNEL, &l.tunnel);
                put(&mut f, k::SOCKET, &l.socket);
                put(&mut f, k::CHECKED, &l.checked);
                put(&mut f, k::CHECK_ERROR, &l.check_error);
                put(&mut f, k::SHIM_WARNING, &l.shim_warning);
                if !l.leases.is_empty() {
                    f.insert(k::LEASES.into(), serde_json::json!(l.leases).to_string());
                }
                if let Some(n) = l.wire {
                    f.insert(k::WIRE.into(), n.to_string());
                }
                if let Some(n) = l.wire_oldest {
                    f.insert(k::WIRE_OLDEST.into(), n.to_string());
                }
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
pub const BINDING: &str = key::BINDING;
/// A seat's `fields` key holding its `spawned.json` record (JSON text).
pub const SPAWNED: &str = "spawned";
/// A seat's `fields` keys for what builds a `place::Seated`: the label, the
/// working directory, the agent (`{kind, name, args}` as JSON text), the
/// census state word (`place::State::as_str`) and the session id.
pub const LABEL: &str = "label";
pub const CWD: &str = "cwd";
pub const AGENT: &str = "agent";
pub const CENSUS: &str = key::CENSUS;
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
/// the files path builds (`Compound::seated_as`); and `governs`, the scope the
/// seat governs (`place::governs`), which `wsp_view::stage::agent_of` reads as
/// `needs_a_person`'s third input (wsp-308).
pub fn seat_records(
    fields: &mut BTreeMap<String, String>,
    binding: Option<&serde_json::Value>,
    spawned: Option<&serde_json::Value>,
    seated: &crate::place::Seated,
    governs: Option<&str>,
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
    set(fields, key::GOVERNS, governs.unwrap_or_default());
}

/// The scope a seat governs, as `stage::census` asks it: by its pane, within
/// its workspace.
pub fn governs_of(governors: &BTreeMap<String, serde_json::Value>, seat: &str) -> Option<String> {
    let workspace = seat.split(':').next().unwrap_or(seat);
    crate::place::governs(governors, &crate::place::seat_query(workspace, Some(seat)))
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
    // state, never Unknown (wsp-094). `wsp-view`'s reading, so the stage
    // overlay over `State` and this one agree (wsp-308).
    let state = match wsp_view::stage::census_word(seat) {
        "empty" => State::Empty,
        "starting" => State::Starting,
        "idle" => State::Idle,
        "working" => State::Working,
        "blocked" => State::Blocked,
        "gone" => State::Gone,
        "unreachable" => State::Unreachable,
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
    with_machines(store, read_records(store), local)
}

/// What [`read_rows_etc`] reads of everything but `machines.json`: the rows,
/// claims and lists, and the seat's own facts. The mirror thread keeps the
/// last one, so a pass that only `machines.json` moved re-reads the machines
/// alone (wsp-767): an unreachable machine's tunnel rewrites that file every
/// few seconds, and each rewrite cost a whole re-read of every row and list.
#[derive(Clone, Debug)]
pub struct Records {
    entities: Vec<Entity>,
    seat: BTreeMap<String, String>,
}

/// [`Records`], read now.
pub fn read_records(store: &Store) -> Records {
    let tasks = store.tasks();
    let worklists = store.worklists();
    let mut entities = row_half(store, &tasks, &worklists);
    entities.extend(list_half(store, &tasks, worklists, Shown::Every));
    Records { entities, seat: seat_facts(store) }
}

/// `records` with the machines as the files say them now, the seat's facts
/// riding on the one named `local`: [`read_rows_etc`]'s whole answer.
pub fn with_machines(store: &Store, records: Records, local: &str) -> Vec<Entity> {
    let mut out = records.entities;
    out.extend(machines(&store.machines(), &store.machines_live(), local));
    seat_machine(&mut out, records.seat, local);
    out
}

/// [`read_rows_etc`]'s rows, with a review row's project chain, and claims.
fn row_half(store: &Store, tasks: &[Task], worklists: &[Worklist]) -> Vec<Entity> {
    let claims_file = store.claims();
    let texts = texts_of(store, tasks);
    let mut out = rows(tasks, &claims_file, worklists, &texts, &crate::stage::sayings(store));
    // A review row's project chain, for `yours --json -p`'s count.
    let index = crate::resolve::Index::new(store.projects());
    for e in out.iter_mut() {
        if let Entity::Row(r) = e {
            let project = r.fields.get(key::PROJECT).cloned().unwrap_or_default();
            if r.fields.get(key::STATUS).map(String::as_str) == Some("review") && !project.is_empty() {
                r.fields.insert(wsp_view::asks::key::CHAIN.to_string(), chain_of(&index, &project));
            }
        }
    }
    out.extend(claims(&claims_file));
    out
}

/// [`read_rows_etc`]'s lists, with [`list_facts`] and `shown`'s show facts.
fn list_half(store: &Store, tasks: &[Task], worklists: Vec<Worklist>, shown: Shown) -> Vec<Entity> {
    let mut ls = lists(&worklists, tasks, &list_texts_of(store, &worklists), &store.governors());
    list_facts_of(store, worklists, &mut ls, shown);
    ls
}

/// Which lists a read builds `worklist show --json`'s facts for
/// ([`LIST_SHOW`]): each costs a pass over its members' stages, groups and
/// history, and `show <slug>` printing one list must not pay for 200 (wsp-766).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shown<'a> {
    /// Every list: the mirror's and the daemon's whole State.
    Every,
    /// None: `worklist ls --json` prints the heads alone.
    Heads,
    /// The one list `show <slug> --json` prints.
    One(&'a str),
    /// Each list `show --all --json` prints without `--include-closed`.
    Open,
}

impl Shown<'_> {
    fn takes(self, l: &crate::worklist::Listed) -> bool {
        match self {
            Shown::Every => true,
            Shown::Heads => false,
            Shown::One(id) => l.list.id == id,
            Shown::Open => l.segment != crate::worklist::Segment::Closed,
        }
    }
}

/// The lists alone, as [`state_of`] has them, but with `show --json`'s facts
/// only on those `shown` takes: what `worklist ls --json` and `worklist show
/// --json` print from (wsp-766). Every list's head facts are still built —
/// `ls`'s order, segment and queue place are read across all of them.
pub fn lists_state(store: &Store, shown: Shown) -> c::State {
    let tasks = store.tasks();
    let mut s = c::State::default();
    for e in list_half(store, &tasks, store.worklists(), shown) {
        if let Entity::List(l) = e {
            s.lists.insert(l.id.clone(), l);
        }
    }
    s
}

/// The rows, their claims and the seats, as [`state_of`] has them: what `ls
/// --json` prints from (a row's stage reads its seats), without the lists'
/// facts, the machines or the asks (wsp-766).
pub fn rows_state(store: &Store, local: &str) -> c::State {
    let tasks = store.tasks();
    let worklists = store.worklists();
    let mut s = c::State::default();
    let seats = crate::fold::seats_of(store, local).into_iter().map(Entity::Seat);
    for e in row_half(store, &tasks, &worklists).into_iter().chain(seats) {
        match e {
            Entity::Row(r) => {
                s.rows.insert(r.id.clone(), r);
            }
            Entity::Claim(x) => {
                s.claims.insert(x.row.clone(), x);
            }
            Entity::Seat(x) => {
                s.seats.insert(x.id.clone(), x);
            }
            _ => {}
        }
    }
    s
}

/// The agents counted against the seat's cap, named. Installed by `main`
/// (`cmd_machine` is above the mirror); unset, none are.
pub static SEAT_WORKING: std::sync::OnceLock<fn(&Store) -> Vec<String>> = std::sync::OnceLock::new();

/// This seat's own line of `machine ls`: it has no machine record, so its
/// hostname, cap and the agents counted against it ride on the machine named
/// `local` (wsp-308; keys in [`wsp_view::machine::key`]). A record named
/// `local` takes the facts too.
fn seat_machine(out: &mut Vec<Entity>, f: BTreeMap<String, String>, local: &str) {
    for e in out.iter_mut() {
        if let Entity::Machine(m) = e {
            if m.name.0 == local {
                m.facts.extend(f);
                return;
            }
        }
    }
    out.push(Entity::Machine(c::Machine { name: MachineName(local.to_string()), transport: Transport::Local, beat: None, facts: f }));
}

/// [`seat_machine`]'s facts: nothing in them is read from `machines.json`.
fn seat_facts(store: &Store) -> BTreeMap<String, String> {
    use wsp_view::machine::key as k;
    let cap = store.seat_cap();
    let mut f = BTreeMap::new();
    // `yours --json`'s two readings that belong to no ask: the rows the run
    // closed that nobody has looked at, and the retro lines.
    f.insert(wsp_view::asks::key::UNSEEN.to_string(), crate::auto_close::unseen(store).len().to_string());
    f.insert(wsp_view::asks::key::RETRO.to_string(), serde_json::json!(crate::retro_yours::yours_lines(store)).to_string());
    f.insert(k::SEAT.to_string(), wsp_base::util::hostname());
    if let Some(n) = cap {
        f.insert(k::SEAT_CAP.to_string(), n.to_string());
        f.insert(k::SEAT_WORKING.to_string(), serde_json::json!(SEAT_WORKING.get().map(|f| f(store)).unwrap_or_default()).to_string());
    }
    f
}

/// A list's declared outputs as `{name, kind, file, present}` and the
/// directory they are in, read from disk. Installed by `main` (`cmd_report`
/// is above the mirror); unset, a list says no outputs.
pub static LIST_OUTPUTS: std::sync::OnceLock<fn(&Worklist) -> (Vec<serde_json::Value>, String)> = std::sync::OnceLock::new();

/// What `worklist show --json` reads beside the head, per list of `listed`
/// in its order: `(key, value)` pairs under [`wsp_view::worklist::key`]'s
/// `show_*` keys and `overview` (wsp-308). Installed by `main` (`cmd_worklist`
/// is above the mirror); unset, a list carries none and `show` over State
/// draws it bare.
pub static LIST_SHOW: std::sync::OnceLock<ListShow> = std::sync::OnceLock::new();

/// [`LIST_SHOW`]'s shape.
pub type ListShow = fn(&Store, &[crate::worklist::Listed]) -> Vec<Vec<(&'static str, String)>>;

/// What `worklist ls --json` reads that the files alone do not say, written
/// into each list's `fields` (wsp-308; keys in [`wsp_view::worklist::key`]):
/// phase word and flags, the group it stands at, which declared outputs are on
/// disk, members gone, unwritten barriers, the segment and its place in `ls`'s
/// order, activity, the priority word, and a planned list's queue place and
/// what it waits on. One `worklist::listing` and one `waits::read`, as the CLI does;
/// and, through [`LIST_SHOW`], what `worklist show --json` draws beside the head.
pub fn list_facts(store: &Store, lists: Vec<Worklist>, out: &mut [Entity]) {
    list_facts_of(store, lists, out, Shown::Every)
}

/// [`list_facts`], with [`LIST_SHOW`]'s facts only for the lists `shown` takes.
pub fn list_facts_of(store: &Store, lists: Vec<Worklist>, out: &mut [Entity], shown: Shown) {
    use wsp_view::worklist::key as k;
    let listed = crate::worklist::listing(store, lists);
    let waits = crate::waits::read(store, &listed);
    for (order, l) in listed.iter().enumerate() {
        let Some(Entity::List(e)) = out.iter_mut().find(|e| matches!(e, Entity::List(x) if x.id.0 == l.list.id)) else { continue };
        let phase = l.phase(store);
        let caption = caption::of_worklist(&l.list);
        let (outputs, dir) = LIST_OUTPUTS.get().map(|f| f(&l.list)).unwrap_or_default();
        let f = &mut e.fields;
        put(f, k::CAPTION_FROM, caption.from);
        put(f, k::PHASE, phase.status.word());
        for (key, on) in [(k::HELD, phase.flags.held), (k::WAITING_ON_ED, phase.flags.waiting_on_ed), (k::PARKED, phase.flags.parked)] {
            put(f, key, if on { "true" } else { "" });
        }
        put(f, k::STORED, l.list.status_raw.trim());
        put(f, k::GROUPS, &l.at.of.to_string());
        put(f, k::AT, &l.at.at.map(|n| n.to_string()).unwrap_or_default());
        put(f, k::OUTPUTS, &serde_json::Value::Array(outputs).to_string());
        put(f, k::OUTPUTS_DIR, &dir);
        put(f, k::SEGMENT, l.segment.word());
        put(f, k::ORDER, &order.to_string());
        put(f, k::OPEN, &l.open.to_string());
        put(f, k::GONE, &serde_json::json!(l.gone).to_string());
        put(f, k::UNWRITTEN, &serde_json::json!(l.at.unwritten).to_string());
        put(f, k::ACTIVITY, &l.activity.to_string());
        put(f, k::PRIO_WORD, crate::waits::Prio::of(&l.list).word());
        if let Some(n) = waits.iter().position(|(id, _)| *id == l.list.id) {
            put(f, k::QUEUE, &(n + 1).to_string());
            put(f, k::WAITS, &waits[n].1.as_ref().map(|w| w.text()).unwrap_or_default());
        }
    }
    let Some(show) = LIST_SHOW.get() else { return };
    let listed: Vec<crate::worklist::Listed> = listed.into_iter().filter(|l| shown.takes(l)).collect();
    if listed.is_empty() {
        return;
    }
    for (l, facts) in listed.iter().zip(show(store, &listed)) {
        let Some(Entity::List(e)) = out.iter_mut().find(|e| matches!(e, Entity::List(x) if x.id.0 == l.list.id)) else { continue };
        for (key, value) in facts {
            e.fields.insert(key.into(), value);
        }
    }
}

/// The whole [`c::State`] the files say, by the projections the mirror and
/// the fold apply (wsp-308): rows, claims, lists and machines
/// ([`read_rows_etc`]), asks ([`read_attention`]), and seats as the fold
/// builds them (`fold::seat_of`), one per seat this machine's backend's census
/// names, at its census word. What the CLI's `--json` paths hand `wsp-view`,
/// so they print what web-chrome draws from the daemon's fold.
pub fn state_of(store: &Store, local: &str) -> c::State {
    let mut s = c::State::default();
    let seats = crate::fold::seats_of(store, local).into_iter().map(Entity::Seat);
    for e in read_rows_etc(store, local).into_iter().chain(read_attention(store)).chain(seats) {
        match e {
            Entity::Row(r) => {
                s.rows.insert(r.id.clone(), r);
            }
            Entity::List(l) => {
                s.lists.insert(l.id.clone(), l);
            }
            Entity::Seat(x) => {
                s.seats.insert(x.id.clone(), x);
            }
            Entity::Claim(x) => {
                s.claims.insert(x.row.clone(), x);
            }
            Entity::Ask(x) => {
                s.asks.insert(x.id.clone(), x);
            }
            Entity::Machine(x) => {
                s.machines.insert(x.name.clone(), x);
            }
            _ => {}
        }
    }
    s
}

/// Reads the attention half: messages and flags.
pub fn read_attention(store: &Store) -> Vec<Entity> {
    let mut out = asks(&store.messages(), &store.flags());
    ask_facts(store, &mut out);
    out
}

/// The project an item belongs to: its task's, or for a list its first
/// member's that still exists — a list has no project of its own.
pub fn project_of(store: &Store, about: &About) -> Option<String> {
    let task = match about {
        About::Task(t) => store.task(t),
        About::Scope(l) | About::Group { scope: l, .. } => store
            .worklist(l)
            .and_then(|w| w.groups().iter().flat_map(|g| g.members.clone()).find_map(|m| store.task(&m))),
        About::Nothing => None,
    }?;
    task.project.clone().filter(|p| !p.is_empty())
}

/// What an item holds: the list and group it unblocks, or the row.
pub fn unblocks(store: &Store, q: &Message) -> Option<String> {
    match &q.about {
        About::Group { scope, group } => Some(format!("{scope} group {group}")),
        About::Scope(s) => Some(s.clone()),
        About::Task(t) => match crate::worklist::Running::read(store).of(t) {
            Some(at) => Some(format!("{} group {}", at.list, at.group)),
            None => Some(t.clone()),
        },
        About::Nothing => None,
    }
}

/// A project and its ancestors, nearest first, joined by `,`.
fn chain_of(index: &crate::resolve::Index, project: &str) -> String {
    let mut all = vec![project.to_string()];
    all.extend(index.ancestors(project));
    all.join(",")
}

/// What `flag --person --json` and `yours --json` read off each message that
/// the record does not say (wsp-308; keys in [`wsp_view::asks::key`]): whether
/// it is raised, the person's, wanting an answer, a question for the person;
/// its project and that project's chain, and what it unblocks.
fn ask_facts(store: &Store, out: &mut [Entity]) {
    use wsp_view::asks::key as k;
    let index = crate::resolve::Index::new(store.projects());
    for e in out.iter_mut() {
        let Entity::Ask(a) = e else { continue };
        let Some(m) = message_of(a) else { continue };
        let raised = m.needs_attention();
        let flag = |b: bool| if b { "true" } else { "" };
        put(&mut a.fields, k::RAISED, flag(raised));
        put(&mut a.fields, k::PERSON, flag(raised && !m.is_reply() && m.is_the_persons()));
        put(&mut a.fields, k::WANTS, flag(m.wants_answering()));
        put(&mut a.fields, k::PERSON_Q, flag(m.shape() == Some(Shape::Question) && m.for_person));
        let project = project_of(store, &m.about);
        put(&mut a.fields, k::PROJECT, project.as_deref().unwrap_or(""));
        put(&mut a.fields, k::CHAIN, &project.as_deref().map(|p| chain_of(&index, p)).unwrap_or_default());
        put(&mut a.fields, k::UNBLOCKS, unblocks(store, &m).as_deref().unwrap_or(""));
    }
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
        Ok(c) => {
            // Where it listens is what api.json says, not a guess at the machine's addresses.
            let at = std::fs::read_to_string(state.join(wsp_api::LISTEN_FILE))
                .ok()
                .and_then(|t| serde_json::from_str::<wsp_api::Listen>(&t).ok())
                .map(|l| l.addrs.join(", "))
                .unwrap_or_default();
            notes.push(format!("API answers at {at} (head event {})", c.head().0));
        }
        Err(why) => problems.push(format!("api.json names an address that does not answer: {why}")),
    }
}

/// The machine name `read_rows_etc` marks `Transport::Local`.
pub const LOCAL: &str = "local";

/// What moves a half: the stamp, plus (for the rows half) the mtimes of the
/// state files neither stamp covers, `claims.json`, `machines.json` and
/// `governors.json` (a vacated post takes a done list's [`c::WIP_SEATED`] off).
fn stamp_of(store: &Store, half: Half) -> Stamp {
    match half {
        Half::Attention => (store.attention_stamp(), [None, None, None]),
        Half::RowsEtc => {
            let m = |n: &str| std::fs::metadata(store.state_file(n)).ok().and_then(|m| m.modified().ok());
            (store.fingerprint(), [m("claims.json"), m("machines.json"), m("governors.json")])
        }
    }
}

fn read_half(store: &Store, half: Half) -> Vec<Entity> {
    match half {
        Half::RowsEtc => read_rows_etc(store, LOCAL),
        Half::Attention => read_attention(store),
    }
}

/// [`stamp_of`]'s answer for a half.
pub type Stamp = (u64, [Option<std::time::SystemTime>; 3]);

/// The rows half's re-read at stamp `now`. Where `kept` holds the last
/// reading and only `machines.json` has moved since its stamp — the
/// fingerprint, `claims.json` and `governors.json` as they were — the
/// machines alone are read again and the rest is `kept`'s (wsp-767);
/// otherwise it is the whole [`read_rows_etc`], and `kept` becomes it.
pub fn rows_etc_pass(store: &Store, now: &Stamp, kept: &mut Option<(Stamp, Records)>, local: &str) -> Vec<Entity> {
    if let Some(((fp, f), r)) = kept.as_ref() {
        if *fp == now.0 && f[0] == now.1[0] && f[2] == now.1[2] {
            return with_machines(store, r.clone(), local);
        }
    }
    let r = read_records(store);
    *kept = Some((*now, r.clone()));
    with_machines(store, r, local)
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
            let mut stamps: [Option<Stamp>; 2] = [None, None];
            // The rows half's last reading but its machines ([`rows_etc_pass`]).
            let mut kept: Option<(Stamp, Records)> = None;
            let mut last: [Option<BTreeMap<Subject, Entity>>; 2] = [None, None];
            // The soonest `until` a held barrier waits for (`wsp-095`): no file
            // moves when it comes, so the rows half is re-read then.
            let mut due: Option<i64> = None;
            while !flag.load(Ordering::SeqCst) {
                if due.is_some_and(|d| wsp_base::util::epoch_secs() >= d) {
                    stamps[0] = None;
                    kept = None;
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
                    let now = match half {
                        Half::RowsEtc => rows_etc_pass(&store, &now_stamp, &mut kept, LOCAL),
                        Half::Attention => read_half(&store, half),
                    };
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

    #[test]
    fn a_person_is_mirrored_as_the_engineer_the_record_names_else_as_person() {
        let text = |p: &Party| wsp_api::principal::text(&principal(p));
        assert_eq!(text(&Party::engineer("jesper")), "person:jesper");
        assert_eq!(text(&Party::Human(None)), "person:person");
    }

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

    /// `wsp-171`: a task's and a worklist's `workspace:` is the row's and the list's; a private
    /// record has none, and a move to another workspace is a different entity (a put).
    #[test]
    fn a_rows_and_a_lists_workspace_is_the_files() {
        let s = seed("mirror-workspace");
        let core: c::WorkspaceId = "shared:core".parse().unwrap();
        let ws = |s: &Store| {
            let es = read_rows_etc(s, "here");
            let of = |sub: Subject| match key(&es).remove(&sub).unwrap() {
                Entity::Row(r) => r.workspace,
                Entity::List(l) => l.workspace,
                _ => panic!(),
            };
            (of(Subject::Row(RowId("wsp-1".into()))), of(Subject::List(ListId("list-a".into()))))
        };
        assert_eq!(ws(&s), (Default::default(), Default::default()));
        let mut t = s.find_task("wsp-1").unwrap();
        t.workspace = core.clone();
        s.save_task(&t).unwrap();
        let mut w = s.worklists().into_iter().find(|w| w.id == "list-a").unwrap();
        w.workspace = core.clone();
        s.save_worklist(&w).unwrap();
        assert_eq!(ws(&s), (core.clone(), core));
    }

    /// `wsp-409`: a record's `expires` mirrors as a moment (ms, the contract's
    /// `At`); a record from before it, which never expires, stays `At(0)`.
    #[test]
    fn a_claims_expiry_mirrors_and_an_old_record_has_none() {
        let mut m = BTreeMap::new();
        m.insert("new".to_string(), json!({ "workspace": "w", "expires": "2026-11-30T10:00:00Z" }));
        m.insert("old".to_string(), json!({ "workspace": "w" }));
        let got: BTreeMap<String, u64> = claims(&m)
            .into_iter()
            .map(|e| match e {
                Entity::Claim(c) => (c.row.0, c.expires.0),
                _ => panic!(),
            })
            .collect();
        assert_eq!(got["new"], wsp_base::util::epoch_of("2026-11-30T10:00:00Z") as u64 * 1000);
        assert_ne!(got["new"], 0);
        assert_eq!(got["old"], 0);
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

    /// `wsp-767`: an unreachable machine's tunnel rewrites `machines.json`
    /// every few seconds. A pass where only that file moved reads the machines
    /// again and nothing else — no task directory walked — and says what the
    /// whole re-read says; a task that moves is a whole re-read again.
    #[test]
    fn a_pass_where_only_machines_json_moved_reads_the_machines_alone() {
        let s = seed("mirror-machines-only");
        let live = |tunnel: &str| wsp_store::store::MachineLive { tunnel: tunnel.into(), ..Default::default() };
        let mut kept = None;
        let first = rows_etc_pass(&s, &stamp_of(&s, Half::RowsEtc), &mut kept, LOCAL);
        assert_eq!(key(&first), key(&read_rows_etc(&s, LOCAL)));
        s.set_machine_live("far", &live("retrying"));
        let walks = wsp_store::store::dir_walks();
        let moved = rows_etc_pass(&s, &stamp_of(&s, Half::RowsEtc), &mut kept, LOCAL);
        assert_eq!(wsp_store::store::dir_walks(), walks, "no record directory is read for a machines.json move");
        assert_eq!(key(&moved), key(&read_rows_etc(&s, LOCAL)), "what the whole re-read says");
        let Entity::Machine(far) = &key(&moved)[&Subject::Machine(MachineName("far".into()))] else { panic!() };
        assert_eq!(far.facts.get(wsp_view::machine::key::TUNNEL).map(String::as_str), Some("retrying"));
        let Command::Mirror { put, gone } = diff_half(&key(&first), moved.clone(), Half::RowsEtc) else { panic!() };
        assert_eq!((put.iter().map(subject_of).collect::<Vec<_>>(), gone), (vec![Subject::Machine(MachineName("far".into()))], vec![]));
        // A task written is the whole re-read, and the row says it.
        let mut t = s.task("wsp-1").unwrap();
        t.status_raw = "doing".into();
        s.save_task(&t).unwrap();
        s.set_machine_live("far", &live("starting"));
        let walks = wsp_store::store::dir_walks();
        let whole = rows_etc_pass(&s, &stamp_of(&s, Half::RowsEtc), &mut kept, LOCAL);
        assert!(wsp_store::store::dir_walks() > walks);
        assert_eq!(key(&whole), key(&read_rows_etc(&s, LOCAL)));
        let Entity::Row(r1) = &key(&whole)[&Subject::Row(RowId("wsp-1".into()))] else { panic!() };
        assert_eq!(r1.state, RowState::Doing);
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
        let fields = |now: i64| match &lists_with(std::slice::from_ref(&w), &|_| true, &BTreeMap::new(), &|_| false, now)[0] {
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
        let es = lists_with(std::slice::from_ref(&w), &|_| true, &BTreeMap::new(), &|_| false, 1_000);
        assert_eq!(next_due(&es), Some(2_000));
        assert_eq!(next_due(&lists_with(std::slice::from_ref(&w), &|_| true, &BTreeMap::new(), &|_| false, 3_000)), None);
    }

    /// `wsp-096`: a group waiting on rows is follow-up and names them; one
    /// whose check reads it, or a running one, carries no `attention`.
    #[test]
    fn a_group_waiting_on_rows_is_follow_up_and_one_its_check_reads_is_neither() {
        let mut w = Worklist::new("m1", "m1");
        w.set_status(WorklistStatus::Held);
        w.set_groups(&[wsp_store::model::Group { members: vec!["a-1".into()], agent: "claude".into(), ..Default::default() }]);
        w.held_on = vec!["wsp-093".into()];
        let group = |w: &Worklist, settled: &dyn Fn(&str) -> bool| match &lists_with(std::slice::from_ref(w), settled, &BTreeMap::new(), &|_| false, 1_000)[0] {
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

    /// wsp-305: `wsp_view::common` reads off every row the record `Task::json`
    /// prints and the body `show` prints, whichever way the row carries the
    /// task (its file in `body`, or `head` and `prose`): an inbox row with no
    /// project, a derived and an authored caption, tags, refs, `after` and a
    /// parent, open and done.
    #[test]
    fn the_view_reads_back_each_tasks_record_and_body_off_its_row() {
        let s = seed("mirror-view");
        let mut t = Task::new("Inbox thing: with a clause", "inbox-001");
        t.refs = vec!["docs/a.md".into(), "src/b.rs".into()];
        t.after = vec!["wsp-1".into()];
        t.parent = Some("wsp-2".into());
        t.body = "\n## Overview\n\nprose\n".into();
        s.save_task(&t).unwrap();
        let mut done = s.task("wsp-5").unwrap();
        done.body = "## Log\n\n- 2026-11-18T10:00:00Z → done\n".into();
        s.save_task(&done).unwrap();
        let es = read_rows_etc(&s, "here");
        for files in s.tasks() {
            let row = row_of(&es, &files.id);
            assert_eq!(wsp_view::common::task_json(&row), files.json(), "{}", files.id);
            assert_eq!(wsp_view::common::prose(&row), files.body, "{}", files.id);
        }
        assert!(row_of(&es, "wsp-5").fields.contains_key(key::PROSE), "a done row carries its prose beside its head");
        assert_eq!(row_of(&es, "inbox-001").project.0, "inbox", "Row::project stays project_of");
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
            assert!(row.fields.contains_key(HEAD), "{id} head: every row carries one");
            let back = task_of(&row).expect("a task");
            assert_eq!(format!("{back:?}"), format!("{files:?}"), "{id}");
        }
        assert!(task_of(&row_of(&es, "wsp-2")).unwrap().body.contains("some prose"));
        let text = std::fs::read_to_string(s.task_path("wsp-2")).unwrap();
        assert_eq!(row_of(&es, "wsp-2").fields[BODY], text, "the file's text, verbatim");
    }

    /// wsp-695, the shape the mirror really writes: a done row `wip` reads in
    /// full (a `## Verification`) carries `body` and `head`, a plain done row
    /// `head` and `prose`. Folded into SQLite and cut by `wip_snapshot`, every
    /// done row comes back as its head alone, which `task_of` reads as the
    /// bodiless task, and an open row comes back whole, as the files read it.
    #[test]
    fn wip_snapshot_gives_every_done_row_as_the_mirror_writes_it_a_head_task_of_reads() {
        use c::{Commands, Reads};
        let s = seed("mirror-wip-cut");
        let mut verified = Task::new("verified and done", "wsp-6");
        verified.project = Some("wsp".into());
        verified.status_raw = "done".into();
        verified.body = "## Overview\n\nlong prose\n\n## Verification\n- 2026-11-20 holds · read abc123 · pane cpd-9\n  ## Verdict: holds\n".into();
        s.save_task(&verified).unwrap();
        let es = read_rows_etc(&s, "here");
        let full = row_of(&es, "wsp-6");
        assert!(full.fields.contains_key(BODY) && full.fields.contains_key(HEAD), "read in full: body and head");
        assert!(!row_of(&es, "wsp-5").fields.contains_key(BODY), "a plain done row has no body");

        let db = wsp_store_sqlite::SqliteStore::open(s.state.join("state.db"), Arc::new(wsp_server::Mirror)).unwrap();
        let rows: Vec<Entity> = es.into_iter().filter(|e| matches!(e, Entity::Row(_))).collect();
        db.apply(Command::Mirror { put: rows, gone: Vec::new() }, IdempotencyKey("t".into()), Principal::Wsp("test".into())).unwrap();
        let (cut, _) = db.wip_snapshot().unwrap();
        let wip = s.tasks_read_where(wip_reads_in_full);
        for t in s.tasks() {
            let row = &cut.rows[&RowId(t.id.clone())];
            let back = task_of(row).unwrap_or_else(|| panic!("{}: the cut left a row task_of cannot read", t.id));
            if Status::parse(&t.status_raw) == Some(Status::Done) {
                assert!(!row.fields.contains_key(BODY) && !row.fields.contains_key(key::PROSE), "{} kept its text", t.id);
                assert_eq!(format!("{back:?}"), format!("{:?}", TaskHead::of(&t).bodiless()), "{}: the head", t.id);
            } else {
                let files = wip.iter().find(|w| w.id == t.id).unwrap();
                assert_eq!(format!("{back:?}"), format!("{files:?}"), "{}: open, whole", t.id);
            }
        }
        let head = TaskHead::from_json(&serde_json::from_str(&cut.rows[&RowId("wsp-6".into())].fields[HEAD]).unwrap()).unwrap();
        assert!(head.has_section(crate::verification::SECTION), "the head still says the row has a Verification");
    }

    /// wsp-695: `wip_snapshot` over lists as the mirror writes them. A list
    /// that is not finished comes back with its `body`, so [`list_of`] reads
    /// back the very `Worklist` the files hold (wip's running list, homes and
    /// governor come from it); a finished one comes back without its text,
    /// which `list_of` reads as no list, as wip draws nothing from it.
    #[test]
    fn wip_snapshot_keeps_a_running_lists_body_so_list_of_reads_it_back_and_cuts_a_finished_ones() {
        use c::{Commands, Reads};
        let s = seed("mirror-wip-lists");
        let mut old = Worklist::new("list-old", "A finished list");
        let mut g = model::Group::default();
        g.members = vec!["wsp-5".into()];
        g.verdict = "holds".into();
        old.set_groups(&[g]);
        old.set_status(WorklistStatus::Done);
        s.save_worklist(&old).unwrap();
        let es: Vec<Entity> = read_rows_etc(&s, "here").into_iter().filter(|e| matches!(e, Entity::List(_))).collect();
        assert_eq!(es.len(), 2, "a running list and a finished one");
        assert!(es.iter().all(|e| matches!(e, Entity::List(l) if l.fields.contains_key(BODY))), "the mirror writes each with its body");

        let db = wsp_store_sqlite::SqliteStore::open(s.state.join("state.db"), Arc::new(wsp_server::Mirror)).unwrap();
        db.apply(Command::Mirror { put: es, gone: Vec::new() }, IdempotencyKey("t".into()), Principal::Wsp("test".into())).unwrap();
        let (cut, _) = db.wip_snapshot().unwrap();
        let files = s.worklists();
        let running = &cut.lists[&ListId("list-a".into())];
        assert_eq!(running.state, ListState::Running);
        let back = list_of(running).expect("a running list keeps the body list_of reads");
        let want = files.iter().find(|w| w.id == "list-a").unwrap();
        assert_eq!(format!("{back:?}"), format!("{want:?}"), "list_of reads back the running list the files hold");

        let finished = &cut.lists[&ListId("list-old".into())];
        assert_eq!((finished.state, finished.groups.len()), (ListState::Finished, 1), "a finished list keeps its state and groups");
        assert!(c::WIP_BODIES.iter().all(|k| !finished.fields.contains_key(*k)), "a finished list loses its text");
        assert!(list_of(finished).is_none(), "so list_of reads no list from it");
    }

    /// wsp-695: a list that has just finished can still have seats on it, and
    /// wip reads them off it. Its check stands until the tick ends it
    /// ([`crate::barrier::seats`] reads every list for that), and its governor
    /// stays seated until it is stood down. The cut keeps such a list whole:
    /// through `wip_snapshot`, `barrier::seats` over what [`list_of`] reads
    /// back still names the check seat with its list and group, and the
    /// governed list is still in wip's lists. Once the check has ended and the
    /// post is vacated, the mark goes and the list loses its text again.
    #[test]
    fn wip_snapshot_keeps_a_finished_list_whole_while_a_check_or_a_governor_is_still_seated_on_it() {
        use c::{Commands, Reads};
        let s = seed("mirror-wip-seated");
        let done = |id: &str, check: Option<&str>| {
            let mut w = Worklist::new(id, "A list that has just finished");
            let mut g = model::Group::default();
            g.members = vec!["wsp-5".into()];
            g.verdict = "holds".into();
            if let Some(pane) = check {
                let mut c = wsp_store::checks::Check::opened(Some("55c4839abcde".into()), None);
                c.pane = Some(pane.into());
                g.checks = vec![c];
            }
            w.set_groups(&[model::Group { members: vec!["wsp-1".into()], ..Default::default() }, g]);
            w.set_status(WorklistStatus::Done);
            s.save_worklist(&w).unwrap();
        };
        done("list-checked", Some("cpd-77"));
        done("list-governed", None);
        done("list-old", None);
        s.set_governor("list-governed", json!({ "workspace": "w-9", "pane": "cpd-78" }));
        s.set_governor("list-old", json!({ "vacated": "2026-11-20T09:00:00Z", "last": { "workspace": "w-8", "pane": "cpd-70" } }));

        let cut = |s: &Store, tag: &str| {
            let es: Vec<Entity> = read_rows_etc(s, "here").into_iter().filter(|e| matches!(e, Entity::List(_))).collect();
            let db = wsp_store_sqlite::SqliteStore::open(s.state.join(format!("{tag}.db")), Arc::new(wsp_server::Mirror)).unwrap();
            db.apply(Command::Mirror { put: es, gone: Vec::new() }, IdempotencyKey("t".into()), Principal::Wsp("test".into())).unwrap();
            db.wip_snapshot().unwrap().0
        };
        let got = cut(&s, "seated");
        let kept = |id: &str| got.lists[&ListId(id.into())].fields.contains_key(BODY);
        assert!(kept("list-checked"), "a standing check keeps its finished list whole");
        assert!(kept("list-governed"), "a filled governor's post keeps its finished list whole");
        assert!(!kept("list-old"), "a finished list nobody sits on loses its text");

        let lists: Vec<Worklist> = got.lists.values().filter_map(list_of).collect();
        let seats = crate::barrier::seats(&lists);
        assert_eq!(seats.get("cpd-77"), Some(&("list-checked".to_string(), 2)), "wip still reads the check seat's list and group");
        assert!(lists.iter().any(|l| l.id == "list-governed"), "wip still reads the governor's scope as a list");

        // The check ends and the post is vacated: nothing is seated, and the
        // next re-read cuts both lists as it cuts any finished one.
        let mut w = s.worklist("list-checked").unwrap();
        let mut gs = w.groups();
        gs[1].checks[0].ending = wsp_store::checks::Ending::Ended;
        w.set_groups(&gs);
        s.save_worklist(&w).unwrap();
        s.set_governor("list-governed", json!({ "vacated": "2026-11-20T09:30:00Z", "last": { "workspace": "w-9", "pane": "cpd-78" } }));
        let got = cut(&s, "ended");
        for id in ["list-checked", "list-governed"] {
            let l = &got.lists[&ListId(id.into())];
            assert!(c::WIP_BODIES.iter().all(|k| !l.fields.contains_key(*k)) && !l.fields.contains_key(c::WIP_SEATED), "{id} is cut once nobody sits on it");
        }
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
        seat_records(&mut seat.fields, Some(&binding), Some(&spawned), &seated, None);
        assert_eq!(binding_of(&seat), Some(binding));
        assert_eq!(spawned_of(&seat), Some(spawned));
        assert_eq!(seat.fields["task_id"], "wsp-2", "what was there stands");
        let back = seated_of(&seat);
        assert_eq!(format!("{back:?}"), format!("{seated:?}"));
        // A binding that ends goes from the fields.
        seat_records(&mut seat.fields, None, None, &Seated { seat: Seat::new("cpd-2"), state: State::Gone, ..Default::default() }, None);
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
    /// `wsp-105`. A row at review whose newest verification holds carries
    /// `attention` = `needs-you`, the text, and the Mark done action JSON on its
    /// own fields, where the census reads rows. Unverified, sent back, reverted,
    /// or still judged by a held barrier: none of it. Two or more fold into
    /// one item ("N finished rows to close", Mark all done on the first); one
    /// alone carries no group.
    #[test]
    fn a_verified_row_at_review_carries_needs_you_and_mark_done_and_others_carry_nothing() {
        use crate::verification::{record, State as V};
        let s = store("mirror-finished");
        for id in ["wsp-10", "wsp-11", "wsp-12", "wsp-13", "wsp-14", "wsp-16"] {
            let mut t = Task::new(&format!("title of {id}"), id);
            t.project = Some("wsp".into());
            t.status_raw = "review".into();
            if id == "wsp-10" {
                t.log("reverted in a661ec8");
            }
            s.save_task(&t).unwrap();
        }
        for id in ["wsp-10", "wsp-11", "wsp-12", "wsp-16"] {
            record(&s, id, V::Holds, "it holds", None, None).unwrap();
        }
        record(&s, "wsp-14", V::Blocks, "it does not hold", None, None).unwrap();
        let mut back = s.find_task("wsp-14").unwrap();
        back.status_raw = "review".into();
        s.save_task(&back).unwrap();
        let mut w = Worklist::new("held-a", "held");
        w.set_status(WorklistStatus::Held);
        w.set_groups(&[model::Group { members: vec!["wsp-16".into()], ..Default::default() }]);
        s.save_worklist(&w).unwrap();

        let read = |s: &Store| {
            let tasks = s.tasks();
            rows(&tasks, &BTreeMap::new(), &s.worklists(), &texts_of(s, &tasks), &BTreeMap::new())
        };
        let es = read(&s);
        let f = |id: &str| row_of(&es, id).fields;
        let done = |id: &str| json!({ "verb": "done", "args": [id], "sentence": "none", "buttons": [
            { "label": "Mark done", "verb": "done", "args": [id], "sentence": "none" },
        ] });
        for id in ["wsp-11", "wsp-12"] {
            let r = f(id);
            assert_eq!(r.get("attention").map(String::as_str), Some("needs-you"), "{id}: {r:?}");
            assert_eq!(r.get("attention_text").map(String::as_str), Some(crate::classify::FINISHED));
            let action: serde_json::Value = serde_json::from_str(&r["action"]).unwrap();
            assert_eq!(action, done(id));
            assert_eq!(r.get(ATTENTION_GROUP).map(String::as_str), Some("finished"));
        }
        assert_eq!(f("wsp-11").get("group_text").map(String::as_str), Some("2 finished rows to close"));
        assert!(!f("wsp-12").contains_key("group_text"), "the text rides on the first row alone");
        let all: serde_json::Value = serde_json::from_str(&f("wsp-11")["group_action"]).unwrap();
        assert_eq!(all["verb"], "group");
        assert_eq!(all["buttons"][0]["each"], json!([["wsp-11"], ["wsp-12"]]));
        assert_eq!(all["items"][1]["action"], done("wsp-12"));
        // The lowest id in the group, after the exclusions: wsp-10 is lower and
        // verified, but reverted, so it is wsp-11 that carries the group's act.
        assert!(!f("wsp-10").contains_key("group_action") && !f("wsp-12").contains_key("group_action"));
        assert_eq!(all["args"], json!(["wsp-11", "wsp-12"]));
        for (id, why) in [("wsp-13", "unverified"), ("wsp-14", "sent back"), ("wsp-10", "reverted"), ("wsp-16", "a held barrier judges it")] {
            let r = f(id);
            assert!(!r.contains_key("attention") && !r.contains_key("action") && !r.contains_key(ATTENTION_GROUP), "{id} ({why}): {r:?}");
        }

        let mut closed = s.find_task("wsp-12").unwrap();
        closed.status_raw = "done".into();
        s.save_task(&closed).unwrap();
        let alone = row_of(&read(&s), "wsp-11").fields;
        assert_eq!(alone.get("attention").map(String::as_str), Some("needs-you"));
        assert!(!alone.contains_key(ATTENTION_GROUP) && !alone.contains_key("group_action"), "one shows alone: {alone:?}");
    }

    /// `wsp-755`: a group's count moving (a third row finishing) changes the
    /// row that carries the group and the row that joined, and no other
    /// member: `group_text` and `group_action` are off every row but one.
    #[test]
    fn a_finished_row_joining_the_group_changes_only_the_carrier_and_itself() {
        use crate::verification::{record, State as V};
        let s = store("mirror-finished-churn");
        for id in ["wsp-20", "wsp-21", "wsp-22", "wsp-23"] {
            let mut t = Task::new(&format!("title of {id}"), id);
            t.project = Some("wsp".into());
            t.status_raw = "review".into();
            s.save_task(&t).unwrap();
        }
        for id in ["wsp-20", "wsp-21", "wsp-22"] {
            record(&s, id, V::Holds, "it holds", None, None).unwrap();
        }
        let read = |s: &Store| {
            let tasks = s.tasks();
            rows(&tasks, &BTreeMap::new(), &s.worklists(), &texts_of(s, &tasks), &BTreeMap::new())
        };
        let before = read(&s);
        assert_eq!(row_of(&before, "wsp-20").fields.get("group_text").map(String::as_str), Some("3 finished rows to close"));
        record(&s, "wsp-23", V::Holds, "it holds", None, None).unwrap();
        let after = read(&s);
        assert_eq!(row_of(&after, "wsp-20").fields.get("group_text").map(String::as_str), Some("4 finished rows to close"));
        let changed: Vec<String> = ["wsp-20", "wsp-21", "wsp-22", "wsp-23"]
            .iter()
            .filter(|id| row_of(&before, id).fields != row_of(&after, id).fields)
            .map(|id| id.to_string())
            .collect();
        assert_eq!(changed, vec!["wsp-20", "wsp-23"]);
    }
}
