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

/// Stand in for the groups that are not projects, wherever a project id is used
/// as a key. Project ids are slugs, so the parentheses keep these out of their
/// namespace.
const INBOX_KEY: &str = "(inbox)";
const UNATTACHED_KEY: &str = "(unattached)";

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
}

enum Msg {
    Key(Key),
    /// Carries the workspace the event was about, when it named one.
    Herdr(Option<String>),
    Tick,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Key {
    Up,
    Down,
    Enter,
    Left,
    Right,
    Digit(u8),
    Char(char),
    Quit,
}

#[derive(Debug, Clone)]
pub(crate) struct AgentRef {
    pane: String,
    workspace: String,
    state: String,
    where_: String,
}

#[derive(Debug, Clone)]
enum Row {
    Project { id: String, depth: usize, counts: Counts, collapsed: bool, live: usize },
    Task {
        /// Carried so a row can name the task it stands for. Nothing acts on a
        /// task yet; this is what the edit keys will dispatch on.
        #[allow(dead_code)]
        id: String,
        title: String,
        depth: usize,
        status: Status,
        agent: Option<AgentRef>,
        needs_you: bool,
    },
    /// `key` is the project the hidden tasks belong to, or `INBOX_KEY`.
    More { key: String, depth: usize, n: usize },
    /// A group that is not a project: the inbox, loose agents. `key` names it
    /// so it can be folded and so a command can be aimed at it.
    Section { key: String, label: String, count: usize, collapsed: bool },
    Agent { agent: AgentRef, title: String },
}

impl Row {
    /// Every row takes the cursor. A heading you cannot select is a heading you
    /// cannot fold or add to, and both are things the groups need.
    fn selectable(&self) -> bool {
        true
    }
    fn agent(&self) -> Option<&AgentRef> {
        match self {
            Row::Task { agent: Some(a), .. } => Some(a),
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
    inbox: usize,
    blocked: usize,
    sel: usize,
    message: Option<(String, Instant)>,
    self_focused: bool,
    show_done: bool,
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

impl Ui {
    pub(crate) fn selected_target(&self) -> Target {
        match self.rows.get(self.sel) {
            Some(Row::Project { id, .. }) => Target::Project(id.clone()),
            Some(Row::Task { id, .. }) => Target::Task(id.clone()),
            Some(Row::More { key, .. }) => Target::Overflow(key.clone()),
            Some(Row::Agent { agent, .. }) => Target::Pane(agent.pane.clone()),
            Some(Row::Section { key, .. }) if key == UNATTACHED_KEY => Target::Unattached,
            Some(Row::Section { key, .. }) if key == INBOX_KEY => Target::Inbox,
            Some(Row::Section { .. }) => Target::Nothing,
            None => Target::Nothing,
        }
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
    pub agents: Vec<herdr::Agent>,
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
            agents: herdr::agents().unwrap_or_default(),
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
            title: t.title.clone(),
            depth,
            status: t.status(),
            agent: a.clone(),
            needs_you: *needs_you,
        });
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
    let agents = &snap.agents;
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
    // task id -> the agent working it
    let agent_for_task = |task_id: &str| -> Option<AgentRef> {
        agents.iter().find_map(|a| {
            if bound_task_of_pane(&a.pane_id).as_deref() == Some(task_id) {
                Some(AgentRef {
                    pane: a.pane_id.clone(),
                    workspace: a.workspace_id.clone(),
                    state: a.agent_status.clone(),
                    where_: ws_label(&a.workspace_id),
                })
            } else {
                None
            }
        })
    };

    // Which projects are worth showing: any with open tasks, or a live agent.
    let mut live_by_project: std::collections::BTreeMap<String, usize> = Default::default();
    for a in agents.iter() {
        let bound_project = bound_task_of_pane(&a.pane_id)
            .and_then(|id| tasks.iter().find(|t| t.id == id))
            .and_then(|t| t.project.clone());
        let r = resolve::resolve(
            &index,
            &pins,
            bound_project,
            Some(&a.workspace_id),
            Some(&ws_label(&a.workspace_id)),
            Some(&a.cwd),
        );
        if let Some(p) = r.project {
            for id in std::iter::once(p.clone()).chain(index.ancestors(&p)) {
                *live_by_project.entry(id).or_insert(0) += 1;
            }
        }
    }

    // Quiet branches stay folded away, but a project holding only finished work
    // must reappear the moment `show_done` is on — otherwise the toggle looks
    // broken on exactly the projects it exists to reveal.
    let interesting = |id: &str| -> bool {
        let c = counts.get(id).copied().unwrap_or_default();
        c.open > 0 || live_by_project.contains_key(id) || (view.show_done && c.done > 0)
    };

    let mut rows: Vec<Row> = Vec::new();
    let mut needs = 0;

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
            });
            if is_collapsed {
                continue;
            }

            // This project's own tasks, attention first.
            task_rows(tasks, Some(&p.id), depth + 1, view, agent_for_task, rows, needs);

            walk(
                index, Some(&p.id), depth + 1, rows, counts, live, view, tasks, interesting,
                agent_for_task, needs,
            );
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
        &interesting,
        &agent_for_task,
        &mut needs,
    );

    // Unparented tasks. These are real work with a real id — they just have
    // nowhere in the tree to hang, so they get their own heading rather than
    // living on as a number in the footer.
    let inbox_open =
        tasks.iter().filter(|t| t.project.is_none() && t.status().is_open()).count();
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

    // Agents not attached to any task — the panel's other job is making these
    // visible, because they are the ones nobody has decided about.
    let unattached: Vec<&herdr::Agent> = agents
        .iter()
        .filter(|a| bound_task_of_pane(&a.pane_id).is_none())
        .collect();
    if !unattached.is_empty() {
        let folded = view.collapsed.contains(UNATTACHED_KEY);
        rows.push(Row::Section {
            key: UNATTACHED_KEY.to_string(),
            label: "unattached".into(),
            count: unattached.len(),
            collapsed: folded,
        });
        if !folded {
            for a in unattached {
                rows.push(Row::Agent {
                    agent: AgentRef {
                        pane: a.pane_id.clone(),
                        workspace: a.workspace_id.clone(),
                        state: a.agent_status.clone(),
                        where_: ws_label(&a.workspace_id),
                    },
                    title: if a.title.is_empty() {
                        ws_label(&a.workspace_id)
                    } else {
                        a.title.clone()
                    },
                });
            }
        }
    }

    Ui {
        rows,
        agents_total: agents.len(),
        needs,
        inbox: tasks.iter().filter(|t| t.project.is_none() && t.status().is_open()).count(),
        blocked: tasks.iter().filter(|t| t.status() == Status::Blocked).count(),
        sel: 0,
        message: None,
        self_focused,
        show_done: view.show_done,
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
    fn push(&mut self, style: Style, text: impl Into<String>) {
        let text = text.into();
        if !text.is_empty() {
            self.spans.push(Span { text, style });
        }
    }

    fn width(&self) -> usize {
        self.spans.iter().map(|s| s.text.chars().count()).sum()
    }

    fn pad(&mut self, n: usize) {
        self.push(Style::Plain, " ".repeat(n));
    }

    /// Pad or clip to exactly `w` columns.
    fn fit(&mut self, w: usize) {
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

fn line(style: Style, text: impl Into<String>) -> Line {
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
        Row::Project { id, depth, counts, collapsed, live } => {
            l.pad(*depth);
            l.push(Style::Dim, if *collapsed { glyph::CLOSED } else { glyph::OPEN });
            l.push(Style::Plain, " ");
            l.push(Style::Bold, id.clone());

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
        Row::Task { title, depth, status, agent, needs_you, .. } => {
            match num {
                Some(n) => l.push(Style::Dim, n.to_string()),
                None => l.push(Style::Plain, " "),
            }
            l.pad(*depth);
            l.push(Style::Plain, " ");
            match agent {
                Some(a) => {
                    let (st, dot) = state_dot(&a.state);
                    l.push(st, dot);
                }
                None => match status {
                    Status::Blocked => l.push(Style::Warn, glyph::BLOCKED),
                    Status::Review => l.push(Style::Muted, glyph::REVIEW),
                    Status::Done => l.push(Style::Dim, glyph::DONE),
                    _ => l.push(Style::Dim, glyph::QUIET),
                },
            }
            l.push(Style::Plain, " ");
            let flag_w = if *needs_you { 2 } else { 0 };
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
        Row::Agent { agent, title } => {
            match num {
                Some(n) => l.push(Style::Dim, n.to_string()),
                None => l.push(Style::Plain, " "),
            }
            l.push(Style::Plain, " ");
            let (st, dot) = state_dot(&agent.state);
            l.push(st, dot);
            l.push(Style::Plain, " ");
            l.push(Style::Muted, util::truncate(title, w.saturating_sub(6).max(4)));

            let right = line(Style::Dim, util::truncate(&agent.where_, 10));
            let gap = w.saturating_sub(l.width() + right.width());
            if gap >= 2 {
                l.pad(gap);
                l.spans.extend(right.spans);
            }
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
                mark(&[(Style::Accent, g::WORKING), (Style::Accent, "2")], "agents", "agents that resolve to this project"),
            ],
        ),
        (
            "Everywhere else",
            "Rows and markers that are not about a single task.",
            vec![
                mark(&[(Style::Warn, g::NEEDS_YOU)], "wants you", "an idle agent on a task that is still doing — it has stopped and you are the blocker"),
                mark(&[(Style::Warn, "1 "), (Style::Warn, g::NEEDS_YOU)], "how many", "the same count, in the header"),
                mark(&[(Style::Dim, g::MORE), (Style::Muted, " 2 more")], "overflow", "past the six-task cap; ↵ opens the tail in place"),
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

/// The whole panel as styled lines. No escapes, no terminal — a backend turns
/// this into something you can look at.
pub(crate) fn frame(ui: &Ui, w: usize, h: usize) -> Vec<Line> {
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
    let body_rows = h.saturating_sub(lines.len() + footer_rows);
    let keys = hotkeys(&ui.rows);

    // Keep the selection in view.
    let mut scroll = 0usize;
    if body_rows > 0 && ui.sel >= body_rows {
        scroll = ui.sel + 1 - body_rows;
    }

    for (i, row) in ui.rows.iter().enumerate().skip(scroll).take(body_rows) {
        let mut l = render_row(row, w, keys[i]);
        l.selected = i == ui.sel;
        lines.push(l);
    }
    while lines.len() < h.saturating_sub(footer_rows) {
        lines.push(Line::default());
    }

    lines.push(line(Style::Dim, "─".repeat(w)));

    let mut foot = Line::default();
    foot.push(Style::Dim, "inbox");
    foot.push(Style::Plain, format!(" {}  ", ui.inbox));
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

    match &ui.message {
        Some((m, at)) if at.elapsed() < Duration::from_secs(4) => {
            lines.push(line(Style::Accent, util::truncate(m, w)))
        }
        _ => lines.push(line(Style::Dim, "1-9 go  ↵ open  ←→ fold  A done  q")),
    }

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

fn stty(args: &[&str]) {
    if let Ok(tty) = File::open("/dev/tty") {
        let _ = Command::new("stty")
            .args(args)
            .stdin(Stdio::from(tty))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn term_size() -> (usize, usize) {
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
        loop {
            match tty.read(&mut buf) {
                Ok(1) => {
                    let key = match buf[0] {
                        b'q' | 3 => Key::Quit, // raw mode delivers ctrl-c as a byte
                        b'j' => Key::Down,
                        b'k' => Key::Up,
                        b'h' => Key::Left,
                        b'l' => Key::Right,
                        b'\r' | b'\n' => Key::Enter,
                        b'\x1b' => {
                            let mut seq = [0u8; 2];
                            if tty.read(&mut seq[..1]).is_ok()
                                && seq[0] == b'['
                                && tty.read(&mut seq[1..]).is_ok()
                            {
                                match seq[1] {
                                    b'A' => Key::Up,
                                    b'B' => Key::Down,
                                    b'C' => Key::Right,
                                    b'D' => Key::Left,
                                    _ => continue,
                                }
                            } else {
                                continue;
                            }
                        }
                        c if c.is_ascii_digit() && c != b'0' => Key::Digit(c - b'0'),
                        c => Key::Char(c as char),
                    };
                    if tx.send(Msg::Key(key)).is_err() {
                        return;
                    }
                }
                Ok(_) => continue,
                Err(_) => return,
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

fn focus(agent: &AgentRef) {
    let _ = herdr::call("workspace.focus", json!({ "workspace_id": agent.workspace }));
    let _ = herdr::call("pane.focus", json!({ "pane_id": agent.pane }));
}

// ---- main loop ----------------------------------------------------------

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
            std::thread::sleep(Duration::from_secs(5));
            if tx.send(Msg::Tick).is_err() {
                return;
            }
        });
    }

    print!("\x1b[?1049h\x1b[?25l");
    let _ = std::io::stdout().flush();
    stty(&["raw", "-echo"]);

    let code = event_loop(store, &rx, self_ws.as_deref());

    stty(&["sane"]);
    print!("\x1b[?25h\x1b[?1049l");
    let _ = std::io::stdout().flush();
    code
}

/// What a key asked for beyond changing the view.
pub(crate) enum Effect {
    None,
    Refetch,
    Focus(AgentRef),
    Sync,
}

/// The reducer. Deliberately free of I/O — it moves the selection and the fold
/// state and reports what else it wants done, so the storyboard can drive the
/// same transitions the terminal does and get the same frames out.
pub(crate) fn apply_key(k: Key, ui: &mut Ui, view: &mut View) -> Effect {
    let n = ui.rows.len();
    match k {
        Key::Down => {
            let mut i = ui.sel;
            while i + 1 < n {
                i += 1;
                if ui.rows[i].selectable() {
                    break;
                }
            }
            ui.sel = i;
            Effect::None
        }
        Key::Up => {
            let mut i = ui.sel;
            while i > 0 {
                i -= 1;
                if ui.rows[i].selectable() {
                    break;
                }
            }
            ui.sel = i;
            Effect::None
        }
        Key::Left | Key::Right => match ui.rows.get(ui.sel) {
            // Projects and groups fold alike; only their key differs.
            Some(Row::Project { id: key, .. }) | Some(Row::Section { key, .. }) => {
                if k == Key::Left {
                    view.collapsed.insert(key.clone());
                    // Folding forgets that it was showing its long tail, so
                    // unfolding starts clean.
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
        Key::Enter => match ui.rows.get(ui.sel) {
            Some(Row::Project { id: key, collapsed: c, .. })
            | Some(Row::Section { key, collapsed: c, .. }) => {
                if *c {
                    view.collapsed.remove(key);
                } else {
                    view.collapsed.insert(key.clone());
                    view.expanded.remove(key);
                }
                Effect::Refetch
            }
            Some(Row::More { key, .. }) => {
                view.expanded.insert(key.clone());
                Effect::Refetch
            }
            Some(row) => match row.agent() {
                Some(a) => Effect::Focus(a.clone()),
                None => {
                    ui.message = Some(("no agent on that task".into(), Instant::now()));
                    Effect::None
                }
            },
            None => Effect::None,
        },
        Key::Digit(d) => {
            let keys = hotkeys(&ui.rows);
            match keys
                .iter()
                .position(|k| *k == Some(d))
                .and_then(|i| ui.rows[i].agent())
            {
                Some(a) => Effect::Focus(a.clone()),
                None => Effect::None,
            }
        }
        Key::Char('g') => {
            ui.sel = 0;
            Effect::None
        }
        Key::Char('G') => {
            ui.sel = n.saturating_sub(1);
            Effect::None
        }
        Key::Char('A') => {
            view.show_done = !view.show_done;
            ui.message = Some((
                if view.show_done { "showing done" } else { "hiding done" }.into(),
                Instant::now(),
            ));
            Effect::Refetch
        }
        Key::Char('r') => Effect::Sync,
        Key::Char('?') => {
            ui.message = Some((
                "j/k move · ↵ fold/open · 1-9 jump · A done · r sync · q quit".into(),
                Instant::now(),
            ));
            Effect::None
        }
        _ => Effect::None,
    }
}

fn event_loop(store: &Store, rx: &Receiver<Msg>, self_ws: Option<&str>) -> i32 {
    let mut view = View::default();
    let mut ui = collect(&Snapshot::live(store), &view, self_ws);
    let mut last = String::new();
    let mut dirty = false;
    let mut last_fetch = Instant::now();
    let mut last_fingerprint = store.fingerprint();

    let draw = |ui: &Ui, last: &mut String| {
        let (w, h) = term_size();
        let painted = to_ansi(&frame(ui, w, h), w, h);
        if painted != *last {
            print!("{painted}");
            let _ = std::io::stdout().flush();
            *last = painted;
        }
    };
    draw(&ui, &mut last);

    loop {
        let msg = match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(m) => m,
            Err(RecvTimeoutError::Timeout) => Msg::Tick,
            Err(RecvTimeoutError::Disconnected) => return 0,
        };

        let mut refetch = false;
        match msg {
            Msg::Key(Key::Quit) => return 0,
            Msg::Key(k) => match apply_key(k, &mut ui, &mut view) {
                Effect::None => {}
                Effect::Refetch => refetch = true,
                Effect::Focus(a) => focus(&a),
                Effect::Sync => {
                    let mut cache = crate::sync::Cache::default();
                    let _ = crate::sync::sync(store, &mut cache, true);
                    ui.message = Some(("synced".into(), Instant::now()));
                    refetch = true;
                }
            },
            Msg::Herdr(ws) => {
                // An event naming our own workspace means we are probably about
                // to be looked at: redraw now. Otherwise mark dirty and let the
                // tick decide, so twenty-odd background panels stay cheap.
                let concerns_us = ws.is_some() && ws.as_deref() == self_ws;
                if concerns_us || ui.self_focused {
                    std::thread::sleep(Duration::from_millis(250));
                    while rx.try_recv().is_ok() {}
                    refetch = true;
                } else {
                    dirty = true;
                }
            }
            Msg::Tick => {
                let interval = if ui.self_focused {
                    Duration::from_secs(5)
                } else {
                    Duration::from_secs(30)
                };
                let store_changed = store.fingerprint() != last_fingerprint;
                if (dirty || store_changed) && last_fetch.elapsed() >= interval {
                    refetch = true;
                    dirty = false;
                }
            }
        }

        if refetch {
            last_fetch = Instant::now();
            last_fingerprint = store.fingerprint();
            refetch_into(&mut ui, &Snapshot::live(store), &view, self_ws);
        }
        draw(&ui, &mut last);
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
    let Some(target) = panes.first() else {
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
