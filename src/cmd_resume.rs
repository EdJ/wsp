//! `wsp resume` — offer back the agents a restart interrupted, and put the
//! chosen ones on the thread they were on.
//!
//! `robustness-060` made a session *recordable*: a binding carries the id herdr
//! reports as `agent_session.value`, which is Claude Code's `sessionId`, and
//! `cmd_agent::learn_sessions` keeps it true. This is the other half — the
//! reader and the verb — plus the thing 060 could not have known it was
//! missing, and the two boundaries a person set once it existed.
//!
//! # The finding this was rewritten around
//!
//! Checked on the machine on 2026-08-18, with two governors up for a day:
//!
//! - `bindings.json` was `{}`. Both live agents held **seats**, not claims, and
//!   a binding is written per claim — so there was nothing to carry a session.
//! - `governors.json` recorded `host`, `pane`, `since`, `workspace`. No session.
//! - `events.jsonl` held 134 session records and exactly one named either pane,
//!   written before that agent took its seat and released the task it had
//!   borrowed to stand on. So one of the two was recoverable *by accident*,
//!   keyed to work it no longer held; the other was not recoverable at all.
//!
//! **A custodian is the one kind of agent that deliberately holds no task**, so
//! 060 solved this for every agent except the two that live longest. The writer
//! is now [`crate::cmd_govern::learn_seats`], beside the one for bindings and
//! following the same two rules; this file is what reads either of them.
//!
//! # What is resumable, and it is not "everything wsp has ever seen"
//!
//! Ed, 2026-08-18: *"only resume an agent if the user asks for it, or if it was
//! ACTIVE at the moment of the restart"* — and active means **it was in the
//! agents section of the panel**, which is to say the census a person was
//! actually looking at.
//!
//! So the list comes from [`Store::roster`]: one row per running agent, written
//! by `sync` on the same tick that feeds the panel and overwritten whole every
//! time. What that buys is a boundary nothing else can draw. The event log
//! holds every session ever learned — 134 of them on the machine this was
//! written on — and a startup that walked it would open dozens of workspaces
//! nobody asked for; the roster holds the twelve that were up. An agent that
//! ended before the restart is *not* in the last census, and an agent that
//! ended is not one a restart interrupted.
//!
//! **A crash is the case that decides the shape.** Nothing is written at
//! shutdown, because a machine that is going down cannot be relied on to run
//! anything: the roster is rewritten on every tick while things are *normal*,
//! so the newest one is at worst a tick old whether herdr exited cleanly, was
//! killed, or took the power with it. A file written on the way out would be
//! exactly the file missing after the failures worth recovering from.
//!
//! One id a person names is the other door, and it may reach further: see
//! [`Source`]. A list is never built that way.
//!
//! # And the snapshot is checked again before it is acted on
//!
//! The census being a snapshot is right. Treating the *offer* built from it as
//! still true is not, and `render-077` is what that cost: a picker left open
//! across a rebuild re-spawned two agents onto work that had landed, been
//! reviewed and been cleaned up. Every row is therefore re-checked at the
//! moment it is answered rather than at the moment it is drawn — [`Stale`] —
//! and a row that no longer holds is refused with its reason rather than
//! dropped. The rule generalises past this file: anything holding a roster of
//! "who was live" has the same problem, and the answer is always the second
//! lookup rather than a fresher snapshot.
//!
//! # Nothing is resumed without being asked for
//!
//! Ed, same day: *"it is not automatic: on load, ask the user."* The daemon
//! does not bring anything back. What it does at startup is put the question
//! where whoever is there will find it — [`ask_on_startup`] — and the question
//! is a list with a box on each row, because "resume everything" and "resume
//! nothing" are both wrong answers most mornings: a governor is usually worth
//! having back and a task agent that was halfway through something you have
//! since decided against is not.
//!
//! **Found, not forced.** The question opens unfocused and raises a hand on
//! each row instead of taking the screen; [`ask_on_startup`] carries what the
//! old behaviour cost, and it is not a small story.
//!
//! And it is asked about a smaller set than it used to be. Herdr 0.8.0 resumes
//! agents itself where its own integration recorded a session, so the case this
//! path was written for has shrunk to exactly the agents herdr could *not*
//! bring back. [`resumable`] is where that line is drawn, and drawing it wrong
//! is worse than the original fault: an offer to resume an agent already back
//! on screen is a stolen screen for a question with no content.
//!
//! # How far back a resume reaches, for one id a person names
//!
//! The question `060` left open, and the answer is layered because the sources
//! are not equally trustworthy.
//!
//! **The record is the truth. The log is the fallback. The record always wins.**
//!
//! - A **binding** holds the session of an agent on a task, and dies with the
//!   pane. A **seat** holds a custodian's, and outlives its occupant:
//!   [`crate::cmd_govern::vacate`] keeps the last workspace, host, session and
//!   cwd under `last`, so standing an agent down leaves the thread reachable
//!   and only a *new* occupant overwrites it.
//! - `events.jsonl` is append-only and holds every session ever learned. It is
//!   read only when no record survives, because an id in the log may already
//!   have been superseded by a `/clear` the log also recorded — and the log
//!   cannot say which of its lines is still live, only which was last.
//!
//! So the trail never goes cold, and what it loses with age is certainty rather
//! than reach. [`Source`] is on every answer and is printed: "this id was
//! current at 20:53 yesterday" and "this id is current" are different claims,
//! and only one of them is safe to act on without looking.
//!
//! # What resuming does not do
//!
//! It sends no work order. A spawned agent is told what it is for because it
//! has never existed before; a resumed one is picking up a transcript that
//! already contains its brief, its argument and whatever it had half-finished,
//! and a fresh instruction on top of that is the one thing guaranteed to be out
//! of date. The whole value of the id is that the sentence has already been
//! said.

use std::io::{Read, Write};

use serde_json::Value;

use crate::agent_commands;
use crate::cmd_govern;
use crate::cmd_spawn;
use crate::herdr;
use crate::input::{Key, Keys};
use crate::model::Status;
use crate::place::{Agent, Order, Place, Seat};
use crate::place_herdr::Herdr;
use crate::resolve::Index;
use crate::store::Store;
use crate::util::{self, Paint};
use crate::Args;

/// Where a session id came from, which is how much it is worth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The last census: this agent was running when anything last looked.
    Census,
    /// A binding or a seat: the id of the agent that is, or last was, there.
    Record,
    /// The event log, because no record survives. Current as of its line and
    /// not since.
    Log,
}

impl Source {
    fn as_str(self) -> &'static str {
        match self {
            Source::Census => "was running",
            Source::Record => "on the record",
            Source::Log => "from the log",
        }
    }
}

/// One resumable thread: a session, where it was running, and what it was on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thread {
    /// What a person calls this — a task id or a project slug.
    pub what: String,
    pub task: Option<String>,
    /// The project whose seat this is. `Some` only for a custodian: a task's
    /// project is not what is being resumed.
    pub seat_of: Option<String>,
    pub session: String,
    /// The directory the session was running in. `claude --resume` takes an id
    /// and inherits the tree from wherever it is run, so this is half the
    /// answer and not a detail.
    pub cwd: String,
    /// The machine it was last seen on, as that machine calls itself.
    pub host: String,
    /// The room it was in, where that is still known. Empty is normal: a
    /// binding names a pane, and a pane's workspace is gone with it.
    pub workspace: String,
    /// Which agent — `claude`, `codex` — because what a resume *is* differs by
    /// kind and only the kind knows how to say it.
    pub kind: String,
    pub from: Source,
}

impl Thread {
    /// The line a person can run by hand, which is the whole of the recovery
    /// path when herdr itself is what is broken. Printed by `--print` and by
    /// every refusal below, because a verb that cannot do it should still say
    /// how.
    pub fn by_hand(&self) -> String {
        let cd = match self.cwd.is_empty() {
            true => String::new(),
            false => format!("cd {} && ", self.cwd),
        };
        format!("{cd}{} --resume {}", self.kind, self.session)
    }

    /// Ours to reach, or somebody else's machine.
    fn here(&self) -> bool {
        self.host.is_empty() || self.host == util::hostname()
    }

    /// The row a person chooses from: what it was, and where.
    fn row(&self) -> String {
        let where_ = match self.cwd.rsplit('/').next() {
            Some(tail) if !tail.is_empty() => format!("  {tail}"),
            _ => String::new(),
        };
        match &self.seat_of {
            Some(p) => format!("governor · {p}{where_}"),
            None => format!("{}{where_}", self.what),
        }
    }
}

fn text(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

// ---- the list a restart offers back ---------------------------------------

/// One census row as a thread, or `None` for a row nothing can be done with.
fn of_row(row: &Value) -> Option<Thread> {
    let session = text(row, "session");
    if session.is_empty() {
        return None;
    }
    let task = Some(text(row, "task")).filter(|s| !s.is_empty());
    let seat_of = Some(text(row, "seat")).filter(|s| !s.is_empty());
    let pane = text(row, "pane");
    Some(Thread {
        // What to call it, in the order a person would: the work, then the
        // post, then the label herdr had for the pane. A row with none of the
        // three is an agent somebody started by hand, and its own label is the
        // only name it has ever had.
        what: task
            .clone()
            .or_else(|| seat_of.clone())
            .unwrap_or_else(|| match text(row, "label").is_empty() {
                true => pane.clone(),
                false => text(row, "label"),
            }),
        task,
        seat_of,
        session,
        cwd: text(row, "cwd"),
        host: herdr::host_of(&pane).unwrap_or(&util::hostname()).to_string(),
        workspace: text(row, "workspace"),
        kind: match text(row, "kind").is_empty() {
            true => cmd_spawn::DEFAULT_KIND.to_string(),
            false => text(row, "kind"),
        },
        from: Source::Census,
    })
}

/// Whether a row of the last census is one herdr is already answering for.
///
/// A session herdr holds needs nothing from wsp: the agent survived, or has
/// been brought back, or is *about to be* — and offering any of the three would
/// start a second copy of a live conversation. Compared on the session id
/// rather than the pane, because a pane id does not survive a herdr restart and
/// the session is precisely the thing that does.
fn herdr_holds(held: &[String], t: &Thread) -> bool {
    held.iter().any(|s| *s == t.session)
}

/// The census that is on offer: the one a restart interrupted if the daemon
/// held one back, and otherwise whatever is newest.
///
/// Two sources and they are the same list at different ages. The roster is
/// rewritten on every tick — including the tick after a restart, which writes
/// an empty one — so what is worth offering survives only because
/// [`ask_on_startup`] takes a copy before that happens. Reading the copy first
/// is what makes the question still answerable ten minutes later, when a person
/// has walked back to the machine.
fn offered(store: &Store) -> Vec<Value> {
    match store.held() {
        rows if !rows.is_empty() => rows,
        _ => store.roster(),
    }
}

/// What that census holds that herdr is not answering for — the offer, in the
/// order it was drawn.
///
/// **`pane.list` rather than `agent.list`, and that is the whole of the fix in
/// this file.** Herdr resumes agents itself now, and a restart is precisely the
/// moment when what herdr *will* run is not yet running: `persist::restore`
/// hangs the snapshot's `agent_session` on the restored pane and arms a resume
/// plan that fires seconds later, when the pane first gets a rect. Asked at
/// daemon start — which is where [`ask_on_startup`] asks — `agent.list` reads
/// every one of those as gone, and the offer becomes a stolen screen for a
/// question with no content, which is worse than the fault the offer was
/// written for.
///
/// `pane.list` reports the same `agent_session`, from the same terminal record,
/// and it is set on restore *before the process exists*. So it answers "what
/// herdr holds" rather than "what is running", and the two differ by exactly
/// the panes herdr is in the middle of bringing back. What survives the filter
/// is what herdr could not resume — the agents whose session herdr's own
/// integration never reported — which is the set this question is now about.
///
/// The named door is unaffected: `wsp resume <id>` never comes through here, so
/// a person who wants a second copy can still say so.
///
/// An unreachable herdr answers nothing rather than everything: with no listing
/// there is no evidence any of these agents is gone, and offering to resume a
/// machine's worth of live sessions is the one failure this list must not have.
pub fn resumable(store: &Store) -> Vec<Thread> {
    let Ok(panes) = herdr::panes() else { return Vec::new() };
    let held: Vec<String> =
        panes.into_iter().map(|p| p.session_id).filter(|s| !s.is_empty()).collect();
    offered(store)
        .iter()
        .filter_map(of_row)
        .filter(|t| !herdr_holds(&held, t))
        .filter(Thread::here)
        .collect()
}

// ---- what is no longer true -----------------------------------------------

/// Why a row that was true at the restart is not true now.
///
/// The roster is a snapshot of what was live at the restart, and that is
/// deliberate: nothing is written at shutdown, so the last census is the only
/// honest one. The **offer** built from it is a decision taken later, and
/// between the two agents finish, tasks move to `review` and worktrees are
/// removed. The longer the picker sits, the more of it is false.
///
/// `render-077`, on a picker left open across a herdr rebuild: by the time it
/// was answered the two agents it offered had landed, been reviewed and been
/// cleaned up. Answering it re-spawned both onto finished work, dragged their
/// tasks back from `review` to `doing`, and both came up stuck on Claude Code's
/// folder-trust modal because their trees had been removed underneath them. It
/// also cost a false accusation: the seat that saw it read it as another
/// governor spawning on completed tasks, and said so.
///
/// So: **re-check at answer time, not at open time**. Two lookups, no new
/// state. And a row that fails the check is *refused with the reason*, never
/// quietly dropped — a picker that silently loses rows makes exactly that kind
/// of misreading more likely rather than less.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stale {
    /// The work is finished: `review` or `done`. On its own this is a default
    /// rather than a refusal — wanting a finished agent back to ask it what it
    /// did is legitimate — so the row says so and starts unticked.
    Moved(Status),
    /// The task is not in the store at all any more.
    Forgotten,
    /// The recorded tree is not there. This one is always a refusal: a
    /// `--resume` into a directory that no longer exists raises the
    /// folder-trust modal and hangs there (`robustness-035`), which is worse
    /// than failing, because it looks from outside like an agent that started.
    Gone,
}

impl Stale {
    /// The reason, in the same words on the row and in the refusal, so that
    /// what a person read before answering is what they are told afterwards.
    fn why(self) -> String {
        match self {
            Stale::Moved(s) => format!("now at {}", s.as_str()),
            Stale::Forgotten => "no longer in the store".to_string(),
            Stale::Gone => "its tree is gone".to_string(),
        }
    }

    /// Whether this is the kind that cannot be overridden by ticking the box.
    fn fatal(self) -> bool {
        matches!(self, Stale::Gone)
    }
}

/// What is no longer true about a thread, asked now.
///
/// One task lookup and one `stat`, which is what makes this affordable at the
/// moment of acting rather than only at the moment of drawing.
///
/// Two checks rather than one, because finishing and clearing up are separate
/// events and either happens without the other: a task reaches `review` while
/// its tree stands, and `wsp checkout --rm` takes a tree away — properly, since
/// `wsp-093` — while the task is still open. The first makes a row unwanted and
/// the second makes it unrunnable.
pub fn stale(store: &Store, t: &Thread) -> Option<Stale> {
    if let Some(id) = &t.task {
        match store.task(id) {
            None => return Some(Stale::Forgotten),
            Some(task) => match task.status() {
                s @ (Status::Review | Status::Done) => return Some(Stale::Moved(s)),
                _ => {}
            },
        }
    }
    // Only for a path this machine would actually run in. Another host's tree
    // is not ours to stat, and a missing directory here would say nothing true
    // about the machine the row belongs to — which is why `here()` is the same
    // test that decides whether we would start it at all.
    match t.here() && !t.cwd.is_empty() && !std::path::Path::new(&util::expand(&t.cwd)).is_dir() {
        true => Some(Stale::Gone),
        false => None,
    }
}

/// One row of the offer: a thread, and what the check said when the row was put
/// in front of a person.
///
/// Keeping what was *shown* is the whole of the distinction the fix turns on. A
/// row ticked while it said `now at review` is a deliberate answer and is
/// honoured; a row that was clean when it was drawn and is finished by the time
/// `↵` is pressed was never answered at all, and is refused.
#[derive(Debug, Clone)]
pub struct Row {
    pub thread: Thread,
    pub stale: Option<Stale>,
    pub on: bool,
}

impl Row {
    /// Check a thread and start it ticked only if there is nothing to say
    /// against it.
    fn new(store: &Store, thread: Thread) -> Row {
        let stale = stale(store, &thread);
        Row { on: stale.is_none(), thread, stale }
    }

    /// The reason, as a tail on a printed line. Empty for a row with nothing
    /// against it, so every listing can append it unconditionally.
    fn note(&self) -> String {
        match self.stale {
            Some(s) => format!(" · {}", s.why()),
            None => String::new(),
        }
    }

    /// The door left open when a row is refused, which is never "nothing".
    fn door(&self) -> String {
        match (self.stale, &self.thread.task) {
            (Some(Stale::Gone), Some(task)) => {
                format!("`wsp checkout {task}` makes the tree, then `wsp resume {task}`")
            }
            (Some(Stale::Gone), None) => format!("{} is not there", self.thread.cwd),
            _ => format!("`wsp resume {}` brings it back anyway", self.thread.what),
        }
    }
}

// ---- one id a person names ------------------------------------------------

/// The session last learned for a task, out of the event log.
fn logged(store: &Store, key: &str, value: &str) -> Option<Value> {
    store
        .events_of("session-learned")
        .into_iter()
        .filter(|d| d.get(key).and_then(Value::as_str) == Some(value))
        .filter(|d| !text(d, "session").is_empty())
        .next_back()
}

/// The thread an agent working `task` was on.
///
/// The binding first — it is the live answer and carries the session of the
/// pane the agent is actually in — then the log. The claim is read either way,
/// because a binding's cwd is where the *pane* is and the claim's is where the
/// work is, and they differ exactly when somebody has `cd`-ed.
pub fn thread_for_task(store: &Store, task: &str) -> Option<Thread> {
    let claims = store.claims();
    let claim = claims.get(task);
    let bound = store
        .bindings()
        .into_iter()
        .find(|(_, b)| b.get("task_id").and_then(Value::as_str) == Some(task))
        .map(|(_, b)| b);

    // Three rungs, and the middle one is not redundant with the first. A
    // binding is per-seat and is cleared the moment an agent lets go — before
    // the claim is, in both `release_pane` and `done` — so between those two
    // writes the claim is the only record of what was in the seat. `cmd_agent`
    // keeps the two in step: whoever writes one writes the other.
    let recorded = [
        bound.as_ref().map(|b| text(b, "agent_session_id")),
        claim.map(|c| text(c, "agent_session_id")),
    ]
    .into_iter()
    .flatten()
    .find(|s| !s.is_empty());
    let (session, from) = match recorded {
        Some(s) => (s, Source::Record),
        None => (text(&logged(store, "id", task)?, "session"), Source::Log),
    };
    if session.is_empty() {
        return None;
    }

    // Where to stand it up again, most specific first: what the claim says the
    // work is in, then where the pane was, then the tree this task would be
    // checked out into if it were claimed now. The last is not a guess — it is
    // the same function `spawn` uses to make one.
    let cwd = [
        claim.map(|c| text(c, "cwd")).unwrap_or_default(),
        bound.as_ref().map(|b| text(b, "cwd")).unwrap_or_default(),
        tree_of(store, task).unwrap_or_default(),
    ]
    .into_iter()
    .find(|c| !c.is_empty())
    .unwrap_or_default();

    Some(Thread {
        what: task.to_string(),
        task: Some(task.to_string()),
        seat_of: None,
        session,
        cwd,
        host: claim.map(|c| text(c, "host")).unwrap_or_default(),
        workspace: claim.map(|c| text(c, "workspace_id")).unwrap_or_default(),
        kind: cmd_spawn::DEFAULT_KIND.to_string(),
        from,
    })
}

/// The worktree a task would be worked in, for a claim that is gone.
fn tree_of(store: &Store, task: &str) -> Option<String> {
    let t = store.task(task)?;
    let index = Index::new(store.projects());
    let root = index.root_of(t.project.as_deref()?)?;
    crate::cmd_checkout::tree_for(&root, task)
}

/// The thread the custodian of `project` was on.
pub fn thread_for_seat(store: &Store, project: &str) -> Option<Thread> {
    let governors = store.governors();
    let seat = cmd_govern::last_seat(&governors, project);
    let recorded = seat.as_ref().map(|s| s.session.clone()).filter(|s| !s.is_empty());
    let (session, from, cwd) = match recorded {
        Some(s) => (s, Source::Record, seat.as_ref().map(|s| s.cwd.clone()).unwrap_or_default()),
        None => {
            let d = logged(store, "project", project)?;
            (text(&d, "session"), Source::Log, text(&d, "cwd"))
        }
    };
    if session.is_empty() {
        return None;
    }
    Some(Thread {
        what: project.to_string(),
        task: None,
        seat_of: Some(project.to_string()),
        session,
        cwd,
        host: cmd_govern::host_of(&governors, project),
        workspace: seat.map(|s| s.workspace).unwrap_or_default(),
        kind: cmd_spawn::DEFAULT_KIND.to_string(),
        from,
    })
}

/// A task id, a project slug, or nothing that resolves — the same order
/// `spawn` reads its argument in, for the same reason.
///
/// A project resolves to its **seat**, not to a workspace in it. That is what
/// makes `wsp resume wsp` the sentence a person means: the thing worth bringing
/// back by the name of a project is the agent that was custodian of it.
fn thread_for(store: &Store, needle: &str) -> Result<Thread, String> {
    // The census first, and only for an exact name: what a person means by an
    // id they can see on the offered list is that row, with its kind and its
    // cwd, rather than a second reading of the same agent through the store.
    if let Some(t) = resumable(store).into_iter().find(|t| t.what == needle) {
        return Ok(t);
    }
    if let Some(t) = store.find_task(needle) {
        return thread_for_task(store, &t.id)
            .ok_or_else(|| format!("no session recorded against {} — nothing to resume", t.id));
    }
    let index = Index::new(store.projects());
    let Some(proj) = index.find(needle) else {
        return Err(format!("no task or project matching `{needle}`"));
    };
    thread_for_seat(store, &proj.id).ok_or_else(|| {
        format!("no session recorded against the {} seat — nothing to resume", proj.id)
    })
}

// ---- putting one back -----------------------------------------------------

/// Where to start the agent: back in the room it was in, or a new one.
///
/// **Back in the room wherever there is one.** herdr restores workspaces and
/// their layouts across a restart and kills every agent in them
/// (`robustness-053`), so the signature of the case this exists for is a
/// workspace that is still there with nothing running in it. Opening a second
/// workspace for that would leave a person with two rooms called
/// `governor · wsp`, one of them empty, and the seat record pointing at
/// whichever was newer.
///
/// A pane with no agent, because starting one where an agent already is fails
/// and would be the wrong thing if it did not.
fn somewhere_to_stand(t: &Thread) -> Option<Seat> {
    if t.workspace.is_empty() {
        return None;
    }
    let panes = herdr::panes().ok()?;
    panes
        .iter()
        .find(|p| p.workspace_id == t.workspace && p.agent.is_empty())
        .map(|p| Seat::new(&p.pane_id))
}

/// Start the agent, record the assignment, and say where it went.
///
/// Order matters and is the same order `spawn` uses: the assignment is written
/// *before* the agent starts, because a `SessionStart` hook runs `wsp brief`
/// and an agent whose first sight of itself is a brief about holding nothing
/// has been told something false. A resumed agent runs that hook too.
fn bring_back(store: &Store, place: &dyn Place, t: &Thread) -> Result<Seat, String> {
    let kind = t.kind.clone();
    let label = match &t.seat_of {
        Some(project) => cmd_govern::governor_of(project),
        None => store
            .task(t.task.as_deref().unwrap_or_default())
            .and_then(|task| crate::cmd_agent::task_label(&task))
            .unwrap_or_else(|| t.what.clone()),
    };
    let seat = match somewhere_to_stand(t) {
        Some(seat) => seat,
        None => {
            let order = Order {
                label,
                cwd: (!t.cwd.is_empty()).then(|| t.cwd.clone()),
                env: cmd_spawn::seat_env(t.seat_of.as_deref(), t.task.as_deref()),
                on: herdr::host_of(&t.workspace).map(|m| m.to_string()),
                show: false,
            };
            place.open(&order).map_err(|e| e.to_string())?
        }
    };

    match (&t.task, &t.seat_of) {
        (Some(task), _) => {
            // Through the one implementation of claiming, with `--force`: the
            // claim being restored is very often still standing in this
            // agent's own name, and refusing to take work back from a process
            // that no longer exists would make the verb unusable exactly when
            // it is needed.
            if cmd_spawn::cmd_agent_claim(store, task, &[("pane", seat.as_str()), ("force", "true")])
                != 0
            {
                return Err(format!("opened {seat}, but the claim on {task} was refused"));
            }
        }
        (None, Some(project)) => match cmd_spawn::workspace_of(&seat) {
            Some(ws) => {
                cmd_govern::take(store, project, &ws, seat.as_str());
            }
            None => eprintln!(
                "wsp: opened {seat} but could not record the {project} seat — \
                 run `wsp govern {project}` in it"
            ),
        },
        (None, None) => {}
    }

    let how = agent_commands::of(&kind);
    let name = t.what.clone();
    let spawn =
        // No tier: a resumed session picks up the thread it was on, and the
        // model it was started with is that session's, not this command's.
        agent_commands::Spawn {
            full: false,
            name: &name,
            seat: &seat,
            model: None,
            effort: None,
            resume: Some(&t.session),
        };
    let agent = Agent { kind: kind.clone(), name: name.clone(), args: how.args(&spawn) };
    place
        .start(&seat, &agent)
        .map_err(|e| e.to_string())
        .and_then(|()| cmd_spawn::ready(place, how, &spawn, &kind))
        .map_err(|e| {
            cmd_spawn::unreached(how, place, &spawn);
            format!("{kind} did not come back in {seat}: {e}")
        })?;

    // The id this agent is now running under, which is not necessarily the one
    // it was told to resume: a resumed transcript can be given a fresh session,
    // and a record still naming the old one would resume the wrong conversation
    // next time. Cheapest here — the backend has just been asked whether the
    // agent is ready, so it plainly has an opinion.
    if let Ok(rows) = place.census() {
        let ours: Vec<&crate::place::Seated> = rows.iter().filter(|r| r.seat == seat).collect();
        crate::cmd_agent::learn_sessions(
            store,
            ours.iter().map(|r| (r.seat.as_str(), r.session.as_str())),
        );
        if let (true, Some(ws)) = (t.seat_of.is_some(), cmd_spawn::workspace_of(&seat)) {
            cmd_govern::learn_seats(
                store,
                ours.iter().map(|r| (ws.as_str(), r.session.as_str(), r.cwd.as_str())),
            );
        }
    }
    Ok(seat)
}

// ---- the question ---------------------------------------------------------

/// What a keypress did to the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Still choosing.
    Stay,
    /// Resume what is ticked.
    Take,
    /// Resume nothing. The roster is left alone, so the same question can be
    /// asked again by hand.
    Walk,
}

/// A list with a box on each row.
///
/// The shape is the panel's tag picker (`panel::keys::Tags`) rather than a new
/// one: a list you can see, `␣` to flip a row, `↵` to apply the lot and `esc`
/// to walk away from all of it. Nothing happens until `↵`, which is what makes
/// exploring it safe and why a fumble that ends where it started costs nothing.
///
/// A row starts **on** unless [`stale`] has something to say against it. What
/// is being offered is what was running a minute ago, so the common answer is
/// "yes, all of that" and the interesting one is "all of it except the two I
/// have changed my mind about" — which is one keypress each from here, and
/// eleven from an empty list. A row whose task has since gone to `review` is
/// the same shape of exception, decided by the store instead of by the person,
/// and it is *shown* rather than removed so that the exception can be argued
/// with.
pub struct Picker {
    pub rows: Vec<Row>,
    pub sel: usize,
}

impl Picker {
    pub fn new(rows: Vec<Row>) -> Picker {
        Picker { rows, sel: 0 }
    }

    pub fn press(&mut self, key: Key) -> Step {
        let last = self.rows.len().saturating_sub(1);
        match key {
            Key::Up | Key::Char('k') => self.sel = self.sel.saturating_sub(1),
            Key::Down | Key::Char('j') => self.sel = (self.sel + 1).min(last),
            Key::Home | Key::Char('g') => self.sel = 0,
            Key::End | Key::Char('G') => self.sel = last,
            Key::Char(' ') | Key::Char('x') => {
                if let Some(r) = self.rows.get_mut(self.sel) {
                    r.on = !r.on;
                }
            }
            Key::Char('a') => self.rows.iter_mut().for_each(|r| r.on = true),
            Key::Char('n') => self.rows.iter_mut().for_each(|r| r.on = false),
            Key::Enter => return Step::Take,
            Key::Esc | Key::Interrupt | Key::Char('q') => return Step::Walk,
            _ => {}
        }
        Step::Stay
    }

    /// What `↵` takes. Empty is a real answer and means the same as `esc`: the
    /// person looked and wanted none of it.
    pub fn chosen(&self) -> Vec<&Row> {
        self.rows.iter().filter(|r| r.on).collect()
    }
}

/// Draw the list. Plain ANSI on stderr — this is a question rather than output,
/// and `wsp resume --json` on the other branch is what a script reads.
fn draw(p: &Paint, picker: &Picker, when: &str) {
    let mut out = String::from("\x1b[H\x1b[2J");
    out.push_str(&format!(
        "{}\r\n",
        p.bold(&match when.is_empty() {
            true => "these agents were running".to_string(),
            false => format!("these agents were running {} ago", util::duration_human(util::since(when))),
        })
    ));
    out.push_str(&format!(
        "{}\r\n\r\n",
        p.dim("␣ toggle · a all · n none · ↵ resume the ticked · esc none of them")
    ));
    for (i, r) in picker.rows.iter().enumerate() {
        let cursor = match i == picker.sel {
            true => "▸",
            false => " ",
        };
        let box_ = match r.on {
            true => "[x]",
            false => "[ ]",
        };
        // The reason is on the row and not in a footnote: an unticked box with
        // nothing beside it reads as a mistake somebody should correct, which
        // is the opposite of what it means here.
        let why = match r.stale {
            Some(s) => format!("  {}", p.dim(&s.why())),
            None => String::new(),
        };
        out.push_str(&format!("{cursor} {box_} {}{why}\r\n", r.thread.row()));
    }
    let _ = std::io::stderr().write_all(out.as_bytes());
    let _ = std::io::stderr().flush();
}

/// Ask, in a terminal, and give back what was ticked.
///
/// `None` where there is no terminal to ask in — a daemon, a hook, a pipe. The
/// caller must treat that as "nothing chosen" rather than as "everything",
/// which is the whole of *not automatic*.
fn ask(picker: &mut Picker, when: &str) -> Option<Step> {
    if !util::stdin_is_tty() {
        return None;
    }
    let p = Paint::new();
    crate::panel::stty(&["raw", "-echo", "min", "1", "time", "0"]);
    let mut keys = Keys::new();
    let mut pending: Vec<Key> = Vec::new();
    let mut buf = [0u8; 64];
    let step = loop {
        draw(&p, picker, when);
        let n = match std::io::stdin().read(&mut buf) {
            Ok(0) | Err(_) => break Step::Walk,
            Ok(n) => n,
        };
        for b in &buf[..n] {
            keys.feed(*b, &mut pending);
        }
        keys.idle(&mut pending);
        let mut step = Step::Stay;
        for k in pending.drain(..) {
            step = picker.press(k);
            if step != Step::Stay {
                break;
            }
        }
        if step != Step::Stay {
            break step;
        }
    };
    crate::panel::stty(&["sane"]);
    let _ = std::io::stderr().write_all(b"\x1b[H\x1b[2J");
    Some(step)
}

/// `wsp resume [<task|project>] [--print] [--yes]`
pub fn resume(store: &Store, args: &Args) -> i32 {
    let p = Paint::new();

    let threads: Vec<Thread> = match args.rest.first() {
        Some(needle) => match thread_for(store, needle) {
            Ok(t) => vec![t],
            Err(e) => {
                eprintln!("wsp: {e}");
                return 2;
            }
        },
        None => resumable(store),
    };

    if threads.is_empty() {
        if args.json() {
            println!("[]");
        } else {
            println!("{}", p.dim("nothing was running that is not running now"));
        }
        return 0;
    }

    // Checked here, which is the moment a person is looking at the list rather
    // than the moment the roster was written — see [`Stale`]. Every branch
    // below reads the same answer, because a `--json` caller and a person at a
    // picker are owed the same news.
    let rows: Vec<Row> = threads.into_iter().map(|t| Row::new(store, t)).collect();

    if args.json() {
        let rows: Vec<Value> = rows.iter().map(as_json).collect();
        println!("{}", Value::Array(rows));
        return 0;
    }
    if args.has("print") {
        for r in &rows {
            let from = format!("{}{}", r.thread.from.as_str(), r.note());
            println!("{}  {}", p.bold(&r.thread.row()), p.dim(&from));
            println!("  {}", r.thread.by_hand());
        }
        return 0;
    }

    // Named on the command line is the asking. A list nobody asked for is the
    // one that has to be confirmed, and `--yes` is for the caller that has
    // already been asked — a panel key, a script — rather than a way to skip
    // the question.
    let chosen: Vec<Row> = match args.rest.first().is_some() || args.has("yes") {
        true => rows,
        false => {
            let mut picker = Picker::new(rows);
            match ask(&mut picker, &store.held_at()) {
                // Answered, either way: the held census is dropped, because a
                // question that goes on being asked after it has been answered
                // is one a person stops reading. What was not taken is still
                // reachable by name — `wsp resume <id>` — through the record
                // and then the log.
                Some(Step::Take) => {
                    answered(store);
                    picker.chosen().into_iter().cloned().collect()
                }
                Some(_) => {
                    answered(store);
                    println!("{}", p.dim("nothing resumed"));
                    return 0;
                }
                // No terminal: say what there is and how to ask for it. Never
                // start anything — see the module docs.
                None => {
                    println!(
                        "{} — run `wsp resume` in a terminal to pick, or `wsp resume <id>`",
                        p.bold(&format!("{} agent(s) can be resumed", picker.rows.len()))
                    );
                    for r in &picker.rows {
                        let from = format!("{}{}", r.thread.from.as_str(), r.note());
                        println!("  {}  {}", r.thread.row(), p.dim(&from));
                    }
                    return 0;
                }
            }
        }
    };

    if chosen.is_empty() {
        // `↵` on a list with nothing ticked is the same answer as `esc`, and it
        // has to say so: a command that returns in silence reads as one that
        // failed to run.
        println!("{}", p.dim("nothing resumed"));
        return 0;
    }

    let mut failed = 0;
    let (mut resumed, mut refused) = (0, 0);
    for r in &chosen {
        let t = &r.thread;
        if !t.here() {
            // Not a failure and not attempted. The id is another machine's, the
            // tunnel is for herdr rather than for the agent's own runtime, and
            // the honest answer is the line to run over there.
            println!("{} is on {} — {}", p.bold(&t.what), t.host, t.by_hand());
            continue;
        }
        // The second check, and the one that matters: the tick was given to a
        // list that may be hours old. What was true when the row was drawn is
        // in `r.stale`; what is true now is asked again here, and the two
        // together are what separates a deliberate answer from an obsolete one.
        match stale(store, t) {
            // Never, however deliberately it was asked for. There is no tree to
            // resume into and the agent would sit on a trust modal looking
            // started.
            Some(s) if s.fatal() => {
                println!("{} not resumed — {}. {}", p.bold(&t.row()), s.why(), r.door());
                refused += 1;
                continue;
            }
            // Finished *since the row was drawn*, so nobody said yes to this:
            // the question that was answered is not the one now being asked.
            // This is the render-077 case exactly.
            Some(s) if r.stale.is_none() => {
                println!(
                    "{} not resumed — {} since the list was drawn. {}",
                    p.bold(&t.row()),
                    s.why(),
                    r.door()
                );
                refused += 1;
                continue;
            }
            // Ticked, or named, in full knowledge. Said again on the way past,
            // because the reason a person read a minute ago is worth having in
            // the transcript beside what it did.
            Some(s) => println!("{} — {}, resuming anyway", p.bold(&t.row()), p.dim(&s.why())),
            None => {}
        }
        match bring_back(store, &Herdr::new(), t) {
            Ok(seat) => {
                // Off the offer, exactly. See `Store::forget_held`: the agent
                // is known to be back because this line put it back, which is a
                // better answer than waiting for it to turn up in a census
                // under the same id.
                store.forget_held(&t.session);
                // And the hand with it, on the one path that does not go
                // through `answered`: `wsp resume <id>` and `--yes` never see
                // the picker, and a flag left up over an agent that is back on
                // screen is the noise this whole change is about.
                if let Some(task) = &t.task {
                    lower_hand(store, task);
                }
                resumed += 1;
                println!("{} resumed in {seat}", p.bold(&t.row()));
            }
            Err(e) => {
                eprintln!("wsp: {e}");
                eprintln!("wsp: by hand — {}", t.by_hand());
                failed += 1;
            }
        }
    }
    // A refusal is the command working, not failing — one stale row out of six
    // is the whole point. It only becomes a non-zero exit when it is *all* that
    // happened, which is the `wsp resume <id>` case: asked for one thing, did
    // none of it, and a script has to be able to tell.
    match (failed, resumed, refused) {
        (0, 0, n) if n > 0 => 1,
        (0, _, _) => 0,
        _ => 1,
    }
}

fn as_json(r: &Row) -> Value {
    let t = &r.thread;
    serde_json::json!({
        "what": t.what,
        "task": t.task,
        "seat": t.seat_of,
        "session": t.session,
        "cwd": t.cwd,
        "host": t.host,
        "kind": t.kind,
        "from": t.from.as_str(),
        "by_hand": t.by_hand(),
        // Null for a row with nothing against it, so a caller can branch on
        // presence rather than on parsing a sentence.
        "stale": r.stale.map(Stale::why),
    })
}

/// The sentence a raised hand carries, and the way one is recognised again.
///
/// Compared, not just written: [`answered`] lowers only the hands still saying
/// this, so a flag an agent has since raised on the same task by hand is left
/// exactly where it is. A one-line constant is the whole of that guarantee.
const HAND: &str = "its agent did not come back after the restart";

/// Raise a hand on every offered row that names a task.
///
/// The question is in a workspace nobody is looking at, on purpose (see
/// [`ask_on_startup`]), so something has to point at it — and the panel is
/// already drawn in every workspace and already pins flags at its foot. A row
/// per agent rather than one notice, because that is what a person acts on: the
/// task whose agent is missing, with the door on it.
///
/// **Never over a hand somebody else raised.** A flag is one record per task and
/// `set_flag` replaces it; clobbering an agent's own question with a restart
/// notice would lose the more important of the two. The row is still in the
/// picker either way.
///
/// A seat gets no hand, because a custodian holds no task and there is no row
/// to raise one on. The workspace label and the daemon's own line are what that
/// case has, and it is named in the note on `fork-016`.
fn raise_hands(store: &Store, waiting: &[Thread]) {
    let already = store.flags();
    for t in waiting {
        let Some(task) = &t.task else { continue };
        if already.contains_key(task) {
            continue;
        }
        store.set_flag(
            task,
            serde_json::json!({
                "said": HAND,
                "title": "",
                "body": format!("`wsp resume {task}` brings it back on the thread it was on."),
                "ask": "",
                "pane": "",
                "workspace": "",
                "at": util::now_iso(),
            }),
        );
        // The same event `wsp flag` writes, because the seam is the same one:
        // `~/wsp/hooks/on-task-flagged` is how a restart that lost four agents
        // becomes a desktop notification rather than a line in a log.
        store.log_event(
            "task-flagged",
            serde_json::json!({ "id": task, "said": HAND, "pane": "" }),
        );
    }
}

/// The offer has been answered: drop the census and lower the hands it raised.
///
/// Both together, because they are one fact. A hand still up over a list that
/// has been read is a hand that teaches a person to ignore hands.
///
/// Answered covers `esc` as well as `↵`: a person who looked and said no has
/// looked. A row that then refused to resume printed its reason and its door on
/// the way past, which is where that news belongs — not on a flag nobody asked
/// to keep.
fn answered(store: &Store) {
    for r in store.held() {
        let task = text(&r, "task");
        if !task.is_empty() {
            lower_hand(store, &task);
        }
    }
    store.clear_held();
}

/// Take one hand down, if it is still the one this file put up.
fn lower_hand(store: &Store, task: &str) {
    if store.flags().get(task).map(|f| text(f, "said")) != Some(HAND.to_string()) {
        return;
    }
    store.clear_flag(task);
    store.log_event("task-unflagged", serde_json::json!({ "id": task, "said": HAND }));
}

/// Put the question where whoever is there will find it when herdr comes up.
///
/// Called once from the daemon's start, beside the `reconcile` that rebuilds
/// bindings from claims and for the same reason: herdr has just restored its
/// workspaces and killed every agent that was in them, and this is the one
/// moment both facts are true.
///
/// **It starts no agent.** What it opens is one terminal running `wsp resume`,
/// which is the picker above — so the answer comes from a person looking at a
/// list of what they had, and the failure mode of this whole feature is a
/// window nobody wanted rather than a machine full of agents nobody asked for.
///
/// # It does not take the screen, and this is the reason
///
/// It used to, deliberately, and the doc line said why: *"a question behind
/// another window is not one."* That argument was written when wsp had nowhere
/// else to ask. What it cost, on 2026-08-18 and traced on `fork-003`: the new
/// workspace took the screen **and the keyboard**, the two characters Ed had
/// already typed (`nc`) landed in its shell, and the `pane.send_text` below
/// appended `wsp resume\n` to the same line. The pane held `ncwsp resume` and
/// `zsh: command not found: ncwsp`, and it cost this project a false accusation
/// against a governor before the call site was found.
///
/// So: **`show: false`, and `exec` rather than typing at a prompt.** Unfocused
/// is what stops another window's keystrokes arriving here at all; `exec` is
/// what stops a prompt that already has characters in it from fusing with this
/// one, and it makes the command *be* the pane — answering the question closes
/// it, with no shell left behind to close twice. `panel/verbs.rs` opens its
/// full-tree tab exactly this way.
///
/// And the question is still a question, because two things point at it: the
/// workspace is labelled with the count, which herdr draws in its own chrome,
/// and [`raise_hands`] puts a flag on each task, which the panel pins at its
/// foot in every workspace. A hand raised on the row is a better question than
/// a window in front of the face, and it does not land two keystrokes in a
/// shell.
///
/// Silent when the last census is empty, which is every daemon start that is
/// not following a restart — and, since `resumable` started reading `pane.list`,
/// silent on every restart herdr resumed everything from.
pub fn ask_on_startup(store: &Store) -> usize {
    let waiting = resumable(store);
    if waiting.is_empty() {
        return 0;
    }
    // Held before anything else, because the first `sync` after this overwrites
    // the roster with what is running *now* — nothing — and the offer would
    // have a life of one tick. Written even if the terminal below cannot be
    // opened: the question then waits for `wsp resume` typed by hand, which is
    // the failure mode this whole path is allowed to have.
    if store.held().is_empty() {
        store.set_held(store.roster());
    }
    // Before the terminal, and unconditionally: the hand is the half of this
    // that works when no window can be opened at all.
    raise_hands(store, &waiting);
    let order = Order {
        label: format!("resume {}?", waiting.len()),
        cwd: None,
        env: cmd_spawn::seat_env(None, None),
        on: None,
        show: false,
    };
    let place = Herdr::new();
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "wsp".into());
    match place.open(&order) {
        Ok(seat) => match herdr::call(
            "pane.send_text",
            serde_json::json!({
                "pane_id": seat.as_str(),
                "text": format!("exec {} resume\n", util::shell_quote(&exe)),
            }),
        ) {
            Ok(_) => {
                eprintln!(
                    "wsp daemon: {} agent(s) did not come back — waiting in {seat}, and flagged",
                    waiting.len()
                );
                waiting.len()
            }
            Err(e) => {
                eprintln!("wsp daemon: opened {seat} to ask about {} resumable agent(s), but could not type in it: {e}", waiting.len());
                0
            }
        },
        Err(e) => {
            eprintln!(
                "wsp daemon: {} agent(s) can be resumed, but no terminal could be opened to ask: {e}",
                waiting.len()
            );
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn store(tag: &str) -> (crate::util::Isolated, Store) {
        let env = crate::util::isolated(tag);
        let store = Store::open();
        store.ensure_dirs().unwrap();
        (env, store)
    }

    /// A row as the picker would hold it with nothing said against it — what
    /// every offer looked like before `render-077`.
    fn clean(rows: Vec<Thread>) -> Vec<Row> {
        rows.into_iter().map(|t| Row { thread: t, stale: None, on: true }).collect()
    }

    fn row(what: &str, session: &str) -> Value {
        json!({
            "pane": "w1:p1", "workspace": "w1", "session": session,
            "cwd": "/tmp/tree", "kind": "claude", "label": what, "task": what, "seat": null,
        })
    }

    /// The finding `render-061` was refiled on, as a test: the two agents that
    /// live longest hold no claim, so nothing keyed on one can answer for them.
    #[test]
    fn a_seat_carries_its_session_where_a_binding_would_have_none() {
        let (_env, store) = store("resume-seat");
        store.set_governor(
            "wsp",
            json!({ "workspace": "w1", "pane": "w1:p6", "host": util::hostname(), "since": util::now_iso() }),
        );
        assert!(
            thread_for_seat(&store, "wsp").is_none(),
            "a seat nothing has been learned about has no thread to resume"
        );

        cmd_govern::learn_seats(&store, [("w1", "c109006f", "/Users/edjames/claude")].into_iter());
        let t = thread_for_seat(&store, "wsp").expect("the seat now has a session");
        assert_eq!((t.session.as_str(), t.cwd.as_str()), ("c109006f", "/Users/edjames/claude"));
        assert_eq!(t.from, Source::Record);
        assert!(store.bindings().is_empty(), "and it did it without a claim anywhere");
    }

    /// Standing a custodian down empties the position and keeps the way back to
    /// its last thread. The whole of "it outlives its occupant", read from the
    /// resume side.
    #[test]
    fn a_vacated_seat_can_still_be_resumed_by_hand() {
        let (_env, store) = store("resume-vacated");
        store.set_governor(
            "wsp",
            json!({ "workspace": "w1", "pane": "w1:p6", "host": util::hostname(), "since": util::now_iso() }),
        );
        cmd_govern::learn_seats(&store, [("w1", "c109006f", "/tmp/tree")].into_iter());
        cmd_govern::vacate(&store, "wsp");

        assert!(
            cmd_govern::slots(&store.governors()).iter().all(|s| !s.filled()),
            "the slot reads as empty to everything that draws it"
        );
        let t = thread_for_seat(&store, "wsp").expect("and still has a thread");
        assert_eq!(t.session, "c109006f");
        assert_eq!(t.by_hand(), "cd /tmp/tree && claude --resume c109006f");
    }

    /// Vacating twice must not bury the thread one level deeper each time —
    /// `reconcile --reap` runs on every daemon start.
    #[test]
    fn standing_an_empty_seat_down_again_keeps_the_same_thread() {
        let (_env, store) = store("resume-twice");
        store.set_governor("wsp", json!({ "workspace": "w1", "host": util::hostname() }));
        cmd_govern::learn_seats(&store, [("w1", "sess", "/tmp/tree")].into_iter());
        cmd_govern::vacate(&store, "wsp");
        store.set_governor(
            "wsp",
            json!({ "workspace": "w2", "host": util::hostname(), "since": util::now_iso() }),
        );
        cmd_govern::vacate(&store, "wsp");
        assert_eq!(
            thread_for_seat(&store, "wsp").map(|t| t.session),
            Some("sess".to_string()),
            "the last session learned is the one kept, through two vacancies"
        );
    }

    /// The record wins over the log, and the log answers when the record is
    /// gone. Both halves of the decision this file had to make.
    #[test]
    fn the_log_is_read_only_when_no_record_survives() {
        let (_env, store) = store("resume-log");
        store.log_event(
            "session-learned",
            json!({ "project": "wsp", "session": "old", "cwd": "/tmp/a" }),
        );
        store.set_governor(
            "wsp",
            json!({ "workspace": "w1", "host": util::hostname(), "session": "new", "cwd": "/tmp/b" }),
        );
        let t = thread_for_seat(&store, "wsp").unwrap();
        assert_eq!((t.session.as_str(), t.from), ("new", Source::Record));

        store.clear_governor("wsp");
        let t = thread_for_seat(&store, "wsp").unwrap();
        assert_eq!(
            (t.session.as_str(), t.from, t.cwd.as_str()),
            ("old", Source::Log, "/tmp/a"),
            "with nothing recorded, the log is what is left — and says so"
        );
    }

    /// A silent backend must not erase a session, and the seat writer obeys the
    /// same rule as the binding one. Stated here because the two are separate
    /// functions and a change to either can only be caught by asserting it of
    /// both.
    #[test]
    fn silence_from_the_backend_leaves_a_seats_session_alone() {
        let (_env, store) = store("resume-silence");
        store.set_governor("wsp", json!({ "workspace": "w1", "host": util::hostname() }));
        cmd_govern::learn_seats(&store, [("w1", "sess", "/tmp/tree")].into_iter());
        cmd_govern::learn_seats(&store, [("w1", "", "")].into_iter());
        assert_eq!(thread_for_seat(&store, "wsp").map(|t| t.session), Some("sess".to_string()));

        cmd_govern::learn_seats(&store, [("w1", "another", "/tmp/tree")].into_iter());
        assert_eq!(
            thread_for_seat(&store, "wsp").map(|t| t.session),
            Some("another".to_string()),
            "a different session is a correction and is taken"
        );
    }

    /// An agent re-taking the seat it already holds keeps its thread. The
    /// window this closes is small and fatal: `wsp resume` itself calls `take`,
    /// and a herdr restart inside it would lose the seat for good.
    #[test]
    fn re_taking_the_same_seat_does_not_erase_its_session() {
        let (_env, store) = store("resume-retake");
        store.set_governor("wsp", json!({ "workspace": "w1", "host": util::hostname() }));
        cmd_govern::learn_seats(&store, [("w1", "sess", "/tmp/tree")].into_iter());
        cmd_govern::take(&store, "wsp", "w1", "w1:p9");
        assert_eq!(thread_for_seat(&store, "wsp").map(|t| t.session), Some("sess".to_string()));

        cmd_govern::take(&store, "wsp", "w2", "w2:p1");
        assert_eq!(
            cmd_govern::last_seat(&store.governors(), "wsp").map(|s| s.session),
            Some(String::new()),
            "a different workspace is a different occupant and starts with nothing"
        );
        assert_eq!(
            thread_for_seat(&store, "wsp").map(|t| t.from),
            Some(Source::Log),
            "and what the log remembers of the last one is marked as exactly that"
        );
    }

    /// A task's thread comes off its binding, and the claim says where to stand
    /// it up. The pane's own cwd is not the work's.
    #[test]
    fn a_tasks_thread_is_its_binding_and_its_claim() {
        let (_env, store) = store("resume-task");
        store.set_binding(
            "w1:p1",
            json!({ "task_id": "render-061", "agent_session_id": "abc", "cwd": "/tmp/pane" }),
        );
        store.set_claim(
            "render-061",
            json!({ "workspace_id": "w1", "cwd": "/tmp/work", "host": util::hostname() }),
        );
        let t = thread_for_task(&store, "render-061").unwrap();
        assert_eq!(
            (t.session.as_str(), t.cwd.as_str(), t.from),
            ("abc", "/tmp/work", Source::Record)
        );
        assert_eq!(t.by_hand(), "cd /tmp/work && claude --resume abc");
    }

    /// And it comes off the *claim* when the binding is gone, which is the state
    /// every release passes through: `release_pane` and `cmd_task::done` both
    /// drop the binding before they end the claim, so at the one moment anybody
    /// asks what had been running — the end of the attempt — the binding is
    /// already gone.
    ///
    /// The event log is not the answer to this. A claim made in a pane that
    /// already holds an agent reads the session straight off the pane row, so
    /// `learn_sessions` sees nothing change and writes no `session-learned`
    /// event at all — measured in a `--fake` sandbox on 2026-08-18, and it is
    /// why `wsp-060`'s record of what ran came back empty until the claim
    /// carried the session too.
    #[test]
    fn a_thread_survives_the_binding_being_cleared_before_the_claim_is() {
        let (_env, store) = store("resume-claim");
        store.set_claim(
            "render-061",
            json!({
                "workspace_id": "w1",
                "cwd": "/tmp/work",
                "agent_session_id": "abc",
                "host": util::hostname(),
            }),
        );
        let t = thread_for_task(&store, "render-061").expect("the claim is a record too");
        assert_eq!((t.session.as_str(), t.from), ("abc", Source::Record));
    }

    /// The boundary Ed drew: the offer is the last census, and an agent herdr
    /// is answering for is not part of it.
    ///
    /// The middle row is the one this was widened for. `restored` is a session
    /// hanging on a pane herdr has brought back but not yet started — it is in
    /// `pane.list` and not in `agent.list`, and offering it would put a second
    /// copy of a live conversation on the screen a few seconds before the first
    /// one arrived.
    #[test]
    fn what_is_offered_is_the_last_census_minus_what_herdr_holds() {
        let held = vec!["alive".to_string(), "restored".to_string()];
        let rows: Vec<Thread> = [
            row("render-061", "gone"),
            row("render-019", "alive"),
            row("render-020", "restored"),
        ]
        .iter()
        .filter_map(of_row)
        .collect();
        let offered: Vec<&Thread> = rows.iter().filter(|t| !herdr_holds(&held, t)).collect();
        assert_eq!(offered.len(), 1, "{offered:?}");
        assert_eq!(offered[0].what, "render-061");
    }

    /// The hand, and the one thing it must never do.
    #[test]
    fn a_hand_is_raised_per_task_and_never_over_one_somebody_else_raised() {
        let (_env, store) = store("resume-hands");
        store.set_flag("render-019", json!({ "said": "blocked on you", "ask": "claim" }));

        let waiting: Vec<Thread> =
            [row("render-061", "s1"), row("render-019", "s2")].iter().filter_map(of_row).collect();
        raise_hands(&store, &waiting);

        let flags = store.flags();
        assert_eq!(
            flags.get("render-061").map(|f| text(f, "said")),
            Some(HAND.to_string()),
            "the row with nothing on it gets the hand"
        );
        assert_eq!(
            flags.get("render-019").map(|f| text(f, "said")),
            Some("blocked on you".to_string()),
            "and an agent's own question is left exactly where it was"
        );
    }

    /// Answering the question puts the hands down — and only the ones it put
    /// up. The offer is dropped in the same act, because they are one fact.
    #[test]
    fn answering_lowers_the_hands_it_raised_and_leaves_the_others() {
        let (_env, store) = store("resume-answered");
        store.set_held(vec![row("render-061", "s1"), row("render-019", "s2")]);
        store.set_flag("render-019", json!({ "said": "blocked on you" }));
        let waiting: Vec<Thread> = store.held().iter().filter_map(of_row).collect();
        raise_hands(&store, &waiting);

        answered(&store);
        assert!(store.held().is_empty(), "the census goes with the answer");
        assert!(store.flags().get("render-061").is_none(), "our hand comes down");
        assert_eq!(
            store.flags().get("render-019").map(|f| text(f, "said")),
            Some("blocked on you".to_string()),
            "and somebody else's does not"
        );
    }

    /// The copy that outlives the tick, and the two ways a row leaves it. The
    /// ordering this protects is the daemon's: `sync` empties the roster one
    /// second after herdr comes up, so an offer read from the roster alone
    /// would be gone before anybody looked at it.
    #[test]
    fn the_held_census_survives_the_roster_being_overwritten() {
        let (_env, store) = store("resume-held");
        store.set_roster(vec![row("a", "s1"), row("b", "s2")]);
        store.set_held(store.roster());

        store.set_roster(Vec::new());
        assert!(store.roster().is_empty(), "the tick after a restart writes an empty census");
        assert_eq!(store.held().len(), 2, "and the copy is what is still on offer");

        store.forget_held("s1");
        assert_eq!(store.held().len(), 1, "a row that has been resumed comes off");
        store.clear_held();
        assert!(store.held().is_empty(), "and answering the question drops the rest");
    }

    /// A thread, as the census gives one, standing in a directory that exists.
    fn thread(task: &str, cwd: &std::path::Path) -> Thread {
        let mut t = of_row(&row(task, "s1")).unwrap();
        t.cwd = cwd.display().to_string();
        t
    }

    fn task_at(store: &Store, id: &str, status: Status) {
        let mut t = crate::model::Task::new("a task", id);
        t.set_status(status);
        store.save_task(&t).unwrap();
    }

    /// The `render-077` incident, as the check that would have caught it. The
    /// roster said these two were running because they were, at the restart;
    /// by the time anybody answered they had landed and gone to review.
    #[test]
    fn a_row_whose_task_has_gone_to_review_is_said_so_and_starts_unticked() {
        let (env, store) = store("resume-stale-review");
        let tree = env.path("tree");
        std::fs::create_dir_all(&tree).unwrap();
        task_at(&store, "wsp-079", Status::Doing);
        task_at(&store, "fork-009", Status::Review);

        let live = Row::new(&store, thread("wsp-079", &tree));
        assert_eq!(live.stale, None);
        assert!(live.on, "work still in progress is the default yes it has always been");

        let done = Row::new(&store, thread("fork-009", &tree));
        assert_eq!(done.stale, Some(Stale::Moved(Status::Review)));
        assert_eq!(done.stale.unwrap().why(), "now at review");
        assert!(!done.on, "finished work is not resumed by default");
        assert!(
            done.door().contains("wsp resume fork-009"),
            "and the way to ask for it anyway is on the row: {}",
            done.door()
        );
    }

    /// The second half of the same incident: both agents came up on Claude
    /// Code's folder-trust modal, because the trees they were told to resume
    /// into had been removed underneath them.
    #[test]
    fn a_row_whose_tree_has_been_removed_is_never_resumed() {
        let (env, store) = store("resume-stale-tree");
        task_at(&store, "wsp-079", Status::Doing);
        let t = thread("wsp-079", &env.path("tree-that-was-removed"));
        let r = Row::new(&store, t);
        assert_eq!(r.stale, Some(Stale::Gone));
        assert!(!r.on);
        assert!(
            r.stale.unwrap().fatal(),
            "a directory that is not there cannot be resumed into however hard it is asked for"
        );
        assert!(r.door().contains("wsp checkout wsp-079"), "{}", r.door());
    }

    /// A task deleted out from under a row is stale for the same reason a
    /// finished one is, and must not read as `doing`.
    #[test]
    fn a_row_whose_task_is_gone_from_the_store_is_stale() {
        let (env, store) = store("resume-stale-forgotten");
        let tree = env.path("tree");
        std::fs::create_dir_all(&tree).unwrap();
        let r = Row::new(&store, thread("render-999", &tree));
        assert_eq!(r.stale, Some(Stale::Forgotten));
        assert!(!r.on);
    }

    /// Another machine's path is not ours to stat. Without this every row from
    /// a second host reads as a removed tree, which is the false refusal that
    /// mirrors the false resume.
    #[test]
    fn a_thread_on_another_host_is_not_judged_by_this_machines_filesystem() {
        let (env, store) = store("resume-stale-host");
        task_at(&store, "wsp-079", Status::Doing);
        let mut t = thread("wsp-079", &env.path("not-here"));
        t.host = "some-other-mac".to_string();
        assert_eq!(stale(&store, &t), None);
    }

    /// The distinction the fix turns on, stated as the two answers it
    /// separates. Both rows are stale by the time they are acted on; only one
    /// of them was stale when the person looked, and only that one was
    /// answered.
    #[test]
    fn a_row_ticked_knowing_it_was_finished_is_a_different_answer_from_one_that_finished_since() {
        let (env, store) = store("resume-stale-since");
        let tree = env.path("tree");
        std::fs::create_dir_all(&tree).unwrap();
        task_at(&store, "wsp-079", Status::Doing);

        // Drawn while the work was still in flight, and ticked as such.
        let drawn = Row::new(&store, thread("wsp-079", &tree));
        assert_eq!(drawn.stale, None);
        // …and landed while the list sat open.
        task_at(&store, "wsp-079", Status::Review);
        let now = stale(&store, &drawn.thread);
        assert_eq!(now, Some(Stale::Moved(Status::Review)));
        assert!(
            now.is_some() && drawn.stale.is_none(),
            "nobody said yes to this: the question answered is not the one now being asked"
        );

        // Whereas a row that already said `now at review` and was ticked anyway
        // is a person asking for a finished agent back, which is allowed.
        let deliberate = Row::new(&store, thread("wsp-079", &tree));
        assert_eq!(deliberate.stale, now, "shown and now are the same, so nothing has changed");
        assert!(!deliberate.stale.unwrap().fatal());
    }

    /// A census row with no session cannot be offered: the row would fail the
    /// moment it was taken.
    #[test]
    fn a_row_with_no_session_is_not_an_offer() {
        assert!(of_row(&row("render-061", "")).is_none());
        assert!(of_row(&row("render-061", "s")).is_some());
    }

    /// Everything starts ticked, `␣` flips one, and `esc` is a real answer
    /// that takes nothing.
    #[test]
    fn the_list_opens_with_everything_ticked_and_esc_takes_none_of_it() {
        let rows: Vec<Thread> =
            [row("a", "s1"), row("b", "s2"), row("c", "s3")].iter().filter_map(of_row).collect();
        let mut p = Picker::new(clean(rows));
        assert_eq!(p.chosen().len(), 3);

        assert_eq!(p.press(Key::Down), Step::Stay);
        assert_eq!(p.press(Key::Char(' ')), Step::Stay);
        let names: Vec<&str> = p.chosen().iter().map(|r| r.thread.what.as_str()).collect();
        assert_eq!(names, ["a", "c"], "the row under the cursor came off and nothing else moved");

        assert_eq!(p.press(Key::Esc), Step::Walk);
        assert_eq!(p.press(Key::Enter), Step::Take);
        assert_eq!(p.chosen().len(), 2, "walking away does not change what was ticked");
    }

    /// `n` then `↵` is how a person says "none of these" without pressing space
    /// eleven times, and it must not be read as "all of them".
    #[test]
    fn none_then_enter_resumes_nothing() {
        let rows: Vec<Thread> = [row("a", "s1"), row("b", "s2")].iter().filter_map(of_row).collect();
        let mut p = Picker::new(clean(rows));
        p.press(Key::Char('n'));
        assert_eq!(p.press(Key::Enter), Step::Take);
        assert!(p.chosen().is_empty());

        p.press(Key::Char('a'));
        assert_eq!(p.chosen().len(), 2);
    }

    /// The cursor cannot leave the list, including when the list is empty —
    /// which is the state every daemon start that is not after a restart is in.
    #[test]
    fn the_cursor_stays_inside_the_list() {
        let mut p = Picker::new(Vec::new());
        assert_eq!(p.press(Key::Down), Step::Stay);
        assert_eq!(p.press(Key::Up), Step::Stay);
        assert_eq!(p.sel, 0);
        assert!(p.chosen().is_empty());
    }
}
