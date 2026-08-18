//! `wsp kanban` — one project's work, as the four states it moves through.
//!
//! The panel answers "what is there" and the detail pane answers "what is
//! this". Neither answers "where has it all got to", and a tree cannot: it is
//! ordered by attention, so a task's status arrives as one glyph in the second
//! column and the eye has to gather the piles itself. A board makes status the
//! axis instead — four columns, every card in the column its status names, and
//! a key that moves the card rather than editing a field.
//!
//! A tab rather than a pane, for the reason `E` opens a tab: four columns of
//! readable title is ninety columns at the very least, and the sidebar is
//! thirty-four.
//!
//! Data in, frame out, like [`crate::detail`]. [`Ctx`] is everything it reads,
//! so a fixture can render a board with nothing running, and [`collect`] is a
//! pure function of it.

mod keys;
mod render;
mod run;

pub(crate) use keys::{apply_key, Action, Cursor};
pub(crate) use render::frame;
pub use run::run;

use std::collections::BTreeMap;

use serde_json::Value;

use crate::live::{self, AgentRef};
use crate::model::{Priority, Status, Task};
use crate::panel::AgentState;
use crate::resolve::{self, Counts, Index};
use crate::store::Store;
use crate::util;

/// How many done cards the board carries at all.
///
/// The other three columns are bounded by the work in hand; `done` is bounded
/// only by how long the project has existed, and reading two hundred finished
/// tasks off a board is nobody's question. The window scrolls, so this is not
/// about what fits — it is about how far back "done" is still news.
pub(crate) const MAX_DONE: usize = 40;

/// The four states a piece of work moves through, in the order it moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lane {
    Todo,
    Doing,
    Review,
    Done,
}

impl Lane {
    pub(crate) const ALL: [Lane; 4] = [Lane::Todo, Lane::Doing, Lane::Review, Lane::Done];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Lane::Todo => "todo",
            Lane::Doing => "doing",
            Lane::Review => "review",
            Lane::Done => "done",
        }
    }

    /// Which column a status belongs in.
    ///
    /// Six statuses and four columns, because two of them are not places in the
    /// flow. `inbox` is `todo` that nobody has filed, and it sorts as todo
    /// everywhere else. `blocked` is `doing` that has stopped: the work is in
    /// hand and parked, so a column of its own would add a fifth state to a
    /// system that is deliberately four — and would put the cards you most need
    /// to see off in a siding. The card keeps its own ■ instead, so the column
    /// simplifies the layout without swallowing the fact.
    pub(crate) fn of(status: Status) -> Lane {
        match status {
            Status::Inbox | Status::Todo => Lane::Todo,
            Status::Doing | Status::Blocked => Lane::Doing,
            Status::Review => Lane::Review,
            Status::Done => Lane::Done,
        }
    }

    /// The `wsp` verb that moves a task into this lane.
    ///
    /// The CLI is the one implementation of what those verbs mean — `done` over
    /// open sub-tasks, `start` on a blocked task — so the board runs it rather
    /// than writing a status itself, exactly as the panel does.
    pub(crate) fn verb(self) -> &'static str {
        match self {
            Lane::Todo => "reopen",
            Lane::Doing => "start",
            Lane::Review => "review",
            Lane::Done => "done",
        }
    }
}

/// One task, as a board draws it.
#[derive(Debug, Clone)]
pub(crate) struct Card {
    pub(crate) id: String,
    pub(crate) title: String,
    /// Its own status, which the column only approximates: a blocked card is in
    /// `doing` and is not doing.
    pub(crate) status: Status,
    pub(crate) priority: Priority,
    /// The project it is filed under. Drawn on the card only when the board
    /// spans more than one, where it is the thing a column cannot say.
    pub(crate) project: Option<String>,
    /// What the pane holding it is doing, if a pane holds it.
    pub(crate) agent: Option<AgentState>,
    pub(crate) pane: Option<String>,
    /// How long that pane has held it, from the claim.
    pub(crate) held: Option<String>,
    /// Something is written in Overview or Details.
    pub(crate) prose: bool,
    /// Work decomposed beneath it. A parent whose children are elsewhere on the
    /// board is a card that would otherwise look like a single afternoon.
    pub(crate) under: Counts,
}

pub(crate) struct Column {
    pub(crate) lane: Lane,
    pub(crate) cards: Vec<Card>,
    /// Cards the board is not carrying — the `done` tail past [`MAX_DONE`].
    /// Said out loud in the header, because a count that quietly stops counting
    /// is worse than no count.
    pub(crate) dropped: usize,
}

/// A running agent, as the board has to speak of it.
///
/// An agent holding a card is already on the board — it is the mark on that
/// card, and the column it sits in is what the agent is doing. This is for the
/// other question, the one a board of work cannot answer from its columns: how
/// much attention is running at all, and how much of it has stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Agent {
    pub(crate) state: AgentState,
    /// The pane's own name — see [`crate::live::pane_name`] — with the post in
    /// front of it for a custodian, as the panel's census draws it. A governor
    /// holds no task, so the post is the only thing that places its row here.
    pub(crate) name: String,
    pub(crate) pane: String,
    /// The task in its hands, if it has one. `None` is the whole point of the
    /// rail at the foot: an agent with no card appears nowhere else on a board.
    pub(crate) task: Option<String>,
    /// How long it has held it.
    pub(crate) held: Option<String>,
}

pub(crate) struct Board {
    pub(crate) scope: Scope,
    pub(crate) columns: Vec<Column>,
    /// Every agent on the machine, ordered as the panel orders them: what wants
    /// an answer, then what is free, then what is busy, then what has not said.
    ///
    /// The whole census, not the agents working this project — same bargain the
    /// panel's header strip makes. A count that went quiet because you opened a
    /// board on a quiet project is a count you learn to distrust.
    pub(crate) agents: Vec<Agent>,
    /// The board spans more than one project, so a card has to name its own.
    pub(crate) mixed: bool,
    /// Set while the `done` column is off, so the footer can say so.
    pub(crate) show_done: bool,
}

/// What a board is a board of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Scope {
    /// A project and everything filed beneath it. Sub-projects are included
    /// because a parent whose work all lives in its children would otherwise
    /// open as four empty columns.
    Project(String),
    /// Tasks belonging to no project at all.
    Inbox,
    /// Everything in the store. What you get when nothing resolves — better
    /// than an empty board, and the one view that shows the whole backlog by
    /// state rather than by tree.
    Everything,
}

impl Scope {
    pub(crate) fn label(&self) -> String {
        match self {
            Scope::Project(p) => p.clone(),
            Scope::Inbox => "inbox".into(),
            Scope::Everything => "everything".into(),
        }
    }

    /// Read a scope off the command line: a positional, then `-p`, then
    /// whatever the pane is standing in.
    pub(crate) fn of(store: &Store, args: &crate::Args, index: &Index) -> Result<Scope, i32> {
        let named = args.rest.first().cloned().or_else(|| args.get("project"));
        if let Some(name) = named {
            return match name.as_str() {
                "inbox" | "none" => Ok(Scope::Inbox),
                "all" | "everything" => Ok(Scope::Everything),
                _ => match index.find(&name) {
                    Some(p) => Ok(Scope::Project(p.id.clone())),
                    None => {
                        eprintln!("wsp: no such project `{name}`");
                        Err(1)
                    }
                },
            };
        }
        // Nothing named: the same chain every other command resolves against —
        // pin, binding, mandate, cwd, workspace label.
        match crate::cmd_agent::current_project(store, args, index)? {
            Some(p) => Ok(Scope::Project(p)),
            None => Ok(Scope::Everything),
        }
    }

    /// The projects this board covers, or `None` when the question does not
    /// apply. Worked out once: `subtree` walks the whole index, and asking it
    /// per task is the same walk a thousand times.
    fn within(&self, index: &Index) -> Option<Vec<String>> {
        match self {
            Scope::Project(p) => Some(index.subtree(p)),
            _ => None,
        }
    }

    /// Whether a task is on this board.
    fn holds(&self, within: &Option<Vec<String>>, t: &Task) -> bool {
        match self {
            Scope::Everything => true,
            Scope::Inbox => t.project.is_none(),
            Scope::Project(_) => t
                .project
                .as_ref()
                .zip(within.as_ref())
                .map(|(id, ids)| ids.contains(id))
                .unwrap_or(false),
        }
    }
}

/// Everything the board reads.
pub(crate) struct Ctx {
    pub tasks: Vec<Task>,
    pub index: Index,
    /// pane -> the task bound to it.
    pub bindings: BTreeMap<String, Value>,
    /// task -> the claim on it, read for `claimed_at`.
    pub claims: BTreeMap<String, Value>,
    /// Every pane on the machine, with the store's half already filled: see
    /// [`Ctx::live`] for `seat`, which is the one field a runner cannot answer
    /// and both readers below need.
    pub panes: Vec<AgentRef>,
}

impl Ctx {
    pub(crate) fn live(store: &Store) -> Ctx {
        Ctx {
            tasks: store.tasks(),
            index: Index::new(store.projects()),
            bindings: store.bindings(),
            claims: store.claims(),
            // A herdr that is not answering costs the agent marks and nothing
            // else. The durable half is the board.
            panes: seated(live::panes(), &store.governors()),
        }
    }
}

/// Fill in the one half of a pane row a runner cannot answer: which project's
/// slot it is sitting in.
///
/// The same join `panel::rows::collect` makes, and it has to be made here too
/// because `crate::live` sees terminals and a seat is a fact the store keeps.
/// Without it every pane the board drew carried `seat: None`, and an idle
/// custodian holding no task came out as `○ spare hands` — the busiest agent on
/// the machine advertised as free to be given something, while the panel and
/// `wsp wip` beside it said `▣`. See [`crate::panel::agent_state`], which is
/// where a seat changes what silence means.
///
/// Done as the pane list is built rather than at the two places `seat` is read,
/// so [`collect`] stays a pure function of [`Ctx`] and a fixture seats an agent
/// by saying so on the pane.
pub(crate) fn seated(panes: Vec<AgentRef>, governors: &BTreeMap<String, Value>) -> Vec<AgentRef> {
    panes
        .into_iter()
        .map(|p| AgentRef { seat: crate::cmd_govern::governs(governors, &p.workspace), ..p })
        .collect()
}

/// Build the columns.
///
/// Priority orders each column, which is the whole reason priority is a
/// per-project fact: two tasks in different projects are never in the same
/// list, and on a board scoped to one project every card in a column *is* in
/// the same list. Status breaks the tie beneath it — a blocked card sits above
/// a running one at the same priority, because it is the one asking for
/// something — and the id breaks that, so the order is stable between frames.
///
/// `done` is the exception and sorts newest first. Priority on finished work
/// orders nothing anybody is deciding, and it would scatter this morning's
/// three completions through a year of them.
pub(crate) fn collect(ctx: &Ctx, scope: &Scope, show_done: bool) -> Board {
    // pane -> task, inverted once rather than scanned per card.
    let held_by: BTreeMap<&str, &AgentRef> = ctx
        .panes
        .iter()
        .filter_map(|p| {
            let task = ctx
                .bindings
                .get(&p.pane)?
                .get("task_id")?
                .as_str()
                .filter(|s| !s.is_empty())?;
            Some((task, p))
        })
        .collect();

    let within = scope.within(&ctx.index);
    let mine: Vec<&Task> = ctx.tasks.iter().filter(|t| scope.holds(&within, t)).collect();
    let mixed = match scope {
        Scope::Project(p) => mine.iter().any(|t| t.project.as_deref() != Some(p.as_str())),
        Scope::Inbox => false,
        Scope::Everything => true,
    };

    let card = |t: &Task| -> Card {
        let pane = held_by.get(t.id.as_str());
        Card {
            id: t.id.clone(),
            title: t.title.clone(),
            status: t.status(),
            priority: t.priority(),
            project: t.project.clone(),
            // The two halves the panel already joins: what herdr says the pane
            // is doing, and what the store says it is holding while it does it.
            agent: pane.map(|p| crate::panel::agent_state(&p.state, Some(t.status()), p.seat.is_some())),
            pane: pane.map(|p| p.pane.clone()),
            held: ctx
                .claims
                .get(&t.id)
                .and_then(|c| c.get("claimed_at"))
                .and_then(|c| c.as_str())
                .filter(|c| !c.is_empty())
                .map(|c| util::duration_human(util::since(c))),
            prose: crate::model::has_prose(&t.body),
            under: resolve::counts_under(&ctx.tasks, &t.id),
        }
    };

    let lanes = Lane::ALL.iter().copied().filter(|l| show_done || *l != Lane::Done);
    let mut columns: Vec<Column> = Vec::new();
    for lane in lanes {
        let mut here: Vec<&&Task> = mine.iter().filter(|t| Lane::of(t.status()) == lane).collect();
        let mut dropped = 0;
        if lane == Lane::Done {
            // Newest first. `updated` is written by every status change, so the
            // task finished a minute ago is the one at the top; the id breaks
            // the tie, as it does in the other three columns.
            here.sort_by(|a, b| b.updated.cmp(&a.updated).then(a.id.cmp(&b.id)));
            dropped = here.len().saturating_sub(MAX_DONE);
            here.truncate(MAX_DONE);
        } else {
            here.sort_by_key(|t| (t.priority().rank(), t.status().rank(), t.id.clone()));
        }
        columns.push(Column { lane, cards: here.into_iter().map(|t| card(t)).collect(), dropped });
    }

    // Shells are not in the census, for the panel's reason: a pane with nobody
    // in it is a fact about a place, and this is a list of people.
    let held_of = |pane: &str| -> Option<&Task> {
        let id = ctx.bindings.get(pane)?.get("task_id")?.as_str()?;
        ctx.tasks.iter().find(|t| t.id == id)
    };
    let mut agents: Vec<Agent> = ctx
        .panes
        .iter()
        .filter(|p| p.agent)
        .map(|p| {
            let held = held_of(&p.pane);
            Agent {
                state: crate::panel::agent_state(&p.state, held.map(|t| t.status()), p.seat.is_some()),
                name: p.census_name(),
                pane: p.pane.clone(),
                task: held.map(|t| t.id.clone()),
                held: held
                    .and_then(|t| ctx.claims.get(&t.id))
                    .and_then(|c| c.get("claimed_at"))
                    .and_then(|c| c.as_str())
                    .filter(|c| !c.is_empty())
                    .map(|c| util::duration_human(util::since(c))),
            }
        })
        .collect();
    agents.sort_by(|a, b| a.state.cmp(&b.state).then(a.name.cmp(&b.name)));

    Board { scope: scope.clone(), columns, agents, mixed, show_done }
}

impl Board {
    pub(crate) fn card_at(&self, cur: &Cursor) -> Option<&Card> {
        self.columns.get(cur.col)?.cards.get(cur.row)
    }

    /// Where a task is on this board. The cursor is kept on the card rather
    /// than on the slot: pressing `s` moves the card two columns over, and a
    /// cursor that stayed where it was would be pointing at whatever slid up
    /// into its place.
    pub(crate) fn find(&self, id: &str) -> Option<Cursor> {
        for (col, c) in self.columns.iter().enumerate() {
            if let Some(row) = c.cards.iter().position(|k| k.id == id) {
                return Some(Cursor { col, row });
            }
        }
        None
    }

    /// Agents with no card, which on a board is what "free" means.
    ///
    /// These are the only agents a board cannot otherwise show: every other one
    /// is the mark on the card it is holding. They are also the ones the board
    /// is most likely to be read for — you look at the columns to decide what
    /// happens next, and this is who could do it.
    ///
    /// `Quiet` is deliberately absent. herdr says neither working nor idle, so
    /// nothing here is going to advertise it as capacity.
    pub(crate) fn spare(&self) -> Vec<&Agent> {
        self.agents.iter().filter(|a| a.state == AgentState::Spare).collect()
    }

    /// Open work, and how much of it is parked. The header's one number each.
    pub(crate) fn totals(&self) -> (usize, usize) {
        let open: usize = self
            .columns
            .iter()
            .filter(|c| c.lane != Lane::Done)
            .map(|c| c.cards.len())
            .sum();
        let blocked: usize = self
            .columns
            .iter()
            .flat_map(|c| c.cards.iter())
            .filter(|c| c.status == Status::Blocked)
            .count();
        (open, blocked)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, title: &str, project: Option<&str>, status: &str, prio: &str) -> Task {
        let mut t = Task::new(title, id);
        t.project = project.map(|s| s.to_string());
        t.status_raw = status.into();
        t.priority_raw = prio.into();
        t.updated = format!("2026-08-{}T00:00:00Z", &id[id.len() - 2..]);
        t
    }

    fn ctx(tasks: Vec<Task>) -> Ctx {
        let mut projects = vec![crate::model::Project::new("wsp")];
        let mut child = crate::model::Project::new("render");
        child.parent = Some("wsp".into());
        projects.push(child);
        Ctx {
            tasks,
            index: Index::new(projects),
            bindings: BTreeMap::new(),
            claims: BTreeMap::new(),
            panes: Vec::new(),
        }
    }

    fn ids(b: &Board, lane: Lane) -> Vec<String> {
        b.columns
            .iter()
            .find(|c| c.lane == lane)
            .map(|c| c.cards.iter().map(|k| k.id.clone()).collect())
            .unwrap_or_default()
    }

    /// The ordering the board was asked for. Priority first inside a column,
    /// which is the only place priority has ever meant anything — and status
    /// beneath it, so a parked card sits above a running one it is level with.
    #[test]
    fn a_column_is_ordered_by_priority_then_by_what_is_asking() {
        let c = ctx(vec![
            task("t-01", "normal todo", Some("wsp"), "todo", "normal"),
            task("t-02", "low todo", Some("wsp"), "todo", "low"),
            task("t-03", "high todo", Some("wsp"), "todo", "high"),
            task("t-04", "normal doing", Some("wsp"), "doing", "normal"),
            task("t-05", "normal blocked", Some("wsp"), "blocked", "normal"),
            task("t-06", "high blocked", Some("wsp"), "blocked", "high"),
        ]);
        let b = collect(&c, &Scope::Project("wsp".into()), true);
        assert_eq!(ids(&b, Lane::Todo), ["t-03", "t-01", "t-02"]);
        // Blocked is doing that has stopped, so it is in the doing column — and
        // above the work that is still moving, at the same priority.
        assert_eq!(ids(&b, Lane::Doing), ["t-06", "t-05", "t-04"]);
    }

    /// A project's board is the project's whole subtree. `wsp` holds almost no
    /// tasks of its own; every one of them is in a child, and a board that
    /// showed only the parent's own would open empty on exactly the projects
    /// with the most work under them.
    #[test]
    fn a_board_covers_the_projects_beneath_it_and_says_which() {
        let c = ctx(vec![
            task("t-01", "parent work", Some("wsp"), "todo", "normal"),
            task("t-02", "child work", Some("render"), "todo", "normal"),
            task("t-03", "nowhere", None, "todo", "normal"),
        ]);
        let b = collect(&c, &Scope::Project("wsp".into()), true);
        assert_eq!(ids(&b, Lane::Todo), ["t-01", "t-02"]);
        assert!(b.mixed, "two projects on one board, so a card has to name its own");

        let only = collect(&c, &Scope::Project("render".into()), true);
        assert_eq!(ids(&only, Lane::Todo), ["t-02"]);
        assert!(!only.mixed, "one project, so the column heading already said it");

        let inbox = collect(&c, &Scope::Inbox, true);
        assert_eq!(ids(&inbox, Lane::Todo), ["t-03"]);
    }

    /// Finished work is a log, not a queue. Priority orders what to do next and
    /// there is no next here — what you want is what landed most recently.
    #[test]
    fn done_reads_newest_first_whatever_its_priority() {
        let c = ctx(vec![
            task("t-01", "first", Some("wsp"), "done", "high"),
            task("t-02", "second", Some("wsp"), "done", "low"),
            task("t-03", "third", Some("wsp"), "done", "normal"),
        ]);
        let b = collect(&c, &Scope::Project("wsp".into()), true);
        assert_eq!(ids(&b, Lane::Done), ["t-03", "t-02", "t-01"]);
    }

    /// `A` takes the column away rather than emptying it, so the other three
    /// get its width — and the cursor has one fewer column to reach.
    #[test]
    fn the_done_column_can_be_put_away_entirely() {
        let c = ctx(vec![task("t-01", "finished", Some("wsp"), "done", "normal")]);
        let with = collect(&c, &Scope::Project("wsp".into()), true);
        assert_eq!(with.columns.len(), 4);
        let without = collect(&c, &Scope::Project("wsp".into()), false);
        assert_eq!(without.columns.len(), 3);
        assert!(without.columns.iter().all(|c| c.lane != Lane::Done));
    }

    /// The half of the census a board cannot draw from its columns.
    ///
    /// An agent on a card is the mark on that card. An agent on nothing is
    /// nowhere — and it is the one the board is read for, because the columns
    /// tell you what to do next and this tells you who could do it.
    #[test]
    fn the_agents_with_nothing_in_their_hands_are_the_ones_a_board_cannot_show() {
        let pane = |id: &str, status: &str, label: &str| AgentRef {
            pane: id.into(),
            workspace: "w1".into(),
            agent: true,
            kind: "claude".into(),
            state: status.into(),
            label: label.into(),
            ..Default::default()
        };
        let mut c = ctx(vec![
            task("t-01", "in hand", Some("wsp"), "doing", "normal"),
            task("t-02", "parked", Some("wsp"), "blocked", "normal"),
        ]);
        c.panes = vec![
            pane("w1:p1", "working", "in hand"),
            pane("w1:p2", "idle", "parked"),
            pane("w1:p3", "idle", "Verb UI"),
            // A shell is a fact about a place, not a person. It is not capacity
            // and it is not in the census.
            AgentRef { pane: "w1:p4".into(), label: "a shell".into(), ..Default::default() },
        ];
        c.bindings.insert("w1:p1".into(), serde_json::json!({ "task_id": "t-01" }));
        c.bindings.insert("w1:p2".into(), serde_json::json!({ "task_id": "t-02" }));

        let b = collect(&c, &Scope::Project("wsp".into()), true);
        assert_eq!(b.agents.len(), 3, "three agents and a shell");
        // Ordered as the panel orders them: what wants an answer, then what is
        // free, then what is busy.
        assert_eq!(
            b.agents.iter().map(|a| a.state).collect::<Vec<_>>(),
            [AgentState::Blocked, AgentState::Spare, AgentState::Working],
        );
        // And only the one holding nothing is offered as capacity — an idle
        // agent parked on a blocked task is stopped, not free.
        let spare: Vec<&str> = b.spare().iter().map(|a| a.name.as_str()).collect();
        assert_eq!(spare, ["Verb UI"]);
    }

    /// A custodian is the most assigned agent on the machine, and the board
    /// used to draw it as the freest.
    ///
    /// `Ctx` never read the governors map, so every pane it built carried
    /// `seat: None` and an idle governor — which is a governor between the
    /// agents it is sequencing, most of the night — came out as `spare hands`.
    /// The seam is [`seated`]: the join happens once, on the way in, and both
    /// readers get it. The side list then names the post for free, because a
    /// governor holds no task and the post is the only thing that places it.
    #[test]
    fn a_governor_is_seated_on_the_board_and_is_not_offered_as_capacity() {
        let mut c = ctx(vec![task("t-01", "in hand", Some("wsp"), "doing", "normal")]);
        let pane = |id: &str, ws: &str, label: &str| AgentRef {
            pane: id.into(),
            workspace: ws.into(),
            agent: true,
            kind: "claude".into(),
            state: "idle".into(),
            label: label.into(),
            ..Default::default()
        };
        let governors = BTreeMap::from([(
            "wsp".to_string(),
            serde_json::json!({ "workspace": "w2", "pane": "w2:p1" }),
        )]);
        c.panes = seated(
            vec![
                // What the custodian's pane wears from its first `wsp say`: the
                // sentence alone, with nothing in front of it.
                pane("w2:p1", "w2", "restarted after herdr died; 02 is next"),
                pane("w1:p1", "w1", "Verb UI"),
            ],
            &governors,
        );

        let b = collect(&c, &Scope::Project("wsp".into()), true);
        let custodian = b.agents.iter().find(|a| a.pane == "w2:p1").expect("in the census");
        assert_eq!(custodian.state, AgentState::Seated, "an idle custodian is seated, not spare");
        assert!(
            custodian.name.contains("governor · wsp"),
            "the row never says whose voice it is: {}",
            custodian.name,
        );

        // And the seat is the whole of the difference: the ordinary agent in the
        // next workspace is idle on nothing, and that is what capacity means.
        let spare: Vec<&str> = b.spare().iter().map(|a| a.name.as_str()).collect();
        assert_eq!(spare, ["Verb UI"]);
    }

    /// Every status the store can hold lands in exactly one column. A status
    /// added to the model with no home here would silently drop its tasks off
    /// the board, which is the one failure a board must not have.
    #[test]
    fn every_status_has_a_column() {
        for s in [
            Status::Inbox,
            Status::Todo,
            Status::Doing,
            Status::Blocked,
            Status::Review,
            Status::Done,
        ] {
            let lane = Lane::of(s);
            assert!(Lane::ALL.contains(&lane), "{s:?} has nowhere to go");
        }
        // And the two that share: the flow is four states, not six.
        assert_eq!(Lane::of(Status::Blocked), Lane::Doing);
        assert_eq!(Lane::of(Status::Inbox), Lane::Todo);
    }
}
