//! `wsp panel` — the sidebar replacement.
//!
//! herdr's sidebar lists workspaces (really: open folders) and, beneath them,
//! agents — a view that falls out of how herdr is built rather than how work is
//! organised. This panel inverts that: the spine is the project tree, tasks
//! hang off projects, and agents attach to tasks. A workspace is only ever a
//! destination, never a heading.
//!
//! It keeps the two things herdr's sidebar is genuinely good at — live agent
//! status and jumping to a terminal — by subscribing to the same event stream
//! and calling `workspace.focus`/`pane.focus` on Enter.
//!
//! No TUI crate: we own one pane, so a string buffer plus ANSI is enough, and
//! the dependency list stays at `serde_json`.

use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::herdr;
use crate::model::{Status, Task};
use crate::resolve::{self, Counts, Index};
use crate::store::Store;
use crate::util;

const ACCENT: &str = "\x1b[38;2;95;191;164m";
const WARN: &str = "\x1b[38;2;224;138;75m";
const MUTED: &str = "\x1b[38;2;125;140;150m";
const DIMC: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const INV: &str = "\x1b[7m";
const OFF: &str = "\x1b[0m";

const MAX_TASKS_PER_PROJECT: usize = 6;
const MAX_PANES_PER_PROJECT: usize = 4;

/// Stand in for the groups that are not projects, wherever a project id is used
/// as a key. Project ids are slugs, so the parentheses keep these out of their
/// namespace.
const INBOX_KEY: &str = "(inbox)";
const NOPROJECT_KEY: &str = "(noproject)";

/// herdr's label on the panes we install. Ours are furniture and never appear
/// in the tree as work.
pub(crate) const PANEL_LABEL: &str = "wsp";

/// What the viewer has folded, unfolded, or asked to see more of. Held by the
/// event loop and handed to `collect`, which is otherwise a pure function of
/// the store plus herdr.
#[derive(Default)]
pub(crate) struct View {
    /// Projects whose children and tasks are hidden.
    collapsed: HashSet<String>,
    /// Projects (or the inbox) showing past `MAX_TASKS_PER_PROJECT`.
    expanded: HashSet<String>,
    /// Include `done` tasks, and the projects that hold only those.
    show_done: bool,
    /// Projects to show even though they hold nothing yet. A project created
    /// from the panel is empty by definition, so the quiet-branch filter would
    /// swallow it the instant it was made — you would type a name, press
    /// return, and watch nothing appear.
    reveal: HashSet<String>,
    /// What the next keypress means.
    pub(crate) mode: Mode,
    /// What the detail pane is currently showing, so `↵` can close it.
    showing: Option<crate::detail::Focus>,
    /// The key map, docked under the tree. A line in the footer could hold four
    /// of the twenty keys, which is worse than useless: it says there is a list
    /// and then shows you a fifth of it. It takes the rows it needs and no
    /// more, because you read it to press one of the keys in it — and the row
    /// you would press it on has to still be there, and still be selected.
    help: bool,
}

/// Management needs three shapes of input beyond a single key: a value to
/// type, a second row to point at, and a yes before something irreversible.
/// Each is a mode rather than a widget, so the cursor and the tree keep
/// working inside them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum Mode {
    #[default]
    Browse,
    Prompt {
        verb: Ask,
        buffer: String,
    },
    /// Navigation still moves the cursor; `↵` takes whatever it lands on.
    Pick {
        verb: Pick,
    },
    Confirm {
        argv: Vec<String>,
        question: String,
        /// What to offer if `argv` is refused. The panel does not decide on
        /// your behalf that a refusal should be overridden — it puts the
        /// refusal on screen and asks again.
        escalate: Option<Vec<String>>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Ask {
    AddTask { project: Option<String> },
    NewProject { parent: Option<String> },
    Block { task: String },
    Rename { task: String },
    Note { task: String },
}

impl Ask {
    fn label(&self) -> &'static str {
        match self {
            Ask::AddTask { .. } => "task",
            Ask::NewProject { .. } => "project",
            Ask::Block { .. } => "why",
            Ask::Rename { .. } => "title",
            Ask::Note { .. } => "note",
        }
    }

    /// The command this becomes once a value is typed.
    fn argv(&self, value: &str) -> Vec<String> {
        let v = value.trim().to_string();
        match self {
            Ask::AddTask { project: Some(p) } => {
                vec!["add".into(), v, "-p".into(), p.clone()]
            }
            Ask::AddTask { project: None } => vec!["add".into(), v],
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
fn open_workspace(
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
    let mut params = json!({ "label": label, "env": env, "focus": true });
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
}

impl Pick {
    fn hint(&self) -> &'static str {
        match self {
            Pick::MoveTask { .. } => "move to which project?",
            Pick::PaneForTask { .. } => "which agent takes it?",
            Pick::TaskForPane { .. } => "which task does it take?",
        }
    }

    /// `None` when the cursor is somewhere this pick cannot accept.
    fn argv(&self, at: &Target) -> Option<Vec<String>> {
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
            _ => None,
        }
    }
}

enum Msg {
    Key(Key),
    /// Carries the workspace the event was about, when it named one.
    Herdr(Option<String>),
    Tick,
}

/// A key as typed, not as interpreted. `j` used to arrive already meaning
/// "down", which is unanswerable once a prompt needs a literal `j` — so the
/// meaning is decided by the reducer, which knows the mode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Key {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Esc,
    Backspace,
    Char(char),
    /// Ctrl-C, which raw mode delivers as a byte rather than a signal.
    Interrupt,
}

#[derive(Debug, Clone)]
pub(crate) struct AgentRef {
    pane: String,
    workspace: String,
    state: String,
    /// What to call it: the agent's terminal title if there is one, otherwise
    /// the workspace label.
    where_: String,
    /// Whether an agent is running here, or it is just a shell.
    agent: bool,
}

#[derive(Debug, Clone)]
enum Row {
    Project {
        id: String,
        depth: usize,
        counts: Counts,
        collapsed: bool,
        live: usize,
        prose: bool,
    },
    Task {
        /// Carried so a row can name the task it stands for. Nothing acts on a
        /// task yet; this is what the edit keys will dispatch on.
        id: String,
        /// The project it sits under, so `a` on a task can add a sibling and
        /// the row can answer questions without going back to the store.
        project: Option<String>,
        title: String,
        depth: usize,
        status: Status,
        agent: Option<AgentRef>,
        needs_you: bool,
        /// Something is written in Overview or Details. Worth a mark: the
        /// whole point of writing it down is being reminded it is there.
        prose: bool,
    },
    /// `key` is the project the hidden tasks belong to, or `INBOX_KEY`.
    More { key: String, depth: usize, n: usize },
    /// A group that is not a project: the inbox, loose agents. `key` names it
    /// so it can be folded and so a command can be aimed at it.
    Section { key: String, label: String, count: usize, collapsed: bool },
    Agent { agent: AgentRef, title: String, depth: usize },
}

impl Row {
    /// Every row takes the cursor. A heading you cannot select is a heading you
    /// cannot fold or add to, and both are things the groups need.
    fn selectable(&self) -> bool {
        true
    }
    /// The pane this row *is*. A task that has one is not it — the pane sits
    /// on its own row directly beneath, and letting both answer meant two
    /// hotkeys landing on the same terminal.
    fn agent(&self) -> Option<&AgentRef> {
        match self {
            Row::Agent { agent, .. } => Some(agent),
            _ => None,
        }
    }
}

/// Which sort of row the cursor is on. Exposed so a script can drive to a row
/// by asking, rather than hard-coding how many presses it takes to get there —
/// a count that silently rots the moment a fixture gains a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RowKind {
    Project,
    Task,
    More,
    Section,
    Agent,
    Nothing,
}

pub(crate) struct Ui {
    rows: Vec<Row>,
    agents_total: usize,
    needs: usize,
    blocked: usize,
    sel: usize,
    message: Option<(String, Instant)>,
    self_focused: bool,
    show_done: bool,
    /// project id -> its first root, for opening a workspace where the work is.
    roots: std::collections::BTreeMap<String, String>,
}

/// What the cursor is sitting on, in the store's own terms rather than the
/// panel's. This is the seam the edit subcommands dispatch against: a command
/// asks what the target is and refuses the ones it cannot act on, instead of
/// every key having to re-read the row enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
    /// A project id — `wsp add -p <id>`, `wsp project …`.
    Project(String),
    /// A task id — `wsp start` / `done` / `block` / `mv`.
    Task(String),
    /// A group that is not a project. Adding here means a task with no
    /// project; there is nothing to remove.
    Inbox,
    /// Agents with no task. Nothing to add; the useful verb is `claim`.
    Unattached,
    /// A pane to jump to, not a thing to edit.
    Pane(String),
    /// The overflow row, which only ever opens.
    Overflow(String),
    Nothing,
}

/// What a row stands for, in the store's own terms. Pulled out of
/// `selected_target` because the cursor is kept on a row's identity across a
/// refetch, which means asking this of rows other than the selected one.
fn target_of(row: &Row) -> Target {
    match row {
        Row::Project { id, .. } => Target::Project(id.clone()),
        Row::Task { id, .. } => Target::Task(id.clone()),
        Row::More { key, .. } => Target::Overflow(key.clone()),
        Row::Agent { agent, .. } => Target::Pane(agent.pane.clone()),
        Row::Section { key, .. } if key == NOPROJECT_KEY => Target::Unattached,
        Row::Section { key, .. } if key == INBOX_KEY => Target::Inbox,
        Row::Section { .. } => Target::Nothing,
    }
}

impl Ui {
    pub(crate) fn selected_target(&self) -> Target {
        self.rows.get(self.sel).map(target_of).unwrap_or(Target::Nothing)
    }

    pub(crate) fn selected_kind(&self) -> RowKind {
        match self.rows.get(self.sel) {
            Some(Row::Project { .. }) => RowKind::Project,
            Some(Row::Task { .. }) => RowKind::Task,
            Some(Row::More { .. }) => RowKind::More,
            Some(Row::Section { .. }) => RowKind::Section,
            Some(Row::Agent { .. }) => RowKind::Agent,
            None => RowKind::Nothing,
        }
    }

    pub(crate) fn selected_index(&self) -> usize {
        self.sel
    }

    fn task_title(&self, task: &str) -> Option<String> {
        self.rows.iter().find_map(|r| match r {
            Row::Task { id, title, .. } if id == task => Some(title.clone()),
            _ => None,
        })
    }

    /// Where a project lives on disk, if it says. A project with no root has
    /// nowhere to open, so the workspace lands wherever herdr defaults to.
    fn project_root(&self, project: &str) -> Option<String> {
        self.roots.get(project).cloned()
    }

    /// Which project a task row sits under.
    fn project_of_task(&self, task: &str) -> Option<String> {
        self.rows.iter().find_map(|r| match r {
            Row::Task { id, project, .. } if id == task => project.clone(),
            _ => None,
        })
    }

}

// ---- data ---------------------------------------------------------------

/// Everything `collect` reads, gathered into one value.
///
/// `collect` used to call the store and herdr itself, which meant the only way
/// to see a frame was to have both running. Taking the inputs as data instead
/// lets a fixture stand one up and render it offline — the whole point of the
/// snapshot backend.
pub struct Snapshot {
    pub projects: Vec<crate::model::Project>,
    pub tasks: Vec<Task>,
    pub bindings: std::collections::BTreeMap<String, serde_json::Value>,
    pub pins: std::collections::BTreeMap<String, String>,
    pub workspaces: Vec<herdr::Workspace>,
    pub panes: Vec<herdr::Pane>,
}

impl Snapshot {
    /// The live path: read the store, ask herdr. A herdr that isn't answering
    /// degrades to an empty pane list rather than an error, so the panel still
    /// shows the durable half.
    fn live(store: &Store) -> Snapshot {
        Snapshot {
            projects: store.projects(),
            tasks: store.tasks(),
            bindings: store.bindings(),
            pins: store.pins(),
            workspaces: herdr::workspaces().unwrap_or_default(),
            panes: herdr::panes().unwrap_or_default(),
        }
    }
}

fn task_sort_key(t: &Task, has_agent: bool, needs_you: bool) -> (u8, u8, u8, String) {
    (
        u8::from(!needs_you),
        u8::from(!has_agent),
        t.status().rank(),
        t.id.clone(),
    )
}

/// Rows for the tasks of one project — or, when `project` is `None`, the tasks
/// belonging to no project at all. The inbox went unrendered for a while
/// because it had no equivalent of the project walk; sharing this makes that
/// class of omission impossible.
fn task_rows(
    tasks: &[Task],
    project: Option<&str>,
    depth: usize,
    view: &View,
    agent_for_task: &dyn Fn(&str) -> Option<AgentRef>,
    rows: &mut Vec<Row>,
    needs: &mut usize,
) {
    let mut mine: Vec<(&Task, Option<AgentRef>, bool)> = tasks
        .iter()
        .filter(|t| t.project.as_deref() == project)
        .filter(|t| view.show_done || t.status().is_open())
        .map(|t| {
            let a = agent_for_task(&t.id);
            let needs_you =
                a.as_ref().map(|a| a.state == "idle").unwrap_or(false) && t.status() == Status::Doing;
            (t, a, needs_you)
        })
        .collect();
    mine.sort_by_key(|(t, a, n)| task_sort_key(t, a.is_some(), *n));

    let key = project.unwrap_or(INBOX_KEY);
    let shown = if view.expanded.contains(key) {
        mine.len()
    } else {
        mine.len().min(MAX_TASKS_PER_PROJECT)
    };

    for (t, a, needs_you) in mine.iter().take(shown) {
        if *needs_you {
            *needs += 1;
        }
        rows.push(Row::Task {
            id: t.id.clone(),
            project: t.project.clone(),
            title: t.title.clone(),
            depth,
            status: t.status(),
            agent: a.clone(),
            needs_you: *needs_you,
            prose: crate::model::has_prose(&t.body),
        });
        // The pane working it hangs beneath, so the join is visible and the
        // task keeps its own status glyph instead of surrendering it to the
        // agent's — a claimed task used to look identical whether it was todo
        // or doing.
        if let Some(a) = a {
            rows.push(Row::Agent {
                title: a.where_.clone(),
                agent: a.clone(),
                depth: depth + 1,
            });
        }
    }
    if mine.len() > shown {
        rows.push(Row::More { key: key.to_string(), depth, n: mine.len() - shown });
    }
}

/// Rebuild the rows from a snapshot, keeping the cursor where it was and
/// carrying any pending message across. Shared with the storyboard so an
/// offline flow lands on the same row a live one would.
pub(crate) fn refetch_into(ui: &mut Ui, snap: &Snapshot, view: &View, self_ws: Option<&str>) {
    let sel = ui.sel;
    let msg = ui.message.take();
    *ui = collect(snap, view, self_ws);
    ui.sel = sel.min(ui.rows.len().saturating_sub(1));
    if !ui.rows.is_empty() && !ui.rows[ui.sel].selectable() {
        if let Some(next) = (ui.sel..ui.rows.len()).find(|i| ui.rows[*i].selectable()) {
            ui.sel = next;
        }
    }
    ui.message = msg;
}

pub(crate) fn collect(snap: &Snapshot, view: &View, self_ws: Option<&str>) -> Ui {
    let index = Index::new(snap.projects.clone());
    let tasks = snap.tasks.clone();
    let counts = resolve::counts_by_project(&index, &tasks);
    let bindings = &snap.bindings;
    let pins = &snap.pins;

    let workspaces = &snap.workspaces;
    // Our own panels are furniture, not work. Everything else is a pane
    // someone opened, agent or not — a shell sitting in a project is a fact
    // about that project whether or not an agent ever attaches to it.
    let panes: Vec<&herdr::Pane> =
        snap.panes.iter().filter(|p| p.label != PANEL_LABEL && p.label != VIEW_LABEL).collect();
    let self_focused = self_ws
        .and_then(|id| workspaces.iter().find(|w| w.id == id))
        .map(|w| w.focused)
        .unwrap_or(true);

    let ws_label = |id: &str| -> String {
        workspaces.iter().find(|w| w.id == id).map(|w| w.label.clone()).unwrap_or_default()
    };

    // pane -> task id
    let bound_task_of_pane = |pane: &str| -> Option<String> {
        bindings
            .get(pane)
            .and_then(|b| b.get("task_id"))
            .and_then(|t| t.as_str())
            .map(|s| s.to_string())
    };
    let as_ref = |a: &herdr::Pane| AgentRef {
        pane: a.pane_id.clone(),
        workspace: a.workspace_id.clone(),
        state: a.agent_status.clone(),
        where_: if a.title.is_empty() { ws_label(&a.workspace_id) } else { a.title.clone() },
        agent: !a.agent.is_empty(),
    };

    // task id -> the pane claimed to it
    let agent_for_task = |task_id: &str| -> Option<AgentRef> {
        panes.iter().find_map(|a| {
            (bound_task_of_pane(&a.pane_id).as_deref() == Some(task_id)).then(|| as_ref(a))
        })
    };

    // Place every unclaimed pane against a project. Claimed ones already have
    // a home: the task they are bound to.
    let mut live_by_project: std::collections::BTreeMap<String, usize> = Default::default();
    let mut loose_by_project: std::collections::BTreeMap<String, Vec<AgentRef>> = Default::default();
    let mut homeless: Vec<AgentRef> = Vec::new();

    for a in panes.iter() {
        let bound = bound_task_of_pane(&a.pane_id);
        let bound_project = bound
            .as_ref()
            .and_then(|id| tasks.iter().find(|t| &t.id == id))
            .and_then(|t| t.project.clone());
        let r = resolve::resolve(
            &index,
            pins,
            bound_project,
            Some(&a.workspace_id),
            Some(&ws_label(&a.workspace_id)),
            Some(&a.cwd),
        );
        match &r.project {
            Some(p) => {
                for id in std::iter::once(p.clone()).chain(index.ancestors(p)) {
                    *live_by_project.entry(id).or_insert(0) += 1;
                }
                if bound.is_none() {
                    loose_by_project.entry(p.clone()).or_default().push(as_ref(a));
                }
            }
            // Resolves to nothing, or the workspace is deliberately pinned out
            // of the tree. Either way it belongs to no project.
            None => {
                if bound.is_none() {
                    homeless.push(as_ref(a));
                }
            }
        }
    }

    // Quiet branches stay folded away, but a project holding only finished work
    // must reappear the moment `show_done` is on — otherwise the toggle looks
    // broken on exactly the projects it exists to reveal.
    let interesting = |id: &str| -> bool {
        let c = counts.get(id).copied().unwrap_or_default();
        c.open > 0
            || live_by_project.contains_key(id)
            || (view.show_done && c.done > 0)
            || view.reveal.contains(id)
    };

    let mut rows: Vec<Row> = Vec::new();
    let mut needs = 0;

    // Unparented tasks, first. They are the only work with nowhere to belong,
    // so they are what you triage before reading anything that already has a
    // home — and putting them last meant scrolling past every project to find
    // the one list that needs a decision.
    let inbox_open = tasks.iter().filter(|t| t.project.is_none() && t.status().is_open()).count();
    let inbox_any = tasks
        .iter()
        .any(|t| t.project.is_none() && (view.show_done || t.status().is_open()));
    if inbox_any {
        let folded = view.collapsed.contains(INBOX_KEY);
        rows.push(Row::Section {
            key: INBOX_KEY.to_string(),
            label: "inbox".into(),
            count: inbox_open,
            collapsed: folded,
        });
        if !folded {
            task_rows(&tasks, None, 1, view, &agent_for_task, &mut rows, &mut needs);
        }
    }

    // Depth-first over the project tree, skipping quiet branches.
    fn walk(
        index: &Index,
        parent: Option<&str>,
        depth: usize,
        rows: &mut Vec<Row>,
        counts: &std::collections::BTreeMap<String, Counts>,
        live: &std::collections::BTreeMap<String, usize>,
        view: &View,
        tasks: &[Task],
        loose: &std::collections::BTreeMap<String, Vec<AgentRef>>,
        interesting: &dyn Fn(&str) -> bool,
        agent_for_task: &dyn Fn(&str) -> Option<AgentRef>,
        needs: &mut usize,
    ) {
        for p in index.children(parent) {
            if !interesting(&p.id) {
                continue;
            }
            let is_collapsed = view.collapsed.contains(&p.id);
            rows.push(Row::Project {
                id: p.id.clone(),
                depth,
                counts: counts.get(&p.id).copied().unwrap_or_default(),
                collapsed: is_collapsed,
                live: live.get(&p.id).copied().unwrap_or(0),
                prose: crate::model::has_prose(&p.body),
            });
            if is_collapsed {
                continue;
            }

            // This project's own tasks, attention first.
            task_rows(tasks, Some(&p.id), depth + 1, view, agent_for_task, rows, needs);

            walk(
                index, Some(&p.id), depth + 1, rows, counts, live, view, tasks, loose, interesting,
                agent_for_task, needs,
            );

            // Then the panes that resolve here but are working on nothing
            // named — the ones the old panel could not see at all, because a
            // shell is not an agent and `agent.list` never reported them.
            //
            // After the children, and capped: a project whose root everything
            // shares collects a lot of these, and they must not push its own
            // subtree off the screen.
            if let Some(ps) = loose.get(&p.id) {
                let key = format!("{}/panes", p.id);
                let shown = if view.expanded.contains(&key) {
                    ps.len()
                } else {
                    ps.len().min(MAX_PANES_PER_PROJECT)
                };
                for a in ps.iter().take(shown) {
                    rows.push(Row::Agent {
                        title: a.where_.clone(),
                        agent: a.clone(),
                        depth: depth + 1,
                    });
                }
                if ps.len() > shown {
                    rows.push(Row::More { key, depth: depth + 1, n: ps.len() - shown });
                }
            }
        }
    }

    walk(
        &index,
        None,
        0,
        &mut rows,
        &counts,
        &live_by_project,
        view,
        &tasks,
        &loose_by_project,
        &interesting,
        &agent_for_task,
        &mut needs,
    );

    // Panes belonging to no project. Some are there because nothing resolved;
    // some because the workspace is deliberately pinned out of the tree — the
    // orchestrator's own home, and whatever else you opened that is not work.
    if !homeless.is_empty() {
        let folded = view.collapsed.contains(NOPROJECT_KEY);
        rows.push(Row::Section {
            key: NOPROJECT_KEY.to_string(),
            label: "no project".into(),
            count: homeless.len(),
            collapsed: folded,
        });
        if !folded {
            for a in homeless {
                let title = a.where_.clone();
                rows.push(Row::Agent { agent: a, title, depth: 1 });
            }
        }
    }

    Ui {
        rows,
        agents_total: panes.iter().filter(|p| !p.agent.is_empty()).count(),
        needs,
        blocked: tasks.iter().filter(|t| t.status() == Status::Blocked).count(),
        sel: 0,
        message: None,
        self_focused,
        show_done: view.show_done,
        roots: snap
            .projects
            .iter()
            .filter_map(|p| p.roots.first().map(|r| (p.id.clone(), r.clone())))
            .collect(),
    }
}

// ---- the view model -----------------------------------------------------

/// How a run of text is drawn. Kept beside the text rather than baked into it:
/// lines used to be built with the escapes already embedded, which is why
/// measuring one meant scanning back over them to discount the escapes. With
/// style held separately, width is a plain char count and the same frame can
/// go to a terminal or to a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Plain,
    Dim,
    Bold,
    Muted,
    Accent,
    Warn,
}

#[derive(Debug, Clone)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

#[derive(Debug, Clone, Default)]
pub struct Line {
    pub spans: Vec<Span>,
    /// Drawn inverse — the selected row.
    pub selected: bool,
}

impl Line {
    pub(crate) fn push(&mut self, style: Style, text: impl Into<String>) {
        let text = text.into();
        if !text.is_empty() {
            self.spans.push(Span { text, style });
        }
    }

    pub(crate) fn width(&self) -> usize {
        self.spans.iter().map(|s| s.text.chars().count()).sum()
    }

    pub(crate) fn pad(&mut self, n: usize) {
        self.push(Style::Plain, " ".repeat(n));
    }

    /// Pad or clip to exactly `w` columns.
    pub(crate) fn fit(&mut self, w: usize) {
        let have = self.width();
        if have < w {
            self.pad(w - have);
            return;
        }
        if have == w {
            return;
        }
        let mut left = w;
        let mut out: Vec<Span> = Vec::new();
        for s in self.spans.drain(..) {
            let n = s.text.chars().count();
            if n <= left {
                left -= n;
                out.push(s);
            } else {
                out.push(Span { text: s.text.chars().take(left).collect(), style: s.style });
                break;
            }
        }
        self.spans = out;
    }

    /// Style stripped — what the row says. For assertions, once the storyboard
    /// grows checks as well as frames.
    #[allow(dead_code)]
    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }
}

pub(crate) fn line(style: Style, text: impl Into<String>) -> Line {
    let mut l = Line::default();
    l.push(style, text);
    l
}

/// The panel's whole glyph vocabulary, named once so the legend cannot drift
/// from what the rows actually draw.
pub(crate) mod glyph {
    pub const OPEN: &str = "▾";
    pub const CLOSED: &str = "▸";
    pub const WORKING: &str = "●";
    pub const IDLE: &str = "○";
    pub const QUIET: &str = "·";
    pub const BLOCKED: &str = "■";
    pub const REVIEW: &str = "◆";
    pub const DONE: &str = "✓";
    pub const DOING: &str = "▸";
    pub const MORE: &str = "⋯";
    pub const NEEDS_YOU: &str = "←";
    /// A pane with no agent in it.
    pub const SHELL: &str = "▫";
    /// Something is written in Overview or Details.
    pub const NOTES: &str = "≡";
}

fn state_dot(state: &str) -> (Style, &'static str) {
    match state {
        "working" => (Style::Accent, glyph::WORKING),
        "idle" => (Style::Muted, glyph::IDLE),
        _ => (Style::Dim, glyph::QUIET),
    }
}

fn render_row(row: &Row, w: usize, num: Option<u8>) -> Line {
    let mut l = Line::default();
    match row {
        Row::Project { id, depth, counts, collapsed, live, prose } => {
            l.pad(*depth);
            l.push(Style::Dim, if *collapsed { glyph::CLOSED } else { glyph::OPEN });
            l.push(Style::Plain, " ");
            l.push(Style::Bold, id.clone());
            if *prose {
                l.push(Style::Plain, " ");
                l.push(Style::Dim, glyph::NOTES);
            }

            // A done-ratio bar reads as empty until work starts landing, so the
            // right-hand column carries live workload instead: open, in flight,
            // blocked.
            let mut right = Line::default();
            let gap_before = |r: &mut Line| {
                if !r.spans.is_empty() {
                    r.push(Style::Plain, " ");
                }
            };
            if counts.open > 0 {
                gap_before(&mut right);
                right.push(Style::Dim, counts.open.to_string());
            }
            if counts.doing > 0 {
                gap_before(&mut right);
                right.push(Style::Accent, format!("{}{}", glyph::DOING, counts.doing));
            }
            if counts.blocked > 0 {
                gap_before(&mut right);
                right.push(Style::Warn, format!("{}{}", glyph::BLOCKED, counts.blocked));
            }
            if counts.done > 0 && counts.open == 0 {
                gap_before(&mut right);
                right.push(Style::Dim, glyph::DONE);
            }
            if *live > 0 {
                gap_before(&mut right);
                right.push(Style::Accent, format!("{}{live}", glyph::WORKING));
            }
            l.pad(w.saturating_sub(l.width() + right.width()).max(1));
            l.spans.extend(right.spans);
        }
        Row::Task { title, depth, status, agent, needs_you, prose, .. } => {
            match num {
                Some(n) => l.push(Style::Dim, n.to_string()),
                None => l.push(Style::Plain, " "),
            }
            l.pad(*depth);
            l.push(Style::Plain, " ");
            match status {
                Status::Blocked => l.push(Style::Warn, glyph::BLOCKED),
                Status::Review => l.push(Style::Muted, glyph::REVIEW),
                Status::Done => l.push(Style::Dim, glyph::DONE),
                Status::Doing => l.push(Style::Accent, glyph::DOING),
                _ => l.push(Style::Dim, glyph::QUIET),
            }
            l.push(Style::Plain, " ");
            // Reserve the marker's column before truncating, or a long title
            // eats the very sign that there is more to read.
            let flag_w = if *needs_you { 2 } else { 0 } + if *prose { 2 } else { 0 };
            let avail = w.saturating_sub(*depth + 5 + flag_w);
            let body = util::truncate(title, avail.max(4));
            let style = if *needs_you {
                Style::Warn
            } else if *status == Status::Done {
                Style::Dim
            } else if agent.is_some() {
                Style::Plain
            } else {
                Style::Muted
            };
            l.push(style, body);
            if *prose {
                l.push(Style::Plain, " ");
                l.push(Style::Dim, glyph::NOTES);
            }
            if *needs_you {
                l.push(Style::Warn, format!(" {}", glyph::NEEDS_YOU));
            }
        }
        Row::More { depth, n, .. } => {
            l.push(Style::Plain, " ");
            l.pad(*depth);
            l.push(Style::Plain, " ");
            l.push(Style::Dim, glyph::MORE);
            l.push(Style::Plain, " ");
            l.push(Style::Muted, format!("{n} more"));
        }
        Row::Section { label, count, collapsed, .. } => {
            // A caret, like a project — it folds the same way — but kept dim
            // rather than bold, so a group still reads as not-a-project.
            l.push(Style::Dim, if *collapsed { glyph::CLOSED } else { glyph::OPEN });
            l.push(Style::Plain, " ");
            l.push(Style::Muted, label.clone());
            let right = line(Style::Dim, count.to_string());
            l.pad(w.saturating_sub(l.width() + right.width()).max(1));
            l.spans.extend(right.spans);
        }
        Row::Agent { agent, title, depth } => {
            match num {
                Some(n) => l.push(Style::Dim, n.to_string()),
                None => l.push(Style::Plain, " "),
            }
            l.pad(*depth);
            l.push(Style::Plain, " ");
            if agent.agent {
                let (st, dot) = state_dot(&agent.state);
                l.push(st, dot);
            } else {
                // A shell nobody is driving. Distinct from an idle agent: one
                // has stopped, the other never started.
                l.push(Style::Dim, glyph::SHELL);
            }
            l.push(Style::Plain, " ");
            let avail = w.saturating_sub(*depth + 4).max(4);
            l.push(Style::Muted, util::truncate(title, avail));
        }
    }
    l
}

// ---- legend -------------------------------------------------------------

pub(crate) struct Mark {
    /// Drawn through the same Style the rows use, so the colour cannot drift.
    pub sample: Line,
    pub name: &'static str,
    pub note: &'static str,
}

fn mark(spans: &[(Style, &str)], name: &'static str, note: &'static str) -> Mark {
    let mut sample = Line::default();
    for (st, t) in spans {
        sample.push(*st, *t);
    }
    Mark { sample, name, note }
}

/// The vocabulary, grouped by where it appears. Built from the same glyph
/// constants and the same `Style` values the renderer uses.
pub(crate) fn legend() -> Vec<(&'static str, &'static str, Vec<Mark>)> {
    use glyph as g;
    vec![
        (
            "On a task",
            "The first column. When an agent is on the task it shows the agent's \
             state — so the task's own status is not drawn, and a claimed task \
             looks the same whether it is todo or doing.",
            vec![
                mark(&[(Style::Accent, g::WORKING)], "working", "an agent is on this task and busy"),
                mark(&[(Style::Muted, g::IDLE)], "idle", "an agent is on this task and waiting"),
                mark(&[(Style::Dim, g::QUIET)], "no agent", "nobody has picked this up"),
                mark(&[(Style::Warn, g::BLOCKED)], "blocked", "parked, with a reason on the task"),
                mark(&[(Style::Muted, g::REVIEW)], "review", "done enough to look at"),
                mark(&[(Style::Accent, g::DOING)], "doing", "started — the task's own state, which it keeps even when claimed"),
                mark(&[(Style::Dim, g::DONE)], "done", "finished — only shown under A"),
            ],
        ),
        (
            "On a project",
            "A fold marker on the left, and on the right the workload rolled up \
             from every project beneath it.",
            vec![
                mark(&[(Style::Dim, g::OPEN)], "unfolded", "tasks and child projects are showing"),
                mark(&[(Style::Dim, g::CLOSED)], "folded", "← hides them, → brings them back"),
                mark(&[(Style::Dim, "7")], "open", "tasks not yet done, including everything below"),
                mark(&[(Style::Accent, g::DOING), (Style::Accent, "3")], "in flight", "tasks someone has started"),
                mark(&[(Style::Warn, g::BLOCKED), (Style::Warn, "1")], "blocked", "tasks parked and waiting"),
                mark(&[(Style::Dim, g::DONE)], "all clear", "there is work here and all of it is finished"),
                mark(&[(Style::Accent, g::WORKING), (Style::Accent, "2")], "panes", "panes standing in this project, agent or not"),
            ],
        ),
        (
            "Everywhere else",
            "Rows and markers that are not about a single task.",
            vec![
                mark(&[(Style::Warn, g::NEEDS_YOU)], "wants you", "an idle agent on a task that is still doing — it has stopped and you are the blocker"),
                mark(&[(Style::Warn, "1 "), (Style::Warn, g::NEEDS_YOU)], "how many", "the same count, in the header"),
                mark(&[(Style::Dim, g::MORE), (Style::Muted, " 2 more")], "overflow", "past the six-task cap; ↵ opens the tail in place"),
                mark(&[(Style::Accent, g::WORKING), (Style::Plain, " "), (Style::Muted, "Trance Video")], "a pane", "nested under the task it claimed, or under the project it stands in"),
                mark(&[(Style::Dim, g::SHELL), (Style::Plain, " "), (Style::Muted, "Trance Lite")], "a shell", "a pane with no agent — never started, as against an idle one that stopped"),
                mark(&[(Style::Dim, g::NOTES)], "written on", "something is in this row's Overview or Details — E opens it"),
                mark(&[(Style::Dim, g::OPEN), (Style::Plain, " "), (Style::Muted, "inbox")], "a group", "not a project, but still a scope — folds and takes the cursor like one"),
                mark(&[(Style::Dim, "1")], "hotkey", "1-9 jump straight to that agent's terminal"),
                mark(&[(Style::Accent, "+done")], "showing done", "A is on, so finished work is included"),
            ],
        ),
        (
            "Colour on its own",
            "Six roles, used consistently regardless of glyph.",
            vec![
                mark(&[(Style::Plain, "plain")], "claimed", "a task with an agent on it"),
                mark(&[(Style::Muted, "muted")], "unclaimed", "a task nobody is on; agent names"),
                mark(&[(Style::Dim, "dim")], "structure", "carets, counts, punctuation, finished work"),
                mark(&[(Style::Bold, "bold")], "project", "project names only"),
                mark(&[(Style::Accent, "accent")], "live", "running agents and work in flight"),
                mark(&[(Style::Warn, "warn")], "wants a decision", "blocked, or waiting on you"),
            ],
        ),
    ]
}

// ---- the key map --------------------------------------------------------

/// Every key the panel answers to. Keys that do the same kind of thing share a
/// line — `s v` is one idea, not two — because the map is read in a column
/// thirty-four wide, and length is what pushes the tree off the screen.
///
/// The verbs come first. Movement is the half you can find by pressing an arrow
/// and watching; `X` is not. So when a short pane can only fit part of this,
/// what it fits is the part that cannot be guessed.
///
/// Descriptions are written to fit that column. Anything longer is a sentence,
/// and a sentence belongs on the row it describes, not here.
pub(crate) fn keymap() -> Vec<(&'static str, Vec<(&'static str, &'static str)>)> {
    vec![
        (
            "change",
            vec![
                ("a P", "add task, project"),
                ("s v", "start, review"),
                ("d o", "done, reopen"),
                ("b e n", "block, retitle, note"),
                ("m c", "move, claim"),
                ("O", "open a workspace"),
                ("X", "remove, after y/n"),
            ],
        ),
        (
            "look",
            vec![
                ("↵ esc", "open it, close it"),
                ("E", "edit in a tab"),
                ("A r", "show done, sync"),
                ("q", "quit"),
            ],
        ),
        (
            "move",
            vec![
                ("j k ↑ ↓", "up, down"),
                ("h l ← →", "fold, unfold"),
                ("g G", "first, last row"),
                ("1-9", "jump to a terminal"),
            ],
        ),
    ]
}

/// The key map as rows. A heading rules off to the edge rather than sitting
/// above a blank line: a sidebar is short, and separation has to cost nothing.
fn help_lines(w: usize) -> Vec<Line> {
    let keyw = keymap()
        .iter()
        .flat_map(|(_, keys)| keys.iter())
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(0);

    let mut out = Vec::new();
    for (section, keys) in keymap() {
        let mut head = Line::default();
        head.push(Style::Bold, section);
        head.push(Style::Plain, " ");
        head.push(Style::Dim, "─".repeat(w.saturating_sub(head.width())));
        out.push(head);

        for (k, what) in keys {
            let mut l = Line::default();
            l.push(Style::Plain, " ");
            l.push(Style::Plain, k);
            l.pad(keyw - k.chars().count() + 2);
            l.push(Style::Muted, util::truncate(what, w.saturating_sub(keyw + 3).max(4)));
            out.push(l);
        }
    }
    out
}

/// Rows the tree keeps whatever the map wants. A cursor with no neighbours
/// above or below it is not a tree you can aim with, and aiming is the whole
/// reason the map is open.
const MIN_TREE_ROWS: usize = 6;

/// Digits 1-9 address rows that lead somewhere: a terminal.
fn hotkeys(rows: &[Row]) -> Vec<Option<u8>> {
    let mut out = Vec::with_capacity(rows.len());
    let mut n: u8 = 0;
    for r in rows {
        if r.agent().is_some() && n < 9 {
            n += 1;
            out.push(Some(n));
        } else {
            out.push(None);
        }
    }
    out
}

/// Where the body starts, given where the cursor is.
///
/// The selection is held near the middle rather than pushed against an edge.
/// Scrolling only when the cursor reaches the last row means it then *stays*
/// there, and a cursor parked on the bottom line shows you everything you have
/// already walked past and nothing you are about to reach. Both ends clamp, so
/// the first and last screens do not scroll into empty space — the cursor rides
/// up to the top and down to the foot there, which is what those screens mean.
///
/// Derived from `sel` every frame rather than remembered: a stored offset and a
/// cursor are two truths about one thing, and they drift.
fn scroll_for(sel: usize, n: usize, body: usize) -> usize {
    if body == 0 || n <= body {
        return 0;
    }
    sel.saturating_sub(body / 2).min(n - body)
}

/// The whole panel as styled lines. No escapes, no terminal — a backend turns
/// this into something you can look at.
pub(crate) fn frame(ui: &Ui, view: &View, w: usize, h: usize) -> Vec<Line> {
    let mode = &view.mode;
    let mut lines: Vec<Line> = Vec::new();

    let mut head = Line::default();
    head.push(Style::Bold, "wsp");
    head.push(Style::Plain, " ");
    head.push(Style::Dim, "·");
    head.push(Style::Plain, format!(" {} ", ui.agents_total));
    head.push(Style::Dim, "agents ·");
    head.push(Style::Plain, " ");
    if ui.needs > 0 {
        head.push(Style::Warn, format!("{} {}", ui.needs, glyph::NEEDS_YOU));
    } else {
        head.push(Style::Dim, "·");
    }
    lines.push(head);
    lines.push(line(Style::Dim, "─".repeat(w)));

    let footer_rows = 3;
    let room = h.saturating_sub(lines.len() + footer_rows);

    // The map takes the rows it needs out of the tree's, never the other way
    // about, and its first line is a ruled heading — so it needs no separator
    // of its own and costs the tree nothing but its own height.
    let map = if view.help { help_lines(w) } else { Vec::new() };
    let map_rows = map.len().min(room.saturating_sub(MIN_TREE_ROWS));
    let body_rows = room - map_rows;
    let keys = hotkeys(&ui.rows);

    let scroll = scroll_for(ui.sel, ui.rows.len(), body_rows);
    for (i, row) in ui.rows.iter().enumerate().skip(scroll).take(body_rows) {
        let mut l = render_row(row, w, keys[i]);
        l.selected = i == ui.sel;
        lines.push(l);
    }
    while lines.len() < h.saturating_sub(footer_rows + map_rows) {
        lines.push(Line::default());
    }
    let hidden = map.len() - map_rows;
    lines.extend(map.into_iter().take(map_rows));

    lines.push(line(Style::Dim, "─".repeat(w)));

    // No inbox count here: it is a row of its own now, at the top, where it can
    // be folded and aimed at. Restating it would be two places to keep true.
    let mut foot = Line::default();
    if ui.blocked > 0 {
        foot.push(Style::Warn, format!("blocked {}", ui.blocked));
    } else {
        foot.push(Style::Dim, "blocked 0");
    }
    if ui.show_done {
        foot.push(Style::Plain, "  ");
        foot.push(Style::Accent, "+done");
    }
    lines.push(foot);

    // The last line belongs to whatever the panel is waiting for: a value, an
    // answer, a destination — and only otherwise a message or the key hint.
    lines.push(match mode {
        Mode::Prompt { verb, buffer } => {
            let mut l = Line::default();
            l.push(Style::Accent, format!("{}> ", verb.label()));
            // Show the tail once the value outruns the pane, so the caret is
            // always the thing you can see.
            let room = w.saturating_sub(l.width() + 1);
            let shown: String = if buffer.chars().count() > room {
                buffer.chars().skip(buffer.chars().count() - room).collect()
            } else {
                buffer.clone()
            };
            l.push(Style::Plain, shown);
            l.push(Style::Accent, "▌");
            l
        }
        Mode::Confirm { question, .. } => {
            let mut l = Line::default();
            l.push(Style::Warn, util::truncate(question, w.saturating_sub(6)));
            l.push(Style::Dim, "  y/n");
            l
        }
        Mode::Pick { verb } => {
            let mut l = Line::default();
            l.push(Style::Accent, util::truncate(verb.hint(), w.saturating_sub(4)));
            l.push(Style::Dim, "  ↵");
            l
        }
        Mode::Browse => match &ui.message {
            Some((m, at)) if at.elapsed() < Duration::from_secs(4) => {
                line(Style::Accent, util::truncate(m, w))
            }
            // With the map up, the hint is a fifth of what is already on
            // screen. What the line is worth then is the way out — and an
            // honest count of what the pane was too short to hold.
            _ if view.help => {
                let mut l = line(Style::Dim, "? or esc closes");
                if hidden > 0 {
                    l.push(Style::Plain, "  ");
                    l.push(Style::Muted, format!("{hidden} more"));
                }
                l
            }
            _ => line(Style::Dim, "↵ open · E edit · a add · ? keys"),
        },
    });

    lines.truncate(h);
    lines
}

// ---- backends -----------------------------------------------------------

fn ansi_of(style: Style) -> &'static str {
    match style {
        Style::Plain => "",
        Style::Dim => DIMC,
        Style::Bold => BOLD,
        Style::Muted => MUTED,
        Style::Accent => ACCENT,
        Style::Warn => WARN,
    }
}

/// What the live panel prints.
///
/// Inverse is re-asserted per span rather than wrapped around the row: every
/// span ends with a reset, and a reset clears inverse too, so a single opening
/// `INV` only ever highlighted a selected row up to its first styled run.
pub(crate) fn to_ansi(frame: &[Line], w: usize, h: usize) -> String {
    let mut out = String::from("\x1b[H\x1b[2J");
    for (i, l) in frame.iter().take(h).enumerate() {
        out.push_str(&format!("\x1b[{};1H", i + 1));
        let mut l = l.clone();
        l.fit(w);
        for s in &l.spans {
            if l.selected {
                out.push_str(INV);
            }
            out.push_str(ansi_of(s.style));
            out.push_str(&s.text);
            out.push_str(OFF);
        }
    }
    out
}

fn class_of(style: Style) -> &'static str {
    match style {
        Style::Plain => "p",
        Style::Dim => "d",
        Style::Bold => "b",
        Style::Muted => "m",
        Style::Accent => "a",
        Style::Warn => "w",
    }
}

fn esc_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// The same frame as a block of styled spans. The panel is text, so this is
/// not an approximation of the terminal — it is the same cells with the same
/// colours, and it costs no font work and no new dependency.
pub(crate) fn to_html_spans(l: &Line) -> String {
    let mut out = String::new();
    for s in &l.spans {
        out.push_str(&format!(
            "<span class=\"{}\">{}</span>",
            class_of(s.style),
            esc_html(&s.text)
        ));
    }
    out
}

pub(crate) fn to_html(frame: &[Line], w: usize) -> String {
    let mut out = String::from("<pre class=\"wsp\">");
    for l in frame {
        let mut l = l.clone();
        l.fit(w);
        out.push_str(if l.selected { "<span class=\"sel\">" } else { "<span>" });
        out.push_str(&to_html_spans(&l));
        out.push_str("</span>\n");
    }
    out.push_str("</pre>");
    out
}

// ---- terminal -----------------------------------------------------------

pub(crate) fn stty(args: &[&str]) {
    if let Ok(tty) = File::open("/dev/tty") {
        let _ = Command::new("stty")
            .args(args)
            .stdin(Stdio::from(tty))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

pub(crate) fn term_size() -> (usize, usize) {
    if let Ok(tty) = File::open("/dev/tty") {
        if let Ok(out) = Command::new("stty").arg("size").stdin(Stdio::from(tty)).output() {
            let s = String::from_utf8_lossy(&out.stdout);
            let mut it = s.split_whitespace();
            if let (Some(r), Some(c)) = (it.next(), it.next()) {
                if let (Ok(r), Ok(c)) = (r.parse::<usize>(), c.parse::<usize>()) {
                    return (c.max(16), r.max(6));
                }
            }
        }
    }
    (26, 40)
}

fn spawn_input(tx: Sender<Msg>) {
    std::thread::spawn(move || {
        let Ok(mut tty) = File::open("/dev/tty") else { return };
        let mut buf = [0u8; 1];
        // `stty min 0 time 1` makes a read with nothing waiting return 0 after
        // ~100ms. That is what lets a bare Esc be told apart from the start of
        // an arrow key, which is otherwise undecidable on a blocking read.
        let next = |tty: &mut File, buf: &mut [u8; 1]| -> Option<u8> {
            match tty.read(buf) {
                Ok(1) => Some(buf[0]),
                Ok(_) => None,
                Err(_) => None,
            }
        };
        loop {
            let Some(b) = next(&mut tty, &mut buf) else { continue };
            let mut pending: Option<Key> = None;
            let key = match b {
                3 => Key::Interrupt,
                b'\r' | b'\n' => Key::Enter,
                0x7f | 0x08 => Key::Backspace,
                b'\x1b' => match next(&mut tty, &mut buf) {
                    // Nothing followed: a real Esc.
                    None => Key::Esc,
                    Some(b'[') => match next(&mut tty, &mut buf) {
                        Some(b'A') => Key::Up,
                        Some(b'B') => Key::Down,
                        Some(b'C') => Key::Right,
                        Some(b'D') => Key::Left,
                        _ => continue,
                    },
                    // Esc then something else — deliver both, in order.
                    Some(other) => {
                        pending = Some(Key::Char(other as char));
                        Key::Esc
                    }
                },
                c if c.is_ascii_graphic() || c == b' ' => Key::Char(c as char),
                _ => continue,
            };
            if tx.send(Msg::Key(key)).is_err() {
                return;
            }
            if let Some(p) = pending {
                if tx.send(Msg::Key(p)).is_err() {
                    return;
                }
            }
        }
    });
}

fn spawn_events(tx: Sender<Msg>) {
    std::thread::spawn(move || loop {
        let tx2 = tx.clone();
        let res = herdr::subscribe(
            &[
                "workspace.created",
                "workspace.closed",
                "workspace.renamed",
                "workspace.focused",
                "pane.created",
                "pane.exited",
                "pane.agent_status_changed",
            ],
            move |_e, d| {
                let ws = d
                    .get("workspace_id")
                    .and_then(|x| x.as_str())
                    .map(|s| s.to_string());
                tx2.send(Msg::Herdr(ws)).is_ok()
            },
        );
        if res.is_err() {
            std::thread::sleep(Duration::from_secs(3));
        } else {
            std::thread::sleep(Duration::from_millis(500));
        }
    });
}

/// The label herdr carries on a detail pane, so we can find ours again.
pub(crate) const VIEW_LABEL: &str = "wsp:view";

/// Shut the workspace's detail pane, if it has one.
fn close_view(store: &Store, self_ws: Option<&str>) -> bool {
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
fn pop_out(argv: &[String], label: &str, self_ws: Option<&str>) -> String {
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
        json!({ "workspace_id": ws, "label": label, "focus": true }),
    ) else {
        return "could not create a tab".into();
    };
    let tab = r
        .get("tab")
        .and_then(|t| t.get("tab_id"))
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .unwrap_or_default();
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
    // only its own section.
    let Some(right) = split(&work, "right", 0.5) else {
        return "could not split the editors".into();
    };

    // Whichever editor is quit second takes the tab down. A one-byte marker per
    // editor, because closing on the first would kill the other with its work
    // still open — and leaving the tab means every edit strands a husk.
    let mark = std::env::temp_dir().join(format!("wsp-edit-{}", tab.replace(':', "-")));
    let m = shell_quote(&mark.display().to_string());
    let done = format!(
        "; printf x >> {m}; [ \"$(wc -c < {m} | tr -d ' ')\" -ge 2 ] && {{ rm -f {m}; herdr tab close {}; }}",
        shell_quote(&tab)
    );
    let _ = std::fs::remove_file(&mark);

    for (pane, section) in [(&work, "overview"), (&right, "details")] {
        // Label the pane as well as the file: herdr shows one, the editor's
        // status line shows the other, and between them there is no way to be
        // looking at a buffer without knowing which half it is.
        let _ = herdr::call("pane.rename", json!({ "pane_id": pane, "label": section }));
        let cmd = std::iter::once(shell_quote(&exe))
            .chain(base.iter().map(|a| shell_quote(a)))
            .collect::<Vec<_>>()
            .join(" ");
        run(pane, format!("{cmd} --{section}{done}\n"));
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
fn inspect(store: &Store, self_ws: Option<&str>, focus: &crate::detail::Focus) -> String {
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
fn run_wsp(argv: &[String]) -> Result<String, String> {
    let exe = std::env::current_exe().unwrap_or_else(|_| "wsp".into());
    let out = Command::new(exe)
        .args(argv)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();

    match out {
        Ok(o) if o.status.success() => Ok(argv.join(" ")),
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            let first = err.lines().next().unwrap_or("failed").trim();
            Err(first.strip_prefix("wsp: ").unwrap_or(first).to_string())
        }
        Err(e) => Err(format!("cannot run wsp: {e}")),
    }
}

fn focus(agent: &AgentRef) {
    let _ = herdr::call("workspace.focus", json!({ "workspace_id": agent.workspace }));
    let _ = herdr::call("pane.focus", json!({ "pane_id": agent.pane }));
}

// ---- main loop ----------------------------------------------------------

/// Size and mtime of the binary we are running. Cheap enough to check on every
/// tick, and enough to notice an `install` underneath us.
pub(crate) fn exe_stamp() -> Option<(u64, u64)> {
    let path = std::env::current_exe().ok()?;
    let m = std::fs::metadata(path).ok()?;
    let secs = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    Some((m.len(), secs))
}

/// Why the loop stopped.
enum Outcome {
    Quit,
    /// The binary changed on disk. Twenty-two panes each holding a stale image
    /// is a real cost while this is under active development — a key silently
    /// doing what it used to do is worse than one that errors.
    Reload,
}

pub fn run(store: &Store) -> i32 {
    if !herdr::available() {
        eprintln!("wsp: no herdr socket");
        return 1;
    }
    let self_ws = herdr::Env::read().workspace_id;

    let (tx, rx) = mpsc::channel::<Msg>();
    spawn_input(tx.clone());
    spawn_events(tx.clone());
    {
        let tx = tx.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(200));
            if tx.send(Msg::Tick).is_err() {
                return;
            }
        });
    }

    print!("\x1b[?1049h\x1b[?25l");
    let _ = std::io::stdout().flush();
    stty(&["raw", "-echo", "min", "0", "time", "1"]);

    let outcome = event_loop(store, &rx, self_ws.as_deref());

    stty(&["sane"]);
    print!("\x1b[?25h\x1b[?1049l");
    let _ = std::io::stdout().flush();

    if let Outcome::Reload = outcome {
        // Replace this process rather than spawning beside it: the pane, its
        // pty and its place in the layout all survive, and nothing has to
        // reattach.
        if let Ok(exe) = std::env::current_exe() {
            use std::os::unix::process::CommandExt;
            let err = Command::new(exe).arg("panel").exec();
            eprintln!("wsp: could not reload: {err}");
            return 1;
        }
    }
    0
}

/// What a key asked for beyond changing the view.
pub(crate) enum Effect {
    None,
    Refetch,
    Focus(AgentRef),
    Sync,
    Quit,
    /// Open a herdr workspace for this row, then claim the task into it if
    /// there is one.
    Open { label: String, cwd: Option<String>, project: Option<String>, task: Option<String> },
    /// Show this row in the detail pane, making one if there is not one yet.
    Inspect(crate::detail::Focus),
    /// Shut the detail pane.
    CloseView,
    /// Open the row full-size in a tab of its own, to be written in.
    PopOut { argv: Vec<String>, label: String },
    /// Argv for this binary. Running the CLI rather than reimplementing it
    /// means the event log, the hooks and the git commit all still happen,
    /// because it is the same code path a person at a shell would take.
    Run { argv: Vec<String>, escalate: Option<Vec<String>> },
}

fn say(ui: &mut Ui, m: impl Into<String>) {
    ui.message = Some((m.into(), Instant::now()));
}

/// Typing a value. Every printable key is text here — including `q`, which is
/// why the input layer stopped deciding what keys mean.
fn prompt_key(k: Key, ui: &mut Ui, view: &mut View, verb: Ask, mut buffer: String) -> Effect {
    match k {
        Key::Char(c) => {
            buffer.push(c);
            view.mode = Mode::Prompt { verb, buffer };
            Effect::None
        }
        Key::Backspace => {
            buffer.pop();
            view.mode = Mode::Prompt { verb, buffer };
            Effect::None
        }
        Key::Esc | Key::Interrupt => {
            view.mode = Mode::Browse;
            say(ui, "cancelled");
            Effect::None
        }
        Key::Enter => {
            if buffer.trim().is_empty() {
                view.mode = Mode::Browse;
                say(ui, "nothing typed");
                return Effect::None;
            }
            let argv = verb.argv(&buffer);
            if let Ask::NewProject { .. } = &verb {
                // The CLI slugifies the name; do the same so we know what to
                // keep visible without having to read the result back.
                view.reveal.insert(util::slugify(&buffer));
            }
            view.mode = Mode::Browse;
            Effect::Run { argv, escalate: None }
        }
        _ => Effect::None,
    }
}

/// Pointing at a second row. Navigation is untouched, so the tree itself is
/// the picker — no separate list to build, and folding still works while you
/// hunt for the project you want.
fn pick_key(k: Key, ui: &mut Ui, view: &mut View, verb: Pick) -> Effect {
    match k {
        Key::Esc | Key::Interrupt => {
            view.mode = Mode::Browse;
            say(ui, "cancelled");
            Effect::None
        }
        Key::Enter => match verb.argv(&ui.selected_target()) {
            Some(argv) => {
                view.mode = Mode::Browse;
                Effect::Run { argv, escalate: None }
            }
            None => {
                say(ui, "not a valid destination");
                Effect::None
            }
        },
        // Movement and folding stay live inside the pick.
        Key::Up | Key::Down | Key::Left | Key::Right | Key::Char('j') | Key::Char('k')
        | Key::Char('h') | Key::Char('l') => {
            let nav = match k {
                Key::Char('j') => Key::Down,
                Key::Char('k') => Key::Up,
                Key::Char('h') => Key::Left,
                Key::Char('l') => Key::Right,
                other => other,
            };
            move_or_fold(nav, ui, view)
        }
        _ => Effect::None,
    }
}

fn confirm_key(
    k: Key,
    ui: &mut Ui,
    view: &mut View,
    argv: Vec<String>,
    escalate: Option<Vec<String>>,
) -> Effect {
    match k {
        Key::Char('y') | Key::Char('Y') => {
            view.mode = Mode::Browse;
            Effect::Run { argv, escalate }
        }
        Key::Char('n') | Key::Char('N') | Key::Esc | Key::Interrupt | Key::Enter => {
            view.mode = Mode::Browse;
            say(ui, "left alone");
            Effect::None
        }
        _ => Effect::None,
    }
}

/// Cursor movement and folding, shared by browse and pick.
fn move_or_fold(k: Key, ui: &mut Ui, view: &mut View) -> Effect {
    let n = ui.rows.len();
    match k {
        Key::Down => {
            if ui.sel + 1 < n {
                ui.sel += 1;
            }
            Effect::None
        }
        Key::Up => {
            ui.sel = ui.sel.saturating_sub(1);
            Effect::None
        }
        Key::Left | Key::Right => match ui.rows.get(ui.sel) {
            Some(Row::Project { id: key, .. }) | Some(Row::Section { key, .. }) => {
                if k == Key::Left {
                    view.collapsed.insert(key.clone());
                    view.expanded.remove(key);
                } else {
                    view.collapsed.remove(key);
                }
                Effect::Refetch
            }
            Some(Row::More { key, .. }) if k == Key::Right => {
                view.expanded.insert(key.clone());
                Effect::Refetch
            }
            _ => Effect::None,
        },
        _ => Effect::None,
    }
}

/// The reducer. Deliberately free of I/O — it moves the cursor, changes the
/// mode, and reports what else it wants done, so the storyboard can drive the
/// same transitions the terminal does and get the same frames out.
pub(crate) fn apply_key(k: Key, ui: &mut Ui, view: &mut View) -> Effect {
    // Taken rather than borrowed: every branch may replace it.
    match std::mem::take(&mut view.mode) {
        Mode::Browse => browse_key(k, ui, view),
        Mode::Prompt { verb, buffer } => {
            view.mode = Mode::Prompt { verb: verb.clone(), buffer: buffer.clone() };
            prompt_key(k, ui, view, verb, buffer)
        }
        Mode::Pick { verb } => {
            view.mode = Mode::Pick { verb: verb.clone() };
            pick_key(k, ui, view, verb)
        }
        Mode::Confirm { argv, question, escalate } => {
            view.mode = Mode::Confirm {
                argv: argv.clone(),
                question,
                escalate: escalate.clone(),
            };
            confirm_key(k, ui, view, argv, escalate)
        }
    }
}

/// Reading the key map. Everything that is not scrolling or closing is
/// swallowed: the map is open precisely because you are not sure what a key
/// does, which is the worst possible moment for one of them to fire.
fn browse_key(k: Key, ui: &mut Ui, view: &mut View) -> Effect {
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
        Key::Char('g') => {
            ui.sel = 0;
            Effect::None
        }
        Key::Char('G') => {
            ui.sel = n.saturating_sub(1);
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
            match hotkeys(&ui.rows)
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
        Key::Char('r') => Effect::Sync,
        // Nothing else changes while it is up: the tree keeps the cursor, and
        // every key on the map still does what the map says it does — which is
        // the only way to read one and act on it in the same breath.
        Key::Char('?') => {
            view.help = !view.help;
            Effect::None
        }

        // ---- create ----
        Key::Char('a') => match scope(&target, ui) {
            Some(project) => {
                view.mode = Mode::Prompt { verb: Ask::AddTask { project }, buffer: String::new() };
                Effect::None
            }
            None => {
                say(ui, "nowhere to add that");
                Effect::None
            }
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
            Target::Task(id) => {
                view.mode = Mode::Pick { verb: Pick::MoveTask { task: id.clone() } };
                Effect::None
            }
            _ => {
                say(ui, "only a task moves");
                Effect::None
            }
        },
        Key::Char('c') => match &target {
            Target::Task(id) => {
                view.mode = Mode::Pick { verb: Pick::PaneForTask { task: id.clone() } };
                Effect::None
            }
            Target::Pane(p) => {
                view.mode = Mode::Pick { verb: Pick::TaskForPane { pane: p.clone() } };
                Effect::None
            }
            _ => {
                say(ui, "claim joins a task to an agent");
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
fn toggle(view: &mut View, want: crate::detail::Focus) -> Effect {
    if view.showing.as_ref() == Some(&want) {
        Effect::CloseView
    } else {
        Effect::Inspect(want)
    }
}

/// Status verbs all have the same shape: one key, a task, no input.
fn task_verb(target: &Target, ui: &mut Ui, verb: &str) -> Effect {
    match target {
        Target::Task(id) => Effect::Run { argv: vec![verb.to_string(), id.clone()], escalate: None },
        _ => {
            say(ui, format!("{verb} needs a task"));
            Effect::None
        }
    }
}

fn event_loop(store: &Store, rx: &Receiver<Msg>, self_ws: Option<&str>) -> Outcome {
    let started_as = exe_stamp();
    let mut view = View::default();
    let mut ui = collect(&Snapshot::live(store), &view, self_ws);
    let mut last = String::new();
    let mut dirty = false;
    let mut last_fetch = Instant::now();
    let mut last_fingerprint = store.fingerprint();

    let draw = |ui: &Ui, view: &View, last: &mut String| {
        let (w, h) = term_size();
        let painted = to_ansi(&frame(ui, view, w, h), w, h);
        if painted != *last {
            print!("{painted}");
            let _ = std::io::stdout().flush();
            *last = painted;
        }
    };
    draw(&ui, &view, &mut last);

    loop {
        let msg = match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(m) => m,
            Err(RecvTimeoutError::Timeout) => Msg::Tick,
            Err(RecvTimeoutError::Disconnected) => return Outcome::Quit,
        };

        let mut refetch = false;
        match msg {
            Msg::Key(k) => match apply_key(k, &mut ui, &mut view) {
                Effect::Quit => return Outcome::Quit,
                Effect::None => {}
                Effect::Refetch => refetch = true,
                Effect::Focus(a) => focus(&a),
                Effect::Sync => {
                    let mut cache = crate::sync::Cache::default();
                    let _ = crate::sync::sync(store, &mut cache, true);
                    ui.message = Some(("synced".into(), Instant::now()));
                    refetch = true;
                }
                Effect::Inspect(focus) => {
                    let msg = inspect(store, self_ws, &focus);
                    if msg.is_empty() {
                        view.showing = Some(focus);
                    } else {
                        say(&mut ui, msg);
                    }
                }
                Effect::CloseView => {
                    if close_view(store, self_ws) {
                        say(&mut ui, "closed");
                    }
                    view.showing = None;
                }
                Effect::PopOut { argv, label } => {
                    say(&mut ui, pop_out(&argv, &label, self_ws));
                }
                Effect::Open { label, cwd, project, task } => {
                    match open_workspace(&label, cwd.as_deref(), project.as_deref(), task.as_deref())
                    {
                        Ok((_ws, pane)) => {
                            // The workspace exists; the durable record of what
                            // it is for is the claim, so make it now rather
                            // than relying on the env surviving a restart.
                            match &task {
                                Some(t) => {
                                    say(&mut ui, run_wsp(&[
                                        "claim".into(),
                                        t.clone(),
                                        "--pane".into(),
                                        pane.clone(),
                                    ])
                                    .unwrap_or_else(|e| e));
                                }
                                None => say(&mut ui, format!("opened {label}")),
                            }
                        }
                        Err(e) => say(&mut ui, e),
                    }
                    refetch = true;
                }
                Effect::Run { argv, escalate } => {
                    match (run_wsp(&argv), escalate) {
                        (Ok(m), _) => say(&mut ui, m),
                        // Refused, and there is a stronger form of the same
                        // command: show what the CLI said and ask again.
                        (Err(e), Some(more)) => {
                            view.mode = Mode::Confirm {
                                question: e,
                                argv: more,
                                escalate: None,
                            };
                        }
                        (Err(e), None) => say(&mut ui, e),
                    }
                    refetch = true;
                }
            },
            Msg::Herdr(ws) => {
                // An event naming our own workspace means we are probably about
                // to be looked at. Coalesce the burst that follows a split or a
                // focus change, then mark dirty — the tick is 200ms away and
                // will pick it up, so there is no reason to sleep here.
                while rx.try_recv().is_ok() {}
                let concerns_us = ws.is_some() && ws.as_deref() == self_ws;
                dirty = true;
                if concerns_us || ui.self_focused {
                    refetch = true;
                    dirty = false;
                }
            }
            Msg::Tick => {
                // The one you are looking at should feel immediate; the twenty
                // behind it should cost nothing. Both the fingerprint stat and
                // the two socket calls sit behind this gate, so an idle
                // background panel does no work at all between refreshes.
                let interval = if ui.self_focused {
                    Duration::from_millis(250)
                } else {
                    Duration::from_secs(30)
                };
                if last_fetch.elapsed() >= interval {
                    if started_as.is_some() && exe_stamp() != started_as {
                        return Outcome::Reload;
                    }
                    let store_changed = store.fingerprint() != last_fingerprint;
                    if dirty || store_changed {
                        refetch = true;
                        dirty = false;
                    }
                }
            }
        }

        if refetch {
            last_fetch = Instant::now();
            last_fingerprint = store.fingerprint();
            refetch_into(&mut ui, &Snapshot::live(store), &view, self_ws);
        }
        draw(&ui, &view, &mut last);
    }
}

// ---- install / uninstall ------------------------------------------------

fn panel_command() -> Vec<String> {
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "wsp".into());
    vec![exe, "panel".into()]
}

fn panels_state(store: &Store) -> std::collections::BTreeMap<String, String> {
    std::fs::read_to_string(store.state.join("panels.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .map(|m| {
            m.into_iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

fn save_panels(store: &Store, map: &std::collections::BTreeMap<String, String>) {
    let obj: serde_json::Map<String, serde_json::Value> = map
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    let _ = std::fs::create_dir_all(&store.state);
    let _ = crate::store::write_atomic(
        &store.state.join("panels.json"),
        &serde_json::to_string_pretty(&serde_json::Value::Object(obj)).unwrap_or_default(),
    );
}

struct PaneInfo {
    id: String,
    label: String,
}

fn list_panes(ws_id: &str) -> Result<Vec<PaneInfo>, String> {
    let r = herdr::call("pane.list", json!({ "workspace_id": ws_id })).map_err(|e| e.to_string())?;
    let arr = r.get("panes").and_then(|p| p.as_array()).cloned().unwrap_or_default();
    Ok(arr
        .iter()
        // The server has honoured the filter, but a stray pane from another
        // workspace here would mean splitting the wrong tree.
        .filter(|p| p.get("workspace_id").and_then(|x| x.as_str()) == Some(ws_id))
        .filter_map(|p| {
            Some(PaneInfo {
                id: p.get("pane_id")?.as_str()?.to_string(),
                label: p.get("label").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            })
        })
        .collect())
}

/// The pane a panel should be split off: the widest one that is not ours.
/// Width comes from herdr's layout, because "first in the list" is an
/// arbitrary answer that happens to be right only when there is one pane.
fn widest<'a>(ws_id: &str, panes: &'a [PaneInfo]) -> Option<&'a PaneInfo> {
    let mine = |p: &PaneInfo| p.label == PANEL_LABEL || p.label == VIEW_LABEL;
    let candidates: Vec<&PaneInfo> = panes.iter().filter(|p| !mine(p)).collect();
    if candidates.len() <= 1 {
        return candidates.into_iter().next();
    }
    let widths: std::collections::BTreeMap<String, u64> = herdr::call(
        "pane.layout",
        json!({ "pane_id": candidates[0].id, "workspace_id": ws_id }),
    )
    .ok()
    .and_then(|r| r.get("layout").and_then(|l| l.get("panes").cloned()))
    .and_then(|p| p.as_array().cloned())
    .unwrap_or_default()
    .iter()
    .filter_map(|p| {
        let id = p.get("pane_id")?.as_str()?.to_string();
        let w = p.get("rect")?.get("width")?.as_u64()?;
        Some((id, w))
    })
    .collect();

    candidates.into_iter().max_by_key(|p| widths.get(&p.id).copied().unwrap_or(0))
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `pane.split` starts the user's shell and takes no command, so the panel is
/// `exec`d over it — which also means quitting the panel closes the pane
/// instead of leaving a bare prompt behind.
fn launch_panel(pane: &str) {
    let cmd = panel_command().iter().map(|a| shell_quote(a)).collect::<Vec<_>>().join(" ");
    let _ = herdr::call("pane.rename", json!({ "pane_id": pane, "label": "wsp" }));
    let _ = herdr::call(
        "pane.send_text",
        json!({ "pane_id": pane, "text": format!("exec {cmd}\n") }),
    );
}

/// Install into one workspace. Returns Ok(true) when a panel was added,
/// Ok(false) when the workspace already had one.
///
/// This used to build a whole layout tree and hand it to `layout.apply`, with
/// the agent's pane re-referenced by id on the assumption that herdr would
/// carry it across. It does not: the tree is rebuilt from scratch and every
/// pane in it gets a fresh terminal, which killed every running agent. So:
/// `pane.split`, which adds one pane beside a target and touches nothing else.
pub fn install_one(store: &Store, ws_id: &str, ratio: f64) -> Result<bool, String> {
    let panes = list_panes(ws_id)?;
    let mut panels = panels_state(store);

    if let Some(existing) = panels.get(ws_id) {
        if panes.iter().any(|p| p.id == *existing) {
            return Ok(false);
        }
    }
    // A pane labelled `wsp` is ours even when the state file lost track of it
    // — a crash between the split and the save would otherwise install twice.
    if let Some(orphan) = panes.iter().find(|p| p.label == "wsp") {
        panels.insert(ws_id.to_string(), orphan.id.clone());
        save_panels(store, &panels);
        return Ok(false);
    }
    // Never split one of our own panes. A leftover view pane used to be a
    // candidate, and splitting it gave the panel 22% of an already-narrow
    // column — seven usable characters.
    let Some(target) = widest(ws_id, &panes) else {
        return Ok(false);
    };
    let before: HashSet<&str> = panes.iter().map(|p| p.id.as_str()).collect();

    // Splitting `right` at `ratio` leaves the *target* holding `ratio` and puts
    // the new pane in the remainder, so the sidebar arrives on the wrong side
    // at the wrong width. Swapping the two afterwards lands the panel in the
    // narrow left slot without disturbing either process.
    let res = herdr::call(
        "pane.split",
        json!({
            "direction": "right",
            "target_pane_id": target.id,
            "ratio": ratio,
            "focus": false,
        }),
    )
    .map_err(|e| e.to_string())?;

    let new_pane = res
        .get("pane")
        .and_then(|p| p.get("pane_id"))
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            let after = list_panes(ws_id).ok()?;
            after.into_iter().find(|p| !before.contains(p.id.as_str())).map(|p| p.id)
        })
        .ok_or_else(|| "split reported no new pane".to_string())?;

    let _ = herdr::call(
        "pane.swap",
        json!({ "source_pane_id": target.id, "target_pane_id": new_pane }),
    );
    launch_panel(&new_pane);

    panels.insert(ws_id.to_string(), new_pane);
    save_panels(store, &panels);
    Ok(true)
}

/// Auto-install for a newly created workspace — but only once the panel is
/// actually in use, so linking the plugin never surprises anyone.
pub fn install_if_adopted(store: &Store, ws_id: &str) {
    if panels_state(store).is_empty() {
        return;
    }
    let _ = install_one(store, ws_id, 0.22);
}

pub fn install(store: &Store, args: &crate::Args) -> i32 {
    if !herdr::available() {
        eprintln!("wsp: no herdr socket");
        return 1;
    }
    let ratio: f64 = args.get("ratio").and_then(|r| r.parse().ok()).unwrap_or(0.22);
    let only = args.get("workspace").or_else(|| herdr::Env::read().workspace_id);
    let all = args.has("all");

    // Without `--all` this installs into exactly one workspace. Run outside a
    // herdr pane there is no such workspace, and letting the filter fall
    // through would quietly install into every one of them.
    if !all && only.is_none() {
        eprintln!("wsp: not inside a herdr pane — pass --workspace <id> or --all");
        return 1;
    }

    let workspaces = herdr::workspaces().unwrap_or_default();
    let mut added = 0;
    let mut skipped: Vec<String> = Vec::new();

    for ws in &workspaces {
        if !all {
            if let Some(target) = &only {
                if &ws.id != target {
                    continue;
                }
            }
        }
        match install_one(store, &ws.id, ratio) {
            Ok(true) => added += 1,
            Ok(false) => {}
            Err(e) => skipped.push(format!("{}: {e}", ws.label)),
        }
    }

    if args.json() {
        println!("{}", json!({ "installed": added, "skipped": skipped }));
    } else {
        println!("panel installed in {added} workspace(s)");
        for sk in &skipped {
            println!("  skipped {sk}");
        }
    }
    0
}

pub fn uninstall(store: &Store, args: &crate::Args) -> i32 {
    let mut panels = panels_state(store);
    let only = args.get("workspace");
    let mut removed = 0;
    let targets: Vec<(String, String)> = panels
        .iter()
        .filter(|(ws, _)| only.as_ref().map(|o| *ws == o).unwrap_or(true))
        .map(|(a, b)| (a.clone(), b.clone()))
        .collect();

    for (ws, pane) in targets {
        if herdr::call("pane.close", json!({ "pane_id": pane })).is_ok() {
            removed += 1;
        }
        // The view pane is ours too. Leaving it behind orphans a pane nothing
        // will reclaim, and the next install would try to split it.
        if let Ok(ps) = list_panes(&ws) {
            for v in ps.iter().filter(|p| p.label == VIEW_LABEL) {
                let _ = herdr::call("pane.close", json!({ "pane_id": v.id }));
            }
        }
        panels.remove(&ws);
    }
    save_panels(store, &panels);

    if args.json() {
        println!("{}", json!({ "removed": removed }));
    } else {
        println!("panel removed from {removed} workspace(s)");
    }
    0
}
