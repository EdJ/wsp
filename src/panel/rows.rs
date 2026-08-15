//! What is in the tree, and how each row draws.
//!
//! One concept, one file: the [`Row`] a thing becomes, the walk that decides
//! which rows exist at all, and the line each one paints. A field added to a
//! row is read where it is written and drawn a hundred lines further down —
//! splitting the data from its drawing would put one change in two files every
//! time.
//!
//! Nothing here talks to herdr or the store. [`collect`] takes a [`Snapshot`]
//! in, so the same rows come out on a laptop with nothing running.

use std::time::Instant;

use crate::herdr;
use crate::model::{Status, Task};
use crate::resolve::{self, Counts, Index};
use crate::store::Store;
use crate::util;

use super::keys::View;
use super::render::{glyph, line, Line, Style};
use super::{PANEL_LABEL, VIEW_LABEL};

pub(super) const MAX_TASKS_PER_PROJECT: usize = 6;

pub(super) const MAX_PANES_PER_PROJECT: usize = 4;

/// Stand in for the groups that are not projects, wherever a project id is used
/// as a key. Project ids are slugs, so the parentheses keep these out of their
/// namespace.
pub(super) const INBOX_KEY: &str = "(inbox)";

pub(super) const NOPROJECT_KEY: &str = "(noproject)";

/// The dock at the foot: agents holding no task.
pub(super) const UNASSIGNED_KEY: &str = "(unassigned)";

#[derive(Debug, Clone)]
pub(crate) struct AgentRef {
    pub(super) pane: String,
    pub(super) workspace: String,
    pub(super) state: String,
    /// What to call it: the agent's terminal title if there is one, otherwise
    /// the workspace label.
    pub(super) where_: String,
    /// Whether an agent is running here, or it is just a shell.
    pub(super) agent: bool,
}

#[derive(Debug, Clone)]
pub(super) enum Row {
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
        /// Work beneath it, rolled up. A parent row that says nothing about
        /// its children is one you would tick off without looking — and it is
        /// the row a folded sub-tree has to speak for.
        under: Counts,
        /// What to type at a shell to mean this row, when `i` is on. Resolved
        /// here rather than at draw time because how short it can be depends on
        /// the other tasks, and the renderer sees one row at a time.
        ident: Option<String>,
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
    pub(super) fn selectable(&self) -> bool {
        true
    }
    /// The pane this row *is*. A task that has one is not it — the pane sits
    /// on its own row directly beneath, and letting both answer meant two
    /// hotkeys landing on the same terminal.
    pub(super) fn agent(&self) -> Option<&AgentRef> {
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
    pub(super) rows: Vec<Row>,
    /// How many of the trailing rows are the unassigned dock rather than tree.
    pub(super) dock: usize,
    pub(super) agents_total: usize,
    pub(super) needs: usize,
    pub(super) blocked: usize,
    pub(super) sel: usize,
    pub(super) message: Option<(String, Instant)>,
    pub(super) self_focused: bool,
    pub(super) show_done: bool,
    /// project id -> its first root, for opening a workspace where the work is.
    pub(super) roots: std::collections::BTreeMap<String, String>,
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
pub(super) fn target_of(row: &Row) -> Target {
    match row {
        Row::Project { id, .. } => Target::Project(id.clone()),
        Row::Task { id, .. } => Target::Task(id.clone()),
        Row::More { key, .. } => Target::Overflow(key.clone()),
        Row::Agent { agent, .. } => Target::Pane(agent.pane.clone()),
        Row::Section { key, .. } if key == NOPROJECT_KEY => Target::Unattached,
        Row::Section { key, .. } if key == UNASSIGNED_KEY => Target::Unattached,
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

    pub(super) fn task_title(&self, task: &str) -> Option<String> {
        self.rows.iter().find_map(|r| match r {
            Row::Task { id, title, .. } if id == task => Some(title.clone()),
            _ => None,
        })
    }

    /// Where a project lives on disk, if it says. A project with no root has
    /// nowhere to open, so the workspace lands wherever herdr defaults to.
    pub(super) fn project_root(&self, project: &str) -> Option<String> {
        self.roots.get(project).cloned()
    }

    /// Which project a task row sits under.
    pub(super) fn project_of_task(&self, task: &str) -> Option<String> {
        self.rows.iter().find_map(|r| match r {
            Row::Task { id, project, .. } if id == task => project.clone(),
            _ => None,
        })
    }

}

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
    pub(super) fn live(store: &Store) -> Snapshot {
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

pub(super) fn task_sort_key(t: &Task, has_agent: bool, needs_you: bool) -> (u8, u8, u8, String) {
    (
        u8::from(!needs_you),
        u8::from(!has_agent),
        t.status().rank(),
        t.id.clone(),
    )
}

/// What you would type to mean this task, and no more of it than you have to.
///
/// `t-260815-004` is thirteen columns of a pane that is thirty wide, and eleven
/// of them are the same on every row. The suffix is what the CLI resolves —
/// `wsp start 004` — so the suffix is what the panel shows, unless another
/// open task shares it, in which case the date is the part that separates them
/// and the whole thing has to be typed anyway.
fn ident_of(tasks: &[Task], task: &Task) -> String {
    let full = || task.id.strip_prefix("t-").unwrap_or(&task.id).to_string();
    // A bare suffix resolves against *open* tasks only, so a finished one has
    // to be named in full — showing `014` for a task `wsp show 014` cannot
    // find would be worse than showing nothing.
    if !task.status().is_open() {
        return full();
    }
    let suffix = task.id.rsplit('-').next().unwrap_or(&task.id);
    let taken = tasks
        .iter()
        .filter(|t| t.status().is_open() && t.id != task.id)
        .any(|t| t.id.rsplit('-').next() == Some(suffix));
    if taken {
        full()
    } else {
        suffix.to_string()
    }
}

/// Rows for the tasks of one project — or, when `project` is `None`, the tasks
/// belonging to no project at all. The inbox went unrendered for a while
/// because it had no equivalent of the project walk; sharing this makes that
/// class of omission impossible.
pub(super) fn task_rows(
    tasks: &[Task],
    project: Option<&str>,
    depth: usize,
    view: &View,
    agent_for_task: &dyn Fn(&str) -> Option<AgentRef>,
    rows: &mut Vec<Row>,
    needs: &mut usize,
) {
    let mut mine: Vec<Task> = tasks
        .iter()
        .filter(|t| t.project.as_deref() == project)
        .filter(|t| view.show_done || t.status().is_open())
        .cloned()
        .collect();
    mine.sort_by_key(|t| {
        let a = agent_for_task(&t.id);
        let needs_you =
            a.as_ref().map(|a| a.state == "idle").unwrap_or(false) && t.status() == Status::Doing;
        task_sort_key(t, a.is_some(), needs_you)
    });

    // Sub-tasks follow their parent, indented. The cap counts top-level work
    // only: hiding a child while its parent is on screen would make the parent
    // lie about how much is under it, and the parent already carries the
    // count that says so.
    let nested = resolve::nest(&mine);
    let tops = nested.iter().filter(|(_, d)| *d == 0).count();
    let key = project.unwrap_or(INBOX_KEY);
    let cap = if view.expanded.contains(key) { tops } else { tops.min(MAX_TASKS_PER_PROJECT) };

    let mut seen_tops = 0;
    for (t, sub) in &nested {
        if *sub == 0 {
            seen_tops += 1;
            if seen_tops > cap {
                break;
            }
        }
        let a = agent_for_task(&t.id);
        let needs_you =
            a.as_ref().map(|a| a.state == "idle").unwrap_or(false) && t.status() == Status::Doing;
        if needs_you {
            *needs += 1;
        }
        rows.push(Row::Task {
            id: t.id.clone(),
            project: t.project.clone(),
            title: t.title.clone(),
            depth: depth + sub,
            status: t.status(),
            agent: a.clone(),
            needs_you,
            prose: crate::model::has_prose(&t.body),
            // Against every task, not the filtered set: a parent whose
            // children are all done should say so rather than fall silent
            // because `A` is off.
            under: resolve::counts_under(tasks, &t.id),
            ident: view.ids.then(|| ident_of(tasks, t)),
        });
        // The pane working it hangs beneath, so the join is visible and the
        // task keeps its own status glyph instead of surrendering it to the
        // agent's — a claimed task used to look identical whether it was todo
        // or doing.
        if let Some(a) = a {
            rows.push(Row::Agent {
                title: a.where_.clone(),
                agent: a.clone(),
                depth: depth + sub + 1,
            });
        }
    }
    if tops > cap {
        rows.push(Row::More { key: key.to_string(), depth, n: tops - cap });
    }
}

/// Rebuild the rows from a snapshot, keeping the cursor where it was and
/// carrying any pending message across. Shared with the storyboard so an
/// offline flow lands on the same row a live one would.
pub(crate) fn refetch_into(ui: &mut Ui, snap: &Snapshot, view: &mut View, self_ws: Option<&str>) {
    let sel = ui.sel;
    let was = ui.selected_target();
    let msg = ui.message.take();
    *ui = collect(snap, view, self_ws);
    // The row, not the slot it was in. Claiming re-sorts the tree under the
    // cursor — the pane row leaves one task and reappears under another, often
    // several lines away — and holding the index would leave the eye on
    // whatever slid into its place. Falls back to the index when the row is
    // genuinely gone, which is the only case where there is nothing to follow.
    ui.sel = match was {
        Target::Nothing => sel,
        want => ui.rows.iter().position(|r| target_of(r) == want).unwrap_or(sel),
    };
    ui.sel = ui.sel.min(ui.rows.len().saturating_sub(1));
    if !ui.rows.is_empty() && !ui.rows[ui.sel].selectable() {
        if let Some(next) = (ui.sel..ui.rows.len()).find(|i| ui.rows[*i].selectable()) {
            ui.sel = next;
        }
    }
    ui.message = msg;

    // Something was just created: put the cursor on it. Done after the rebuild
    // because until then the row does not exist.
    if let Some(want) = view.land_on.take() {
        if let Some(i) = ui.rows.iter().position(|r| match r {
            Row::Task { id, .. } => *id == want,
            Row::Project { id, .. } => *id == want,
            _ => false,
        }) {
            ui.sel = i;
        }
    }
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
    // An agent holding no task, wherever it is standing. These are the panes
    // there is something to *do* about, and the tree is the wrong place for
    // them: it is ordered by work, and an unassigned agent is precisely the one
    // that has none. They go in a dock at the foot instead, where they cannot
    // scroll out of sight.
    let mut unassigned: Vec<AgentRef> = Vec::new();

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
                    if a.agent.is_empty() {
                        loose_by_project.entry(p.clone()).or_default().push(as_ref(a));
                    } else {
                        unassigned.push(as_ref(a));
                    }
                }
            }
            // Resolves to nothing, or the workspace is deliberately pinned out
            // of the tree. Either way it belongs to no project.
            None => {
                if bound.is_none() {
                    // A shell that resolves nowhere is still a fact about
                    // nothing in particular, and stays in the tree's own
                    // group. An agent is a person's worth of attention going
                    // spare, and goes in the dock.
                    if a.agent.is_empty() {
                        homeless.push(as_ref(a));
                    } else {
                        unassigned.push(as_ref(a));
                    }
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
    pub(super) fn walk(
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

    // The dock. Last in the row list and pinned to the bottom of the frame, so
    // it is reachable by the cursor — `c` has to be able to land on one of
    // these — while never scrolling away like the tree above it.
    let dock = if unassigned.is_empty() {
        0
    } else {
        let folded = view.collapsed.contains(UNASSIGNED_KEY);
        rows.push(Row::Section {
            key: UNASSIGNED_KEY.to_string(),
            label: "unassigned".into(),
            count: unassigned.len(),
            collapsed: folded,
        });
        if folded {
            1
        } else {
            let n = unassigned.len();
            for a in unassigned {
                let title = a.where_.clone();
                rows.push(Row::Agent { agent: a, title, depth: 1 });
            }
            n + 1
        }
    };

    Ui {
        rows,
        dock,
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

pub(super) fn state_dot(state: &str) -> (Style, &'static str) {
    match state {
        "working" => (Style::Accent, glyph::WORKING),
        "idle" => (Style::Muted, glyph::IDLE),
        _ => (Style::Dim, glyph::QUIET),
    }
}

pub(super) fn render_row(row: &Row, w: usize, num: Option<u8>) -> Line {
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
        Row::Task { title, depth, status, agent, needs_you, prose, under, ident, .. } => {
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

            // Work beneath it, in the same vocabulary a project row uses —
            // the count is the same question asked one level down.
            let mut right = Line::default();
            if under.open > 0 {
                right.push(Style::Dim, under.open.to_string());
            }
            if under.doing > 0 {
                right.push(Style::Accent, format!(" {}{}", glyph::DOING, under.doing));
            }
            if under.blocked > 0 {
                right.push(Style::Warn, format!(" {}{}", glyph::BLOCKED, under.blocked));
            }
            if under.open == 0 && under.done > 0 {
                right.push(Style::Dim, format!(" {}", glyph::DONE));
            }
            // Reserve the marker's column before truncating, or a long title
            // eats the very sign that there is more to read.
            let flag_w = if *needs_you { 2 } else { 0 } + if *prose { 2 } else { 0 };
            let count_w = if right.spans.is_empty() { 0 } else { right.width() + 1 };
            // The id is the point when it is on, so it comes out of the
            // title's budget rather than off the end of the row.
            let id_w = ident.as_ref().map(|i| i.chars().count() + 2).unwrap_or(0);
            let avail = w.saturating_sub(*depth + 5 + flag_w + count_w + id_w);
            if let Some(id) = ident {
                l.push(Style::Dim, format!("{id}: "));
            }
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
            // Right-aligned, where a project row keeps the same numbers: they
            // are read down the column, not along the row.
            if !right.spans.is_empty() {
                l.pad(w.saturating_sub(l.width() + right.width()).max(1));
                l.spans.extend(right.spans);
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

/// Digits 1-9 address rows that lead somewhere: a terminal.
pub(super) fn hotkeys(rows: &[Row]) -> Vec<Option<u8>> {
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
