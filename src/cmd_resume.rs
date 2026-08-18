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
//! # Nothing is resumed without being asked for
//!
//! Ed, same day: *"it is not automatic: on load, ask the user."* The daemon
//! does not bring anything back. What it does at startup is put the question in
//! front of whoever is there — [`ask_on_startup`] — and the question is a list
//! with a box on each row, because "resume everything" and "resume nothing" are
//! both wrong answers most mornings: a governor is usually worth having back
//! and a task agent that was halfway through something you have since decided
//! against is not.
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

/// Whether a row of the last census is one the restart actually interrupted.
///
/// A session that is running *now* needs nothing: the agent survived, or has
/// already been brought back, and offering it again would start a second copy
/// of a live conversation. Compared on the session id rather than the pane,
/// because a pane id does not survive a herdr restart and the session is
/// precisely the thing that does.
fn still_running(live: &[String], t: &Thread) -> bool {
    live.iter().any(|s| *s == t.session)
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

/// What that census holds that is not running now — the offer, in the order it
/// was drawn.
///
/// An unreachable herdr answers nothing rather than everything: with no live
/// list there is no evidence any of these agents is gone, and offering to
/// resume a machine's worth of live sessions is the one failure this list must
/// not have.
pub fn resumable(store: &Store) -> Vec<Thread> {
    let Ok(live) = herdr::agents() else { return Vec::new() };
    let live: Vec<String> =
        live.into_iter().map(|p| p.session_id).filter(|s| !s.is_empty()).collect();
    offered(store)
        .iter()
        .filter_map(of_row)
        .filter(|t| !still_running(&live, t))
        .filter(Thread::here)
        .collect()
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

    let recorded =
        bound.as_ref().map(|b| text(b, "agent_session_id")).filter(|s| !s.is_empty());
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
/// Everything starts **on**. What is being offered is what was running a minute
/// ago, so the common answer is "yes, all of that" and the interesting one is
/// "all of it except the two I have changed my mind about" — which is one
/// keypress each from here, and eleven from an empty list.
pub struct Picker {
    pub rows: Vec<Thread>,
    pub on: Vec<bool>,
    pub sel: usize,
}

impl Picker {
    pub fn new(rows: Vec<Thread>) -> Picker {
        Picker { on: vec![true; rows.len()], rows, sel: 0 }
    }

    pub fn press(&mut self, key: Key) -> Step {
        let last = self.rows.len().saturating_sub(1);
        match key {
            Key::Up | Key::Char('k') => self.sel = self.sel.saturating_sub(1),
            Key::Down | Key::Char('j') => self.sel = (self.sel + 1).min(last),
            Key::Home | Key::Char('g') => self.sel = 0,
            Key::End | Key::Char('G') => self.sel = last,
            Key::Char(' ') | Key::Char('x') => {
                if let Some(on) = self.on.get_mut(self.sel) {
                    *on = !*on;
                }
            }
            Key::Char('a') => self.on.iter_mut().for_each(|o| *o = true),
            Key::Char('n') => self.on.iter_mut().for_each(|o| *o = false),
            Key::Enter => return Step::Take,
            Key::Esc | Key::Interrupt | Key::Char('q') => return Step::Walk,
            _ => {}
        }
        Step::Stay
    }

    /// What `↵` takes. Empty is a real answer and means the same as `esc`: the
    /// person looked and wanted none of it.
    pub fn chosen(&self) -> Vec<&Thread> {
        self.rows.iter().zip(&self.on).filter(|(_, on)| **on).map(|(t, _)| t).collect()
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
    for (i, (t, on)) in picker.rows.iter().zip(&picker.on).enumerate() {
        let cursor = match i == picker.sel {
            true => "▸",
            false => " ",
        };
        let box_ = match on {
            true => "[x]",
            false => "[ ]",
        };
        out.push_str(&format!("{cursor} {box_} {}\r\n", t.row()));
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

    if args.json() {
        let rows: Vec<Value> = threads.iter().map(as_json).collect();
        println!("{}", Value::Array(rows));
        return 0;
    }
    if args.has("print") {
        for t in &threads {
            println!("{}  {}", p.bold(&t.row()), p.dim(t.from.as_str()));
            println!("  {}", t.by_hand());
        }
        return 0;
    }

    // Named on the command line is the asking. A list nobody asked for is the
    // one that has to be confirmed, and `--yes` is for the caller that has
    // already been asked — a panel key, a script — rather than a way to skip
    // the question.
    let chosen: Vec<Thread> = match args.rest.first().is_some() || args.has("yes") {
        true => threads,
        false => {
            let mut picker = Picker::new(threads);
            match ask(&mut picker, &store.held_at()) {
                // Answered, either way: the held census is dropped, because a
                // question that goes on being asked after it has been answered
                // is one a person stops reading. What was not taken is still
                // reachable by name — `wsp resume <id>` — through the record
                // and then the log.
                Some(Step::Take) => {
                    store.clear_held();
                    picker.chosen().into_iter().cloned().collect()
                }
                Some(_) => {
                    store.clear_held();
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
                    for t in &picker.rows {
                        println!("  {}  {}", t.row(), p.dim(t.from.as_str()));
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
    for t in &chosen {
        if !t.here() {
            // Not a failure and not attempted. The id is another machine's, the
            // tunnel is for herdr rather than for the agent's own runtime, and
            // the honest answer is the line to run over there.
            println!("{} is on {} — {}", p.bold(&t.what), t.host, t.by_hand());
            continue;
        }
        match bring_back(store, &Herdr::new(), t) {
            Ok(seat) => {
                // Off the offer, exactly. See `Store::forget_held`: the agent
                // is known to be back because this line put it back, which is a
                // better answer than waiting for it to turn up in a census
                // under the same id.
                store.forget_held(&t.session);
                println!("{} resumed in {seat}", p.bold(&t.row()));
            }
            Err(e) => {
                eprintln!("wsp: {e}");
                eprintln!("wsp: by hand — {}", t.by_hand());
                failed += 1;
            }
        }
    }
    match failed {
        0 => 0,
        _ => 1,
    }
}

fn as_json(t: &Thread) -> Value {
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
    })
}

/// Put the question in front of whoever is there when herdr comes up.
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
/// Silent when the last census is empty, which is every daemon start that is
/// not following a restart.
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
    let order = Order {
        label: "resume?".to_string(),
        cwd: None,
        env: cmd_spawn::seat_env(None, None),
        on: None,
        // The one place a seat is opened in front of the person on purpose:
        // it is a question, and a question behind another window is not one.
        show: true,
    };
    let place = Herdr::new();
    match place.open(&order) {
        Ok(seat) => match herdr::call(
            "pane.send_text",
            serde_json::json!({ "pane_id": seat.as_str(), "text": "wsp resume\n" }),
        ) {
            Ok(_) => {
                eprintln!(
                    "wsp daemon: {} agent(s) were running before the restart — asking in {seat}",
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

    /// The boundary Ed drew: the offer is the last census, and an agent that is
    /// still running is not part of it.
    #[test]
    fn what_is_offered_is_the_last_census_minus_what_is_still_running() {
        let live = vec!["alive".to_string()];
        let rows: Vec<Thread> =
            [row("render-061", "gone"), row("render-019", "alive")].iter().filter_map(of_row).collect();
        let offered: Vec<&Thread> = rows.iter().filter(|t| !still_running(&live, t)).collect();
        assert_eq!(offered.len(), 1, "{offered:?}");
        assert_eq!(offered[0].what, "render-061");
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
        let mut p = Picker::new(rows);
        assert_eq!(p.chosen().len(), 3);

        assert_eq!(p.press(Key::Down), Step::Stay);
        assert_eq!(p.press(Key::Char(' ')), Step::Stay);
        let names: Vec<&str> = p.chosen().iter().map(|t| t.what.as_str()).collect();
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
        let mut p = Picker::new(rows);
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
