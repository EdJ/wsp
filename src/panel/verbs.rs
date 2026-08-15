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

use serde_json::json;

use crate::herdr;
use crate::input::Key;
use crate::store::Store;
use crate::util;

use super::install::{list_panes, store_env};
use crate::util::shell_quote;
use super::keys::{move_or_fold, say, Effect, Mode, View};
use super::rows::{hotkeys, AgentRef, Target, Ui};
use super::{PANEL_LABEL, VIEW_LABEL};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Ask {
    AddTask { project: Option<String>, parent: Option<String> },
    NewProject { parent: Option<String> },
    Block { task: String },
    Rename { task: String },
    Note { task: String },
}

impl Ask {
    pub(super) fn label(&self) -> &'static str {
        match self {
            Ask::AddTask { parent: Some(_), .. } => "sub-task",
            Ask::AddTask { .. } => "task",
            Ask::NewProject { .. } => "project",
            Ask::Block { .. } => "why",
            Ask::Rename { .. } => "title",
            Ask::Note { .. } => "note",
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
            Ask::Rename { task } => vec!["rename".into(), task.clone(), v],
            Ask::Note { task } => vec!["note".into(), task.clone(), v],
        }
    }
}

/// Open a workspace for a piece of work, rooted where that work lives.
///
/// `WSP_PROJECT` and `WSP_TASK` go into the workspace environment, so every
/// pane inside it knows what it is for without anyone having to infer it from
/// a path. herdr does not persist env across a restart, which is why the
/// durable answer is a claim rather than this — but for the life of the
/// session it is exact, and exactness is what the cwd heuristic lacks.
pub(super) fn open_workspace(
    label: &str,
    cwd: Option<&str>,
    project: Option<&str>,
    task: Option<&str>,
) -> Result<(String, String), String> {
    let mut env = serde_json::Map::new();
    if let Some(p) = project {
        env.insert("WSP_PROJECT".into(), json!(p));
    }
    if let Some(t) = task {
        env.insert("WSP_TASK".into(), json!(t));
    }
    // The store first, then what this workspace is for — the latter wins if
    // someone has both, which is right: it is more specific.
    let mut merged = store_env();
    merged.extend(env);
    let mut params = json!({ "label": label, "env": merged, "focus": true });
    if let Some(c) = cwd {
        params["cwd"] = json!(util::expand(c).display().to_string());
    }
    let r = herdr::call("workspace.create", params).map_err(|e| e.to_string())?;
    let ws = r
        .get("workspace")
        .and_then(|w| w.get("workspace_id"))
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "workspace.create returned no id".to_string())?;
    // The pane it opened with — what `claim` needs to bind to, since claim
    // speaks in panes and knows nothing about workspaces.
    let pane = r
        .get("root_pane")
        .and_then(|p| p.get("pane_id"))
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "workspace.create returned no pane".to_string())?;
    Ok((ws, pane))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Pick {
    /// Move a task: land on a project, or on the inbox to unfile it.
    MoveTask { task: String },
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
            Pick::MoveTask { .. } => "move to which project?",
            Pick::PaneForTask { .. } => "which agent takes it?",
            Pick::TaskForPane { .. } => "which task does it take?",
            Pick::WorkForAgent { .. } => "which project does it work?",
        }
    }

    /// `None` when the cursor is somewhere this pick cannot accept.
    pub(super) fn argv(&self, at: &Target) -> Option<Vec<String>> {
        match (self, at) {
            (Pick::MoveTask { task }, Target::Project(p)) => {
                Some(vec!["mv".into(), task.clone(), "-p".into(), p.clone()])
            }
            // Unfiling. `mv` already understands `inbox` as "no project".
            (Pick::MoveTask { task }, Target::Inbox) => {
                Some(vec!["mv".into(), task.clone(), "-p".into(), "inbox".into()])
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
    /// appears.
    pub(super) fn escalate(&self, argv: &[String]) -> Option<Vec<String>> {
        match self {
            Pick::PaneForTask { .. } | Pick::TaskForPane { .. } => {
                let mut forced = argv.to_vec();
                forced.push("--force".into());
                Some(forced)
            }
            Pick::MoveTask { .. } | Pick::WorkForAgent { .. } => None,
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
    pub(crate) text: String,
    /// Thirty-four columns' worth. The sentence above is for the agent; this is
    /// for the person who pressed the key.
    pub(crate) note: String,
}

/// Send an agent looking for its own work.
pub(super) fn tell_find_work(a: &AgentRef, project: &str) -> Tell {
    Tell {
        pane: a.pane.clone(),
        text: format!(
            "Find your next piece of work: run `wsp next -p {project}`, `wsp claim` what it \
             names, then do it. If nothing is actionable, say so and stop."
        ),
        note: format!("{} → looking in {project}", a.where_),
    }
}

/// Tell an agent about a task it has just been handed.
///
/// `wsp brief` rather than the id alone: the brief is what a session gets on
/// the way in, so an agent handed work mid-session lands in the same place it
/// would have started from — its project, its claim, the decisions that bind it
/// and who else is in the tree.
pub(super) fn tell_claimed(a: &AgentRef, task: &str) -> Tell {
    Tell {
        pane: a.pane.clone(),
        text: format!("You have been claimed onto {task}. Run `wsp brief`, then work it."),
        note: format!("{} → {task}", a.where_),
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

/// Type a sentence into a pane and press return.
///
/// Two writes with a pause between, the same bargain the editor panes make: a
/// TUI that takes a burst of input as a paste swallows the return on the end of
/// it, and the sentence then sits in the prompt unsent — which looks exactly
/// like an agent that read the instruction and ignored it.
pub(super) fn send_tell(t: &Tell) -> Result<(), String> {
    herdr::call("pane.send_text", json!({ "pane_id": t.pane, "text": t.text }))
        .map_err(|e| e.to_string())?;
    std::thread::sleep(std::time::Duration::from_millis(150));
    herdr::call("pane.send_text", json!({ "pane_id": t.pane, "text": "\r" }))
        .map_err(|e| e.to_string())?;
    Ok(())
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

/// Open a file full-size in a tab of its own, in the user's editor.
///
/// A tab rather than a split: the store is Markdown and editing a task means
/// its whole body — notes, acceptance criteria, the log — which wants width,
/// and a tab gives that without disturbing a layout you will come back to.
pub(super) fn pop_out(argv: &[String], label: &str, self_ws: Option<&str>) -> String {
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
        json!({ "workspace_id": ws, "label": label, "focus": true, "env": store_env() }),
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
pub(super) fn inspect(store: &Store, self_ws: Option<&str>, focus: &crate::detail::Focus) -> String {
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
pub(super) struct Made {
    pub(super) label: String,
    pub(super) id: Option<String>,
}

pub(super) fn run_wsp(argv: &[String]) -> Result<Made, String> {
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
        // The tree names work by title, which is what you read it for. The id
        // is what you type at a shell, and there was no way to get from one to
        // the other without opening the row.
        Key::Char('i') => {
            view.ids = !view.ids;
            say(ui, if view.ids { "showing ids" } else { "hiding ids" });
            Effect::Refetch
        }
        Key::Char('r') => Effect::Sync,
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
        Key::Char('e') => match &target {
            Target::Task(id) => {
                view.mode =
                    Mode::Prompt { verb: Ask::Rename { task: id.clone() }, buffer: String::new() };
                Effect::None
            }
            _ => {
                say(ui, "only a task can be retitled here");
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
        Key::Char('m') => match &target {
            Target::Task(id) => begin(view, Pick::MoveTask { task: id.clone() }),
            _ => {
                say(ui, "only a task moves");
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
        // The dock's own verb. `c` needs you to have decided what the work is;
        // this needs only that there is some.
        Key::Char('f') => match ui.rows.get(ui.sel).and_then(|r| r.agent()).cloned() {
            Some(a) => find_work(&a, ui, view),
            None => {
                say(ui, "f sets an agent looking — aim it at one");
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

        // ---- open a workspace for this row ----
        Key::Char('O') => match &target {
            Target::Task(id) => {
                let project = ui.project_of_task(id);
                match ui.task_title(id) {
                    Some(title) => Effect::Open {
                        label: title,
                        cwd: project.as_deref().and_then(|p| ui.project_root(p)),
                        project,
                        task: Some(id.clone()),
                    },
                    None => Effect::None,
                }
            }
            Target::Project(p) => Effect::Open {
                label: p.clone(),
                cwd: ui.project_root(p),
                project: Some(p.clone()),
                task: None,
            },
            _ => {
                say(ui, "nothing there to open a workspace for");
                Effect::None
            }
        },

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

