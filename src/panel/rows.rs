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
use super::{BOARD_LABEL, PANEL_LABEL, VIEW_LABEL};

pub(super) const MAX_TASKS_PER_PROJECT: usize = 6;

pub(super) const MAX_PANES_PER_PROJECT: usize = 4;

/// Stand in for the groups that are not projects, wherever a project id is used
/// as a key. Project ids are slugs, so the parentheses keep these out of their
/// namespace.
pub(super) const INBOX_KEY: &str = "(inbox)";

pub(super) const NOPROJECT_KEY: &str = "(noproject)";

/// What both surfaces call a pane that resolves to no project at all: the
/// tree's group of shells, and the agents view's last heading. One string,
/// because it is one fact and two words for it would read as two.
pub(super) const NOPROJECT_LABEL: &str = "no project";

/// The section pinned at the foot: the agents, whatever they are doing.
pub(super) const AGENTS_KEY: &str = "(agents)";

/// And above them, what an agent has raised a hand about.
pub(super) const FLAGS_KEY: &str = "(flagged)";

/// How many raised hands the foot draws before the rest need `→` on the tail.
///
/// Three, where the agents get five, and for the opposite reason: the agents
/// section is a standing census that is always the same length, and this is
/// news. Three is enough for the case that happens — one or two agents asking
/// at once — and small enough that a burst of them cannot push the census off
/// the pane it is pinned to.
pub(super) const MAX_FLAGS_DOCKED: usize = 3;

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
    /// Reaches outside the panel: the board draws the same mark for the same
    /// state, and two copies of this table would drift the first time one of
    /// them gained a state.
    pub(crate) fn mark(self) -> (Style, &'static str) {
        match self {
            AgentState::Asking => (Style::Warn, glyph::NEEDS_YOU),
            AgentState::Blocked => (Style::Query, glyph::QUESTION),
            AgentState::Working => (Style::Accent, glyph::WORKING),
            AgentState::Spare => (Style::Muted, glyph::IDLE),
            AgentState::Quiet => (Style::Dim, glyph::QUIET),
        }
    }

    /// The colour the pane's own name is drawn in beside that mark. The mark's
    /// colour everywhere but `Quiet`, where dim is reserved for structure and a
    /// pane nobody has heard from is still a pane. Shared, because the tree and
    /// the agents view draw the same agent and must not colour it two ways.
    pub(crate) fn ink(self) -> Style {
        match self {
            AgentState::Quiet => Style::Muted,
            _ => self.mark().0,
        }
    }
}

/// What an agent row carries when it is standing on its own, in the agents
/// view, rather than nested under the task or project that explains it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Census {
    /// Where its work lives: the project of the task it holds, else the one it
    /// is standing in or pointed at. The tree says this by where it draws the
    /// row, and the agents view by which heading the row is under — so there it
    /// is what the runs are cut on, and the row itself carries `None`. The dock
    /// has neither a branch nor a heading, and is where it is drawn in words.
    pub(super) project: Option<String>,
    /// The task in its hands, as the panel would draw it anywhere else.
    pub(super) task: Option<(Status, String)>,
    /// How long it has held that task, if the claim says when it took it.
    pub(super) held: Option<String>,
}

/// Read the two halves together: what herdr says the pane is doing, and what
/// the store says it is holding while it does it.
pub(crate) fn agent_state(herdr_state: &str, holds: Option<Status>) -> AgentState {
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
        /// An agent has raised a hand about this task. The section at the foot
        /// is where the sentence is; this is so the work can still be found
        /// *in place*, under the project it belongs to, which is where you go
        /// once you have read the sentence and want to know what it is near.
        ///
        /// Set by a pass over the finished rows rather than by the walk that
        /// builds them: a flag is not a fact about the task, it is a fact about
        /// somebody asking, and threading it through four call sites to reach
        /// one glyph would put it in the walk's vocabulary for good.
        flagged: bool,
    },
    /// `key` is the project the hidden tasks belong to, or `INBOX_KEY`.
    More { key: String, depth: usize, n: usize },
    /// A group that is not a project: the inbox, loose agents. `key` names it
    /// so it can be folded and so a command can be aimed at it.
    Section { key: String, label: String, count: usize, collapsed: bool },
    /// `state` is what the pane is waiting for, and is carried on every agent
    /// row rather than only the ones standing alone: the tree used to draw an
    /// agent from herdr's two words, so a pane stopped in front of you and a
    /// pane parked behind a question both came out as the same grey `○` —
    /// the answer the agents view had all along, two lines further down.
    ///
    /// `census` is set only in the agents view, where the row stands on its
    /// own: there it has to say where it belongs and what is in its hands,
    /// because there is no branch above it doing either. In the tree it is
    /// `None` and the branch says both.
    Agent { agent: AgentRef, title: String, depth: usize, state: AgentState, census: Option<Census> },
    /// The project a run of agents belongs to, in the agents view: the heading
    /// the tree says by where it draws a row, and a list has to say in words.
    ///
    /// Like [`Row::Detail`] it never takes the cursor, and for the same reason
    /// read the other way round: every row in that view leads to a terminal, so
    /// `↵`, `c` and the digits mean something wherever you stand. A heading you
    /// could land on would be the one row where none of the three did — which
    /// is also why it does not fold. `Section` is the heading that does both,
    /// and it is the tree's.
    ///
    /// `depth` is how far inside another drawn group this project sits, so the
    /// runs keep the shape the tree gives the same projects. The agents under
    /// it do not move with it: a heading steps right, its rows stay in their
    /// column, and thirty-four characters go on being thirty-four.
    Group { project: Option<String>, depth: usize, n: usize },
    /// A line that belongs to the row above it. The cursor never lands on one
    /// and no digit addresses one: it is the second and third line of one
    /// agent, in the view that has the room to spend three lines on one.
    ///
    /// A row rather than extra lines inside one, because the frame maps screen
    /// rows to row indices one for one — a row that drew two lines would put
    /// every click below it on the wrong thing.
    Detail(Detail),
    /// A hand an agent has raised: this task, and why.
    ///
    /// It stands for a task and answers as one, so every verb already aimed at
    /// a task works here — `↵` opens it, `c` claims it, `s` starts it — and the
    /// row is the deeplink the agent could not otherwise draw. What it carries
    /// beyond the id is what the tree cannot say: the sentence, and which pane
    /// said it.
    Flag { card: Card },
}

/// A raised hand, in full: what the row draws and what the card over the panel
/// draws, which are the same fact at two sizes.
///
/// One value rather than two, because the row and the card have to agree. A row
/// is thirty-four columns and a card is a paragraph, and the moment they were
/// built from different fields the section would say one thing and the popup
/// another about the same ask.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Card {
    pub(super) task: String,
    /// The card's heading: the agent's own, else the task's title. An agent
    /// gets to name it because the task's title is often the wrong sentence for
    /// the moment — "the store will not parse" on a task called something else
    /// is the case, and re-titling the task to say so would be a lie about the
    /// work.
    pub(super) title: String,
    /// The one line the row draws.
    pub(super) said: String,
    /// The paragraph the card draws, when there is one. Falls back to `said`,
    /// and then to the task's own overview — a card with a heading and nothing
    /// under it is a knock on the door with nobody there.
    pub(super) body: String,
    /// The pane that raised it.
    pub(super) who: String,
    /// The one thing a keypress can answer, if it asked for anything.
    pub(super) ask: Option<Request>,
    /// Read already: the card has been put away, and the row is what is left.
    pub(super) seen: bool,
}

/// What a raised hand can ask for.
///
/// A closed vocabulary, and that is the whole of the security model here: the
/// answer to a card is a keystroke that runs a command, so an agent naming its
/// own argv would be an agent deciding what `y` does on somebody else's panel.
/// It names a question the panel already knows how to answer instead, and the
/// panel decides what answering it means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Request {
    /// "Let me take this one." `y` claims the task into the pane that asked and
    /// tells it so — which is exactly what `c` does, arriving from the other
    /// direction.
    Claim,
}

impl Request {
    fn parse(s: &str) -> Option<Request> {
        match s {
            "claim" => Some(Request::Claim),
            _ => None,
        }
    }

    /// What the card says it is asking, in the fewest words that are true.
    pub(super) fn asking(self) -> &'static str {
        match self {
            // Short enough to survive the narrowest pane a panel is installed
            // in. The line under it is the keys, and a phrase that gets cut at
            // the border reads as a card that has gone wrong rather than as one
            // that ran out of room.
            Request::Claim => "asks to take it",
        }
    }

    /// The keys, in the order they are read: the answers first, then the two
    /// ways of not answering yet. Two lines rather than one wrapped, because
    /// what separates them is what they do — one pair settles the question and
    /// the other pair does not.
    pub(super) fn keys(self) -> &'static str {
        match self {
            Request::Claim => "y hand it over · n no\no open · esc later",
        }
    }
}

impl Card {
    /// The task this ask is about. The only field anything outside the panel
    /// has business with — it is what a test checks a card against, and what
    /// the answer's command names.
    #[cfg(test)]
    pub(crate) fn task(&self) -> &str {
        &self.task
    }

    /// The one line the row draws, for a test about a card whose words changed
    /// under it.
    #[cfg(test)]
    pub(crate) fn said(&self) -> &str {
        &self.said
    }

    /// Everything the card needs, out of one flag record and the task it names.
    fn of(task: &str, f: &serde_json::Value, tasks: &[Task]) -> Card {
        let field = |k: &str| {
            f.get(k).and_then(|x| x.as_str()).unwrap_or_default().trim().to_string()
        };
        let found = tasks.iter().find(|t| t.id == task);
        let said = field("said");
        let title = match field("title") {
            t if !t.is_empty() => t,
            // The id when the task is gone. A hand raised about something that
            // has since been retired is still a hand raised, and the row has to
            // stay selectable — `x` on it is the only thing that will ever take
            // it down.
            _ => found.map(|t| t.title.clone()).unwrap_or_else(|| task.to_string()),
        };
        let body = match (field("body"), said.as_str()) {
            (b, _) if !b.is_empty() => b,
            (_, s) if !s.is_empty() => s.to_string(),
            // Neither, which is `wsp flag <id>` on its own — "look at this, it
            // exists". The task's own overview is the honest thing to put under
            // that heading: it is what the person is being asked to look at,
            // and the panel is holding it already.
            _ => found
                .and_then(|t| crate::model::section_of(&t.body, "Overview"))
                .unwrap_or_default(),
        };
        Card {
            task: task.to_string(),
            title,
            said,
            body: util::truncate(body.trim(), 600),
            who: field("pane"),
            ask: Request::parse(&field("ask")),
            seen: f.get("seen").and_then(|x| x.as_bool()).unwrap_or(false),
        }
    }
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
    /// Every row in the tree takes the cursor: a heading you cannot select is a
    /// heading you cannot fold or add to, and both are things the groups need.
    /// The two exceptions are the agents view's, where a row that is not an
    /// agent is a row three verbs have nothing to say about — an agent's own
    /// lines, and the project heading over a run of them.
    pub(super) fn selectable(&self) -> bool {
        !matches!(self, Row::Detail(_) | Row::Group { .. })
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
    /// A project heading over a run of agents in the agents view. Never
    /// selected, like [`RowKind::Detail`].
    Group,
    /// A line under an agent in the agents view. Never selected, so a driver
    /// that hunts for one has gone wrong.
    Detail,
    /// A raised hand in the section at the foot. It answers as a task, so a
    /// driver that wants the flag rather than the task it points at has to ask
    /// for the row.
    Flag,
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
    /// Hands raised, whether or not the section at the foot had room to draw
    /// them. The footer says this number for the pane too short to show the
    /// section at all.
    pub(super) flagged: usize,
    /// The ask that has not been read yet, waiting to come up over the tree.
    /// Held on the `Ui` rather than decided in the event loop so the storyboard
    /// reaches the same state the live panel does, through the same rebuild.
    pub(super) pending: Option<Card>,
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
        // The same target as the task's own row in the tree, deliberately: a
        // flag is a second way of reaching one piece of work, not a second
        // piece of work. Which of the two the cursor is on is [`Cursor`]'s
        // `docked`, exactly as it is for a pane drawn under its task and again
        // in the section at the foot.
        Row::Flag { card } => Target::Task(card.task.clone()),
        Row::More { key, .. } => Target::Overflow(key.clone()),
        Row::Agent { agent, .. } => Target::Pane(agent.pane.clone()),
        Row::Section { key, .. } if key == NOPROJECT_KEY => Target::Unattached,
        Row::Section { key, .. } if key == AGENTS_KEY => Target::Unattached,
        Row::Section { key, .. } if key == INBOX_KEY => Target::Inbox,
        Row::Section { .. } => Target::Nothing,
        // Neither is ever the cursor's, so neither has anything to be aimed at.
        // A group heading names a project and deliberately does not answer as
        // one: `S`, `a` and `↵` on it would be verbs pressed on a row the
        // cursor cannot reach.
        Row::Group { .. } | Row::Detail(_) => Target::Nothing,
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

    /// The task the cursor is on, if an agent has raised a hand about it.
    ///
    /// Either row answers: the one in the section at the foot and the task's
    /// own row up in the tree, which is the point of marking both. You lower a
    /// flag from wherever you were when you read it — having to walk back down
    /// to the section to be rid of a mark you are looking at is the kind of
    /// thing that leaves flags up.
    pub(super) fn selected_flag(&self) -> Option<String> {
        match self.rows.get(self.sel) {
            Some(Row::Flag { card }) => Some(card.task.clone()),
            Some(Row::Task { id, flagged: true, .. }) => Some(id.clone()),
            _ => None,
        }
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
            Some(Row::Group { .. }) => RowKind::Group,
            Some(Row::Detail(_)) => RowKind::Detail,
            Some(Row::Flag { .. }) => RowKind::Flag,
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
    /// task id -> flag record: an agent has raised a hand about this task and
    /// said why. Machine-local state like the claims beside it, and the one
    /// input here that arrives from outside the person's own hands.
    pub flags: std::collections::BTreeMap<String, serde_json::Value>,
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
            flags: store.flags(),
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
            flagged: false,
        });
        // The pane working it hangs beneath, so the join is visible and the
        // task keeps its own status glyph instead of surrendering it to the
        // agent's — a claimed task used to look identical whether it was todo
        // or doing.
        if let Some(a) = a {
            rows.push(Row::Agent {
                title: a.where_.clone(),
                // The task is right here, so the row can say what the pane is
                // waiting for rather than only whether it is running — which
                // is the whole of the difference between `←` and `?`.
                state: agent_state(&a.state, Some(t.status())),
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
    rebuild(ui, snap, view, self_ws, &was, sel);

    // Something was just created, or a row elsewhere has asked to be shown
    // here: put the cursor on it. Done after the rebuild because until then the
    // row does not exist.
    if let Some(want) = view.land_on.take() {
        // And when it still does not, the tree is holding it out of sight
        // rather than not having it. A panel that answers "go to this task"
        // by leaving the cursor where it was is one you stop pressing, so
        // loosen whatever is covering the row and build again — see
        // [`hiding`] for what the three are and why they are tried in this
        // order. Each rebuild happens only in the case that would otherwise
        // have failed, and the loop stops the moment the row is there.
        for loosen in hiding() {
            if row_for(&ui.rows, &want).is_some() {
                break;
            }
            if loosen(view, snap, &want) {
                rebuild(ui, snap, view, self_ws, &was, sel);
            }
        }
        if let Some(i) = row_for(&ui.rows, &want) {
            ui.sel = i;
            // And the view owes it a look. The tree is allowed to sit with the
            // cursor off the pane — a wheel puts it there — but not when the
            // cursor has just been moved onto something you made: you would
            // type a name, press return, and watch nothing appear.
            view.keyed = true;
        }
    }
}

/// The rows again, with the cursor kept on the row it was on rather than the
/// slot that row was in.
///
/// Claiming re-sorts the tree under the cursor — the pane row leaves one task
/// and reappears under another, often several lines away — and holding the
/// index would leave the eye on whatever slid into its place. Falls back to the
/// index when the row is genuinely gone, which is the only case where there is
/// nothing to follow.
///
/// Which side of the dock it was on is part of the row's identity here: a pane
/// is drawn twice, and taking the first match took the tree copy every time.
/// See [`Cursor`].
fn rebuild(
    ui: &mut Ui,
    snap: &Snapshot,
    view: &View,
    self_ws: Option<&str>,
    was: &Cursor,
    sel: usize,
) {
    let msg = ui.message.take();
    *ui = collect(snap, view, self_ws);
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
}

/// Where a task or a project is drawn, if it is drawn at all.
fn row_for(rows: &[Row], want: &str) -> Option<usize> {
    rows.iter().position(|r| match r {
        Row::Task { id, .. } => id == want,
        Row::Project { id, .. } => id == want,
        _ => false,
    })
}

/// The three things that keep a task off the tree, each with the one change
/// that undoes it, cheapest first.
///
/// Every one of them is a decision the reader made — a branch folded, a long
/// list left at its first few, finished work put away — so they are tried in
/// order and stopped at the first that works, rather than swept aside together.
/// Folding is first because it hides the most and costs the least to undo; the
/// filters are last because turning one off changes what the whole tree is
/// showing, and that is a price paid only when nothing else will do.
///
/// Each returns whether it actually changed anything, so a rebuild happens only
/// when there is something new to build.
fn hiding() -> [fn(&mut View, &Snapshot, &str) -> bool; 3] {
    [unfold, uncap, unfilter]
}

/// The project the task is filed in, and every project above it, opened.
fn unfold(view: &mut View, snap: &Snapshot, task: &str) -> bool {
    let Some(t) = snap.tasks.iter().find(|t| t.id == task) else {
        return false;
    };
    let Some(p) = t.project.clone() else {
        return view.collapsed.remove(INBOX_KEY);
    };
    let index = Index::new(snap.projects.clone());
    std::iter::once(p.clone())
        .chain(index.ancestors(&p))
        .fold(false, |moved, id| view.collapsed.remove(&id) || moved)
}

/// The cap off the list it is in, so a task sitting seventh in its project is
/// drawn rather than rolled into the `n more` row.
fn uncap(view: &mut View, snap: &Snapshot, task: &str) -> bool {
    let Some(t) = snap.tasks.iter().find(|t| t.id == task) else {
        return false;
    };
    view.expanded.insert(t.project.clone().unwrap_or_else(|| INBOX_KEY.to_string()))
}

/// The filters that would leave it out: `A` for work that is finished, `R` for
/// work that is not at review. Both say so in the footer, which is what makes
/// them safe to turn off from here — the tree changing under you is explained
/// on the line beneath it.
fn unfilter(view: &mut View, snap: &Snapshot, task: &str) -> bool {
    let Some(t) = snap.tasks.iter().find(|t| t.id == task) else {
        return false;
    };
    let mut moved = false;
    if view.review_only && t.status() != Status::Review {
        view.review_only = false;
        moved = true;
    }
    if !view.show_done && !t.status().is_open() {
        view.show_done = true;
        moved = true;
    }
    moved
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
    //
    // A board is furniture too, and the most misleading kind: it is a pane with
    // no agent in it, standing in the project it is a board *of*, so the tree
    // would draw the project's own summary as a shell sitting inside it.
    let furniture = [PANEL_LABEL, VIEW_LABEL, BOARD_LABEL];
    let panes: Vec<&herdr::Pane> =
        snap.panes.iter().filter(|p| !furniture.contains(&p.label.as_str())).collect();
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
    let mut census: Vec<(AgentState, Census, AgentRef)> = Vec::new();

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
                agent_state(&a.agent_status, holds.map(|t| t.status())),
                Census {
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
                        // Shells, every one of them — nothing is bound here, so
                        // there is no task to read a state off and the row
                        // draws `▫` regardless.
                        state: agent_state(&a.state, None),
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
            label: NOPROJECT_LABEL.into(),
            count: homeless.len(),
            collapsed: folded,
        });
        if !folded {
            for a in homeless {
                let title = a.where_.clone();
                let state = agent_state(&a.state, None);
                rows.push(Row::Agent { agent: a, title, depth: 1, state, census: None });
            }
        }
    }

    census.sort_by(|(a, _, ar), (b, _, br)| a.cmp(b).then(ar.where_.cmp(&br.where_)));

    // The agents, either way round.
    //
    // Pinned at the foot the panel keeps the first few on screen whatever the
    // tree is doing — who has stopped and who is free is the question you ask
    // between reading anything else, and the answer must not be a keystroke
    // away. `w` gives the same list the whole pane and three lines each: the
    // difference is how much room there is to say it in, not what is being
    // said.
    //
    // Given the whole pane it is also given headings. Seven agents is a list
    // you read; twenty is one you scan for the two that are yours, and the
    // project each one is standing in was a word on the right of every row —
    // the same word repeated down a column, in the place a row is least read.
    // As a heading it is read once, and the run beneath it is the answer to
    // "who is on this project" without anybody counting.
    let (rows, dock) = if view.agents {
        let mut list: Vec<Row> = Vec::new();
        let all: Vec<usize> = (0..census.len()).collect();
        for (project, depth, group) in by_project(&census, &index, &all) {
            list.push(Row::Group { project: project.clone(), depth, n: group.len() });
            for i in group {
                let (state, c, a) = &census[i];
                list.push(Row::Agent {
                    title: a.where_.clone(),
                    agent: a.clone(),
                    depth: 1,
                    state: *state,
                    census: Some(Census {
                        // The heading over the run says which project this is,
                        // so the row saying it again would be the same word
                        // down every line of the group — and it is the width
                        // the pane's name is short of. The dock's rows keep it:
                        // there is no heading over those.
                        project: None,
                        ..c.clone()
                    }),
                });
                list.push(Row::Detail(Detail::Standing {
                    state: *state,
                    pane: a.pane.clone(),
                    held: c.held.clone(),
                    // Only when it differs from where the row already says it
                    // is: a mandate that agrees with the cwd is not news. Read
                    // against the group's project rather than the row's, which
                    // is now blank for the heading's sake.
                    direction: a.project.clone().filter(|d| Some(d) != c.project.as_ref()),
                }));
                if let Some((status, title)) = &c.task {
                    list.push(Row::Detail(Detail::Holding {
                        status: *status,
                        title: title.clone(),
                    }));
                }
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
            //
            // In runs, like the view `w` gives — one surface over the census,
            // so it is arranged one way. The cap still picks by state, so the
            // five on screen are the five that most want you; the tree only
            // says where they sit once picked. The headings are rows the foot
            // did not spend before, and they are what the right-hand column
            // used to spend on saying the same project down every line.
            let before = rows.len();
            let more = view.expanded.contains(AGENTS_KEY);
            let shown = if more { census.len() } else { census.len().min(MAX_AGENTS_DOCKED) };
            let take: Vec<usize> = (0..shown).collect();
            for (project, depth, group) in by_project(&census, &index, &take) {
                // A step in on the section's own indent, where the view that
                // has no section above it starts at the margin.
                rows.push(Row::Group { project, depth: depth + 1, n: group.len() });
                for i in group {
                    let (state, c, a) = &census[i];
                    rows.push(Row::Agent {
                        title: a.where_.clone(),
                        agent: a.clone(),
                        depth: 1,
                        state: *state,
                        // The heading says the project here too, so the row
                        // gets the width back — and these are the rows that
                        // needed it most: a name in the foot is cut at half
                        // what the same name has in the tree.
                        census: Some(Census { project: None, ..c.clone() }),
                    });
                }
            }
            let hidden = census.len() - shown;
            if hidden > 0 {
                rows.push(Row::More { key: AGENTS_KEY.to_string(), depth: 1, n: hidden });
            }
            // Counted rather than added up: the headings make the section's
            // height depend on how many projects the five span, and a `dock`
            // that disagreed with the rows would put the seam between tree and
            // foot in the wrong place — which is a cursor that jumps.
            let dock = rows.len() - before + 1;
            (rows, dock)
        }
    };

    // The raised hands, above the agents and below everything else.
    //
    // Pinned for the same reason the agents are and a stronger one: this is
    // somebody asking, and an ask that scrolls off the end of a tree is an ask
    // nobody answers. Above the census because the census is furniture — it
    // says the same thing every minute of the day — and this is news, which is
    // read from the top of the block it is in.
    //
    // Spliced in front of the agents rather than pushed, because the agents
    // section is already built and the dock is a slice off the end of one list.
    // Whichever branch above ran, `dock` says where that slice begins.
    let (rows, dock) = flag_rows(snap, view, rows, dock);

    // The mark on the work itself, wherever the tree happens to draw it. The
    // section says which task and why; this is what makes it findable in place
    // once you have read that and want to see what it sits beside.
    let mut rows = rows;
    for r in rows.iter_mut() {
        if let Row::Task { id, flagged, .. } = r {
            *flagged = snap.flags.contains_key(id);
        }
    }

    Ui {
        rows,
        dock,
        flagged: snap.flags.len(),
        pending: pending_card(snap),
        census: census.into_iter().map(|(state, _, a)| (state, a)).collect(),
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

/// The census in runs, one per project: the heading, how deep it sits, and
/// which agents stand under it, by their place in `census`.
///
/// `take` is which of the census to arrange, in census order — everything, for
/// the view that has room for everything, and the first few for the section at
/// the foot that does not. The cap picks and this arranges: which agents are on
/// screen is a question about who has stopped, and where they sit is a question
/// about the tree, and neither answer is allowed to be the other's.
///
/// In the tree's own order, depth-first through the same walk the tree makes,
/// and indented where one group's project lives inside another's. The panel has
/// one spine and it is the project tree — `render` is inside `wsp` on every
/// other surface, so a view that put it above `wsp` because somebody there had
/// stopped would be a second arrangement of the same projects to hold in your
/// head. Ordering by urgency was that, and it read as a list that had lost the
/// tree: the question "which of these is under which" had no answer on screen.
///
/// Urgency is still answered, twice, in the places that can answer it without
/// moving a project: the strip in the header is the whole census by state, and
/// inside each run the census's own order stands — so an agent sits where the
/// strip and the dock already put it, and what wants you is at the top of the
/// group it belongs to.
///
/// The depth counts only the ancestors that are *drawn*, because only they are
/// on screen to be inside of: a group whose parent has no agents starts at the
/// left margin rather than indented under a heading that is not there. Projects
/// the index has never heard of come after the walk, and the panes that resolve
/// nowhere come last of all — `no project` is the least that can be said about
/// a pane.
fn by_project(
    census: &[(AgentState, Census, AgentRef)],
    index: &Index,
    take: &[usize],
) -> Vec<(Option<String>, usize, Vec<usize>)> {
    let mut groups: Vec<(Option<String>, Vec<usize>)> = Vec::new();
    for &i in take {
        let c = &census[i].1;
        match groups.iter_mut().find(|(p, _)| *p == c.project) {
            Some((_, xs)) => xs.push(i),
            None => groups.push((c.project.clone(), vec![i])),
        }
    }

    // The tree's walk, so the two surfaces name the projects in one order. A
    // project with no agents is not a heading — there is nothing to put under
    // it — but it is still walked through to reach the ones that are.
    fn walk(index: &Index, parent: Option<&str>, out: &mut Vec<String>) {
        for p in index.children(parent) {
            out.push(p.id.clone());
            walk(index, Some(&p.id), out);
        }
    }
    let mut order: Vec<String> = Vec::new();
    walk(index, None, &mut order);

    let drawn: Vec<String> = groups.iter().filter_map(|(p, _)| p.clone()).collect();
    let mut out: Vec<(Option<String>, usize, Vec<usize>)> = Vec::new();
    let mut take = |p: &Option<String>, groups: &mut Vec<(Option<String>, Vec<usize>)>| {
        if let Some(i) = groups.iter().position(|(g, _)| g == p) {
            let (g, xs) = groups.remove(i);
            let depth = g
                .as_deref()
                .map(|id| {
                    index.ancestors(id).iter().filter(|a| drawn.iter().any(|d| d == *a)).count()
                })
                .unwrap_or(0);
            out.push((g, depth, xs));
        }
    };
    for id in order {
        take(&Some(id), &mut groups);
    }
    // Whatever the walk could not reach: a project the store has lost, or one
    // whose parent it has. Named, so it is not folded in with the panes that
    // resolve nowhere — those are a different fact, and the last one.
    groups.sort_by(|(a, _), (b, _)| a.cmp(b));
    let rest: Vec<Option<String>> = groups.iter().map(|(p, _)| p.clone()).collect();
    for p in rest.iter().filter(|p| p.is_some()).chain(rest.iter().filter(|p| p.is_none())) {
        take(p, &mut groups);
    }
    out
}

/// The flags, newest first — the order the section draws them in and the order
/// the cards come up in, which have to be the same one or the popup and the row
/// under it disagree about which ask is next.
///
/// A flag is an interruption, and the one raised while you were reading the
/// last one is the one you have not seen.
fn flags_in_order(snap: &Snapshot) -> Vec<(&String, &serde_json::Value)> {
    let at = |v: &serde_json::Value| {
        v.get("at").and_then(|x| x.as_str()).unwrap_or_default().to_string()
    };
    let mut list: Vec<(&String, &serde_json::Value)> = snap.flags.iter().collect();
    list.sort_by(|a, b| at(b.1).cmp(&at(a.1)));
    list
}

/// The card waiting to come up, if one is.
///
/// The oldest unread one, deliberately, where the section draws the newest
/// first: a queue of asks is answered in the order it arrived, and popping the
/// newest would leave the first agent that asked waiting behind every agent
/// that asked after it.
///
/// Computed from the flags rather than from the rows, because the section can
/// be folded and a card must come up anyway — folding a heading is a statement
/// about a list, not about whether anybody may interrupt you.
pub(super) fn pending_card(snap: &Snapshot) -> Option<Card> {
    flags_in_order(snap)
        .into_iter()
        .rev()
        .map(|(id, f)| Card::of(id, f, &snap.tasks))
        .find(|c| !c.seen)
}

/// The raised hands, as a section pinned above the agents.
///
/// It is drawn in every view, including `w` and under `R`, and that is the
/// point: those are filters over *work*, and a hand raised is somebody asking
/// for you rather than a piece of work waiting. A section that went quiet under
/// a filter would be a section you learn to distrust, and there is no filter you
/// would want to be holding when the answer to "why has nothing happened for
/// twenty minutes" is on the row it hid.
///
/// Newest first. A flag is an interruption, and the one raised while you were
/// reading the last one is the one you have not seen.
fn flag_rows(snap: &Snapshot, view: &View, rows: Vec<Row>, dock: usize) -> (Vec<Row>, usize) {
    if snap.flags.is_empty() {
        return (rows, dock);
    }
    let list = flags_in_order(snap);

    let folded = view.collapsed.contains(FLAGS_KEY);
    let mut out: Vec<Row> = vec![Row::Section {
        key: FLAGS_KEY.to_string(),
        label: "flagged".into(),
        count: list.len(),
        collapsed: folded,
    }];
    if !folded {
        let more = view.expanded.contains(FLAGS_KEY);
        let shown = if more { list.len() } else { list.len().min(MAX_FLAGS_DOCKED) };
        for (id, f) in list.iter().take(shown) {
            out.push(Row::Flag { card: Card::of(id, f, &snap.tasks) });
        }
        let hidden = list.len() - shown;
        if hidden > 0 {
            out.push(Row::More { key: FLAGS_KEY.to_string(), depth: 1, n: hidden });
        }
    }

    // In front of the agents, which are already built and sit at the end of the
    // list: `dock` is where that slice begins whichever branch produced it.
    let n = out.len();
    let mut rows = rows;
    let at = rows.len() - dock;
    rows.splice(at..at, out);
    (rows, dock + n)
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
        Row::Task {
            title, depth, status, priority, agent, needs_you, prose, under, ident, flagged, ..
        } => {
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
            // The raised hand is the loudest thing on the row, so it goes to
            // the left of everything the title is preceded by — read down the
            // column like the status glyph above it, rather than found by
            // reading along whichever row happens to carry one.
            let raised_w = if *flagged { 2 } else { 0 };
            let count_w = if right.spans.is_empty() { 0 } else { right.width() + 1 };
            // The id is the point when it is on, so it comes out of the
            // title's budget rather than off the end of the row.
            let id_w = ident.as_ref().map(|i| i.chars().count() + 2).unwrap_or(0);
            let avail =
                w.saturating_sub(*depth + 5 + flag_w + count_w + id_w + prio_w + raised_w);
            if *flagged {
                l.push(Style::Warn, glyph::FLAG);
                l.push(Style::Plain, " ");
            }
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
        // A heading rather than a section: no caret, because there is nothing
        // to fold — the agents view is the census, and a census with a run of
        // it folded away is a count you cannot trust. Stepped in by the tree's
        // own depth, with the run beneath it always in the same column, so the
        // shape says which heading is inside which and costs the rows nothing.
        Row::Group { project, depth, n } => {
            l.push(Style::Plain, " ");
            l.pad(*depth);
            let right = line(Style::Dim, n.to_string());
            let avail = w.saturating_sub(right.width() + depth + 2).max(4);
            match project {
                Some(p) => l.push(Style::Bold, util::truncate(p, avail)),
                // The words the tree's own group of homeless panes uses. Two
                // names for a pane that resolves nowhere would read as two
                // kinds of pane.
                None => l.push(Style::Muted, util::truncate(NOPROJECT_LABEL, avail)),
            }
            l.pad(w.saturating_sub(l.width() + right.width()).max(1));
            l.spans.extend(right.spans);
        }
        Row::Agent { agent, title, depth, state, census } => {
            match num {
                Some(n) => l.push(Style::Dim, n.to_string()),
                None => l.push(Style::Plain, " "),
            }
            l.pad(*depth);
            l.push(Style::Plain, " ");
            // A shell nobody is driving keeps its own mark wherever it is
            // drawn. Distinct from an idle agent: one has stopped, the other
            // never started, and no state either of them can be in makes that
            // difference legible on its own.
            let (st, dot) =
                if agent.agent { state.mark() } else { (Style::Dim, glyph::SHELL) };
            let ink = if agent.agent { state.ink() } else { Style::Muted };
            l.push(st, dot);
            l.push(Style::Plain, " ");
            // Standing on its own, in the agents view: the project it belongs
            // to goes on the right — in the tree that is said by which branch
            // the row is drawn under, and here there is no branch to say it.
            if let Some(c) = census {
                let mut right = Line::default();
                if let Some(p) = &c.project {
                    right.push(Style::Dim, util::truncate(p, 12));
                }
                let count_w = if right.spans.is_empty() { 0 } else { right.width() + 1 };
                let avail = w.saturating_sub(*depth + 4 + count_w).max(4);
                l.push(ink, util::truncate(title, avail));
                if !right.spans.is_empty() {
                    l.pad(w.saturating_sub(l.width() + right.width()).max(1));
                    l.spans.extend(right.spans);
                }
                return l;
            }
            let avail = w.saturating_sub(*depth + 4).max(4);
            l.push(ink, util::truncate(title, avail));
        }
        // Indented under the name it belongs to and drawn dim throughout: this
        // is the row above, said at length, and it must not compete with the
        // rows the cursor can actually land on.
        Row::Detail(Detail::Standing { state, pane, held, direction }) => {
            // Five, which is where the name of the agent this belongs to
            // starts: the digit, the group's indent, the mark and the spaces
            // either side of it.
            l.pad(5);
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
            l.pad(5);
            let (st, g) = status_mark(*status);
            l.push(st, g);
            l.push(Style::Plain, " ");
            l.push(Style::Muted, util::truncate(title, w.saturating_sub(8).max(4)));
        }
        // The sentence first, because the sentence is the news. The task's own
        // title is one row up in the tree with a `▲` on it, and the pane that
        // raised it is on the right where the agents section keeps its right-
        // hand column — so what is left to say here is the thing only this row
        // says. A flag with nothing written on it falls back to the title,
        // which is the whole of "look at this task, it exists".
        Row::Flag { card } => {
            let (title, said, who) = (&card.title, &card.said, &card.who);
            l.push(Style::Plain, " ");
            l.push(Style::Warn, glyph::FLAG);
            l.push(Style::Plain, " ");
            let mut right = Line::default();
            if !who.is_empty() {
                right.push(Style::Dim, util::truncate(who, 8));
            }
            let count_w = if right.spans.is_empty() { 0 } else { right.width() + 1 };
            let avail = w.saturating_sub(4 + count_w).max(4);
            let body = if said.is_empty() { title } else { said };
            l.push(Style::Warn, util::truncate(body, avail));
            if !right.spans.is_empty() {
                l.pad(w.saturating_sub(l.width() + right.width()).max(1));
                l.spans.extend(right.spans);
            }
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
        // Never selected, so never asked for — answered in its own words for
        // the same reason a detail line is.
        Row::Group { project, .. } => {
            project.clone().unwrap_or_else(|| NOPROJECT_LABEL.to_string())
        }
        // Both halves, and the title as well: the row draws the sentence and
        // cuts it at a pane's width, and `F` is where the whole of a raised
        // hand is read — which task, what was said, and who said it.
        Row::Flag { card } => {
            let (title, said, who) = (&card.title, &card.said, &card.who);
            let mut out = title.clone();
            if !said.is_empty() {
                out = format!("{said} — {out}");
            }
            match who.is_empty() {
                true => out,
                false => format!("{out} · {who}"),
            }
        }
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
pub(crate) fn word(state: AgentState) -> &'static str {
    match state {
        AgentState::Asking => "wants you",
        AgentState::Blocked => "blocked",
        AgentState::Spare => "spare",
        AgentState::Working => "working",
        AgentState::Quiet => "no word yet",
    }
}

/// A task's own status, in the first column, exactly as a task row draws it.
pub(crate) fn status_mark(status: Status) -> (Style, &'static str) {
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
