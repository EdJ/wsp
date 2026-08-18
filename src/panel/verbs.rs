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
use crate::live::AgentRef;
use crate::model::Priority;
use crate::store::Store;

use super::install::{list_panes, widest, PaneInfo};
use crate::util::shell_quote;
use super::keys::{move_or_fold, say, Effect, Mode, Tags, View};
use super::rows::{hotkeys, Target, Ui};
use super::{BOARD_LABEL, FULL_LABEL, PANEL_LABEL, VIEW_LABEL};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Ask {
    AddTask { project: Option<String>, parent: Option<String> },
    NewProject { parent: Option<String> },
    Block { task: String },
    /// The other half of `Block`, and a different question: not what somebody
    /// owes you, but what would make this worth starting. The prompt asks for
    /// it in one word — `until` — because a park with nothing written on it is
    /// the state this status exists to end.
    Park { task: String },
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
    /// A sentence for the agent in a project's slot.
    ///
    /// The one prompt here that does not write anything down. Everything else
    /// on this list changes the store and the agents read it; this is a person
    /// talking to the custodian — directing it, correcting its sequencing,
    /// telling it what to start next — which is what a governor has been for
    /// since before wsp knew the position existed. Addressed to the project,
    /// never to the pane: the row you are standing on is the post, and who is
    /// in it is `wsp govern`'s business at the moment the sentence lands.
    Tell { project: String },
}

impl Ask {
    pub(super) fn label(&self) -> String {
        match self {
            Ask::AddTask { parent: Some(_), .. } => "sub-task".into(),
            Ask::AddTask { .. } => "task".into(),
            Ask::NewProject { .. } => "project".into(),
            Ask::Block { .. } => "why".into(),
            Ask::Park { .. } => "until".into(),
            Ask::Rename { .. } => "title".into(),
            Ask::RenameProject { .. } => "name".into(),
            Ask::Note { .. } => "note".into(),
            Ask::Tell { project } => format!("say to {project}"),
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
            Ask::Park { task } => vec!["park".into(), task.clone(), v],
            Ask::Rename { task, .. } => vec!["rename".into(), task.clone(), v],
            // One argv element, spaces and all: `project set` splits on the
            // first `=` and takes the rest whole, so a name is never quoted
            // and never re-parsed.
            Ask::RenameProject { project, .. } => {
                vec!["project".into(), "set".into(), project.clone(), format!("name={v}")]
            }
            Ask::Note { task } => vec!["note".into(), task.clone(), v],
            // Through the CLI like every other key here, which is what keeps
            // the panel out of the business of finding the pane, choosing the
            // agent's kind and deciding what a clear would cost — all of which
            // `wsp govern --tell` already answers once, for the shell and the
            // panel alike.
            Ask::Tell { project } => {
                vec!["govern".into(), project.clone(), "--tell".into(), v]
            }
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

    /// The question to put in front of this pick's `↵`, where it needs one.
    ///
    /// Only the three that move an agent. A pick is already two deliberate acts
    /// — the key, then the row — so it is the last place a y/n looks earned;
    /// what makes it earned here is that the second act is `↵`, the panel's
    /// busiest key, and the tree under it is full of rows. Landing it one row
    /// out on a claim binds the wrong pane and empties the wrong context.
    ///
    /// [`Pick::MoveTask`] and [`Pick::MoveProject`] get none. They rewrite a
    /// field, `m` puts it back, and nothing that has been thinking for an hour
    /// is affected either way.
    pub(super) fn confirm(&self, at: &Target) -> Option<String> {
        match (self, at) {
            (Pick::PaneForTask { task }, Target::Pane(pane)) => Some(format!("hand {task} to {pane}?")),
            (Pick::TaskForPane { pane }, Target::Task(task)) => Some(format!("put {pane} on {task}?")),
            (Pick::WorkForAgent { pane, .. }, Target::Project(p)) => {
                Some(format!("set {pane} to work {p}?"))
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

/// Put a question in front of a deed: the key has decided *what*, and this is
/// the panel asking whether you meant to press it.
///
/// Everything an agent has is behind five of these keys, and each of them is
/// one shift or one finger away from something harmless: `S` from the `s` that
/// starts a task, `C` from the `c` beside it, `f` from the `d` that finishes
/// one, `u` from the `i` that shows ids. The cost of the slip is not
/// symmetrical — nothing undoes an emptied context window, and a claim landing
/// on the wrong pane takes two agents down rather than one. So the letters that
/// generate or re-aim an agent go through here, and the ones that merely look,
/// move or change a record do not: a y/n on every keystroke is a y/n nobody
/// reads.
///
/// `O` is the deliberate exception among neighbours: it opens a workspace and
/// costs a directory, where `S` beside it costs a model and a context window.
fn ask(view: &mut View, question: String, deed: Effect) -> Effect {
    view.mode = Mode::Confirm { question, deed: Box::new(deed) };
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
        note: format!("{} → looking in {project}", a.where_()),
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
        note: format!("{} ← {task}", a.where_()),
        clear: Some(clear_command(&a.kind)?),
    })
}

/// Tell an agent about a task it has just been handed. The sentence itself is
/// [`crate::cmd_spawn::claimed_text`], which is also what an agent `spawn` has
/// just started hears — one work order, however it was handed over.
///
/// `Running` because this agent has been sitting in that pane since before the
/// claim existed: its session-start brief was a brief about holding nothing, so
/// unlike a spawned agent it does have to go and fetch. `spawn`'s case is the
/// other arm, and the difference is the whole reason the case is named.
pub(super) fn tell_claimed(a: &AgentRef, task: &str) -> Tell {
    Tell {
        pane: a.pane.clone(),
        text: Some(crate::cmd_spawn::work_order(task, crate::cmd_spawn::Handover::Running)),
        note: format!("{} → {task}", a.where_()),
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
        note: format!("{} → not {task}", a.where_()),
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
    // On what the task is *at*, not on there being one — see
    // [`crate::live::Claimed::in_hand`]. The commonest agent on the machine has
    // finished, put its task to `review` and is standing there idle, and
    // `claim` leaves that task where it is: the `v` this used to demand first
    // would have changed nothing. Mid-task the refusal is right and stays.
    if let Some(t) = a.task.as_ref().filter(|t| t.in_hand()) {
        say(ui, format!("it holds {} — v hands that back first", t.id));
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
    // No `say` in front of it any more: the note is what [`super::run::tell`]
    // puts on the footer once the sentence has actually gone in, and while the
    // question is up the footer is the question.
    let tell = tell_find_work(a, &project);
    // The project and not the agent, because the confirm is a footer line over
    // a tree that has not moved: the agent is the row the cursor is sitting on
    // and naming it again spends the twenty-eight columns the footer has on the
    // one fact already on screen. Where it went is what you cannot see — it
    // comes off a mandate or off the pane's directory, and getting it wrong
    // sends an agent to work somewhere it will refuse to be.
    ask(view, format!("find work in {project}?"), Effect::Tell(tell))
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
fn hand_over(target: &Target, ui: &mut Ui, view: &mut View) -> Effect {
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
    // The question names the pane, because this is the one key that chose it
    // for you: `c` was pointed at somebody, and here the census was. A slip
    // lands work on whichever agent happened to sort first, and the y/n is the
    // only place that name is ever put in front of you.
    ask(
        view,
        format!("hand {id} to {}?", a.where_()),
        Effect::Run { argv, escalate: Some(forced), then: Some(tell_claimed(&a, id)) },
    )
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
fn unassign(target: &Target, ui: &mut Ui, view: &mut View) -> Effect {
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
    let Some(task) = a.task.as_ref().map(|t| t.id.clone()) else {
        say(ui, format!("{} holds nothing already", a.where_()));
        return Effect::None;
    };
    let idle = a.agent && a.state == "idle";
    let then = idle.then(|| tell_released(&a, &task)).flatten();
    // Whether it is mid-task belongs in the question rather than in a note
    // beside it: the footer is one line and the confirm has it, so anything
    // said here would be said to nobody. It is also the fact you most want
    // before answering — taking work off an agent that is still doing it is
    // the case this key exists for and the case a slip does most harm in.
    let question = match idle {
        true => format!("take {task} off {}?", a.where_()),
        false => format!("{} is working · take {task} off it?", a.where_()),
    };
    ask(
        view,
        question,
        Effect::Run {
            // No `--force`. `release` refuses on nothing: a pane either holds a
            // binding or it does not, and the panel only got here by finding one.
            argv: vec!["release".into(), "--pane".into(), a.pane.clone()],
            escalate: None,
            then,
        },
    )
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
    let Some(task) = a.task.as_ref().map(|t| t.id.clone()) else {
        // Nothing to be shown, and the tree would be the wrong place to look
        // for it anyway: an agent holding no task is one you give work to, not
        // one you follow. `f` and `c` are the keys for that, and saying so is
        // more use than saying no.
        say(ui, format!("{} holds nothing · f or c gives it some", a.where_()));
        return Effect::None;
    };
    view.agents = false;
    view.land_on = Some(task);
    say(ui, format!("{} · in the tree", a.where_()));
    Effect::Refetch
}

/// The workspace an action opens into.
///
/// A panel is *in* one. It was split into a workspace, the panes it draws are
/// that workspace's, and `↵` puts the detail pane beside itself; asking any
/// other question would be answering about somewhere it is not.
///
/// A surface is in none — herdr's sidebar belongs to no workspace, which is
/// the property the fork exists to get — so what it acts on is the workspace
/// **on screen**: the one the reader is looking at as they press the key. That
/// is the same answer a person would give if asked where they expected the
/// view to open, and it is why every verb below still opens a real pane in a
/// real workspace rather than needing somewhere new to put things.
///
/// Asked here, at the key, and never carried. Which workspace is on screen is
/// the fastest-moving fact herdr has, and a surface outlives all of them; one
/// resolved when the sidebar started would be right until the first click
/// elsewhere and quietly wrong after that, splitting panes into a workspace
/// nobody is looking at.
///
/// It does move under you. Measured 2026-08-18 against the live server: herdr
/// raises `workspace.focused` on its own — an agent coming back, a hover on
/// its own sidebar — and the focused workspace changed twice in a few minutes
/// with nobody typing. That is not a reason to hold one instead. A workspace
/// herdr has put on screen *is* what the reader is looking at, so opening
/// there is right by the same argument; and the alternative, a workspace
/// chosen when the sidebar started, is wrong in exactly the cases this one is
/// right in.
///
/// `None` means herdr has no workspace at all — its start screen — and the
/// callers say so rather than guessing at one.
fn stage(self_ws: Option<&str>) -> Option<String> {
    self_ws.map(str::to_string).or_else(herdr::focused_workspace)
}

/// The detail pane this panel opened, if it has one.
///
/// Ours means *in this tab*, not merely in this workspace: `pane.list` is per
/// workspace, and the fullscreen panel `Z` opens is a second panel in a tab of
/// its own. Reading the workspace's first view pane would have each of the two
/// closing the other's, and `↵` in the fullscreen retargeting a pane in a tab
/// nobody is looking at — which is a key that appears to do nothing.
fn my_view(ws: &str, me: Option<&str>) -> Option<String> {
    let panes = list_panes(ws).ok()?;
    let mine = me.and_then(|id| panes.iter().find(|p| p.id == id)).map(|p| p.tab.clone());
    panes
        .iter()
        .filter(|p| p.label == VIEW_LABEL)
        .find(|p| mine.as_ref().is_none_or(|tab| &p.tab == tab))
        .map(|p| p.id.clone())
}

/// Shut this panel's detail pane, if it has one.
pub(super) fn close_view(store: &Store, self_ws: Option<&str>, me: Option<&str>) -> bool {
    let Some(ws) = stage(self_ws) else { return false };
    let ws = ws.as_str();
    crate::detail::set_focus(store, ws, &crate::detail::Focus::Nothing);
    match my_view(ws, me) {
        Some(pane) => herdr::call("pane.close", json!({ "pane_id": pane })).is_ok(),
        None => false,
    }
}

/// The whole tree, in a tab of its own.
///
/// The panel drawn at the width of the workspace rather than of a sidebar: the
/// same rows, the same one row to a line, and a row wide enough to say what the
/// work is instead of its first twenty-five characters. It is a second process
/// and that costs nothing, because the folds, the filters and the cursor are in
/// the store — two panels on one tree are the same panel, which is the whole
/// bargain `panel-view.json` makes.
///
/// This was `pane.zoom` first, and a zoom is not a bigger pane: it is a display
/// mode over the whole tab, set by one pane and outliving it. See
/// [`super::FULL_LABEL`] for what that cost and how it was measured.
///
/// One at a time. A second one would be a second window onto a view both of
/// them read out of the same file, so pressing `Z` again from the sidebar goes
/// to the one that is open rather than making another.
///
/// **From a surface this is still a tab, and it is the one thing on this page
/// that ought not to be.** `Z` from a sidebar should widen the sidebar: the
/// whole tree in place, at the width the reader already has their eyes on.
/// That takes a message this side does not have — the host owns the rect, and
/// today the wire runs one way for anything but frames — so `Z` opens the tab
/// it has always opened rather than becoming a key that does nothing. When the
/// host can be asked, this is where the asking goes.
pub(super) fn open_full(self_ws: Option<&str>) -> String {
    let Some(ws) = stage(self_ws) else { return "no workspace on screen to open a tab in".into() };
    let ws = ws.as_str();

    if let Some(open) = list_panes(ws).ok().and_then(|ps| ps.into_iter().find(|p| p.label == FULL_LABEL)) {
        let _ = herdr::call("tab.focus", json!({ "tab_id": open.tab }));
        let _ = herdr::call("pane.focus", json!({ "pane_id": open.id }));
        return "the whole tree".into();
    }

    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "wsp".into());
    let Ok(r) = herdr::call(
        "tab.create",
        json!({ "workspace_id": ws, "label": "wsp", "focus": true, "env": crate::util::store_env() }),
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
    // `exec`, so the command *is* the pane: `q` quits the panel, the pane goes
    // and the tab with it. A shell left behind would be a tab you close twice.
    let _ = herdr::call("pane.rename", json!({ "pane_id": pane, "label": FULL_LABEL }));
    let _ = herdr::call(
        "pane.send_text",
        json!({ "pane_id": pane, "text": format!("exec {} panel --full\n", shell_quote(&exe)) }),
    );
    "the whole tree · q closes it".into()
}

/// Open a board full-size in a tab of its own.
///
/// The plain half of [`pop_out`]: a tab, its root pane, and the command in it.
/// No splits, no editors, no marker files — a board wants the whole width and
/// draws its own screen.
///
/// A tab from a surface too, and deliberately: a board is columns of cards
/// across a workspace, and a sidebar is one column of anything. This is the
/// same asymmetry [`pop_out`] argues, for the same reason.
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
    let Some(ws) = stage(self_ws) else { return "no workspace on screen to open a tab in".into() };
    let ws = ws.as_str();
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
///
/// **Still a tab from a surface, and that is not an oversight.** Everything
/// else the sidebar rents is being given back — the panel itself is drawn by
/// the host now, and `↵` opens a pane beside the work rather than one this
/// process owns — but an editor is not chrome. `wsp edit` runs `$EDITOR` on
/// prose in the reader's own terminal, and prose in a thirty-column strip is
/// worse than prose in a tab, whoever is drawing the strip. The rule under
/// both: the panel stops renting screen for *itself*, and goes on opening the
/// full-size things a person asked for.
pub(crate) fn pop_out(argv: &[String], label: &str, self_ws: Option<&str>) -> String {
    let Some(ws) = stage(self_ws) else { return "no workspace on screen to open a tab in".into() };
    let ws = ws.as_str();
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

/// The pane the detail is split off.
///
/// Three answers, in the order they stop being true:
///
/// 1. **Our own pane.** The detail shares the panel's column and the pane
///    being worked in beside it is never touched. By pane id rather than by
///    label, because the fullscreen panel `Z` opens is not the pane labelled
///    `wsp` and a detail split off the sidebar two tabs away is one it cannot
///    see. Checked against this workspace's panes: a pane id from somewhere
///    else is not one to split here — see [`super::run::Where`] for how a
///    surface comes to be handed one.
/// 2. **The pane labelled `wsp`**, for a caller with no pane of its own. A
///    board hands a task to the *sidebar's* detail pane and then closes.
/// 3. **The widest pane that is not ours**, which is what a surface gets and
///    is the same choice `panel install` makes about where a sidebar goes. The
///    detail belongs beside the work, and from outside every workspace the
///    only thing that says where the work is, is how much room it was given.
///
/// `None` is an empty workspace: nothing to split, and nothing to say about it
/// beyond that.
fn view_target(ws: &str, me: Option<&str>, panes: &[PaneInfo]) -> Option<String> {
    if let Some(mine) = me.filter(|id| panes.iter().any(|p| p.id == *id)) {
        return Some(mine.to_string());
    }
    if let Some(panel) = panes.iter().find(|p| p.label == PANEL_LABEL) {
        return Some(panel.id.clone());
    }
    widest(ws, panes).map(|p| p.id.clone())
}

/// Point the workspace's detail pane at something, making one if there is not
/// one yet.
///
/// One pane per workspace, reused: opening a second thing retargets the pane
/// you are already reading rather than stacking another beside it. The target
/// goes through a file the view polls, so retargeting costs no process churn —
/// the alternative, killing and relaunching, would blink the pane on every
/// press of a key whose whole job is to be cheap.
pub(crate) fn inspect(
    store: &Store,
    self_ws: Option<&str>,
    focus: &crate::detail::Focus,
    me: Option<&str>,
) -> String {
    let Some(ws) = stage(self_ws) else {
        return "no workspace on screen to open a view in".into();
    };
    let ws = ws.as_str();
    crate::detail::set_focus(store, ws, focus);

    if my_view(ws, me).is_some() {
        return String::new();
    }

    let panes = match list_panes(ws) {
        Ok(panes) => panes,
        Err(e) => return e,
    };
    let Some(target) = view_target(ws, me, &panes) else {
        return "no pane there to open a view beside".into();
    };
    let res = herdr::call(
        "pane.split",
        json!({ "direction": "down", "target_pane_id": target, "ratio": 0.45, "focus": false }),
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
        // Then the search. It is the thing most in front of you — a tree with
        // four rows in it where there were three hundred — and the one filter
        // whose whole life is measured in seconds, so it is the first of them
        // this chain gives back.
        Key::Char('q') | Key::Esc if !view.filter.is_empty() => {
            view.filter.clear();
            say(ui, "the whole tree");
            Effect::Refetch
        }
        Key::Char('q') | Key::Esc if view.showing.is_some() => Effect::CloseView,
        // …and in the tab `Z` opened, the panel itself is what is in front of
        // you, so the same keys close that. It is not installed furniture —
        // nothing is lost by quitting it and `Z` opens it again — and a
        // fullscreen with no `q` is one you go looking for the way out of.
        Key::Char('q') | Key::Esc if view.full => Effect::Quit,
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
        // The same two keys over more than one row, and the three scopes read
        // in the order the map lists them: a row, the branch it is standing in,
        // the lot. `<` and `>` because they are the arrows `←` and `→` said
        // louder, which is exactly what they do — and what they save is the
        // walk: thirty-one projects, nested, is a lot of rows to fold one at a
        // time to get back to a tree you can read.
        Key::Char('<') => super::keys::fold_branch(true, ui, view),
        Key::Char('>') => super::keys::fold_branch(false, ui, view),
        Key::Char('H') => super::keys::fold_tree(true, ui, view),
        Key::Char('L') => super::keys::fold_tree(false, ui, view),
        Key::Char('g') | Key::Home => {
            // The first row that takes the cursor, which in the agents view is
            // not the first line: a project heading stands over the first run.
            ui.sel = 0;
            if !ui.rows.is_empty() && !ui.rows[0].selectable() {
                ui.sel = super::keys::step(&ui.rows, 0, true);
            }
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
            // A filled slot goes to the custodian's terminal, for the same
            // reason a pane row does. An empty one has no terminal to go to and
            // says the thing you would do about that instead — the row exists
            // to be filled, and nothing else here fills it.
            Target::Seat(project) => match ui.rows.get(ui.sel).and_then(|r| r.agent()) {
                Some(a) => Effect::Focus(a.clone()),
                None => {
                    say(ui, format!("empty · wsp spawn -p {project} --govern"));
                    Effect::None
                }
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
                // A search goes for the same reason and a stronger one: it is
                // a question about work, and this view has no work in it to
                // narrow. It would sit in the footer over a list of panes it
                // had not touched.
                view.filter.clear();
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
        // The finding aid. Two hundred and seventy-six tasks across thirty-one
        // projects is past what anybody scrolls: Ed's words were "I'm
        // struggling to find issues in this list now we have literally
        // hundreds". `/` because that is the key this is in everything else,
        // and it opens holding whatever is already filtered so a search can be
        // widened by a backspace instead of retyped.
        Key::Char('/') => {
            // Asked in the agents view it means the tree, the same way `R`
            // does: this is a question about work, and that view holds none.
            let back = std::mem::take(&mut view.agents);
            view.mode = Mode::Find { buffer: view.filter.clone() };
            match back {
                true => Effect::Refetch,
                false => Effect::None,
            }
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
        // The whole tree, in a tab. From the tab itself the same key is the way
        // out — one key for one idea, and a fullscreen that opens with `Z` and
        // closes with something else is a fullscreen you leave open.
        Key::Char('Z') if view.full => Effect::Quit,
        Key::Char('Z') => Effect::Full,
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
                    armed: false,
                };
                Effect::None
            }
            other => match scope(other, ui) {
                Some(project) => {
                    view.mode = Mode::Prompt {
                        verb: Ask::AddTask { project, parent: None },
                        buffer: String::new(),
                        armed: false,
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
            view.mode = Mode::Prompt {
                verb: Ask::NewProject { parent },
                buffer: String::new(),
                armed: false,
            };
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
                view.mode = Mode::Prompt {
                    verb: Ask::Block { task: id.clone() },
                    buffer: String::new(),
                    armed: false,
                };
                Effect::None
            }
            _ => {
                say(ui, "only a task can be blocked");
                Effect::None
            }
        },
        // Beside `b`, and asking for a sentence rather than doing it silently,
        // for `b`'s reason: a stopped task that does not say why is the one you
        // cannot act on later. Here the sentence is a condition rather than a
        // question, which is the whole difference between the two keys.
        Key::Char('p') => match &target {
            Target::Task(id) => {
                view.mode = Mode::Prompt {
                    verb: Ask::Park { task: id.clone() },
                    buffer: String::new(),
                    armed: false,
                };
                Effect::None
            }
            _ => {
                say(ui, "only a task can be parked");
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
                    armed: false,
                };
                Effect::None
            }
            Target::Project(p) => {
                let from = ui.name_of_project(p).unwrap_or_default();
                view.mode = Mode::Prompt {
                    verb: Ask::RenameProject { project: p.clone(), from: from.clone() },
                    buffer: from,
                    armed: false,
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
                view.mode = Mode::Prompt {
                    verb: Ask::Note { task: id.clone() },
                    buffer: String::new(),
                    armed: false,
                };
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
        Key::Char('C') => hand_over(&target, ui, view),
        // The other direction. `c` joins a task to an agent; this takes the
        // join apart from whichever end you are looking at it from.
        Key::Char('u') => unassign(&target, ui, view),
        // The dock's own verb. `c` needs you to have decided what the work is;
        // this needs only that there is some.
        //
        // Not on a seat, even though the row answers with an agent: a custodian
        // sent to find its own work is a custodian that has stopped being one,
        // and the whole failure this position was built out of is a governor
        // that picked up a task to have somewhere to stand.
        Key::Char('f') if matches!(target, Target::Seat(_)) => {
            say(ui, "a governor is not looking for work — T talks to it");
            Effect::None
        }
        Key::Char('f') => match ui.rows.get(ui.sel).and_then(|r| r.agent()).cloned() {
            Some(a) => find_work(&a, ui, view),
            None => {
                say(ui, "f sets an agent looking — aim it at one");
                Effect::None
            }
        },

        // ---- a word with the custodian ----
        //
        // The half of a position that a record could never be: you can see the
        // governor of `wsp` in the tree, and this is how you say something to
        // it. Everywhere else in the panel a key hands an agent *work*; this
        // hands it a sentence, because the job in that slot is the conversation
        // — sequencing, direction, correction — and until now every word of it
        // happened outside wsp, in a terminal somebody had to go and find.
        Key::Char('T') => match &target {
            Target::Seat(project) => {
                view.mode = Mode::Prompt {
                    verb: Ask::Tell { project: project.clone() },
                    buffer: String::new(),
                    armed: false,
                };
                Effect::None
            }
            _ => {
                say(ui, "T talks to a project's governor — aim at one");
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
        // Two keys rather than one that asks which, because they are two
        // different decisions and only one of them is expensive: `O` is a
        // place to work and `S` is a colleague, and the second costs a model
        // and a context window. That asymmetry is also why only `S` puts a y/n
        // in front of itself — a single key that first asked *which* would
        // have put a question in front of the cheap one every time.
        Key::Char('O') => spawn(&target, ui, view, false),
        Key::Char('S') => spawn(&target, ui, view, true),

        // ---- destructive ----
        Key::Char('X') => match &target {
            Target::Task(id) => ask(
                view,
                format!("retire {id}?"),
                Effect::Run { argv: vec!["rm".into(), id.clone()], escalate: None, then: None },
            ),
            // Deliberately without --force. If the project still holds work
            // the CLI refuses, and that refusal becomes the next question
            // rather than something the panel quietly overrode.
            Target::Project(p) => ask(
                view,
                format!("remove {p}?"),
                Effect::Run {
                    argv: vec!["project".into(), "rm".into(), p.clone()],
                    escalate: Some(vec![
                        "project".into(),
                        "rm".into(),
                        p.clone(),
                        "--force".into(),
                    ]),
                    then: None,
                },
            ),
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
fn spawn(target: &Target, ui: &mut Ui, view: &mut View, agent: bool) -> Effect {
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
    // The one place the two keys differ beyond the agent, and it is the reason
    // `spawn` no longer focuses on its own: `O` is somebody asking for a
    // workspace to stand in and a key that opened one somewhere off-screen would
    // read as having done nothing, while `S` is somebody handing work over and
    // then carrying on reading the panel. Said here rather than defaulted in the
    // CLI, because the CLI's other caller is the queue and it wants neither.
    match agent {
        true => argv.push("--agent".into()),
        false => argv.push("--focus".into()),
    }
    let note = match agent {
        true => format!("starting an agent on {what}…"),
        false => format!("opening {what}…"),
    };
    let deed = Effect::Spawn { argv, note };
    // Only the expensive half asks. A workspace is a directory and a pane; an
    // agent is a model, a context window and a colleague who will start
    // working on whatever the cursor happened to be on — and `S` sits one
    // shift away from the `s` that starts a task.
    match agent {
        true => ask(view, format!("start an agent on {what}?"), deed),
        false => deed,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(id: &str, label: &str) -> PaneInfo {
        PaneInfo { id: id.into(), label: label.into(), tab: "t1".into() }
    }

    /// The panel's own column, which is what makes `↵` cheap: the detail
    /// arrives under the tree and the pane somebody is working in beside it is
    /// not touched at all.
    #[test]
    fn a_panel_splits_its_own_pane_so_the_detail_lands_under_the_tree() {
        let panes = vec![pane("w1:p1", ""), pane("w1:p2", PANEL_LABEL)];

        assert_eq!(view_target("w1", Some("w1:p2"), &panes).as_deref(), Some("w1:p2"));
    }

    /// The `Z` panel is a panel and is not the pane labelled `wsp`, so the
    /// label alone would send its detail to a sidebar two tabs away — a key
    /// that appears to do nothing.
    #[test]
    fn the_fullscreen_panel_splits_itself_rather_than_the_sidebar_it_came_from() {
        let panes = vec![pane("w1:p2", PANEL_LABEL), pane("w1:p7", FULL_LABEL)];

        assert_eq!(view_target("w1", Some("w1:p7"), &panes).as_deref(), Some("w1:p7"));
    }

    /// The environment lies to a surface, and this is where the lie would do
    /// damage: herdr hands its child its own environment, so a herdr started
    /// from inside a herdr pane gives the sidebar a `HERDR_PANE_ID` belonging
    /// to a pane in some other workspace. Splitting that would put a detail
    /// pane in front of whoever is working there. See [`super::run::Where`].
    #[test]
    fn a_pane_id_that_is_not_in_this_workspace_is_not_one_to_split() {
        let panes = vec![pane("w1:p1", "")];

        assert_eq!(view_target("w1", Some("w9:p4"), &panes).as_deref(), Some("w1:p1"));
    }

    /// What a surface gets, and the reason `↵` works from a sidebar that is in
    /// no workspace at all: the detail goes beside the work. Our own panes are
    /// never the answer — a detail split off a detail is seven characters wide,
    /// which is how this was found the first time.
    #[test]
    fn a_surface_has_no_pane_so_the_detail_goes_beside_the_work() {
        let panes = vec![
            pane("w1:p1", ""),
            pane("w1:p3", VIEW_LABEL),
            pane("w1:p4", BOARD_LABEL),
        ];

        assert_eq!(view_target("w1", None, &panes).as_deref(), Some("w1:p1"));
    }

    /// The board's case: it opens a task in the *sidebar's* detail pane and
    /// then closes itself, so it asks with no pane of its own to offer.
    #[test]
    fn a_caller_with_no_pane_of_its_own_hands_the_task_to_the_sidebar() {
        let panes = vec![pane("w1:p1", ""), pane("w1:p2", PANEL_LABEL)];

        assert_eq!(view_target("w1", None, &panes).as_deref(), Some("w1:p2"));
    }

    /// A workspace whose only panes are ours. Nothing to split, and the caller
    /// says so rather than sending `pane.split` an id it invented.
    #[test]
    fn a_workspace_with_nothing_but_our_own_panes_has_nowhere_to_put_a_view() {
        let panes = vec![pane("w1:p3", VIEW_LABEL)];

        assert_eq!(view_target("w1", None, &panes), None);
    }

    /// A panel acts on the workspace it is in, and never asks which one is on
    /// screen: it is looking at its own workspace's panes, and answering about
    /// the one somebody has just clicked into would split a pane in a tree it
    /// is not drawing. The laziness here is the whole of that guarantee — with
    /// no herdr to ask, this test only passes because the question is not put.
    #[test]
    fn a_panel_acts_on_its_own_workspace_and_not_on_whichever_is_focused() {
        assert_eq!(stage(Some("w4")).as_deref(), Some("w4"));
    }
}
