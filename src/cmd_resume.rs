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
//!
//! # What "brought back" means on `compound` (`compound-122`)
//!
//! Everything above was written for herdr, where the event is one thing: herdr
//! restarts, restores every workspace it drew, and kills every agent that was
//! in them (`robustness-053`). One restart, one moment, one census of what to
//! offer back — which is exactly what [`resumable`] and [`ask_on_startup`]
//! are: a read of `sync`'s roster, itself nothing but a projection of the
//! store into *herdr's own sidebar tokens* (see that module's own first
//! line). Neither reaches past herdr's panes, on purpose, and neither should
//! be widened to.
//!
//! **A compound-hosted session does not have that event.** `place_compound`'s
//! own docs are the reason: `compound-sup` daemonizes with `setsid()` and
//! keeps a seat's agent running "whether or not anything is attached to it".
//! The host — wsp's own window, or herdr's, whichever spawned it — can close,
//! restart, or crash, and the seat is untouched, because it was never the
//! thing holding the agent up. So the herdr question — "what was running
//! before the thing hosting it went away" — has no compound answer, because
//! closing the host is not an event a compound seat survives *badly*; it is
//! one it does not notice.
//!
//! The event that does end a compound seat is narrower: `compound-sup`
//! itself being killed — a crash, a `kill -9`, the machine going down — which
//! takes the pty and the agent under it with it. That is the only thing on
//! compound this verb is for, and it is reached the one way this file already
//! reaches anything durable: **by name.** `wsp resume <task>` /
//! `wsp resume <project>` reads the claim or the governor record — backend-
//! agnostic, written by whichever backend ran the agent — rather than
//! herdr's roster, and [`bring_back`] has started an agent on whichever
//! backend it is given since `compound-076`. So a compound seat killed this
//! way is found and restarted exactly the way a herdr one is, through the
//! door that was never herdr's to begin with.
//!
//! **What stays out of scope, and why it is not this row's gap to close:**
//! the *batch* offer — `wsp resume` with no id, and the one [`ask_on_startup`]
//! puts up unasked — stays herdr-only. There is no single moment analogous to
//! "herdr just restarted" that a compound machine's daemon start can key off:
//! compound-sup processes are independent of wsp's own restart and of each
//! other, so there is no one roster of "what a restart just interrupted" to
//! read back, and inventing one — polling every seat's liveness on every
//! daemon start to guess what might have died since — would be answering a
//! question this backend does not ask in the shape herdr asks it. A smaller,
//! correct verb: named resume reaches compound; the unprompted offer does
//! not, and is not supposed to.

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
    /// The directory the session was running in. A resume takes an id and
    /// inherits the tree from wherever it is run — for every kind wsp knows —
    /// so this is half the answer and not a detail.
    pub cwd: String,
    /// The machine it was last seen on, as that machine calls itself.
    pub host: String,
    /// The room it was in, where that is still known. Empty is normal: a
    /// binding names a pane, and a pane's workspace is gone with it.
    pub workspace: String,
    /// Which agent — `claude`, `opencode` — because what a resume *is* differs
    /// by kind and only the kind knows how to say it: the binary to run, the
    /// flag that spells "pick this session up", and whether there is a
    /// transcript to read afterwards at all.
    ///
    /// Never empty. Every constructor puts a record's answer through
    /// [`crate::cmd_spawn::kind_or_default`], so a reader here is asking about
    /// an agent rather than about whether wsp wrote something down; the argument
    /// for which default, and for why an absence does not last, is there.
    pub kind: String,
    pub from: Source,
}

impl Thread {
    /// The line a person can run by hand, which is the whole of the recovery
    /// path when herdr itself is what is broken. Printed by `--print` and by
    /// every refusal below, because a verb that cannot do it should still say
    /// how.
    ///
    /// `None` for a kind wsp has no resume spelling for. This read
    /// `{kind} --resume {session}` until `core-031`, which is Claude Code's flag
    /// on whatever binary the row happened to name: driven against a live
    /// opencode it printed `claude --resume ses_fdaf7f65…`, wrong in both halves
    /// and offered as the thing to type when nothing else works. The spelling is
    /// [`crate::agent_commands::Kind::resume_flag`]'s, and a kind that has none
    /// gets no line rather than a plausible one.
    pub fn by_hand(&self) -> Option<String> {
        let flag = agent_commands::of(&self.kind).resume_flag()?;
        let cd = match self.cwd.is_empty() {
            true => String::new(),
            false => format!("cd {} && ", self.cwd),
        };
        Some(format!("{cd}{} {flag} {}", self.kind, self.session))
    }

    /// [`Thread::by_hand`] for printing, with the reason standing in where there
    /// is no line. A refusal that says nothing about what a person could do
    /// instead is the failure `by_hand` exists to prevent, and an empty string
    /// in the middle of a sentence is exactly that.
    fn by_hand_says(&self) -> String {
        self.by_hand()
            .unwrap_or_else(|| format!("wsp has no way to resume a `{}` by hand", self.kind))
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
        kind: cmd_spawn::kind_or_default(&text(row, "kind")),
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
        // [`Store::task_now`] and not [`Store::task`]: the id in a census row
        // was written down when the row was, and a task renumbered since is
        // not at the path it names. The raw read called that `Forgotten` — a
        // row unticked, and labelled "no longer in the store" about work that
        // is plainly still there — so `↵` on the offer quietly resumed one
        // agent fewer, with the only explanation being a sentence that was
        // false. `resume-held.json` is rewritten by a renumbering now, which
        // shuts the frozen census's window; this shuts the roster's, which
        // stays open because `resumable.json` is deliberately left out of that
        // rewrite and is what `offered` falls back to for the twenty seconds
        // before the daemon's next tick replaces it.
        //
        // One lookup either way: the file read is the same one, and `ids.json`
        // is only reached when the exact path missed.
        match store.task_now(&store.renamed_ids(), id) {
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
    // The kind comes off the same three rungs in the same order, because it is
    // half of the same answer: an id with no binary to hand it to is not a
    // thread anybody can pick up. Read separately rather than beside the session
    // because the two are learned under separate rules — a binding written
    // before `core-031` carries a session and no kind, and reading them as a
    // pair would let the empty half throw away the good one.
    let kind = [
        bound.as_ref().map(|b| text(b, "agent_kind")),
        claim.map(|c| text(c, "agent_kind")),
    ]
    .into_iter()
    .flatten()
    .find(|s| !s.is_empty());
    let (session, kind, from) = match recorded {
        Some(s) => (s, kind.unwrap_or_default(), Source::Record),
        None => {
            let d = logged(store, "id", task)?;
            (text(&d, "session"), text(&d, "kind"), Source::Log)
        }
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
        kind: cmd_spawn::kind_or_default(&kind),
        from,
    })
}

/// The worktree a task would be worked in, for a claim that is gone.
fn tree_of(store: &Store, task: &str) -> Option<String> {
    let t = store.task(task)?;
    let index = Index::new(store.projects());
    let root = index.root_for(t.project.as_deref()?, &t.refs)?;
    crate::cmd_checkout::tree_for(&root, task)
}

/// The thread the custodian of `project` was on.
pub fn thread_for_seat(store: &Store, project: &str) -> Option<Thread> {
    let governors = store.governors();
    let seat = cmd_govern::last_seat(&governors, project);
    let recorded = seat.as_ref().map(|s| s.session.clone()).filter(|s| !s.is_empty());
    let (session, from, cwd, kind) = match recorded {
        Some(s) => (
            s,
            Source::Record,
            seat.as_ref().map(|s| s.cwd.clone()).unwrap_or_default(),
            seat.as_ref().map(|s| s.kind.clone()).unwrap_or_default(),
        ),
        None => {
            let d = logged(store, "project", project)?;
            (text(&d, "session"), Source::Log, text(&d, "cwd"), text(&d, "kind"))
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
        workspace: seat.is_some().then(|| cmd_govern::room_of(&governors, project)).unwrap_or_default(),
        kind: cmd_spawn::kind_or_default(&kind),
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

/// Whether this kind's seat has an environment in it, and so may only be one
/// wsp opened itself.
///
/// The whole of the rule [`somewhere_to_stand`] applies about the kind, split
/// out because it needs no socket to be true and a test should not need one to
/// say so. `env` rather than a property of the kind, because the question is
/// *what would this resume have to deliver* — the same argument [`bring_back`]
/// passes — so a kind that one day needs configuring only sometimes answers
/// correctly without this line being revisited.
///
/// Asked with the emptiest seat there is — no brief, nothing outside the tree —
/// so the answer is about the kind and not about which row is being resumed.
fn needs_a_seat_wsp_opened(kind: &str) -> bool {
    !agent_commands::of(kind).env(None, &cmd_spawn::Reach::default()).is_empty()
}

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
///
/// **Except for a kind that needs an environment, and that is the half
/// `core-038` found by driving.** A seat wsp did not open cannot be given one:
/// `agent.start` takes `pane_id`, `kind`, `name`, `args` and a timeout and no
/// environment at all (herdr protocol 19), and the only calls that carry `env`
/// are the ones that make a new shell — `workspace.create` and `pane.split`.
/// So whatever the restored pane's shell has is what the agent gets, and
/// measured 2026-08-22 against a sandbox herdr stopped and started again, what
/// it has is the *server's* environment: the pane came back with the same id
/// and the same cwd and `OPENCODE_CONFIG_CONTENT` unset. That is precisely the
/// case this function exists for, so for opencode the tidy answer and the
/// correct one are opposites, and correct wins — a resumed agent under
/// opencode's shipped policy instead of wsp's is an agent that can run `wsp
/// done`, which is Ed's by the decision of 2026-08-19. `core-041` narrowed the
/// distance between those two policies to exactly the four denied verbs and did
/// not close it, so the reason to carry the environment is smaller than it was
/// and is the same reason.
///
/// The cost is paid by one kind and is the cost the paragraph above declines:
/// an empty room left standing beside the new one. It is not paid by `claude`,
/// whose [`agent_commands::Kind::env`] is empty, so every resume that worked
/// this way still does.
///
/// What is asked is *the environment this resume would have to deliver* —
/// `env(None)`, the same argument [`bring_back`] passes — rather than a
/// property of the kind, so a kind that one day needs configuring only
/// sometimes gets the right answer without this line being revisited.
///
/// **`WSP_PROJECT` and `WSP_TASK` are lost here too and that is survivable.**
/// They were already, before this row: the claim is the durable record of what
/// a seat is for and `wsp brief` reads it, which is why `order`'s doc calls the
/// environment exact for the life of a session rather than durable. A
/// permission policy has no such second copy, which is the whole difference.
fn somewhere_to_stand(t: &Thread) -> Option<Seat> {
    if t.workspace.is_empty() || needs_a_seat_wsp_opened(&t.kind) {
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
    // Refused before a workspace is opened, because the alternative is silent
    // and worse. A kind with no resume spelling passes `Spawn::resume` to an
    // `args` that ignores it — so the agent starts, the claim is re-taken, the
    // line says "resumed", and what is actually in the seat is a fresh session
    // with none of the transcript this whole verb exists to get back. A wrong
    // answer that looks like the right one is the failure `core-031` is about;
    // saying so costs a line.
    if agent_commands::of(&kind).resume_flag().is_none() {
        return Err(format!(
            "{} was a `{kind}`, and wsp knows no way to resume one — \
             starting it fresh would lose the session it is named after",
            t.what
        ));
    }
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
                // The kind's own configuration, through the one builder — a
                // resumed agent is the same agent doing the same work and is
                // owed the same policy. `core-038`; the argument is on
                // `cmd_spawn::seat_env`.
                //
                // **No brief, and that was driven rather than inherited.** A
                // resumed session already holds its brief, in the transcript
                // rather than only in the system context it was loaded into:
                // 2026-08-22, a sandbox opencode spawned with its brief in
                // `instructions`, killed, and resumed with no `instructions`
                // key at all, quoted a token that existed nowhere but that
                // file. So laying a second copy would be ~3,300 tokens of
                // duplicate at the top of every resume. It is also the only
                // answer that is safe: `brief_path` is named for the subject
                // and `despawn` deletes it, so a path named here without
                // writing one would point at a file that is either absent —
                // which opencode ignores in silence — or left over from an
                // earlier spawn onto the same subject, which is worse.
                env: cmd_spawn::seat_env(
                    Some(cmd_spawn::Occupant {
                        kind: &kind,
                        brief: None,
                        // The same world the spawn had. `core-042` is a
                        // permission policy like the rest of this env, and
                        // `core-038`'s finding was that resume took the `WSP_*`
                        // half and none of the runtime's — a resumed agent
                        // doing the same work under a narrower policy would be
                        // that defect written again.
                        reach: &cmd_spawn::reach(
                            store,
                            t.task.as_deref(),
                            // A custodian's project is recorded as its scope
                            // rather than as its task's project, and `reach`
                            // reads the row's own when it is given nothing — so
                            // a resumed seat reaches exactly what the spawn
                            // reached.
                            t.seat_of.as_deref(),
                            Some(&t.cwd),
                        ),
                    }),
                    t.seat_of.as_deref(),
                    t.task.as_deref(),
                    // A resumed custodian is still a custodian: `seat_of` is
                    // only ever set for one, so it is both the honest answer
                    // here and the same one the original spawn made.
                    t.seat_of.is_some(),
                ),
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
            // Same answer as `full`, and for the same reason: resume rebuilds
            // the preamble of a session that already exists, and what it
            // rebuilt yesterday is what it should rebuild today. An
            // exploration-heavy task wants `wsp spawn --subagents` at birth;
            // nothing here re-decides that on the way back in.
            subagents: false,
            name: &name,
            seat: &seat,
            model: None,
            effort: None,
            // A resumed agent is picking up a thread, not being handed a new
            // piece of work: whatever it was told is already in the session it
            // is resuming, and saying it again would restate an order it has
            // already acted on.
            order: None,
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
        let ours: Vec<&crate::place::Seated> = rows.seats().filter(|r| r.seat == seat).collect();
        crate::cmd_agent::learn_sessions(
            store,
            ours.iter().map(|r| (r.seat.as_str(), r.session.as_str(), r.agent.kind.as_str())),
        );
        if let (true, Some(ws)) = (t.seat_of.is_some(), cmd_spawn::workspace_of(&seat)) {
            cmd_govern::learn_seats(
                store,
                ours.iter().map(|r| {
                    (
                        ws.as_str(),
                        r.seat.as_str(),
                        r.session.as_str(),
                        r.cwd.as_str(),
                        r.agent.kind.as_str(),
                    )
                }),
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
            println!("  {}", r.thread.by_hand_says());
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
    // `cmd_spawn::backend(args)`, not a bare `Herdr::new()` — `compound-076`.
    // `bring_back` used to hardcode herdr because herdr was the only place an
    // agent could BE; `compound-064` made that false, and a `compound`-hosted
    // session still could not be resumed through this command at all until
    // this line asked the same question `spawn` and `stamp` already ask.
    for r in &chosen {
        let t = &r.thread;
        if !t.here() {
            // Not a failure and not attempted. The id is another machine's, the
            // tunnel is for herdr rather than for the agent's own runtime, and
            // the honest answer is the line to run over there.
            println!("{} is on {} — {}", p.bold(&t.what), t.host, t.by_hand_says());
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
        match bring_back(store, cmd_spawn::backend(args).as_ref(), t) {
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
                eprintln!("wsp: by hand — {}", t.by_hand_says());
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
/// **Never twice for the same restart, and never a word about anybody else's
/// hand.** This used to be a stronger claim than it needed to be: a flag was one
/// record per task and `set_flag` replaced it, so raising here at all risked
/// clobbering an agent's own question with a restart notice — and the guard was
/// therefore *raise nothing if anything is up*. `worklist-017` removed the
/// hazard: a hand is a message with an id of its own and there is nothing left
/// to overwrite, so the guard narrows to what it was always about, which is
/// idempotence. A daemon that starts twice puts up one notice, and an agent's
/// own question standing beside it is two hands rather than one lost.
///
/// A seat gets no hand, because a custodian holds no task and there is no row
/// to raise one on. The workspace label and the daemon's own line are what that
/// case has, and it is named in the note on `fork-016`.
fn raise_hands(store: &Store, waiting: &[Thread]) {
    for t in waiting {
        let Some(task) = &t.task else { continue };
        if ours(store, task).is_some() {
            continue;
        }
        // `wsp` and not a pane, because this is the one raised hand on the
        // machine that no agent put up: the daemon noticed a restart and said
        // so. A byline naming a pane that no longer exists would be worse than
        // none, and `Party::Human` would read as Ed having typed it.
        let mut m = crate::message::Message::new(
            crate::message::Party::Agent("wsp".into()),
            crate::message::Kind::Note,
            &crate::message::compose(
                HAND,
                "",
                &format!("`wsp resume {task}` brings it back on the thread it was on."),
            ),
        );
        m.about = crate::message::About::Task(task.clone());
        // The same record and the same event `wsp flag` writes, because the
        // seam is the same one: `~/wsp/hooks/on-message-raised` is how a
        // restart that lost four agents becomes a desktop notification rather
        // than a line in a log.
        let _ = crate::message::raise(store, &m);
    }
}

/// The restart notice this file put up on one task, if it is still up.
///
/// Recognised by its sentence, which is what [`HAND`] is for: a hand an agent
/// has since raised on the same task is left exactly where it is, and so is one
/// a person raised. Under the old record that mattered because there was only
/// one slot; under this one it still matters, because lowering somebody else's
/// hand is the act this whole area exists to make impossible.
fn ours(store: &Store, task: &str) -> Option<crate::message::Message> {
    crate::message::raised(store)
        .into_iter()
        .find(|m| m.about.task() == Some(task) && m.title() == HAND)
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
///
/// Acknowledged rather than deleted, which is what lowering a hand means under
/// the record: *I have this and I am not passing it on*. The disposition is an
/// event; the record then goes, because what is standing is what a panel draws.
fn lower_hand(store: &Store, task: &str) {
    let Some(m) = ours(store, task) else { return };
    let by = crate::message::Party::Agent("wsp".into());
    if crate::message::acknowledge(store, &m.id, &by).is_ok() {
        let _ = store.forget_message(&m.id);
    }
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
        // No occupant: what this seat is for is `wsp resume` at a shell, and
        // no agent is started in it. Configuring a runtime nobody is launching
        // would be a variable in a shell a person is about to type in.
        env: cmd_spawn::seat_env(None, None, None, false),
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
    use std::collections::BTreeMap;

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

        cmd_govern::learn_seats(
            &store,
            [("w1", "w1:p6", "c109006f", "/Users/edjames/claude", "claude")].into_iter(),
        );
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
        cmd_govern::learn_seats(
            &store,
            [("w1", "w1:p6", "c109006f", "/tmp/tree", "claude")].into_iter(),
        );
        cmd_govern::vacate(&store, "wsp");

        assert!(
            cmd_govern::slots(&store.governors()).iter().all(|s| !s.filled()),
            "the slot reads as empty to everything that draws it"
        );
        let t = thread_for_seat(&store, "wsp").expect("and still has a thread");
        assert_eq!(t.session, "c109006f");
        assert_eq!(t.by_hand().as_deref(), Some("cd /tmp/tree && claude --resume c109006f"));
    }

    /// A custodian's kind travels with its session — through the re-take that
    /// `wsp resume` itself performs, and through standing the seat down.
    ///
    /// The two are asserted together because they are the two ways the record
    /// is rewritten with the agent already gone, and either one dropping the
    /// kind leaves a thread whose id nobody can say which binary to hand to.
    #[test]
    fn a_vacated_seat_remembers_which_binary_to_resume_it_with() {
        let (_env, store) = store("resume-seat-kind");
        store.set_governor(
            "wsp",
            json!({ "workspace": "w1", "pane": "w1:p6", "host": util::hostname() }),
        );
        cmd_govern::learn_seats(
            &store,
            [("w1", "w1:p6", "ses_fdaf7f65", "/tmp/tree", "opencode")].into_iter(),
        );
        cmd_govern::take(&store, "wsp", "w1", "w1:p9");
        assert_eq!(
            thread_for_seat(&store, "wsp").map(|t| t.kind),
            Some("opencode".to_string()),
            "re-taking the room an agent is already in keeps its kind with its session"
        );

        cmd_govern::vacate(&store, "wsp");
        let t = thread_for_seat(&store, "wsp").expect("the thread outlives the occupancy");
        assert_eq!(
            t.by_hand().as_deref(),
            Some("cd /tmp/tree && opencode --session ses_fdaf7f65")
        );
    }

    /// Vacating twice must not bury the thread one level deeper each time —
    /// `reconcile --reap` runs on every daemon start.
    #[test]
    fn standing_an_empty_seat_down_again_keeps_the_same_thread() {
        let (_env, store) = store("resume-twice");
        store.set_governor("wsp", json!({ "workspace": "w1", "host": util::hostname() }));
        cmd_govern::learn_seats(
            &store,
            [("w1", "w1:p1", "sess", "/tmp/tree", "claude")].into_iter(),
        );
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
        cmd_govern::learn_seats(
            &store,
            [("w1", "w1:p1", "sess", "/tmp/tree", "claude")].into_iter(),
        );
        cmd_govern::learn_seats(
            &store,
            [("w1", "w1:p1", "", "", "")].into_iter(),
        );
        assert_eq!(thread_for_seat(&store, "wsp").map(|t| t.session), Some("sess".to_string()));

        cmd_govern::learn_seats(
            &store,
            [("w1", "w1:p1", "another", "/tmp/tree", "claude")].into_iter(),
        );
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
        cmd_govern::learn_seats(
            &store,
            [("w1", "w1:p1", "sess", "/tmp/tree", "claude")].into_iter(),
        );
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
        assert_eq!(t.by_hand().as_deref(), Some("cd /tmp/work && claude --resume abc"));
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

    /// The defect `core-031` was filed on, at the line that decides it.
    ///
    /// Driven against a live opencode in `core-026`, `wsp resume ocsand-001
    /// --print` offered `claude --resume ses_fdaf7f65…` — wrong binary and
    /// wrong flag, because `--kind` was read by `spawn` and written nowhere. The
    /// record now carries what is in the seat, and both halves of the line come
    /// off it.
    #[test]
    fn an_opencode_thread_is_resumed_as_an_opencode_and_not_as_a_claude() {
        let (_env, store) = store("resume-kind");
        store.set_binding(
            "w1:p1",
            json!({
                "task_id": "ocsand-001",
                "agent_session_id": "ses_fdaf7f65",
                "agent_kind": "opencode",
            }),
        );
        store.set_claim(
            "ocsand-001",
            json!({ "workspace_id": "w1", "cwd": "/tmp/work", "host": util::hostname() }),
        );
        let t = thread_for_task(&store, "ocsand-001").unwrap();
        assert_eq!(t.kind, "opencode");
        assert_eq!(
            t.by_hand().as_deref(),
            Some("cd /tmp/work && opencode --session ses_fdaf7f65"),
            "the binary and the flag are both the kind's, and neither is claude's"
        );
    }

    /// And it survives the binding, which is the one moment anybody asks. Both
    /// `release_pane` and `done` drop the binding before they end the claim, so
    /// `cmd_agent::ran_at` reads a claim and nothing else — and a kind that
    /// lived only on the binding would have `Claude::ran` looking for an
    /// opencode's transcript under `~/.claude/projects`.
    #[test]
    fn the_kind_outlives_the_binding_because_that_is_when_it_is_read() {
        let (_env, store) = store("resume-kind-claim");
        store.set_claim(
            "ocsand-001",
            json!({
                "workspace_id": "w1",
                "cwd": "/tmp/work",
                "agent_session_id": "ses_fdaf7f65",
                "agent_kind": "opencode",
                "host": util::hostname(),
            }),
        );
        let t = thread_for_task(&store, "ocsand-001").unwrap();
        assert_eq!((t.kind.as_str(), t.from), ("opencode", Source::Record));
    }

    /// `compound-122`: what the named door reaches that the batch offer
    /// cannot, and the reason the two differ.
    ///
    /// `resumable` is herdr's own roster, filtered by herdr's own panes — it
    /// exists because herdr restarting kills every agent in the workspaces it
    /// restores, and `sync` is the projection that feeds herdr's sidebar in
    /// the first place (see that module's own doc line one). A compound seat
    /// is never in it, and every test in this file runs against a herdr that
    /// answers nothing at all (`crate::util::isolated`), which is this
    /// machine's honest state once `compound-112` made compound the default:
    /// no herdr, so an empty roster and an empty offer, correctly.
    ///
    /// `wsp resume <task>` does not go through that roster. It reads the
    /// claim — the durable record `bring_back` and every backend already
    /// share — so a task never offered because no herdr ever held it is still
    /// found by name, and `compound-076`'s threaded backend still starts it.
    /// Nothing here asks herdr anything, which is the whole of the answer.
    #[test]
    fn a_task_named_directly_is_found_though_the_batch_offer_never_held_it() {
        let (_env, store) = store("resume-compound-task");
        task_at(&store, "cpd-001", Status::Doing);
        store.set_claim(
            "cpd-001",
            json!({
                "agent_session_id": "ses-1",
                "agent_kind": "opencode",
                "cwd": "/tmp/work",
                "host": util::hostname(),
            }),
        );
        assert!(
            resumable(&store).is_empty(),
            "herdr never held this session, so it is not on the batch offer"
        );
        let t = thread_for(&store, "cpd-001").expect("the claim finds it anyway");
        assert_eq!(t.session, "ses-1");
        assert!(t.workspace.is_empty(), "no herdr workspace was ever recorded for it");

        let place = Opens(std::cell::RefCell::new(Vec::new()));
        let seat = bring_back(&store, &place, &t).expect("it starts on whatever place it is given");
        assert_eq!(seat.as_str(), "w9:p1");
    }

    /// `core-031`'s second decision, asserted rather than described: a record
    /// written before the kind was recorded reads as `claude`.
    ///
    /// The alternative — reading the absence as *unknown* — resolves to
    /// `agent_commands::Plain`, which has no resume flag, and would take
    /// `wsp resume` away from every thread in the store on the day it landed.
    #[test]
    fn a_record_written_before_the_kind_was_recorded_still_resumes() {
        let (_env, store) = store("resume-kindless");
        store.set_binding(
            "w1:p1",
            json!({ "task_id": "render-061", "agent_session_id": "abc" }),
        );
        store.set_claim(
            "render-061",
            json!({ "workspace_id": "w1", "cwd": "/tmp/work", "host": util::hostname() }),
        );
        let t = thread_for_task(&store, "render-061").unwrap();
        assert_eq!(t.kind, cmd_spawn::DEFAULT_KIND);
        assert_eq!(t.by_hand().as_deref(), Some("cd /tmp/work && claude --resume abc"));
    }

    /// A kind wsp has no resume spelling for gets no line rather than a
    /// plausible one, and `bring_back` refuses rather than starting a fresh
    /// session under the name of the old thread.
    ///
    /// This is the failure the whole row is about, one kind further out: a
    /// `codex` row off the census used to print claude's flag, and answering
    /// the picker on it would have started a new session and reported it
    /// resumed.
    #[test]
    fn a_kind_wsp_cannot_resume_says_so_instead_of_guessing_claudes_flag() {
        let (_env, store) = store("resume-unknown-kind");
        let t = Thread {
            what: "t-1".into(),
            task: Some("t-1".into()),
            seat_of: None,
            session: "abc".into(),
            cwd: "/tmp/work".into(),
            host: util::hostname(),
            workspace: String::new(),
            kind: "codex".into(),
            from: Source::Census,
        };
        assert_eq!(t.by_hand(), None);
        assert!(t.by_hand_says().contains("codex"), "{}", t.by_hand_says());
        let e = bring_back(&store, &crate::place_herdr::Herdr::new(), &t).unwrap_err();
        assert!(e.contains("codex"), "{e}");
        assert!(
            store.claims().get("t-1").is_none(),
            "and it refused before it took a claim or opened anything"
        );
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

    /// Every hand this file raised is up, and it stands *beside* the one the
    /// agent raised for itself rather than on top of it.
    ///
    /// `worklist-017` is what changed here, and it is worth the extra
    /// assertion: under `flags.json` the second of these two would have
    /// replaced the first and this test could only ever have checked that we
    /// declined to write. Now both are up, and that is the fact to protect.
    fn hands_on(store: &Store, task: &str) -> Vec<String> {
        crate::message::raised(store)
            .into_iter()
            .filter(|m| m.about.task() == Some(task))
            .map(|m| m.title().to_string())
            .collect()
    }

    #[test]
    fn a_hand_is_raised_per_task_and_never_over_one_somebody_else_raised() {
        let (_env, store) = store("resume-hands");
        let mut theirs = crate::message::Message::new(
            crate::message::Party::pane("w1:p6", "ws"),
            crate::message::Kind::Note,
            "blocked on you",
        );
        theirs.about = crate::message::About::Task("render-019".into());
        crate::message::raise(&store, &theirs).unwrap();

        let waiting: Vec<Thread> =
            [row("render-061", "s1"), row("render-019", "s2")].iter().filter_map(of_row).collect();
        raise_hands(&store, &waiting);

        assert_eq!(hands_on(&store, "render-061"), vec![HAND.to_string()], "the row gets the hand");
        assert_eq!(
            hands_on(&store, "render-019"),
            vec!["blocked on you".to_string(), HAND.to_string()],
            "an agent's own question is untouched, and the restart notice stands beside it",
        );

        // Twice is once. A daemon that restarts must not put a second copy of
        // the same notice on a row somebody has not answered yet.
        raise_hands(&store, &waiting);
        assert_eq!(hands_on(&store, "render-061"), vec![HAND.to_string()], "raised twice");
    }

    /// Answering the question puts the hands down — and only the ones it put
    /// up. The offer is dropped in the same act, because they are one fact.
    #[test]
    fn answering_lowers_the_hands_it_raised_and_leaves_the_others() {
        let (_env, store) = store("resume-answered");
        store.set_held(vec![row("render-061", "s1"), row("render-019", "s2")]);
        let mut theirs = crate::message::Message::new(
            crate::message::Party::pane("w1:p6", "ws"),
            crate::message::Kind::Note,
            "blocked on you",
        );
        theirs.about = crate::message::About::Task("render-019".into());
        crate::message::raise(&store, &theirs).unwrap();
        let waiting: Vec<Thread> = store.held().iter().filter_map(of_row).collect();
        raise_hands(&store, &waiting);

        answered(&store);
        assert!(store.held().is_empty(), "the census goes with the answer");
        assert!(hands_on(&store, "render-061").is_empty(), "our hand comes down");
        assert_eq!(
            hands_on(&store, "render-019"),
            vec!["blocked on you".to_string()],
            "and somebody else's does not — including the one it was standing beside",
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

    /// A backend that remembers only what it was asked to open, and answers
    /// ready to everything else.
    struct Opens(std::cell::RefCell<Vec<crate::place::Order>>);

    impl Place for Opens {
        fn open(&self, order: &crate::place::Order) -> crate::place::Result<Seat> {
            self.0.borrow_mut().push(order.clone());
            Ok(Seat::new("w9:p1"))
        }
        fn start(&self, _: &Seat, _: &crate::place::Agent) -> crate::place::Result<()> {
            Ok(())
        }
        fn state(&self, _: &Seat) -> crate::place::Result<crate::place::State> {
            Ok(crate::place::State::Idle)
        }
        fn census(&self) -> crate::place::Result<crate::place::Census> {
            Ok(crate::place::Census::heard("", Vec::new()))
        }
        fn stop(&self, _: &Seat) -> crate::place::Result<()> {
            panic!("resume does not end seats")
        }
        fn tell(&self, _: &Seat, _: &str) -> crate::place::Result<crate::place::Delivery> {
            panic!("a resumed agent is told nothing — its order is in the session")
        }
        fn watch(
            &self,
            _: &mut dyn FnMut(crate::place::Event) -> bool,
        ) -> crate::place::Result<()> {
            panic!("resume does not wait for anything")
        }
        fn here(&self) -> Option<Seat> {
            panic!("resume opens a seat rather than asking which one it is in")
        }
    }

    /// `core-038`, as the check that would have caught it: the agent is the
    /// same agent doing the same work, so the seat it is put back in is
    /// configured the way the seat it was spawned into was.
    ///
    /// Driven before it was written — a sandbox opencode spawned with
    /// `core-020` d1's policy stopped on `git status` and asked; resumed, it
    /// ran the same command without asking. `core-041` allowed `git status`
    /// and the driving no longer reproduces on that command, but the property
    /// it established is the one under test and has not changed: there is no
    /// second copy of a permission policy anywhere, so a resume that drops it
    /// is an agent running under a policy nobody chose, with nothing saying so.
    ///
    /// **No brief in it**, and that half was driven too: a resumed session
    /// already holds its brief in the transcript, so naming a second copy would
    /// be duplication at best and — since `despawn` deletes the file
    /// `brief_path` names — a path at nothing at worst.
    #[test]
    fn a_resumed_agent_is_owed_the_policy_its_spawn_was_given() {
        let (_env, store) = store("resume-config");
        task_at(&store, "oc-001", Status::Doing);
        let mut t = thread("oc-001", std::path::Path::new("/tmp/tree"));
        t.kind = "opencode".into();
        // Nowhere to stand: what this asserts is the order wsp opens with, and
        // a room still standing is `somewhere_to_stand`'s question.
        t.workspace = String::new();

        let place = Opens(std::cell::RefCell::new(Vec::new()));
        bring_back(&store, &place, &t).expect("the resume was refused");
        let opened = place.0.borrow();
        let env = &opened.first().expect("no seat was opened").env;
        let cfg = env.get("OPENCODE_CONFIG_CONTENT").expect("resumed with no configuration at all");
        assert!(cfg.contains("permission"), "the brake is what the config is for: {cfg}");
        assert!(!cfg.contains("instructions"), "the session already holds its brief: {cfg}");
        assert_eq!(env.get("WSP_TASK").map(String::as_str), Some("oc-001"));
    }

    /// The half no order can fix: a pane wsp did not open cannot be given an
    /// environment, so a kind that needs one may not be stood back up in it.
    ///
    /// herdr's `agent.start` takes a pane, a kind, a name and args — no
    /// environment — and the only calls that carry one make a new shell. Driven
    /// 2026-08-22 against a sandbox herdr stopped and started again: the pane
    /// came back with the same id and cwd, and `OPENCODE_CONFIG_CONTENT` unset.
    /// That is the exact case `somewhere_to_stand` exists for, so for this one
    /// kind the tidy answer and the correct one are opposites.
    #[test]
    fn a_kind_that_needs_configuring_may_not_be_stood_back_up_where_wsp_cannot_configure_it() {
        assert!(needs_a_seat_wsp_opened("opencode"));
        assert!(!needs_a_seat_wsp_opened("claude"), "every resume that worked this way still does");
        assert!(!needs_a_seat_wsp_opened("codex"), "and a kind wsp knows nothing about is unchanged");
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

    /// The renumbering, from the offer's side: a member renamed between the
    /// census being frozen and a person answering it.
    ///
    /// Both halves are asserted, because either one alone leaves a hole. The
    /// rewrite repairs a held census written from now on and nothing already
    /// on disk; the resolving read repairs both, and is also the only thing
    /// covering the roster — which `offered` falls back to and which is
    /// deliberately *not* rewritten, because the daemon overwrites it whole
    /// every twenty seconds.
    ///
    /// What the raw read cost is the shape worth keeping in the test name:
    /// not an error, a shorter list. The row came up unticked under the words
    /// "no longer in the store", about work that was plainly still there.
    #[test]
    fn a_row_whose_task_was_renumbered_since_the_census_is_still_on_the_offer() {
        let (env, store) = store("resume-renumbered");
        let tree = env.path("tree");
        std::fs::create_dir_all(&tree).unwrap();
        task_at(&store, "t-260815-014", Status::Doing);
        store.set_held(vec![json!({
            "pane": "w1:p1", "workspace": "w1", "session": "s1",
            "cwd": tree.display().to_string(), "kind": "claude",
            "label": "t-260815-014", "task": "t-260815-014",
        })]);

        let map = BTreeMap::from([("t-260815-014".to_string(), "worklist-002".to_string())]);
        store.rename_tasks(&map).unwrap();

        assert_eq!(
            text(&store.held()[0], "task"),
            "worklist-002",
            "the frozen census is state that holds a task id, and a renumbering rewrites it",
        );

        // And the read stands on its own, for the census that was frozen
        // before this existed and for the roster, which is never rewritten.
        let mut old = thread("t-260815-014", &tree);
        old.task = Some("t-260815-014".to_string());
        assert_eq!(
            stale(&store, &old),
            None,
            "the work is there under its new name, and a shorter list is not an error message",
        );
        assert!(Row::new(&store, old).on, "so it is still ticked, as it was before the rename");
    }

    /// The half that has to be asserted through the list rather than beside
    /// it: a test keeping its own copy of a hand-kept list is the same mistake
    /// with a green tick over it.
    #[test]
    fn the_frozen_census_is_named_as_state_a_renumbering_has_to_rewrite() {
        assert!(
            Store::state_files_with_ids().contains(&"resume-held.json"),
            "a renumbering would walk past the census a restart is offering back",
        );
        assert!(
            !Store::state_files_with_ids().contains(&"resumable.json"),
            "the live roster is overwritten whole every tick and pays a read for nothing",
        );
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
