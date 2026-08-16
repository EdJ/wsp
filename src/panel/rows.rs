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
use crate::model::{Priority, Status, Task};
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

/// The section pinned at the foot: the agents, whatever they are doing.
pub(super) const AGENTS_KEY: &str = "(agents)";

/// How many agents the foot keeps on screen before the rest need `w`. Five is
/// what a pane can spare beside a tree and still be a tree — the strip in the
/// header carries the census in full, and this carries the top of it in the
/// form you can aim a verb at.
pub(super) const MAX_AGENTS_DOCKED: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentRef {
    pub(super) pane: String,
    pub(super) workspace: String,
    pub(super) state: String,
    /// What to call it — see [`pane_name`]. The pane's own label first, which
    /// `claim` and `wsp say` keep current, then its terminal title, then the
    /// workspace it stands in.
    pub(super) where_: String,
    /// Whether an agent is running here, or it is just a shell.
    pub(super) agent: bool,
    /// Which agent, as herdr spells it — `claude`, `codex`, `gemini` — and the
    /// empty string for a shell. Carried because one thing a verb does to a
    /// pane is not the same sentence at every kind: emptying a context is
    /// `/clear` at Claude Code and something else, or nothing, everywhere else.
    pub(super) kind: String,
    /// The task claimed to this pane, if it holds one. Carried so a verb aimed
    /// at the pane can refuse on the ground that matters — it already has work
    /// — rather than on where the row happens to sit.
    pub(super) task: Option<String>,
    /// The project this pane would take work from: its mandate if it has one,
    /// else wherever it is standing. Deliberately not the same question as
    /// which branch of the tree the row is drawn under — standing direction
    /// says what a pane is *for*, and the tree places it by where it *is*.
    pub(super) project: Option<String>,
}

impl AgentRef {
    /// The terminal this stands for. The only field anything outside the panel
    /// has business with: a pane id is what a command names and what a test can
    /// check a click against.
    #[cfg(test)]
    pub(crate) fn pane(&self) -> &str {
        &self.pane
    }
}

/// What an agent is waiting for, as far as anything here can tell.
///
/// herdr reports two states, working and idle, and `idle` is an answer to a
/// question nobody asked: an agent that has stopped is waiting for *something*,
/// and which something decides whether you have to get up. The store holds the
/// other half of it. A pane still holding a task that is `doing` has stopped
/// part-way through and is waiting on you; one holding a task at `blocked` is
/// waiting on a decision that is at least written down; one holding nothing is
/// waiting for work. Same two states from herdr, three different answers.
///
/// Declaration order is the order they are drawn in and sorted by: what wants
/// an answer, then what is free, then what is busy, then what has not said.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum AgentState {
    /// Stopped, on a task that is still `doing`. You are what it is waiting for.
    Asking,
    /// Stopped, on a task it has parked with a question written on it.
    Blocked,
    /// Stopped, holding nothing. A person's worth of attention going spare.
    ///
    /// Above `Working` deliberately. Both the strip and the list are read for
    /// what to do next, and there is nothing to do about an agent that is
    /// working — where a dozen busy panes sorted first, the one going spare
    /// fell off the end of the five the panel keeps on screen, which is the
    /// row the whole section exists to show.
    Spare,
    Working,
    /// herdr says neither working nor idle, so nothing here is going to pretend
    /// to know. Mostly a pane whose agent has not spoken since it started.
    Quiet,
}

impl AgentState {
    pub(super) fn mark(self) -> (Style, &'static str) {
        match self {
            AgentState::Asking => (Style::Warn, glyph::NEEDS_YOU),
            AgentState::Blocked => (Style::Warn, glyph::BLOCKED),
            AgentState::Working => (Style::Accent, glyph::WORKING),
            AgentState::Spare => (Style::Muted, glyph::IDLE),
            AgentState::Quiet => (Style::Dim, glyph::QUIET),
        }
    }
}

/// What an agent row carries when it is standing on its own, in the agents
/// view, rather than nested under the task or project that explains it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Census {
    pub(super) state: AgentState,
    /// Where its work lives: the project of the task it holds, else the one it
    /// is standing in or pointed at. The tree says this by where it draws the
    /// row; a flat list has to say it in words.
    pub(super) project: Option<String>,
    /// The task in its hands, as the panel would draw it anywhere else.
    pub(super) task: Option<(Status, String)>,
    /// How long it has held that task, if the claim says when it took it.
    pub(super) held: Option<String>,
}

/// Read the two halves together: what herdr says the pane is doing, and what
/// the store says it is holding while it does it.
pub(super) fn agent_state(herdr_state: &str, holds: Option<Status>) -> AgentState {
    match herdr_state {
        "working" => AgentState::Working,
        // `done` is a third value herdr sends and nothing here knew about. It
        // is a turn ending rather than an agent leaving: 120s of the event
        // stream held three `working -> done` and three `done -> idle`, and
        // forty one-second samples of `pane.list` never caught a pane in it at
        // all. So it means what `idle` means, and mapping it to `Quiet` —
        // "herdr says neither working nor idle, so nothing here will pretend to
        // know" — would draw the one moment we know most about as the one we
        // know nothing about. Which the panel is now far likelier to sample,
        // since a status change is what it refetches on.
        "idle" | "done" => match holds {
            Some(Status::Blocked) => AgentState::Blocked,
            // A claim left on finished work is not work: the agent is free
            // whatever the binding still says.
            Some(Status::Done) | None => AgentState::Spare,
            // Everything else is a task in its hands with the hands stopped —
            // `doing` most of the time, `todo` for the moment between a claim
            // landing and the agent starting, `review` when it has handed the
            // work back and is standing there waiting to be told what next.
            Some(_) => AgentState::Asking,
        },
        _ => AgentState::Quiet,
    }
}

#[derive(Debug, Clone)]
pub(super) enum Row {
    Project {
        id: String,
        /// What `wsp project show` calls it, carried but never drawn.
        ///
        /// The tree is ids: a slug is short, unique and the thing you type at
        /// a shell, and every project in the real store has a name that is
        /// longer — `strata-strategy` is "Questions & Strategy" in a pane
        /// thirty-four columns wide. So the row draws the id and this is here
        /// for the one key that needs the other string: `e` opens its prompt
        /// holding the name it is about to change, the same as a task's, and
        /// the reducer has no store to go and ask.
        name: String,
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
        /// Where it sits in its project's queue. Carried on the row because
        /// `!` has to know what it is cycling away from, and because it is
        /// what the rows are ordered by beneath status.
        priority: Priority,
        /// The task's *own* tags, and deliberately not the effective ones.
        /// `t` marks these as the ones it can actually take off.
        tags: Vec<String>,
        /// The tags that reach this task from its project chain, each with the
        /// project it comes from.
        ///
        /// Carried beside the task's own rather than merged into them, because
        /// the two are not the same thing and the difference is exactly what
        /// `t` has to draw. `wsp show` and the detail pane merge them, which is
        /// right for reading and wrong for editing: a task under `render` reads
        /// `rust herdr` and owns neither, so a picker that offered only its own
        /// tags showed `rust` as *absent* on a task every other surface says is
        /// tagged `rust` — and removing it looked broken rather than
        /// impossible.
        inherited: Vec<(String, String)>,
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
    /// `census` is set only in the agents view, where the row stands on its
    /// own: there it has to say what the pane is waiting for and where it
    /// belongs, because there is no branch above it doing either. In the tree
    /// it is `None` and the row draws as it always has, under the thing that
    /// explains it.
    Agent { agent: AgentRef, title: String, depth: usize, census: Option<Census> },
    /// A line that belongs to the row above it. The cursor never lands on one
    /// and no digit addresses one: it is the second and third line of one
    /// agent, in the view that has the room to spend three lines on one.
    ///
    /// A row rather than extra lines inside one, because the frame maps screen
    /// rows to row indices one for one — a row that drew two lines would put
    /// every click below it on the wrong thing.
    Detail(Detail),
}

#[derive(Debug, Clone)]
pub(super) enum Detail {
    /// What it is waiting for, which terminal it is, and how long it has been
    /// holding what it holds.
    Standing { state: AgentState, pane: String, held: Option<String>, direction: Option<String> },
    /// The task in its hands, drawn the way the tree draws a task.
    Holding { status: Status, title: String },
}

impl Row {
    /// Every row takes the cursor. A heading you cannot select is a heading you
    /// cannot fold or add to, and both are things the groups need.
    pub(super) fn selectable(&self) -> bool {
        !matches!(self, Row::Detail(_))
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
    /// A line under an agent in the agents view. Never selected, so a driver
    /// that hunts for one has gone wrong.
    Detail,
    Nothing,
}

#[derive(Clone)]
pub(crate) struct Ui {
    pub(super) rows: Vec<Row>,
    /// How many of the trailing rows are the unassigned dock rather than tree.
    pub(super) dock: usize,
    /// One entry per running agent, sorted, for the strip in the header. Every
    /// agent on the machine is in here whichever view is up — the strip is the
    /// census, and a census that only counted what the tree happened to be
    /// showing would go quiet exactly when a filter was on.
    ///
    /// The pane comes with the state because a mark in the strip is clickable:
    /// it is the one row-less thing on the panel that leads somewhere.
    pub(super) census: Vec<(AgentState, AgentRef)>,
    pub(super) blocked: usize,
    /// Tasks an agent has finished with and handed back. `review` is where an
    /// agent's work ends; only a person says `done`, so this is a count of
    /// things waiting on one.
    pub(super) review: usize,
    pub(super) sel: usize,
    pub(super) message: Option<(String, Instant)>,
    pub(super) self_focused: bool,
    /// Every tag anything in the store carries, commonest first.
    ///
    /// Tags are a small closed vocabulary — nineteen across the whole store,
    /// counting projects — and that is the fact `t` is built on. Picking from
    /// a list you can see beats typing a name you have to remember, and it is
    /// the only way to take one *off* without spelling it out. Gathered from
    /// projects as well as tasks, because a tag's first use is nearly always
    /// on a project and it has to be offerable before any task carries it.
    pub(super) vocabulary: Vec<String>,
    pub(super) show_done: bool,
    pub(super) review_only: bool,
    /// The agents are in place of the tree.
    pub(super) agents: bool,
}

/// What the cursor is sitting on, in the store's own terms rather than the
/// panel's. This is the seam the edit subcommands dispatch against: a command
/// asks what the target is and refuses the ones it cannot act on, instead of
/// every key having to re-read the row enum.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
    #[default]
    Nothing,
}

/// Where the cursor is: what the row *is*, and which of the two lists it is in.
///
/// The identity alone is ambiguous, because the panel draws some rows twice on
/// purpose. A pane appears under the task it claimed and again in the dock at
/// the foot; the `agents` heading and the `no project` heading are both
/// [`Target::Unattached`]. Keeping the cursor by identity across a rebuild —
/// which is the whole point of keeping it by identity — then meant taking the
/// first row that matched, and the first is always the one in the tree. So a
/// cursor walked down into the dock was pulled straight back up to wherever
/// that agent's work happens to sit: scrolling down jumped the view up, and
/// went on doing it every quarter-second for as long as the cursor was there.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Cursor {
    pub(crate) target: Target,
    /// In the pinned section at the foot rather than the tree above it.
    pub(crate) docked: bool,
}

impl From<Target> for Cursor {
    /// A row in the tree, which is where all but a handful of them are. The
    /// dock is the exception and has to say so.
    fn from(target: Target) -> Cursor {
        Cursor { target, docked: false }
    }
}

impl Cursor {
    /// The row this names, looked for on the side of the dock it was last on.
    /// Falls across to the other side rather than giving up: the dock empties
    /// when the last agent goes, and a pane row that was in it can genuinely
    /// reappear in the tree.
    pub(super) fn find_in(&self, rows: &[Row], tree_len: usize) -> Option<usize> {
        let hit = |from: usize, to: usize| {
            rows.get(from..to)?
                .iter()
                .position(|r| target_of(r) == self.target)
                .map(|i| i + from)
        };
        let n = rows.len();
        if self.docked {
            hit(tree_len, n).or_else(|| hit(0, tree_len))
        } else {
            hit(0, tree_len).or_else(|| hit(tree_len, n))
        }
    }
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
        Row::Section { key, .. } if key == AGENTS_KEY => Target::Unattached,
        Row::Section { key, .. } if key == INBOX_KEY => Target::Inbox,
        Row::Section { .. } => Target::Nothing,
        Row::Detail(_) => Target::Nothing,
    }
}

impl Ui {
    /// Move the cursor without going through a key, so a test can ask what a
    /// click would leave behind.
    #[cfg(test)]
    pub(crate) fn select_for_test(&mut self, i: usize) {
        self.sel = i;
    }

    /// How many rows there are, for a test that has to speak for all of them
    /// rather than for the one under the cursor.
    #[cfg(test)]
    pub(crate) fn rows_for_test(&self) -> usize {
        self.rows.len()
    }

    /// How many of those are the pinned section at the foot.
    #[cfg(test)]
    pub(crate) fn dock_for_test(&self) -> usize {
        self.dock
    }

    /// Every row standing for the same thing, for a test that has to prove the
    /// panel draws one of them twice on purpose.
    #[cfg(test)]
    pub(crate) fn rows_for_target(&self, want: &Target) -> Vec<usize> {
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, r)| target_of(r) == *want)
            .map(|(i, _)| i)
            .collect()
    }

    /// The census the header strip is drawn from, for a test that has to check
    /// the strip against the agents it stands for.
    #[cfg(test)]
    pub(crate) fn census_for_test(&self) -> Vec<(AgentState, AgentRef)> {
        self.census.clone()
    }

    pub(crate) fn selected_target(&self) -> Target {
        self.rows.get(self.sel).map(target_of).unwrap_or(Target::Nothing)
    }

    /// Where the tree ends and the pinned dock begins.
    pub(super) fn tree_len(&self) -> usize {
        self.rows.len().saturating_sub(self.dock)
    }

    /// Where the cursor is, in full: the row's identity *and* which of the two
    /// lists it is standing in. See [`Cursor`] for why the identity alone is
    /// not enough.
    pub(crate) fn cursor(&self) -> Cursor {
        Cursor { target: self.selected_target(), docked: self.sel >= self.tree_len() }
    }

    pub(crate) fn selected_kind(&self) -> RowKind {
        match self.rows.get(self.sel) {
            Some(Row::Project { .. }) => RowKind::Project,
            Some(Row::Task { .. }) => RowKind::Task,
            Some(Row::More { .. }) => RowKind::More,
            Some(Row::Section { .. }) => RowKind::Section,
            Some(Row::Agent { .. }) => RowKind::Agent,
            Some(Row::Detail(_)) => RowKind::Detail,
            None => RowKind::Nothing,
        }
    }

    pub(crate) fn selected_index(&self) -> usize {
        self.sel
    }

    /// The row for a pane, wherever in the tree it is drawn. A claim names a
    /// pane that may be nowhere near the cursor — `c` on a task picks one from
    /// the dock — so the pane has to be found rather than read off the
    /// selection.
    pub(super) fn agent_at_pane(&self, pane: &str) -> Option<&AgentRef> {
        self.rows.iter().filter_map(|r| r.agent()).find(|a| a.pane == pane)
    }

    /// The agent holding a task, if one is.
    ///
    /// Off the census rather than the rows, unlike [`Ui::agent_at_pane`]. A
    /// pane row can be absent for reasons that have nothing to do with the
    /// claim — a folded project, the dock's cap, the tree filtered to review —
    /// and `u` on a task row is a question about who holds it, which is a fact
    /// about the world and not about what is currently drawn.
    pub(super) fn agent_on_task(&self, task: &str) -> Option<&AgentRef> {
        self.census.iter().map(|(_, a)| a).find(|a| a.task.as_deref() == Some(task))
    }

    /// Somebody free to hand work to, preferring one already pointed at the
    /// project the work is in.
    ///
    /// Off the census, like [`Ui::agent_on_task`] and for the same reason: who
    /// is free is a fact about the machine, and the dock draws five of them.
    /// `C` would otherwise miss the spare agent that a fold, a filter or the
    /// cap happened to be hiding, and say nobody was free while the strip in
    /// the header was showing one.
    ///
    /// [`AgentState::Spare`] is the whole test. It already means "stopped, and
    /// holding no live work" — the two halves a hand-over needs — and it is the
    /// same word the strip, the dock and the agents view draw, so the agent
    /// this finds is one you can see it choose.
    ///
    /// The preference is exact and does not climb the tree. A pane pointed at
    /// `verb` is in that checkout; the one standing in its parent is somewhere
    /// else on disk, and calling that a match would hand over work in a tree
    /// the agent is not in while an exact answer sat further down the list.
    /// When nothing matches, any spare agent will do — that is what `c` already
    /// lets you pick, and refusing here would leave the key useless on exactly
    /// the afternoons it is for.
    pub(super) fn spare_agent(&self, project: Option<&str>) -> Option<&AgentRef> {
        let spare = || {
            self.census
                .iter()
                .filter(|(state, _)| *state == AgentState::Spare)
                .map(|(_, a)| a)
        };
        project
            .and_then(|p| spare().find(|a| a.project.as_deref() == Some(p)))
            .or_else(|| spare().next())
    }

    /// Which project a task row sits under.
    pub(super) fn project_of_task(&self, task: &str) -> Option<String> {
        self.rows.iter().find_map(|r| match r {
            Row::Task { id, project, .. } if id == task => project.clone(),
            _ => None,
        })
    }

    /// What a task is called, in full. The row carries the whole title and
    /// cuts it only at draw time, so retitling can start from what is
    /// already there without going back to the store for it.
    pub(super) fn title_of_task(&self, task: &str) -> Option<String> {
        self.rows.iter().find_map(|r| match r {
            Row::Task { id, title, .. } if id == task => Some(title.clone()),
            _ => None,
        })
    }

    /// What it is now, so one key can say what it becomes. Read off the row
    /// for the same reason the title is: the frame in front of you is what the
    /// keystroke is answering.
    pub(super) fn priority_of_task(&self, task: &str) -> Option<Priority> {
        self.rows.iter().find_map(|r| match r {
            Row::Task { id, priority, .. } if id == task => Some(*priority),
            _ => None,
        })
    }

    /// The tags it carries of its own — the ones `t` can take off. Empty is a
    /// real answer and not a missing one.
    pub(super) fn tags_of_task(&self, task: &str) -> Vec<String> {
        self.rows
            .iter()
            .find_map(|r| match r {
                Row::Task { id, tags, .. } if id == task => Some(tags.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// The tags that reach it from its project, and where each comes from.
    /// Drawn by the picker and never editable there: the only way one of these
    /// comes off is on the project that carries it.
    pub(super) fn inherited_tags_of_task(&self, task: &str) -> Vec<(String, String)> {
        self.rows
            .iter()
            .find_map(|r| match r {
                Row::Task { id, inherited, .. } if id == task => Some(inherited.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// What a project is called, as against what it is filed under. See
    /// [`Row::Project::name`] for why the two are different things here.
    pub(super) fn name_of_project(&self, project: &str) -> Option<String> {
        self.rows.iter().find_map(|r| match r {
            Row::Project { id, name, .. } if id == project => Some(name.clone()),
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
    /// workspace id -> mandate record. Here because a pane with no task still
    /// has a project it is *for*, and that is the one the panel sends it to
    /// look in.
    pub mandates: std::collections::BTreeMap<String, serde_json::Value>,
    /// task id -> claim record. Read for one field, `claimed_at`: how long a
    /// pane has been holding what it holds is the difference between an agent
    /// working and an agent stuck, and it is the one fact herdr cannot supply.
    pub claims: std::collections::BTreeMap<String, serde_json::Value>,
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
            mandates: store.mandates(),
            claims: store.claims(),
            workspaces: herdr::workspaces().unwrap_or_default(),
            panes: herdr::panes().unwrap_or_default(),
        }
    }
}

/// What to call a pane's row: its label, else its terminal title, else the
/// workspace it stands in.
///
/// The label comes first because it is the only one of the three that is kept
/// up to date. `claim` writes the task into it and `wsp say` writes whatever
/// the agent is doing right now, so on a task's own row the line beneath reads
/// as progress — the task above, the state of it below.
///
/// The terminal title is what an agent called itself when it started and never
/// revises: a pane three tasks later still announced its opening prompt, which
/// is worse than useless, because it is a specific and confident answer to the
/// question and it is wrong. It stays as the fallback for panes wsp has never
/// named, where it is the best thing on offer — and the workspace label behind
/// that, for a shell, which has no title of its own but is still worth naming
/// by where it stands.
pub(crate) fn pane_name(label: &str, title: &str, workspace: &str) -> String {
    [label, title, workspace]
        .into_iter()
        .map(str::trim)
        .find(|s| !s.is_empty())
        .unwrap_or_default()
        .to_string()
}

pub(super) fn task_sort_key(t: &Task, has_agent: bool, needs_you: bool) -> (u8, u8, u8, u8, String) {
    (
        u8::from(!needs_you),
        u8::from(!has_agent),
        t.status().rank(),
        // Under status, not over it: `!` on something not started must not
        // jump it above work already in hand. This is the whole of what
        // priority does here — it orders one project's tasks against each
        // other, and the tree is what keeps that comparison local, since two
        // tasks in different projects are never in the same list to sort.
        t.priority().rank(),
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
    index: &Index,
    agent_for_task: &dyn Fn(&str) -> Option<AgentRef>,
    rows: &mut Vec<Row>,
) {
    let mut mine: Vec<Task> = tasks
        .iter()
        .filter(|t| t.project.as_deref() == project)
        .filter(|t| {
            if view.review_only {
                t.status() == Status::Review
            } else {
                view.show_done || t.status().is_open()
            }
        })
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
        rows.push(Row::Task {
            id: t.id.clone(),
            project: t.project.clone(),
            title: t.title.clone(),
            depth: depth + sub,
            status: t.status(),
            priority: t.priority(),
            tags: t.tags.clone(),
            inherited: t
                .project
                .as_deref()
                .map(|p| {
                    index
                        .effective_tags(p)
                        .into_iter()
                        .filter_map(|tag| {
                            index.tag_source(p, &tag).map(|from| (tag, from))
                        })
                        .collect()
                })
                .unwrap_or_default(),
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
                census: None,
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
    let was = ui.cursor();
    let msg = ui.message.take();
    *ui = collect(snap, view, self_ws);
    // The row, not the slot it was in. Claiming re-sorts the tree under the
    // cursor — the pane row leaves one task and reappears under another, often
    // several lines away — and holding the index would leave the eye on
    // whatever slid into its place. Falls back to the index when the row is
    // genuinely gone, which is the only case where there is nothing to follow.
    //
    // Which side of the dock it was on is part of the row's identity here: a
    // pane is drawn twice, and taking the first match took the tree copy every
    // time. See [`Cursor`].
    let tree_len = ui.tree_len();
    ui.sel = match was.target {
        Target::Nothing => sel,
        _ => was.find_in(&ui.rows, tree_len).unwrap_or(sel),
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
            // And the view owes it a look. The tree is allowed to sit with the
            // cursor off the pane — a wheel puts it there — but not when the
            // cursor has just been moved onto something you made: you would
            // type a name, press return, and watch nothing appear.
            view.keyed = true;
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
    // `project` is what the pane would take *work* from, which the caller has
    // (see `pane_name` above for how a pane row is named)
    // usually just resolved for its own reasons — passed in rather than worked
    // out again here, because resolution canonicalises every project root and
    // this runs four times a second against every pane on the machine.
    let as_ref = |a: &herdr::Pane, project: Option<String>| AgentRef {
        pane: a.pane_id.clone(),
        workspace: a.workspace_id.clone(),
        state: a.agent_status.clone(),
        where_: pane_name(&a.label, &a.title, &ws_label(&a.workspace_id)),
        agent: !a.agent.is_empty(),
        kind: a.agent.clone(),
        task: bound_task_of_pane(&a.pane_id),
        project,
    };

    // task id -> the pane claimed to it. No project: a pane holding a task is
    // not looking for one, and every verb that would ask refuses on the task
    // first.
    let agent_for_task = |task_id: &str| -> Option<AgentRef> {
        panes.iter().find_map(|a| {
            (bound_task_of_pane(&a.pane_id).as_deref() == Some(task_id)).then(|| as_ref(a, None))
        })
    };

    // Place every unclaimed pane against a project. Claimed ones already have
    // a home: the task they are bound to.
    let mut live_by_project: std::collections::BTreeMap<String, usize> = Default::default();
    let mut loose_by_project: std::collections::BTreeMap<String, Vec<AgentRef>> = Default::default();
    let mut homeless: Vec<AgentRef> = Vec::new();
    // Every running agent, with what it is waiting for and where its work is.
    // Gathered in this same pass because resolving a pane's project is the
    // expensive half of this function and it runs four times a second — and
    // because the two answers must not be worked out twice and disagree.
    let mut census: Vec<(Census, AgentRef)> = Vec::new();

    for a in panes.iter() {
        let bound = bound_task_of_pane(&a.pane_id);
        let bound_project = bound
            .as_ref()
            .and_then(|id| tasks.iter().find(|t| &t.id == id))
            .and_then(|t| t.project.clone());
        // The claim is per workspace and the binding per pane, so a
        // workspace with three shells in it places all three by the task it
        // holds — which is the whole of what `adopt` wrote down and nothing
        // was reading.
        let r = resolve::resolve(
            &index,
            pins,
            resolve::Held {
                binding: bound_project,
                claim: resolve::claimed_project(
                    &snap.claims,
                    &tasks,
                    Some(&a.workspace_id),
                    Some(&ws_label(&a.workspace_id)),
                ),
            },
            Some(&a.workspace_id),
            Some(&ws_label(&a.workspace_id)),
            Some(&a.cwd),
        );
        // Where it stands places the row; standing direction says what it is
        // for, and that is what a verb sends it to work. A mandate on `data`
        // and a cwd in `wsp` are both true at once — the tree wants the second
        // and `f` wants the first.
        let direction = crate::cmd_mandate::from_map(&snap.mandates, &a.workspace_id)
            .filter(|p| index.get(p).is_some())
            .or_else(|| r.project.clone());
        // Shells are not in the census. A pane with nobody in it is a fact
        // about a place, and the agents view is a list of people.
        if !a.agent.is_empty() {
            let holds = bound.as_ref().and_then(|id| tasks.iter().find(|t| &t.id == id));
            census.push((
                Census {
                    state: agent_state(&a.agent_status, holds.map(|t| t.status())),
                    // Where it stands, before where it is aimed: a pane holding
                    // a task is placed by that task's project, and only one
                    // with no work of its own is described by its direction.
                    project: r.project.clone().or_else(|| direction.clone()),
                    task: holds.map(|t| (t.status(), t.title.clone())),
                    // From the claim rather than the binding: a binding is
                    // remade whenever a pane is, and would reset the clock on
                    // work nobody had put down.
                    held: holds
                        .and_then(|t| snap.claims.get(&t.id))
                        .and_then(|c| c.get("claimed_at"))
                        .and_then(|c| c.as_str())
                        .filter(|c| !c.is_empty())
                        .map(|c| util::duration_human(util::since(c))),
                },
                as_ref(a, direction.clone()),
            ));
        }
        match &r.project {
            Some(p) => {
                for id in std::iter::once(p.clone()).chain(index.ancestors(p)) {
                    *live_by_project.entry(id).or_insert(0) += 1;
                }
                // Shells only. An agent is in the census, and the census is
                // pinned at the foot where it cannot scroll away — putting it
                // in the tree as well would be the same pane twice in one
                // glance, which is one pane too many to count.
                if bound.is_none() && a.agent.is_empty() {
                    loose_by_project.entry(p.clone()).or_default().push(as_ref(a, direction));
                }
            }
            // Resolves to nothing, or the workspace is deliberately pinned out
            // of the tree. Either way it belongs to no project.
            None => {
                // A shell that resolves nowhere is still a fact about nothing
                // in particular, and keeps the tree's own group for it.
                if bound.is_none() && a.agent.is_empty() {
                    homeless.push(as_ref(a, direction));
                }
            }
        }
    }

    // Quiet branches stay folded away, but a project holding only finished work
    // must reappear the moment `show_done` is on — otherwise the toggle looks
    // broken on exactly the projects it exists to reveal.
    let interesting = |id: &str| -> bool {
        let c = counts.get(id).copied().unwrap_or_default();
        // Under the review filter a branch earns its row only by holding
        // something at review. A project row with nothing beneath it is the
        // whole tree pretending the filter did nothing.
        if view.review_only {
            return c.review > 0;
        }
        c.open > 0
            || live_by_project.contains_key(id)
            || (view.show_done && c.done > 0)
            // A project holding nothing at all is not a quiet branch. There is
            // no work behind the row to go and look at, so folding it away
            // tidies nothing — it takes the project itself out of the panel,
            // and `a`, `X`, `O` and `S` are all pressed on that row. Retiring a
            // project's last task used to leave the project unreachable from
            // the panel that had just emptied it, with the CLI the only way
            // back. Distinct from all-finished, which stays quiet: `show_done`
            // is the question that asks for that.
            || (c.open == 0 && c.done == 0)
            || view.reveal.contains(id)
    };

    let mut rows: Vec<Row> = Vec::new();

    // Unparented tasks, first. They are the only work with nowhere to belong,
    // so they are what you triage before reading anything that already has a
    // home — and putting them last meant scrolling past every project to find
    // the one list that needs a decision.
    let inbox_open = tasks.iter().filter(|t| t.project.is_none() && t.status().is_open()).count();
    let inbox_any = tasks.iter().any(|t| {
        t.project.is_none()
            && if view.review_only {
                t.status() == Status::Review
            } else {
                view.show_done || t.status().is_open()
            }
    });
    if inbox_any {
        let folded = view.collapsed.contains(INBOX_KEY);
        rows.push(Row::Section {
            key: INBOX_KEY.to_string(),
            label: "inbox".into(),
            count: inbox_open,
            collapsed: folded,
        });
        if !folded {
            task_rows(&tasks, None, 1, view, &index, &agent_for_task, &mut rows);
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
    ) {
        for p in index.children(parent) {
            if !interesting(&p.id) {
                continue;
            }
            let is_collapsed = view.collapsed.contains(&p.id);
            rows.push(Row::Project {
                id: p.id.clone(),
                name: p.name.clone(),
                depth,
                // Under the review filter the right-hand column counts what
                // is *shown*. A project reading `5 ▸3 ■1` beside one visible
                // row is the tree describing a tree that is not there.
                counts: {
                    let c = counts.get(&p.id).copied().unwrap_or_default();
                    if view.review_only {
                        crate::resolve::Counts { open: c.review, ..Default::default() }
                    } else {
                        c
                    }
                },
                collapsed: is_collapsed,
                // Zero under the filter for the same reason as the counts:
                // three agents at work is true and is not what this view is
                // answering. One question at a time.
                live: if view.review_only { 0 } else { live.get(&p.id).copied().unwrap_or(0) },
                prose: crate::model::has_prose(&p.body),
            });
            if is_collapsed {
                continue;
            }

            // This project's own tasks, attention first.
            task_rows(tasks, Some(&p.id), depth + 1, view, index, agent_for_task, rows);

            walk(
                index, Some(&p.id), depth + 1, rows, counts, live, view, tasks, loose, interesting,
                agent_for_task,
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
                        census: None,
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
    );

    // Panes belonging to no project. Some are there because nothing resolved;
    // some because the workspace is deliberately pinned out of the tree — the
    // orchestrator's own home, and whatever else you opened that is not work.
    // Both pane groups go under the review filter. Neither is work waiting on
    // you — they are terminals — and leaving them is the filter answering a
    // question nobody asked while hiding half the answer to the one they did.
    if !homeless.is_empty() && !view.review_only {
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
                rows.push(Row::Agent { agent: a, title, depth: 1, census: None });
            }
        }
    }

    census.sort_by(|(a, ar), (b, br)| a.state.cmp(&b.state).then(ar.where_.cmp(&br.where_)));

    // The agents, either way round.
    //
    // Pinned at the foot the panel keeps the first few on screen whatever the
    // tree is doing — who has stopped and who is free is the question you ask
    // between reading anything else, and the answer must not be a keystroke
    // away. `w` gives the same list the whole pane and three lines each: the
    // difference is how much room there is to say it in, not what is being
    // said.
    let (rows, dock) = if view.agents {
        let mut list: Vec<Row> = Vec::new();
        for (c, a) in census.iter() {
            list.push(Row::Agent {
                title: a.where_.clone(),
                agent: a.clone(),
                depth: 0,
                census: Some(c.clone()),
            });
            list.push(Row::Detail(Detail::Standing {
                state: c.state,
                pane: a.pane.clone(),
                held: c.held.clone(),
                // Only when it differs from where the row already says it is:
                // a mandate that agrees with the cwd is not news.
                direction: a.project.clone().filter(|d| Some(d) != c.project.as_ref()),
            }));
            if let Some((status, title)) = &c.task {
                list.push(Row::Detail(Detail::Holding {
                    status: *status,
                    title: title.clone(),
                }));
            }
        }
        // No dock: every agent is already here, and pinning five of them to the
        // foot would be the same list twice.
        (list, 0)
    } else if census.is_empty() || view.review_only {
        // Under the review filter the panel is answering one question, and this
        // is not it — the strip in the header still carries the census, so
        // nothing is hidden, only set aside.
        (rows, 0)
    } else {
        let mut rows = rows;
        let folded = view.collapsed.contains(AGENTS_KEY);
        rows.push(Row::Section {
            key: AGENTS_KEY.to_string(),
            label: "agents".into(),
            count: census.len(),
            collapsed: folded,
        });
        if folded {
            (rows, 1)
        } else {
            // The cap is the point of the section: five rows of who, beside a
            // tree that is still a tree. The heading counts them all, so a
            // sixth agent is never silently absent, and `⋯` opens the rest in
            // place for anyone who would rather not leave the tree at all.
            let more = view.expanded.contains(AGENTS_KEY);
            let shown = if more { census.len() } else { census.len().min(MAX_AGENTS_DOCKED) };
            for (c, a) in census.iter().take(shown) {
                rows.push(Row::Agent {
                    title: a.where_.clone(),
                    agent: a.clone(),
                    depth: 1,
                    census: Some(c.clone()),
                });
            }
            let hidden = census.len() - shown;
            if hidden > 0 {
                rows.push(Row::More { key: AGENTS_KEY.to_string(), depth: 1, n: hidden });
            }
            let dock = shown + usize::from(hidden > 0) + 1;
            (rows, dock)
        }
    };

    Ui {
        rows,
        dock,
        census: census.into_iter().map(|(c, a)| (c.state, a)).collect(),
        agents: view.agents,
        blocked: tasks.iter().filter(|t| t.status() == Status::Blocked).count(),
        review: tasks.iter().filter(|t| t.status() == Status::Review).count(),
        sel: 0,
        message: None,
        self_focused,
        vocabulary: vocabulary(snap),
        show_done: view.show_done,
        review_only: view.review_only,
    }
}

/// Every tag in use, commonest first and alphabetical inside that.
///
/// Frequency rather than the alphabet, because the tag you are reaching for is
/// nearly always one already in use and the list is read from the top. Ties
/// broken alphabetically so the order is stable between frames — a picker whose
/// rows shuffle under the cursor is worse than no picker.
///
/// Projects count. A tag's first use is usually on a project, and `t` has to be
/// able to offer one before any task has ever carried it.
pub(super) fn vocabulary(snap: &Snapshot) -> Vec<String> {
    let mut n: std::collections::BTreeMap<&str, usize> = Default::default();
    let carried = snap
        .tasks
        .iter()
        .flat_map(|t| t.tags.iter())
        .chain(snap.projects.iter().flat_map(|p| p.tags.iter()));
    for t in carried {
        *n.entry(t.as_str()).or_default() += 1;
    }
    let mut out: Vec<(usize, &str)> = n.into_iter().map(|(t, c)| (c, t)).collect();
    out.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
    out.into_iter().map(|(_, t)| t.to_string()).collect()
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
        Row::Project { id, depth, counts, collapsed, live, prose, .. } => {
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
        Row::Task { title, depth, status, priority, agent, needs_you, prose, under, ident, .. } => {
            match num {
                Some(n) => l.push(Style::Dim, n.to_string()),
                None => l.push(Style::Plain, " "),
            }
            l.pad(*depth);
            l.push(Style::Plain, " ");
            let (st, g) = status_mark(*status);
            l.push(st, g);
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
            // Priority is drawn only when it is not `normal`, so it costs the
            // other nine rows in ten nothing at all. A column reserved on
            // every row to say "unremarkable" is two of the thirty-four this
            // pane has.
            let prio = match priority {
                Priority::High => Some((Style::Warn, glyph::HIGH)),
                Priority::Low => Some((Style::Dim, glyph::LOW)),
                Priority::Normal => None,
            };
            let prio_w = if prio.is_some() { 2 } else { 0 };
            let count_w = if right.spans.is_empty() { 0 } else { right.width() + 1 };
            // The id is the point when it is on, so it comes out of the
            // title's budget rather than off the end of the row.
            let id_w = ident.as_ref().map(|i| i.chars().count() + 2).unwrap_or(0);
            let avail = w.saturating_sub(*depth + 5 + flag_w + count_w + id_w + prio_w);
            // Before the id and the title, not after them: it is read down the
            // column, and a mark that moves left and right with the length of
            // what precedes it is one you have to hunt for on every row.
            if let Some((st, g)) = prio {
                l.push(st, g);
                l.push(Style::Plain, " ");
            }
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
        Row::Agent { agent, title, depth, census } => {
            match num {
                Some(n) => l.push(Style::Dim, n.to_string()),
                None => l.push(Style::Plain, " "),
            }
            l.pad(*depth);
            l.push(Style::Plain, " ");
            // Standing on its own, in the agents view: the glyph says what it
            // is waiting for rather than only whether it is running, and the
            // project it belongs to goes on the right — in the tree that is
            // said by which branch the row is drawn under, and here there is
            // no branch to say it.
            if let Some(c) = census {
                let (st, dot) = c.state.mark();
                l.push(st, dot);
                l.push(Style::Plain, " ");
                let mut right = Line::default();
                if let Some(p) = &c.project {
                    right.push(Style::Dim, util::truncate(p, 12));
                }
                let count_w = if right.spans.is_empty() { 0 } else { right.width() + 1 };
                let avail = w.saturating_sub(*depth + 4 + count_w).max(4);
                let ink = match c.state {
                    AgentState::Asking | AgentState::Blocked => Style::Warn,
                    AgentState::Working => Style::Accent,
                    _ => Style::Muted,
                };
                l.push(ink, util::truncate(title, avail));
                if !right.spans.is_empty() {
                    l.pad(w.saturating_sub(l.width() + right.width()).max(1));
                    l.spans.extend(right.spans);
                }
                return l;
            }
            if agent.agent {
                let (st, dot) = state_dot(&agent.state);
                l.push(st, dot);
            } else {
                // A shell nobody is driving. Distinct from an idle agent: one
                // has stopped, the other never started.
                l.push(Style::Dim, glyph::SHELL);
            }
            l.push(Style::Plain, " ");
            // An agent line is live work, and accent is what live work looks
            // like everywhere else in the tree — the counts on a project row,
            // the ● on a working pane. Muted put a running agent in the same
            // ink as an unclaimed task, which is the one row it is never
            // telling you about.
            //
            // A shell is not live work: nobody is driving it. It stays muted,
            // so the ▫/● distinction still separates "stopped" from "never
            // started" in colour as well as in glyph.
            let avail = w.saturating_sub(*depth + 4).max(4);
            let ink = if agent.agent { Style::Accent } else { Style::Muted };
            l.push(ink, util::truncate(title, avail));
        }
        // Indented under the name it belongs to and drawn dim throughout: this
        // is the row above, said at length, and it must not compete with the
        // rows the cursor can actually land on.
        Row::Detail(Detail::Standing { state, pane, held, direction }) => {
            l.pad(4);
            let (st, _) = state.mark();
            l.push(st, word(*state));
            l.push(Style::Dim, format!(" · {pane}"));
            if let Some(d) = direction {
                l.push(Style::Dim, format!(" · for {d}"));
            }
            if let Some(h) = held {
                let right = line(Style::Dim, h.clone());
                l.pad(w.saturating_sub(l.width() + right.width()).max(1));
                l.spans.extend(right.spans);
            }
        }
        Row::Detail(Detail::Holding { status, title }) => {
            l.pad(4);
            let (st, g) = status_mark(*status);
            l.push(st, g);
            l.push(Style::Plain, " ");
            l.push(Style::Muted, util::truncate(title, w.saturating_sub(7).max(4)));
        }
    }
    l
}

/// What the row says, with nothing cut.
///
/// [`render_row`] fits a row to the pane, which for a task means about
/// twenty-five characters of a title that averages sixty-four. This is the same
/// text before that happens, for the focus dock — beside `render_row` so the
/// two cannot come to disagree about which field a row is named by.
pub(super) fn full_text(row: &Row) -> String {
    match row {
        Row::Project { id, .. } => id.clone(),
        Row::Task { title, .. } => title.clone(),
        Row::More { n, .. } => format!("{n} more"),
        Row::Section { label, .. } => label.clone(),
        Row::Agent { agent, title, .. } => {
            // The title is the pane's name, which is what the row draws. What
            // it does not draw, and what a name alone will not tell you, is
            // which terminal it is.
            format!("{title} · {}", agent.pane)
        }
        // Never selected, so never asked for. Answering with the line's own
        // words rather than nothing, in case a future row kind takes the
        // cursor here.
        Row::Detail(Detail::Standing { state, pane, .. }) => format!("{} · {pane}", word(*state)),
        Row::Detail(Detail::Holding { title, .. }) => title.clone(),
    }
}

/// What an agent is waiting for, in the fewest words that are true. Beside the
/// mark rather than instead of it: the glyph is what you read in the strip, and
/// the word is what tells you the glyph's name the first time you meet it.
pub(super) fn word(state: AgentState) -> &'static str {
    match state {
        AgentState::Asking => "wants you",
        AgentState::Blocked => "blocked",
        AgentState::Spare => "spare",
        AgentState::Working => "working",
        AgentState::Quiet => "no word yet",
    }
}

/// A task's own status, in the first column, exactly as a task row draws it.
pub(super) fn status_mark(status: Status) -> (Style, &'static str) {
    match status {
        Status::Blocked => (Style::Warn, glyph::BLOCKED),
        Status::Review => (Style::Muted, glyph::REVIEW),
        Status::Done => (Style::Dim, glyph::DONE),
        Status::Doing => (Style::Accent, glyph::DOING),
        _ => (Style::Dim, glyph::QUIET),
    }
}

/// Digits 1-9 address rows that lead somewhere: a terminal.
///
/// The pinned agents at the foot are numbered first, because they are the rows
/// that are always on screen — a digit you can see is worth more than a digit
/// in the order the rows happen to be built in. And one terminal takes one
/// digit: a pane can be drawn twice, under its task and again in the section,
/// and spending two of the nine on one pane would leave the ninth agent with
/// none.
pub(super) fn hotkeys(ui: &Ui) -> Vec<Option<u8>> {
    let rows = &ui.rows;
    let mut out = vec![None; rows.len()];
    let mut n: u8 = 0;
    let mut taken: Vec<&str> = Vec::new();
    let tree = rows.len() - ui.dock;
    let order = (tree..rows.len()).chain(0..tree);
    for i in order {
        let Some(a) = rows[i].agent() else { continue };
        if taken.contains(&a.pane.as_str()) || n >= 9 {
            continue;
        }
        n += 1;
        taken.push(&a.pane);
        out[i] = Some(n);
    }
    out
}
