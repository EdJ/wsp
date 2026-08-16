//! What the letters do.
//!
//! The change half of the panel: [`Ask`] and [`Pick`] are the questions a verb
//! stops to ask, and the rest is what happens when it has its answer — a `wsp`
//! subcommand run against the row under the cursor, a workspace opened, an
//! editor popped out into a tab of its own.
//!
//! A verb is added here and nowhere else: its key, its prompt, its entry in
//! the map, and the command it becomes.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::herdr;
use crate::input::Key;
use crate::model::Priority;
use crate::store::Store;

use super::install::list_panes;
use crate::util::shell_quote;
use super::keys::{move_or_fold, say, Effect, Mode, Tags, View};
use super::rows::{hotkeys, AgentRef, Target, Ui};
use super::{BOARD_LABEL, PANEL_LABEL, VIEW_LABEL};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Ask {
    AddTask { project: Option<String>, parent: Option<String> },
    NewProject { parent: Option<String> },
    Block { task: String },
    /// `from` is the title the prompt opened with. Carried so pressing `↵` on
    /// an untouched line can be nothing rather than a rename to the same
    /// words — which is a log entry, an event and a commit saying nothing.
    Rename { task: String, from: String },
    /// The same gesture on a project, and a different field: a project's
    /// *name*, not its id. The id is the slug — what the tree draws, what
    /// `-p` takes, what every task and pin and mandate refers to it by — and
    /// nothing in wsp can change one. So `e` here changes the other string,
    /// which is what `wsp project show` and the detail pane lead with.
    RenameProject { project: String, from: String },
    Note { task: String },
}

impl Ask {
    pub(super) fn label(&self) -> String {
        match self {
            Ask::AddTask { parent: Some(_), .. } => "sub-task".into(),
            Ask::AddTask { .. } => "task".into(),
            Ask::NewProject { .. } => "project".into(),
            Ask::Block { .. } => "why".into(),
            Ask::Rename { .. } => "title".into(),
            Ask::RenameProject { .. } => "name".into(),
            Ask::Note { .. } => "note".into(),
        }
    }

    /// The value the prompt opened holding, for the verbs that open holding
    /// one. `↵` on it untouched is not a change, and running it anyway spends
    /// a log line, an event and a commit on a keystroke.
    pub(super) fn opened_with(&self) -> Option<&str> {
        match self {
            Ask::Rename { from, .. } | Ask::RenameProject { from, .. } => Some(from),
            _ => None,
        }
    }

    /// The command this becomes once a value is typed.
    pub(super) fn argv(&self, value: &str) -> Vec<String> {
        let v = value.trim().to_string();
        match self {
            Ask::AddTask { project, parent } => {
                let mut argv = vec!["add".into(), v];
                if let Some(p) = parent {
                    argv.push("--parent".into());
                    argv.push(p.clone());
                }
                match project {
                    Some(p) => {
                        argv.push("-p".into());
                        argv.push(p.clone());
                    }
                    // `wsp add` with no project resolves one from the cwd, and
                    // the panel's cwd is wherever it happens to be installed —
                    // so the inbox has to be asked for, not left implied. A
                    // sub-task needs neither: it goes where its parent is.
                    None if parent.is_none() => argv.push("--inbox".into()),
                    None => {}
                }
                argv
            }
            Ask::NewProject { parent: Some(p) } => {
                vec!["project".into(), "add".into(), v, "--parent".into(), p.clone()]
            }
            Ask::NewProject { parent: None } => vec!["project".into(), "add".into(), v],
            Ask::Block { task } => vec!["block".into(), task.clone(), v],
            Ask::Rename { task, .. } => vec!["rename".into(), task.clone(), v],
            // One argv element, spaces and all: `project set` splits on the
            // first `=` and takes the rest whole, so a name is never quoted
            // and never re-parsed.
            Ask::RenameProject { project, .. } => {
                vec!["project".into(), "set".into(), project.clone(), format!("name={v}")]
            }
            Ask::Note { task } => vec!["note".into(), task.clone(), v],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Pick {
    /// Move a task: land on a project to put it at the top of one, on another
    /// task to make it a sub-task of that one, or on the inbox to unfile it.
    ///
    /// The two answers are one question — "where does this belong" — asked at
    /// two scales, which is why they share a key. A tree deeper than one level
    /// is the expected result and not a mistake: work decomposes as far as it
    /// decomposes, and `add --parent` could already build the shape this now
    /// lets you build after the fact.
    ///
    /// Landing on itself, or on anything already beneath it, is refused by the
    /// CLI rather than here — the same division as [`Pick::MoveProject`], for
    /// the same reason: a cycle is a fact about the sub-tree and the panel has
    /// no index to ask.
    MoveTask { task: String },
    /// Move a project under another one, sub-tree and all.
    ///
    /// Only a project answers. There is no row that means the top of the tree
    /// — the inbox is tasks filed nowhere, which is a different thing — so
    /// detaching one is `wsp project set <id> parent=none` and stays a
    /// deliberate act at a shell. Landing on itself, or on anything already
    /// beneath it, is refused by the CLI rather than here: the rule needs the
    /// subtree and the panel has no index to ask.
    MoveProject { project: String },
    /// Bind a pane to the task the cursor started on.
    PaneForTask { task: String },
    /// Point an agent at different work — this is task 025's migration.
    TaskForPane { pane: String },
    /// Say what an idle agent is *for*, when nothing else can. Lands on a
    /// project and becomes a mandate, so the next `f` on that pane needs no
    /// picking at all.
    WorkForAgent { pane: String, workspace: String },
}

impl Pick {
    pub(super) fn hint(&self) -> &'static str {
        match self {
            Pick::MoveTask { .. } => "under which project or task?",
            Pick::MoveProject { .. } => "under which project?",
            Pick::PaneForTask { .. } => "which agent takes it?",
            Pick::TaskForPane { .. } => "which task does it take?",
            Pick::WorkForAgent { .. } => "which project does it work?",
        }
    }

    /// `None` when the cursor is somewhere this pick cannot accept.
    pub(super) fn argv(&self, at: &Target) -> Option<Vec<String>> {
        match (self, at) {
            // A project row means the top level of that project, so the detach
            // travels with the move. The row you land on is the whole answer to
            // where the task belongs, and one that stayed a sub-task of
            // something else would have answered it twice — it is also the only
            // way back out of a sub-tree from here, since `--parent <task>` can
            // add a level and never remove one.
            //
            // `mv` says nothing about a detachment that detached nothing, so a
            // task that was already at the top level logs the plain move it is.
            (Pick::MoveTask { task }, Target::Project(p)) => Some(vec![
                "mv".into(),
                task.clone(),
                "-p".into(),
                p.clone(),
                "--parent".into(),
                "none".into(),
            ]),
            // Unfiling. `mv` already understands `inbox` as "no project", and
            // the top of the inbox is a top level like any other.
            (Pick::MoveTask { task }, Target::Inbox) => Some(vec![
                "mv".into(),
                task.clone(),
                "-p".into(),
                "inbox".into(),
                "--parent".into(),
                "none".into(),
            ]),
            // The same question one level down: this work belongs *inside*
            // that, not beside it. No `-p`, because the project follows the
            // parent — `mv` refuses a `-p` that disagrees with one, and naming
            // the project the cursor happened to be in would be exactly that
            // disagreement whenever the two rows are in different projects.
            (Pick::MoveTask { task }, Target::Task(parent)) => {
                Some(vec!["mv".into(), task.clone(), "--parent".into(), parent.clone()])
            }
            (Pick::MoveProject { project }, Target::Project(p)) => {
                Some(vec!["project".into(), "set".into(), project.clone(), format!("parent={p}")])
            }
            (Pick::PaneForTask { task }, Target::Pane(pane)) => {
                Some(vec!["claim".into(), task.clone(), "--pane".into(), pane.clone()])
            }
            (Pick::TaskForPane { pane }, Target::Task(task)) => {
                Some(vec!["claim".into(), task.clone(), "--pane".into(), pane.clone()])
            }
            // A mandate rather than nothing at all. Picking a project for an
            // idle agent *is* standing direction — "work here without asking"
            // is the whole content of the gesture — and recording it is what
            // stops the same question being asked every time. The sentence that
            // follows is what makes it act on it now.
            (Pick::WorkForAgent { workspace, .. }, Target::Project(p)) => {
                Some(vec!["mandate".into(), p.clone(), "-w".into(), workspace.clone()])
            }
            _ => None,
        }
    }

    /// The stronger form of [`argv`](Pick::argv), for when the CLI refuses.
    ///
    /// Only the two claims have one. `claim` refuses on work that is done, on
    /// work a live agent is holding, and on a blocked task — three rules the
    /// panel would otherwise have to learn a copy of, since a `Target::Task`
    /// carries no status for a pick to refuse on. Carrying `--force` here turns
    /// each refusal into the next question instead, which is how `done` over
    /// open sub-tasks and `project rm` already work.
    ///
    /// `mv` and `mandate` have nothing to refuse on, so there is nothing
    /// stronger to offer and offering it anyway would be a y/n that never
    /// appears. `project set parent=` does refuse — a project cannot go inside
    /// itself — and still has none, because that refusal is not a policy to be
    /// overridden. There is no `--force` for it and there should not be: what
    /// is on the other side of it is a branch that disappears from every list
    /// with its files still on disk.
    pub(super) fn escalate(&self, argv: &[String]) -> Option<Vec<String>> {
        match self {
            Pick::PaneForTask { .. } | Pick::TaskForPane { .. } => {
                let mut forced = argv.to_vec();
                forced.push("--force".into());
                Some(forced)
            }
            Pick::MoveTask { .. } | Pick::MoveProject { .. } | Pick::WorkForAgent { .. } => None,
        }
    }
}


/// Start a pick, putting the tree back if it is not there.
///
/// The agents view holds the panes and nothing else, and it is exactly where
/// you notice an agent with nothing to do — so it is where `c` and `f` get
/// pressed. Both then ask you to point at work, and there was none on screen to
/// point at: every row refused, and no key in the pick brings the tree back.
///
/// The same switch `R` and `w` already make on each other. A view is what you
/// are looking at, and asking which task is a question about the tree.
fn begin(view: &mut View, verb: Pick) -> Effect {
    view.mode = Mode::Pick { verb };
    if view.agents {
        view.agents = false;
        return Effect::Refetch;
    }
    Effect::None
}

/// A sentence to type into an agent's pane, and the line the footer shows for
/// having typed it.
///
/// A claim is a fact in the store and nothing at all in the pane it names: an
/// idle agent goes on sitting at its prompt until somebody types in it. This is
/// the missing half — the panel could always change the work, and never tell
/// anyone about it.
///
/// A prompt rather than a command. What goes in is what a person would type,
/// which means the agent reads the store itself and the panel is not quietly
/// deciding on its behalf what `next` should have said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Tell {
    pub(crate) pane: String,
    /// The sentence, where there is one. `None` for [`tell_released`], whose
    /// whole content is the clear in front of it: an agent that has just had
    /// its work taken away is not being told to do anything, and a line typed
    /// into it to say so would be the one thing left in its context.
    pub(crate) text: Option<String>,
    /// Thirty-four columns' worth. The sentence above is for the agent; this is
    /// for the person who pressed the key.
    pub(crate) note: String,
    /// What to type first to empty the context this sentence would otherwise
    /// land on top of — see [`clear_command`]. `None` where nothing here knows
    /// how to ask, and the sentence goes in on its own as it always did.
    pub(crate) clear: Option<&'static str>,
}

/// What empties this kind of agent's context, for the kinds we know how to ask.
///
/// Claude Code and nothing else. `/clear` is Claude Code's spelling, herdr
/// starts twenty other kinds, and the cost of guessing is a work order with a
/// line in front of it that the agent reads as the first half of its
/// instructions. `spawn` starts `claude` unless told otherwise, so the case
/// this covers is very nearly all of them, and the rest lose nothing they had.
fn clear_command(kind: &str) -> Option<&'static str> {
    match kind {
        "claude" => Some("/clear"),
        _ => None,
    }
}

/// Send an agent looking for its own work.
pub(super) fn tell_find_work(a: &AgentRef, project: &str) -> Tell {
    Tell {
        pane: a.pane.clone(),
        text: Some(format!(
            "Find your next piece of work: run `wsp next -p {project}`, `wsp claim` what it \
             names, then do it. If nothing is actionable, say so and stop."
        )),
        note: format!("{} → looking in {project}", a.where_),
        clear: clear_command(&a.kind),
    }
}

/// Leave an agent that has just handed work back with an empty window.
///
/// The clear and nothing after it. `c` empties a context so a work order does
/// not land on the last task's reasoning; `u` empties it because there is no
/// next task yet, and an agent sitting on a finished transcript is one that
/// reads its next instruction — whenever it arrives, from whoever sends it —
/// through work it no longer holds.
///
/// `None` where there is no clear to send. A `Tell` with neither a sentence nor
/// a clear is a thread started to do nothing, and a footer line about it is a
/// line saying something happened that did not.
pub(super) fn tell_released(a: &AgentRef, task: &str) -> Option<Tell> {
    Some(Tell {
        pane: a.pane.clone(),
        text: None,
        // The mirror of `tell_claimed`'s `→`: the work going back the way it
        // came. The name is the one the row had a moment ago — the release
        // takes it off the pane, so by the time this is drawn it is already
        // what the agent *was* called.
        note: format!("{} ← {task}", a.where_),
        clear: Some(clear_command(&a.kind)?),
    })
}

/// Tell an agent about a task it has just been handed. The sentence itself is
/// [`crate::cmd_spawn::claimed_text`], which is also what an agent `spawn` has
/// just started hears — one work order, however it was handed over.
pub(super) fn tell_claimed(a: &AgentRef, task: &str) -> Tell {
    Tell {
        pane: a.pane.clone(),
        text: Some(crate::cmd_spawn::claimed_text(task)),
        note: format!("{} → {task}", a.where_),
        clear: clear_command(&a.kind),
    }
}

/// What a pick has to say to the pane it named, once its command has worked.
///
/// `None` covers a busy agent and a shell alike, and covers them the same way:
/// the command itself still lands, and it is only the sentence that is
/// withheld. A shell would run it as a command; a working agent is in the
/// middle of something, and its prompt may not even be a prompt.
pub(super) fn pick_tell(verb: &Pick, at: &Target, ui: &Ui) -> Option<Tell> {
    let told = |pane: &str, f: &dyn Fn(&AgentRef) -> Tell| -> Option<Tell> {
        let a = ui.agent_at_pane(pane)?;
        (a.agent && a.state == "idle").then(|| f(a))
    };
    match (verb, at) {
        (Pick::PaneForTask { task }, Target::Pane(pane)) => told(pane, &|a| tell_claimed(a, task)),
        (Pick::TaskForPane { pane }, Target::Task(task)) => told(pane, &|a| tell_claimed(a, task)),
        (Pick::WorkForAgent { pane, .. }, Target::Project(p)) => {
            told(pane, &|a| tell_find_work(a, p))
        }
        _ => None,
    }
}

/// The sentence for a pane a *card* named, rather than one the cursor landed
/// on.
///
/// Same guard as [`pick_tell`] and for the same reasons — a shell would run the
/// sentence as a command, and a working agent's prompt may not be a prompt —
/// so an answer given to an agent that has gone back to work lands in the store
/// and goes untold, exactly as a claim from the tree does.
pub(super) fn tell_for_pane(ui: &Ui, pane: &str, task: &str) -> Option<Tell> {
    let a = ui.agent_at_pane(pane)?;
    (a.agent && a.state == "idle").then(|| tell_claimed(a, task))
}

/// No, not this one.
///
/// A refusal is worth typing back. The agent raised its hand and carried on
/// with something else, so silence is indistinguishable from an answer that
/// never came — and an agent waiting on one is an agent doing nothing.
///
/// No clear in front of it, unlike every other sentence here. The others hand
/// over new work and want the last task's reasoning out of the way first; this
/// one is an answer to a question asked *during* whatever the agent is still
/// holding, and emptying its context to deliver a `no` would cost the work the
/// answer was about.
pub(super) fn tell_refused(ui: &Ui, pane: &str, task: &str) -> Option<Tell> {
    let a = ui.agent_at_pane(pane)?;
    (a.agent && a.state == "idle").then(|| Tell {
        pane: a.pane.clone(),
        text: Some(format!(
            "Not {task} — your flag on it was answered no. Carry on with what you have, and \
             `wsp flag` again if you think it is still the right next thing."
        )),
        note: format!("{} → not {task}", a.where_),
        clear: None,
    })
}

/// Between the sentence and the return that sends it.
const RETURN_MS: u64 = 150;
/// How long to wait for a cleared agent to come back as a new session, and how
/// often to ask. Measured at four hundred milliseconds on this machine, which
/// is a Claude Code resetting and running its `SessionStart` hooks; the ceiling
/// is for a machine having a worse day, not for a clear that is never coming.
const CLEAR_MS: u64 = 5_000;
const CLEAR_POLL_MS: u64 = 100;

/// Hand an agent a sentence, on an empty context where we can give it one.
///
/// The clear is the point. A pane that takes a claim is nearly always an agent
/// that has just finished something else, and the work order was landing on a
/// full window of the last task's reasoning — the agent then reads its brief
/// through whatever it was thinking about before. `spawn` hands over the same
/// sentence to an agent that has just booted, and this is what makes the two
/// hand-overs the same hand-over.
pub(super) fn send_tell(t: &Tell) -> Result<(), String> {
    if let Some(cmd) = t.clear {
        clear_agent(&t.pane, cmd)?;
    }
    match &t.text {
        Some(text) => type_line(&t.pane, text),
        None => Ok(()),
    }
}

/// Type a line into a pane and press return.
///
/// Two writes with a pause between, the same bargain the editor panes make: a
/// TUI that takes a burst of input as a paste swallows the return on the end of
/// it, and the sentence then sits in the prompt unsent — which looks exactly
/// like an agent that read the instruction and ignored it.
fn type_line(pane: &str, text: &str) -> Result<(), String> {
    herdr::call("pane.send_text", json!({ "pane_id": pane, "text": text }))
        .map_err(|e| e.to_string())?;
    std::thread::sleep(Duration::from_millis(RETURN_MS));
    herdr::call("pane.send_text", json!({ "pane_id": pane, "text": "\r" }))
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Empty an agent's context, and wait for the session that replaces it.
///
/// The waiting is on the session id, not on the status. A clear is not a turn:
/// `agent.prompt --wait` spent five seconds on one and answered
/// `agent_prompt_stalled` — "no observed state change" — with the clear long
/// since done, and the status either side of it was the same word. What does
/// change is the agent's session: herdr learns the id from Claude Code's own
/// `SessionStart` hook, so a new one appearing here means the clear has landed
/// *and* the session that replaced it has started its hooks, which is the same
/// moment `wsp brief` is being read into it.
///
/// Not an error when the wait runs out. The sentence goes in either way,
/// because a work order that never arrives is worse than one arriving on a
/// context that was not emptied, and an agent still busy at that point queues
/// what is typed at it rather than dropping it.
fn clear_agent(pane: &str, cmd: &str) -> Result<(), String> {
    let before = session_of(pane);
    type_line(pane, cmd)?;
    let deadline = Instant::now() + Duration::from_millis(CLEAR_MS);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(CLEAR_POLL_MS));
        let now = session_of(pane);
        if now.is_some() && now != before {
            return Ok(());
        }
    }
    Ok(())
}

/// The agent session herdr can see in this pane, if it can see one.
fn session_of(pane: &str) -> Option<String> {
    let r = herdr::call("agent.get", json!({ "target": pane })).ok()?;
    let v = r.get("agent")?.get("agent_session")?.get("value")?;
    v.as_str().map(|s| s.to_string())
}

/// `f`: send an idle agent to find its own work.
///
/// The counterpart to `c`, and the difference is who picks: `c` hands over a
/// task you chose, this hands over a project and lets the agent choose inside
/// it. Both end in the same place — a sentence in a pane.
///
/// A pane that resolves to no project is the common case rather than the odd
/// one, and refusing there made `f` useless: herdr reports where a pane's
/// *shell* started, which for every agent launched from `~/claude` is the
/// parent of the checkout it actually works in — one directory above every
/// project root there is. So the fallback is to ask, and the answer is
/// recorded as a mandate rather than spent on one keystroke.
fn find_work(a: &AgentRef, ui: &mut Ui, view: &mut View) -> Effect {
    if !a.agent {
        say(ui, "a shell has nobody to tell");
        return Effect::None;
    }
    if let Some(t) = &a.task {
        say(ui, format!("it holds {t} — v hands that back first"));
        return Effect::None;
    }
    if a.state != "idle" {
        say(ui, "it is working — leave it be");
        return Effect::None;
    }
    let Some(project) = a.project.clone() else {
        return begin(
            view,
            Pick::WorkForAgent { pane: a.pane.clone(), workspace: a.workspace.clone() },
        );
    };
    let tell = tell_find_work(a, &project);
    say(ui, tell.note.clone());
    Effect::Tell(tell)
}

/// `C`: hand this task to whoever is spare, if anybody is.
///
/// `c` with the picking taken out. The pick is right when you have somebody in
/// mind, and it is three keys and a hunt down the dock when you have not — and
/// the commonest case by far is the second: you are reading the tree, there is
/// a task that should be moving, and the only question about *who* is whether
/// anyone at all is free. That question the panel can already answer, because
/// the census is exactly the list it would have made you walk.
///
/// Everything downstream is `c`'s: the same `claim --pane`, the same `--force`
/// behind a refusal, the same sentence typed into the pane. Only the answer to
/// "which pane" is arrived at differently, so a hand-over is one thing however
/// it was started — see [`Ui::spare_agent`] for how the pane is chosen.
///
/// It refuses on nothing but the absence of anybody to hand to. Whether the
/// task is finished, blocked, or in somebody else's hands is the CLI's to say,
/// and saying it here would be a second copy of `claim`'s rules that could
/// drift from the first — the refusal comes back as the y/n that `escalate`
/// turns it into, exactly as it does from the pick.
fn hand_over(target: &Target, ui: &mut Ui) -> Effect {
    let Target::Task(id) = target else {
        say(ui, "C hands work to a spare agent — aim at a task");
        return Effect::None;
    };
    let project = ui.project_of_task(id);
    let Some(a) = ui.spare_agent(project.as_deref()).cloned() else {
        // Both halves of why, because they want different things done about
        // them: everyone is busy, or there is nobody there at all.
        say(
            ui,
            match ui.census.is_empty() {
                true => "nobody is running · S starts one on it",
                false => "nobody is spare · S starts one on it",
            },
        );
        return Effect::None;
    };
    let argv = vec!["claim".into(), id.clone(), "--pane".into(), a.pane.clone()];
    let mut forced = argv.clone();
    forced.push("--force".into());
    Effect::Run { argv, escalate: Some(forced), then: Some(tell_claimed(&a, id)) }
}

/// `u`: take the work back off an agent, and leave it spare.
///
/// The inverse of `c`, and it undoes all three things a claim did. The binding
/// and the durable claim go — that is `wsp release`, which also puts back the
/// name the claim wrote over the pane and the workspace. The context goes with
/// them, because the rest is only true from the outside: an agent whose window
/// is still full of a task it no longer holds will go on reasoning about that
/// task, and the panel would be showing a free agent that is not one.
///
/// Both ends of the join answer it, exactly as `c` does. On an agent row it is
/// "you are off this"; on a task row it is "nobody is on this" — one gesture,
/// and the row under the cursor says which way round you were thinking about it.
///
/// The clear is withheld from a working agent and from a shell, the same rule
/// [`pick_tell`] uses and for the same reason: a shell would run `/clear` as a
/// command, and an agent in the middle of a turn has a prompt that is not a
/// prompt. The release still lands — taking work off a runaway agent is when
/// you most want this key — and only the emptying waits for it to stop.
fn unassign(target: &Target, ui: &mut Ui) -> Effect {
    let holder = match target {
        Target::Pane(p) => ui.agent_at_pane(p),
        Target::Task(id) => ui.agent_on_task(id),
        _ => {
            say(ui, "u takes work off an agent — aim at one, or at its task");
            return Effect::None;
        }
    };
    let Some(a) = holder.cloned() else {
        say(
            ui,
            match target {
                Target::Task(_) => "nobody is on it",
                _ => "it holds nothing already",
            },
        );
        return Effect::None;
    };
    let Some(task) = a.task.clone() else {
        say(ui, format!("{} holds nothing already", a.where_));
        return Effect::None;
    };
    let idle = a.agent && a.state == "idle";
    let then = idle.then(|| tell_released(&a, &task)).flatten();
    if !idle {
        say(ui, format!("{} → nothing · still working, so not cleared", a.where_));
    }
    Effect::Run {
        // No `--force`. `release` refuses on nothing: a pane either holds a
        // binding or it does not, and the panel only got here by finding one.
        argv: vec!["release".into(), "--pane".into(), a.pane.clone()],
        escalate: None,
        then,
    }
}

/// `W`: leave the agents view standing on the work.
///
/// Aimed at an agent wherever one is drawn — the list under `w`, the section at
/// the foot, the row beneath a claimed task — so the gesture is the same one
/// everywhere: point at somebody, ask what they are on, and be put there.
///
/// It only asks for the row. Which of the tree's folds and filters are covering
/// that task is not something the reducer can see — it has the rows in front of
/// it and no store behind them — and it is not something this has to know:
/// `land_on` is answered against the rebuilt tree, where the task, the project
/// chain and the filters all are. See [`super::rows::refetch_into`].
fn show_in_tree(ui: &mut Ui, view: &mut View) -> Effect {
    let Some(a) = ui.rows.get(ui.sel).and_then(|r| r.agent()).cloned() else {
        say(ui, "W finds an agent's work — aim at an agent");
        return Effect::None;
    };
    let Some(task) = a.task.clone() else {
        // Nothing to be shown, and the tree would be the wrong place to look
        // for it anyway: an agent holding no task is one you give work to, not
        // one you follow. `f` and `c` are the keys for that, and saying so is
        // more use than saying no.
        say(ui, format!("{} holds nothing · f or c gives it some", a.where_));
        return Effect::None;
    };
    view.agents = false;
    view.land_on = Some(task);
    say(ui, format!("{} · in the tree", a.where_));
    Effect::Refetch
}

/// Shut the workspace's detail pane, if it has one.
pub(super) fn close_view(store: &Store, self_ws: Option<&str>) -> bool {
    let Some(ws) = self_ws else { return false };
    crate::detail::set_focus(store, ws, &crate::detail::Focus::Nothing);
    let Ok(panes) = list_panes(ws) else { return false };
    match panes.into_iter().find(|p| p.label == VIEW_LABEL) {
        Some(p) => herdr::call("pane.close", json!({ "pane_id": p.id })).is_ok(),
        None => false,
    }
}

/// Open a board full-size in a tab of its own.
///
/// The plain half of [`pop_out`]: a tab, its root pane, and the command in it.
/// No splits, no editors, no marker files — a board wants the whole width and
/// draws its own screen.
///
/// `exec`, so the command *is* the pane: when it quits the pane goes, and the
/// tab with it. A shell left behind would be a tab you have to close twice.
///
/// One at a time, and the pane is labelled. A board holds nothing that is not
/// already in the store — it is the project, drawn by state — so a second one
/// is a second window onto one fact, and a key this cheap to press would leave
/// a stack of them. The label is what makes "the one already open" findable,
/// and what stops the panel drawing a board as a shell standing in the very
/// project it is a board of.
pub(super) fn open_board(argv: &[String], label: &str, self_ws: Option<&str>) -> String {
    let Some(ws) = self_ws else { return "no workspace to open a tab in".into() };
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "wsp".into());

    // Whatever board is open goes first. Closing its pane takes the tab with
    // it, exactly as quitting one does.
    if let Ok(panes) = list_panes(ws) {
        for p in panes.into_iter().filter(|p| p.label == BOARD_LABEL) {
            let _ = herdr::call("pane.close", json!({ "pane_id": p.id }));
        }
    }

    let Ok(r) = herdr::call(
        "tab.create",
        json!({ "workspace_id": ws, "label": label, "focus": true, "env": crate::util::store_env() }),
    ) else {
        return "could not create a tab".into();
    };
    let Some(pane) = r
        .get("root_pane")
        .and_then(|p| p.get("pane_id"))
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
    else {
        return "tab reported no pane".into();
    };
    let cmd = std::iter::once(shell_quote(&exe))
        .chain(argv.iter().map(|a| shell_quote(a)))
        .collect::<Vec<_>>()
        .join(" ");
    let _ = herdr::call("pane.rename", json!({ "pane_id": pane, "label": BOARD_LABEL }));
    let _ = herdr::call("pane.send_text", json!({ "pane_id": pane, "text": format!("exec {cmd}\n") }));
    format!("opened {label}")
}

/// Open a file full-size in a tab of its own, in the user's editor.
///
/// A tab rather than a split: the store is Markdown and editing a task means
/// its whole body — notes, acceptance criteria, the log — which wants width,
/// and a tab gives that without disturbing a layout you will come back to.
pub(crate) fn pop_out(argv: &[String], label: &str, self_ws: Option<&str>) -> String {
    let Some(ws) = self_ws else { return "no workspace to open a tab in".into() };
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "wsp".into());

    // A project has no section machinery yet, so it gets one editor on its
    // file. Named here rather than hidden in a path, so the exception is
    // visible until it can be closed.
    // `wsp edit <id>` for a task, `wsp project edit <id>` for a project. The
    // section flags append to either, so both get the same two editors.
    let id = argv.last().cloned().unwrap_or_default();
    let base: Vec<String> = argv.to_vec();

    let Ok(r) = herdr::call(
        "tab.create",
        json!({ "workspace_id": ws, "label": label, "focus": true, "env": crate::util::store_env() }),
    ) else {
        return "could not create a tab".into();
    };
    // The tab id is deliberately not kept. It was here to build the marker
    // file's name and the `herdr tab close` the last editor ran; closing the
    // tab is the context pane's job now, and it finds its own tab from its own
    // pane. Nothing in this function needs to know which tab it made.
    let Some(top) = r
        .get("root_pane")
        .and_then(|p| p.get("pane_id"))
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
    else {
        return "tab reported no pane".into();
    };

    let split = |target: &str, dir: &str, ratio: f64| -> Option<String> {
        herdr::call(
            "pane.split",
            json!({ "direction": dir, "target_pane_id": target, "ratio": ratio, "focus": false }),
        )
        .ok()?
        .get("pane")?
        .get("pane_id")?
        .as_str()
        .map(|s| s.to_string())
    };
    let run = |pane: &str, text: String| {
        let _ = herdr::call("pane.send_text", json!({ "pane_id": pane, "text": text }));
    };

    // Context across the top, editors beneath. The context is the same live
    // view the sidebar opens, so status, claim and log keep updating while you
    // type — that context was exactly what editing in a bare buffer cost.
    let Some(work) = split(&top, "down", 0.32) else {
        return "could not split the tab".into();
    };
    let _ = herdr::call("pane.rename", json!({ "pane_id": top, "label": VIEW_LABEL }));
    run(
        &top,
        format!("exec {} view {}\n", shell_quote(&exe), shell_quote(&id)),
    );

    // One editor per section, side by side. Each buffer is prose and nothing
    // else — there is no markup left to mangle, which was the point. They are
    // safe to run together because `wsp edit` re-reads the task and writes back
    // only its own section, and because no two columns ever hold the same one.
    //
    // Two columns to open with, not three: the third is a keystroke away and
    // three editors on a laptop split is narrower than most prose wants. `3`
    // in the context pane says otherwise.
    let cols = crate::detail::Columns::new(2);
    let sections = cols.sections();
    let mut panes = vec![work];
    for i in 1..sections.len() {
        // The target keeps `ratio` and the new pane takes the rest, so peeling
        // one column at a time off the right of what is left gives even widths:
        // a third, then half of the remaining two thirds.
        let ratio = 1.0 / (sections.len() - i + 1) as f64;
        let Some(p) = panes.last().and_then(|last| split(last, "right", ratio)) else {
            return "could not split the editors".into();
        };
        panes.push(p);
    }

    let cmd = std::iter::once(shell_quote(&exe))
        .chain(base.iter().map(|a| shell_quote(a)))
        .collect::<Vec<_>>()
        .join(" ");
    for (pane, section) in panes.iter().zip(sections) {
        crate::detail::start_editor(pane, section, &cmd);
    }
    format!("editing {label}")
}

/// Point the workspace's detail pane at something, making one if there is not
/// one yet.
///
/// One pane per workspace, reused: opening a second thing retargets the pane
/// you are already reading rather than stacking another beside it. The target
/// goes through a file the view polls, so retargeting costs no process churn —
/// the alternative, killing and relaunching, would blink the pane on every
/// press of a key whose whole job is to be cheap.
pub(crate) fn inspect(store: &Store, self_ws: Option<&str>, focus: &crate::detail::Focus) -> String {
    let Some(ws) = self_ws else {
        return "no workspace to open a view in".into();
    };
    crate::detail::set_focus(store, ws, focus);

    let existing = list_panes(ws).ok().and_then(|ps| {
        ps.into_iter().find(|p| p.label == VIEW_LABEL).map(|p| p.id)
    });
    if existing.is_some() {
        return String::new();
    }

    // Split downward off our own pane, so the detail shares the sidebar's
    // column and the working pane beside it is never touched.
    let Some(me) = list_panes(ws)
        .ok()
        .and_then(|ps| ps.into_iter().find(|p| p.label == PANEL_LABEL).map(|p| p.id))
    else {
        return "cannot find the panel pane".into();
    };
    let res = herdr::call(
        "pane.split",
        json!({ "direction": "down", "target_pane_id": me, "ratio": 0.45, "focus": false }),
    );
    let Ok(r) = res else { return "could not split a view pane".into() };
    let Some(pane) = r.get("pane").and_then(|p| p.get("pane_id")).and_then(|x| x.as_str()) else {
        return "split reported no pane".into();
    };
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "wsp".into());
    let _ = herdr::call("pane.rename", json!({ "pane_id": pane, "label": VIEW_LABEL }));
    let _ = herdr::call(
        "pane.send_text",
        json!({ "pane_id": pane, "text": format!("exec {} view\n", shell_quote(&exe)) }),
    );
    String::new()
}

/// Run this binary against the store and report in a few words.
///
/// Output is captured, never inherited: the panel owns an alternate screen in
/// raw mode, and a subcommand printing into it would corrupt the frame. stdin
/// is closed so nothing can sit waiting for input the panel will never send.
/// What a command did: a line for the footer, and the id of whatever it made,
/// when it made something.
pub(crate) struct Made {
    pub(crate) label: String,
    pub(super) id: Option<String>,
}

pub(crate) fn run_wsp(argv: &[String]) -> Result<Made, String> {
    let exe = std::env::current_exe().unwrap_or_else(|_| "wsp".into());
    let out = Command::new(exe)
        .args(argv)
        .arg("--json")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();

    match out {
        // Ask for JSON so a caller can learn what was made. Nothing prints it;
        // the panel owns the screen and stdout here is data, not output.
        Ok(o) if o.status.success() => {
            let made = String::from_utf8_lossy(&o.stdout)
                .lines()
                .find_map(|l| serde_json::from_str::<serde_json::Value>(l.trim()).ok())
                .and_then(|v| {
                    v.get("id")
                        .or_else(|| v.get("removed"))
                        .and_then(|x| x.as_str())
                        .map(|s| s.to_string())
                });
            Ok(Made { label: argv.join(" "), id: made })
        }
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            let first = err.lines().next().unwrap_or("failed").trim();
            Err(first.strip_prefix("wsp: ").unwrap_or(first).to_string())
        }
        Err(e) => Err(format!("cannot run wsp: {e}")),
    }
}

/// Reading the key map. Everything that is not scrolling or closing is
/// swallowed: the map is open precisely because you are not sure what a key
/// does, which is the worst possible moment for one of them to fire.
pub(super) fn browse_key(k: Key, ui: &mut Ui, view: &mut View) -> Effect {
    let n = ui.rows.len();
    let target = ui.selected_target();

    // Which project a new task should land in, read from wherever the cursor
    // is: a project takes it directly, a task hands over its own project, and
    // the inbox means deliberately none.
    let scope = |t: &Target, ui: &Ui| -> Option<Option<String>> {
        match t {
            Target::Project(p) => Some(Some(p.clone())),
            Target::Inbox => Some(None),
            Target::Task(id) => Some(ui.project_of_task(id)),
            _ => None,
        }
    };

    match k {
        // `q` and Esc both mean "put away what is in front of me", and the
        // panel itself is never that. It is installed furniture in every
        // workspace, so quitting it by a stray keystroke costs a reinstall and
        // buys nothing — `ctrl-c` still does it, and `wsp panel uninstall` is
        // the deliberate way. The map goes first, then the detail pane.
        Key::Char('q') | Key::Esc if view.help => {
            view.help = false;
            Effect::None
        }
        Key::Char('q') | Key::Esc if view.showing.is_some() => Effect::CloseView,
        Key::Char('q') => {
            say(ui, "nothing to close · ctrl-c quits the panel");
            Effect::None
        }
        Key::Esc => Effect::CloseView,
        Key::Interrupt => Effect::Quit,

        Key::Down | Key::Char('j') => move_or_fold(Key::Down, ui, view),
        Key::Up | Key::Char('k') => move_or_fold(Key::Up, ui, view),
        Key::Left | Key::Char('h') => move_or_fold(Key::Left, ui, view),
        Key::Right | Key::Char('l') => move_or_fold(Key::Right, ui, view),
        Key::Char('g') | Key::Home => {
            ui.sel = 0;
            Effect::None
        }
        Key::Char('G') | Key::End => {
            // The last row that takes the cursor, which in the agents view is
            // not the last line: an agent's detail lines follow it.
            ui.sel = n.saturating_sub(1);
            if !ui.rows.is_empty() && !ui.rows[ui.sel].selectable() {
                ui.sel = super::keys::step(&ui.rows, ui.sel, false);
            }
            Effect::None
        }

        // A raised hand opens the card again — the ask is what a flag row is
        // *for*, and the card is the whole of it: the heading the agent wrote,
        // the paragraph the row had no room for, and the question. The task
        // itself is one more key from there, `o`, which is the right way round:
        // you read the ask and then go looking, rather than being sent to the
        // task with the ask left behind you.
        Key::Enter if matches!(ui.rows.get(ui.sel), Some(super::rows::Row::Flag { .. })) => {
            match ui.rows.get(ui.sel) {
                Some(super::rows::Row::Flag { card }) => {
                    view.mode = Mode::Card(card.clone());
                    Effect::None
                }
                _ => Effect::None,
            }
        }
        Key::Enter => match &target {
            // An overflow row has nothing to look at; opening it is the only
            // thing it does.
            Target::Overflow(_) => move_or_fold(Key::Right, ui, view),
            // A pane's detail *is* the terminal. Going there beats describing it.
            Target::Pane(_) => match ui.rows.get(ui.sel).and_then(|r| r.agent()) {
                Some(a) => Effect::Focus(a.clone()),
                None => Effect::None,
            },
            // Pressing it again on the thing already open shuts the pane, so
            // the same key both opens and closes and nothing has to be
            // remembered.
            Target::Task(id) => toggle(view, crate::detail::Focus::Task(id.clone())),
            Target::Project(p) => toggle(view, crate::detail::Focus::Project(p.clone())),
            Target::Inbox | Target::Unattached | Target::Nothing => {
                say(ui, "nothing to open there");
                Effect::None
            }
        },

        Key::Char(d @ '1'..='9') => {
            let want = d as u8 - b'0';
            match hotkeys(ui)
                .iter()
                .position(|k| *k == Some(want))
                .and_then(|i| ui.rows[i].agent())
            {
                Some(a) => Effect::Focus(a.clone()),
                None => Effect::None,
            }
        }

        // ---- view ----
        Key::Char('A') => {
            view.show_done = !view.show_done;
            say(ui, if view.show_done { "showing done" } else { "hiding done" });
            Effect::Refetch
        }
        // The review filter and `A` are opposites — one narrows to work that is
        // finished-and-handed-back, the other widens to work that is finished
        // and gone — so leaving both on shows nothing and reads as a bug.
        Key::Char('R') => {
            view.review_only = !view.review_only;
            if view.review_only {
                view.show_done = false;
                // `R` is a question about work, and the agents view has no work
                // in it to narrow. Asking it there means the tree, so go back.
                view.agents = false;
            }
            say(
                ui,
                if view.review_only { "review only" } else { "the whole tree" },
            );
            Effect::Refetch
        }
        // The agents, in place of the work. Not a filter over the tree: the
        // tree is ordered by what has to be done and this is ordered by who has
        // stopped, and no arrangement of one row set answers both.
        Key::Char('w') => {
            view.agents = !view.agents;
            if view.agents {
                // The two are one switch from either side. A filter left on
                // under a view that does not use it is a footer saying `review
                // only` over a list of panes, and a tree that comes back
                // narrowed when you turn the view off.
                view.review_only = false;
            }
            say(ui, if view.agents { "the agents" } else { "the work" });
            Effect::Refetch
        }
        // And the way back, standing on the work rather than at the top of the
        // tree. `w` answers "who has stopped"; the next question is always "on
        // what", and until now the only way to ask it was `w` again and then
        // hunting for the title by eye — in a tree that had no idea which of
        // its rows you meant, and might not be drawing it at all.
        Key::Char('W') => show_in_tree(ui, view),
        // The tree names work by title, which is what you read it for. The id
        // is what you type at a shell, and there was no way to get from one to
        // the other without opening the row.
        Key::Char('i') => {
            view.ids = !view.ids;
            say(ui, if view.ids { "showing ids" } else { "hiding ids" });
            Effect::Refetch
        }
        Key::Char('r') => Effect::Sync,
        // A row is one line wide and a title is not. `↵` has always been the
        // way to read the rest, and it opens a second pane and takes the cursor
        // out of the tree — which is a lot to do to read a sentence. This keeps
        // the rest of it under the tree while you go on scrolling.
        Key::Char('F') => {
            view.focus = !view.focus;
            say(ui, if view.focus { "titles in full" } else { "titles as they fit" });
            Effect::None
        }
        // Nothing else changes while it is up: the tree keeps the cursor, and
        // every key on the map still does what the map says it does — which is
        // the only way to read one and act on it in the same breath.
        Key::Char('?') => {
            view.help = !view.help;
            Effect::None
        }

        // ---- create ----
        // On a task the scope is *beneath* it. A sibling is one row away — the
        // project heading above, or any task in it — but nothing else reaches
        // a sub-task, and decomposing work you are looking at is the commoner
        // move by far.
        Key::Char('a') => match &target {
            Target::Task(id) => {
                view.mode = Mode::Prompt {
                    verb: Ask::AddTask {
                        project: ui.project_of_task(id),
                        parent: Some(id.clone()),
                    },
                    buffer: String::new(),
                };
                Effect::None
            }
            other => match scope(other, ui) {
                Some(project) => {
                    view.mode = Mode::Prompt {
                        verb: Ask::AddTask { project, parent: None },
                        buffer: String::new(),
                    };
                    Effect::None
                }
                None => {
                    say(ui, "nowhere to add that");
                    Effect::None
                }
            },
        },
        Key::Char('P') => {
            let parent = match &target {
                Target::Project(p) => Some(p.clone()),
                _ => None,
            };
            view.mode = Mode::Prompt { verb: Ask::NewProject { parent }, buffer: String::new() };
            Effect::None
        }

        // ---- status, one key each ----
        Key::Char('s') => task_verb(&target, ui, "start"),
        Key::Char('v') => task_verb(&target, ui, "review"),
        Key::Char('d') => task_verb(&target, ui, "done"),
        Key::Char('o') => task_verb(&target, ui, "reopen"),

        // ---- what comes first here ----
        //
        // One key for three values, cycled rather than typed: the levels are
        // `high`, `low` and `normal` and a prompt for one of three words is a
        // mode to enter and leave for something a keystroke settles. Blind
        // cycles are a trap, so this one is not blind — the row redraws with
        // its mark under the cursor, the footer carries the argv, which names
        // the level in words, and `Priority::cycled` puts `normal` last so
        // holding the key returns you to where you started.
        Key::Char('!') => match &target {
            Target::Task(id) => {
                let want = ui.priority_of_task(id).unwrap_or(Priority::Normal).cycled();
                Effect::Run {
                    argv: vec!["prio".into(), id.clone(), want.as_str().into()],
                    escalate: None,
                    then: None,
                }
            }
            _ => {
                say(ui, "priority is a task's place in its project");
                Effect::None
            }
        },

        // ---- typed ----
        Key::Char('b') => match &target {
            Target::Task(id) => {
                view.mode =
                    Mode::Prompt { verb: Ask::Block { task: id.clone() }, buffer: String::new() };
                Effect::None
            }
            _ => {
                say(ui, "only a task can be blocked");
                Effect::None
            }
        },
        // Retitling is nearly always a correction — a word swapped, a clause
        // added — and an empty line makes you retype the sixty characters you
        // meant to keep, from a title the row was too narrow to show you in
        // the first place. So the prompt opens holding the whole of it, caret
        // at the end. `ctrl-u` is there for the rarer case of starting over.
        //
        // A project takes the same key and changes its *name*, which is a
        // different string from the id the row is drawn with — see
        // [`Ask::RenameProject`]. It opens holding what it is changing for the
        // same reason, and it is the only rename a project has: the slug is
        // what every task, pin and mandate names it by, and nothing here can
        // move one.
        Key::Char('e') => match &target {
            Target::Task(id) => {
                let from = ui.title_of_task(id).unwrap_or_default();
                view.mode = Mode::Prompt {
                    verb: Ask::Rename { task: id.clone(), from: from.clone() },
                    buffer: from,
                };
                Effect::None
            }
            Target::Project(p) => {
                let from = ui.name_of_project(p).unwrap_or_default();
                view.mode = Mode::Prompt {
                    verb: Ask::RenameProject { project: p.clone(), from: from.clone() },
                    buffer: from,
                };
                Effect::None
            }
            _ => {
                say(ui, "only a task or a project is renamed here");
                Effect::None
            }
        },
        // Tags are what `wsp ls -t` and `wsp project ls -t` cut the store by,
        // and until now the panel could read them and not touch them.
        //
        // A picker rather than a prompt. `wsp tag <id> +dsp -ui` is the right
        // shape for a shell and the wrong one for a sidebar: it makes you spell
        // out every tag, and the one you most want to spell is the one you are
        // *removing* — a name the panel is already holding and you are being
        // asked to remember. The vocabulary is nineteen words across the whole
        // store, so it fits on screen, and picking from what is there also
        // stops `dsp` and `DSP` becoming two tags that read as one.
        //
        // Tasks only: `wsp tag` is a task verb, and a project's tags are set as
        // a whole list by `project set`, which is a different gesture with a
        // different way of going wrong.
        Key::Char('t') => match &target {
            Target::Task(id) => {
                view.mode = Mode::Tags(Tags::new(
                    id,
                    ui.tags_of_task(id),
                    ui.inherited_tags_of_task(id),
                    &ui.vocabulary,
                ));
                // The picker takes rows off the tree, and the row it is about
                // is one of the ones that can go. Tagging a task you can no
                // longer see is the one thing this must not do — so the tree
                // owes the cursor a look, exactly as it does when a key moves
                // it. Otherwise a picker opened after a wheel or a click, where
                // the view is deliberately somewhere else, opens onto a tree
                // with the task nowhere in it.
                view.keyed = true;
                Effect::None
            }
            _ => {
                say(ui, "tags go on a task · wsp project set <id> tags=…");
                Effect::None
            }
        },
        Key::Char('n') => match &target {
            Target::Task(id) => {
                view.mode =
                    Mode::Prompt { verb: Ask::Note { task: id.clone() }, buffer: String::new() };
                Effect::None
            }
            _ => {
                say(ui, "notes go on a task");
                Effect::None
            }
        },

        // ---- picked ----
        // One key, two things that move: a task between projects, and a
        // project inside the tree. Same gesture either way — the tree becomes
        // the picker and `↵` takes the row it lands on — because "where does
        // this belong" is one question, and the row under the cursor already
        // says which of the two is being asked.
        Key::Char('m') => match &target {
            Target::Task(id) => begin(view, Pick::MoveTask { task: id.clone() }),
            Target::Project(p) => begin(view, Pick::MoveProject { project: p.clone() }),
            _ => {
                say(ui, "a task or a project moves");
                Effect::None
            }
        },
        Key::Char('c') => match &target {
            Target::Task(id) => begin(view, Pick::PaneForTask { task: id.clone() }),
            Target::Pane(p) => begin(view, Pick::TaskForPane { pane: p.clone() }),
            _ => {
                say(ui, "claim joins a task to an agent");
                Effect::None
            }
        },
        // The same join, made without being asked who. `c` is for when you
        // know which pane you want; this is for when the only thing you want
        // is that somebody takes it.
        Key::Char('C') => hand_over(&target, ui),
        // The other direction. `c` joins a task to an agent; this takes the
        // join apart from whichever end you are looking at it from.
        Key::Char('u') => unassign(&target, ui),
        // The dock's own verb. `c` needs you to have decided what the work is;
        // this needs only that there is some.
        Key::Char('f') => match ui.rows.get(ui.sel).and_then(|r| r.agent()).cloned() {
            Some(a) => find_work(&a, ui, view),
            None => {
                say(ui, "f sets an agent looking — aim it at one");
                Effect::None
            }
        },

        // ---- a raised hand comes down ----
        //
        // Lowering is deliberately its own key rather than something `↵` does
        // on the way past. Opening a task is how you *read* the ask, and a
        // flag that cleared itself the moment you looked would be gone from
        // every panel before you had decided anything about it — including the
        // decision to leave it up while you finish what you are doing. So the
        // panel takes it down when you say so, and not before.
        //
        // It runs the CLI like every other key here, so the agent that raised
        // it and the person lowering it go through one implementation, one
        // event and one file.
        Key::Char('x') => match ui.selected_flag() {
            Some(id) => Effect::Run {
                argv: vec!["flag".into(), "--clear".into(), id],
                escalate: None,
                then: None,
            },
            None => {
                say(ui, "x lowers a raised flag — aim at one");
                Effect::None
            }
        },

        // ---- pop out, full size, in an editor ----
        Key::Char('E') => match &target {
            // `wsp edit`, not the file: it opens the prose and keeps the
            // frontmatter out of reach, which is the difference between a typo
            // and a task the tools can no longer read.
            Target::Task(id) => Effect::PopOut {
                argv: vec!["edit".into(), id.clone()],
                label: id.clone(),
            },
            Target::Project(p) => Effect::PopOut {
                argv: vec!["project".into(), "edit".into(), p.clone()],
                label: p.clone(),
            },
            _ => {
                say(ui, "nothing there to open");
                Effect::None
            }
        },

        // ---- the same work, by state instead of by tree ----
        //
        // A board is a project's, so a task hands over the project it is filed
        // in rather than refusing: you press this having noticed a card, and
        // what you want to see is the pile it came out of. The inbox is a scope
        // like any other and gets one too.
        Key::Char('K') => {
            let (arg, label) = match &target {
                Target::Project(p) => (p.clone(), p.clone()),
                Target::Task(id) => match ui.project_of_task(id) {
                    Some(p) => (p.clone(), p),
                    None => ("inbox".to_string(), "inbox".to_string()),
                },
                Target::Inbox => ("inbox".to_string(), "inbox".to_string()),
                _ => {
                    say(ui, "a board is a project's — aim at one, or at a task in it");
                    return Effect::None;
                }
            };
            Effect::Board { argv: vec!["kanban".into(), arg], label: format!("board {label}") }
        }

        // ---- open a workspace for this row, with or without somebody in it ----
        //
        // Two keys rather than one that asks, because they are two different
        // decisions and only one of them is expensive: `O` is a place to work
        // and `S` is a colleague, and the second costs a model and a context
        // window. A y/n between the key and the thing would put the same
        // question in front of the cheap one every time.
        Key::Char('O') => spawn(&target, ui, false),
        Key::Char('S') => spawn(&target, ui, true),

        // ---- destructive ----
        Key::Char('X') => match &target {
            Target::Task(id) => {
                view.mode = Mode::Confirm {
                    argv: vec!["rm".into(), id.clone()],
                    question: format!("retire {id}?"),
                    escalate: None,
                    then: None,
                };
                Effect::None
            }
            Target::Project(p) => {
                // Deliberately without --force. If the project still holds
                // work the CLI refuses, and that refusal becomes the next
                // question rather than something the panel quietly overrode.
                view.mode = Mode::Confirm {
                    argv: vec!["project".into(), "rm".into(), p.clone()],
                    question: format!("remove {p}?"),
                    escalate: Some(vec![
                        "project".into(),
                        "rm".into(),
                        p.clone(),
                        "--force".into(),
                    ]),
                    then: None,
                };
                Effect::None
            }
            _ => {
                say(ui, "nothing there to remove");
                Effect::None
            }
        },

        _ => Effect::None,
    }
}

/// `↵` on what is already open means close it.
pub(super) fn toggle(view: &mut View, want: crate::detail::Focus) -> Effect {
    if view.showing.as_ref() == Some(&want) {
        Effect::CloseView
    } else {
        Effect::Inspect(want)
    }
}

/// `O` and `S`: a workspace for the row under the cursor, and for `S` an agent
/// started in it.
///
/// The row decides the argument and nothing else: `wsp spawn` resolves the
/// title, the project and the root it should stand in, so the panel does not
/// carry a second copy of any of that — which is how `O` came to open a
/// workspace in the wrong tree for every task under `wsp/render`, a project
/// whose root is its parent's.
fn spawn(target: &Target, ui: &mut Ui, agent: bool) -> Effect {
    let (mut argv, what) = match target {
        Target::Task(id) => (vec!["spawn".to_string(), id.clone()], id.clone()),
        Target::Project(p) => (
            vec!["spawn".to_string(), "-p".to_string(), p.clone()],
            p.clone(),
        ),
        _ => {
            say(ui, "nothing there to open a workspace for");
            return Effect::None;
        }
    };
    if agent {
        argv.push("--agent".into());
    }
    let note = match agent {
        true => format!("starting an agent on {what}…"),
        false => format!("opening {what}…"),
    };
    Effect::Spawn { argv, note }
}

/// Status verbs all have the same shape: one key, a task, no input.
pub(super) fn task_verb(target: &Target, ui: &mut Ui, verb: &str) -> Effect {
    match target {
        Target::Task(id) => Effect::Run {
            argv: vec![verb.to_string(), id.clone()],
            then: None,
            // `done` on a parent with open sub-tasks is refused by the CLI.
            // Carry the stronger form so that refusal becomes the next
            // question — the same shape `X` on a project already takes, and
            // the reason the panel never has to know the rule itself.
            escalate: (verb == "done")
                .then(|| vec![verb.to_string(), id.clone(), "--force".into()]),
        },
        _ => {
            say(ui, format!("{verb} needs a task"));
            Effect::None
        }
    }
}

