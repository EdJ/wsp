//! Who else is standing in this tree.
//!
//! Three agents worked `~/claude/wsp` on 2026-08-15 and none of them knew about
//! the others until one swept another's uncommitted work into its commit. Every
//! fact needed to have warned all three was already in this binary: herdr knows
//! what panes exist and where each is standing, and the store knows who claimed
//! what. Nothing joined them and said so.
//!
//! So this joins them. The question it answers is deliberately *where a pane is
//! standing*, not *what it claimed* — the pane that caused the damage held no
//! claim for the first twenty minutes of its life, and a report that needs a
//! claim to fire would have been silent through exactly that window. A claim
//! makes the answer better. It cannot be what triggers it.
//!
//! Nothing is filtered out. Two different questions want asking here — *who
//! could clobber my working tree* and *who else is working at all* — and they
//! want different answers, so [`standing_beside`] returns everyone with a
//! [`Relation`] saying how close they are, nearest first. A caller that only
//! wants the near set takes the head of the list; one that wants context reads
//! on. Filtering here would push the second definition back out into callers,
//! which is the thing this file exists to stop.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::herdr;
use crate::model::Task;
use crate::resolve::{self, Index};
use crate::store::Store;
use crate::util;

/// How close another pane is to this one, nearest first.
///
/// The ordering is the point: it is what lets a caller take the head of the
/// list and get precisely the panes that can reach the files under its hands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Relation {
    /// The same directory, or one inside the other. Both are in the same
    /// working tree and either can overwrite what the other is editing.
    SameCwd,
    /// Different directories under the same declared project root — sibling
    /// corners of one checkout. Still one git tree, so still one commit.
    SameRoot,
    /// The same project, reached some other way: a pin, a label, a second root.
    /// Shared work, but not shared files.
    SameProject,
    /// Somewhere else entirely. Context, not a warning.
    Elsewhere,
}

impl Relation {
    /// Whether this pane can reach the files the asking pane is working on.
    /// The line between a warning and a mention.
    pub(crate) fn is_near(&self) -> bool {
        matches!(self, Relation::SameCwd | Relation::SameRoot)
    }

    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            // "tree" rather than "directory": herdr reports where a pane's
            // *shell* started, which for an agent launched from `~/claude` is
            // the parent of the checkout it is actually working in. That
            // containment is the whole reason this fires at all, so the word
            // has to cover it.
            Relation::SameCwd => "same tree",
            Relation::SameRoot => "same checkout",
            Relation::SameProject => "same project",
            Relation::Elsewhere => "elsewhere",
        }
    }
}

/// Another pane, placed: where it is standing, what it is holding, and how
/// close that puts it to the one asking.
#[derive(Debug, Clone)]
pub(crate) struct Standing {
    pub pane: String,
    /// The workspace label rather than its id — the id is herdr's business and
    /// the label is what a person recognises.
    pub workspace: String,
    pub cwd: String,
    /// Whether anyone is driving it. A shell counts: a person at one can
    /// clobber a file as thoroughly as an agent can.
    pub agent: bool,
    /// herdr's `idle`/`working`, empty for a shell.
    pub state: String,
    /// The terminal's own title, which is all a pane holding no task can say
    /// about itself.
    pub title: String,
    pub task: Option<Task>,
    pub project: Option<String>,
    /// Seconds it has held its claim, when it holds one.
    pub since: Option<i64>,
    pub relation: Relation,
    /// Path components shared with the asking pane. Carried because it is what
    /// orders two panes of the same [`Relation`], and a caller ranking its own
    /// subset should not have to work it out again.
    pub depth: usize,
}

impl Standing {
    pub(crate) fn json(&self) -> Value {
        json!({
            "pane": self.pane,
            "workspace": self.workspace,
            "cwd": self.cwd,
            "agent": self.agent,
            "state": self.state,
            "title": self.title,
            "task": self.task.as_ref().map(|t| t.json()),
            "project": self.project,
            "since": self.since,
            "relation": self.relation.as_str(),
            "near": self.relation.is_near(),
            "needs_you": self.needs_you(),
        })
    }

    /// An idle agent on a task that is still `doing`: it has stopped, and a
    /// person is the blocker. The panel draws this as `←`, `wsp wip` counts it,
    /// and the brief leads with it — so it is worth having one definition of it
    /// rather than each of them keeping its own.
    pub(crate) fn needs_you(&self) -> bool {
        self.agent
            && self.state == "idle"
            && self.task.as_ref().map(|t| t.status() == crate::model::Status::Doing).unwrap_or(false)
    }

    /// What to call it in one column: the task it claimed, else the terminal's
    /// title, else the pane id — in the order of how much each actually says.
    pub(crate) fn name(&self) -> String {
        if let Some(t) = &self.task {
            return t.title.clone();
        }
        if !self.title.trim().is_empty() {
            return format!("({})", self.title.trim());
        }
        // The workspace label before the pane id: a caller printing the id in
        // its own column got it twice and learned nothing the second time.
        if !self.workspace.trim().is_empty() {
            return self.workspace.clone();
        }
        self.pane.clone()
    }
}

/// Everything the reckoning reads, taken in rather than fetched — so a test can
/// stand six panes in a fabricated tree with no herdr and no store, the same
/// bargain [`crate::panel`] makes with its `Snapshot`.
pub(crate) struct World {
    pub panes: Vec<herdr::Pane>,
    pub workspaces: Vec<herdr::Workspace>,
    pub tasks: Vec<Task>,
    pub index: Index,
    pub pins: BTreeMap<String, String>,
    pub bindings: BTreeMap<String, Value>,
    pub claims: BTreeMap<String, Value>,
}

impl World {
    /// The live join: herdr for what exists, the store for who holds what.
    pub(crate) fn live(store: &Store) -> World {
        let (panes, workspaces) = if herdr::available() {
            (herdr::panes().unwrap_or_default(), herdr::workspaces().unwrap_or_default())
        } else {
            (Vec::new(), Vec::new())
        };
        World {
            panes,
            workspaces,
            tasks: store.tasks(),
            index: Index::new(store.projects()),
            pins: store.pins(),
            bindings: store.bindings(),
            claims: store.claims(),
        }
    }
}

/// The deepest declared project root containing `cwd`, if any.
///
/// Deepest rather than first: `~/claude` and `~/claude/wsp` can both be roots,
/// and a pane in the latter is in the more specific tree.
fn root_for(index: &Index, cwd: &str) -> Option<PathBuf> {
    let target = util::real(cwd);
    let mut best: Option<PathBuf> = None;
    for p in &index.projects {
        for r in &p.roots {
            let root = util::real(r);
            if target.starts_with(&root) {
                let deeper = best.as_ref().map(|b| root.components().count() > b.components().count());
                if deeper.unwrap_or(true) {
                    best = Some(root);
                }
            }
        }
    }
    best
}

/// Path components two directories share from the left.
fn shared_depth(a: &str, b: &str) -> usize {
    let (a, b) = (util::real(a), util::real(b));
    a.components().zip(b.components()).take_while(|(x, y)| x == y).count()
}

fn same_tree(a: &str, b: &str) -> bool {
    let (a, b) = (util::real(a), util::real(b));
    a.starts_with(&b) || b.starts_with(&a)
}

/// Everyone else standing anywhere, nearest first.
///
/// `my_cwd` overrides what herdr believes about the asking pane, because a
/// process knows its own directory better than the pane it is running in does —
/// herdr reports the shell's cwd, which is stale the moment anyone `cd`s.
pub(crate) fn standing_beside(w: &World, me: &str, my_cwd: Option<&str>) -> Vec<Standing> {
    let label_of = |ws: &str| {
        w.workspaces.iter().find(|x| x.id == ws).map(|x| x.label.clone()).unwrap_or_default()
    };
    let task_of = |pane: &str| -> Option<&Task> {
        let id = w.bindings.get(pane)?.get("task_id")?.as_str()?;
        w.tasks.iter().find(|t| t.id == id)
    };
    let project_of = |p: &herdr::Pane| {
        resolve::resolve(
            &w.index,
            &w.pins,
            resolve::Held {
                binding: task_of(&p.pane_id).and_then(|t| t.project.clone()),
                claim: resolve::claimed_project(
                    &w.claims,
                    &w.tasks,
                    Some(&p.workspace_id),
                    Some(&label_of(&p.workspace_id)),
                ),
            },
            Some(&p.workspace_id),
            Some(&label_of(&p.workspace_id)),
            Some(&p.cwd),
        )
        .project
    };

    // Where the asking pane is standing. A pane herdr has never heard of still
    // gets an answer, since `wsp overlap` is worth running from a bare shell.
    let mine = w.panes.iter().find(|p| p.pane_id == me);
    let my_cwd = my_cwd
        .map(|c| c.to_string())
        .or_else(|| mine.map(|p| p.cwd.clone()))
        .unwrap_or_default();
    let my_project = mine.and_then(project_of);
    let my_root = root_for(&w.index, &my_cwd);

    let mut out: Vec<Standing> = Vec::new();
    for p in &w.panes {
        if p.pane_id == me {
            continue;
        }
        // Our own furniture is not company. Every workspace has a panel and
        // most have a view, and reporting them would bury the one pane that
        // matters under the ones we installed ourselves.
        if p.label == crate::panel::PANEL_LABEL || p.label == crate::panel::VIEW_LABEL {
            continue;
        }

        let task = task_of(&p.pane_id).cloned();
        let project = project_of(p);
        let relation = if !my_cwd.is_empty() && !p.cwd.is_empty() && same_tree(&my_cwd, &p.cwd) {
            Relation::SameCwd
        } else if my_root.is_some() && my_root == root_for(&w.index, &p.cwd) {
            Relation::SameRoot
        } else if project.is_some() && project == my_project {
            Relation::SameProject
        } else {
            Relation::Elsewhere
        };

        let since = task
            .as_ref()
            .and_then(|t| w.claims.get(&t.id))
            .and_then(|c| c.get("claimed_at"))
            .and_then(|c| c.as_str())
            .map(util::since);

        out.push(Standing {
            pane: p.pane_id.clone(),
            workspace: label_of(&p.workspace_id),
            cwd: p.cwd.clone(),
            agent: !p.agent.is_empty(),
            state: p.agent_status.clone(),
            title: p.title.clone(),
            task,
            project,
            since,
            relation,
            depth: shared_depth(&my_cwd, &p.cwd),
        });
    }

    // Nearest first: by relation, then by how much of the path is shared, then
    // by pane id so the order never wobbles between two runs.
    out.sort_by(|a, b| {
        a.relation
            .cmp(&b.relation)
            .then(b.depth.cmp(&a.depth))
            .then(a.pane.cmp(&b.pane))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Project;

    fn pane(id: &str, ws: &str, cwd: &str) -> herdr::Pane {
        herdr::Pane {
            pane_id: id.into(),
            workspace_id: ws.into(),
            cwd: cwd.into(),
            agent: "claude".into(),
            agent_status: "working".into(),
            ..Default::default()
        }
    }

    fn world(panes: Vec<herdr::Pane>) -> World {
        let mut wsp = Project::new("wsp");
        wsp.roots = vec!["/home/ed/claude/wsp".into()];
        let mut vst = Project::new("vst");
        vst.roots = vec!["/home/ed/claude/vst".into()];
        World {
            panes,
            workspaces: Vec::new(),
            tasks: Vec::new(),
            index: Index::new(vec![wsp, vst]),
            pins: BTreeMap::new(),
            bindings: BTreeMap::new(),
            claims: BTreeMap::new(),
        }
    }

    /// The failure this exists for: two panes in one directory, neither
    /// claiming anything. Nothing in the store connects them — only the ground
    /// they are standing on.
    #[test]
    fn an_unclaimed_pane_is_still_company() {
        let w = world(vec![
            pane("w0:p3", "w0", "/home/ed/claude/wsp"),
            pane("wP:p3", "wP", "/home/ed/claude/wsp"),
        ]);
        let out = standing_beside(&w, "w0:p3", None);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].pane, "wP:p3");
        assert_eq!(out[0].relation, Relation::SameCwd);
        assert!(out[0].relation.is_near());
        assert!(out[0].task.is_none());
    }

    #[test]
    fn a_subdirectory_is_the_same_tree() {
        let w = world(vec![
            pane("a", "w1", "/home/ed/claude/wsp"),
            pane("b", "w2", "/home/ed/claude/wsp/src/panel"),
        ]);
        assert_eq!(standing_beside(&w, "a", None)[0].relation, Relation::SameCwd);
        assert_eq!(standing_beside(&w, "b", None)[0].relation, Relation::SameCwd);
    }

    /// Siblings under one checkout: neither contains the other, but a commit
    /// from either takes both.
    #[test]
    fn siblings_under_one_root_share_a_checkout() {
        let w = world(vec![
            pane("a", "w1", "/home/ed/claude/wsp/src"),
            pane("b", "w2", "/home/ed/claude/wsp/herdr-plugin"),
        ]);
        let out = standing_beside(&w, "a", None);
        assert_eq!(out[0].relation, Relation::SameRoot);
        assert!(out[0].relation.is_near());
    }

    #[test]
    fn a_different_checkout_is_elsewhere() {
        let w = world(vec![
            pane("a", "w1", "/home/ed/claude/wsp"),
            pane("b", "w2", "/home/ed/claude/vst"),
        ]);
        let out = standing_beside(&w, "a", None);
        assert_eq!(out[0].relation, Relation::Elsewhere);
        assert!(!out[0].relation.is_near());
    }

    /// Everyone comes back, not just the near ones — the caller decides where
    /// to draw the line, and gets both questions answered from one list.
    #[test]
    fn nobody_is_filtered_out_and_the_near_come_first() {
        let w = world(vec![
            pane("me", "w0", "/home/ed/claude/wsp"),
            pane("far", "w1", "/home/ed/other"),
            pane("near", "w2", "/home/ed/claude/wsp/src"),
            pane("checkout", "w3", "/home/ed/claude/wsp"),
        ]);
        let out = standing_beside(&w, "me", None);
        assert_eq!(out.len(), 3);
        assert_eq!(
            out.iter().map(|s| s.pane.as_str()).collect::<Vec<_>>(),
            ["checkout", "near", "far"],
            "same cwd, then deeper shared path, then the rest"
        );
        assert_eq!(out.iter().filter(|s| s.relation.is_near()).count(), 2);
    }

    /// A shell is a fact about the tree: someone is standing there and can
    /// overwrite a file whether or not an agent is driving.
    #[test]
    fn a_shell_counts_and_says_so() {
        let mut sh = pane("sh", "w1", "/home/ed/claude/wsp");
        sh.agent = String::new();
        sh.agent_status = String::new();
        let w = world(vec![pane("me", "w0", "/home/ed/claude/wsp"), sh]);
        let out = standing_beside(&w, "me", None);
        assert_eq!(out.len(), 1);
        assert!(!out[0].agent);
        assert_eq!(out[0].relation, Relation::SameCwd);
    }

    /// The panel and the view are in every workspace by construction. Counting
    /// them would bury the one pane that matters.
    #[test]
    fn our_own_furniture_is_not_company() {
        let mut panel = pane("p", "w1", "/home/ed/claude/wsp");
        panel.label = crate::panel::PANEL_LABEL.into();
        let mut view = pane("v", "w1", "/home/ed/claude/wsp");
        view.label = crate::panel::VIEW_LABEL.into();
        let w = world(vec![pane("me", "w0", "/home/ed/claude/wsp"), panel, view]);
        assert!(standing_beside(&w, "me", None).is_empty());
    }

    /// A process knows its own directory; herdr only knows where the pane's
    /// shell started.
    #[test]
    fn the_callers_cwd_wins_over_herdrs() {
        let w = world(vec![
            pane("me", "w0", "/home/ed/somewhere/stale"),
            pane("b", "w2", "/home/ed/claude/wsp"),
        ]);
        assert_eq!(standing_beside(&w, "me", None)[0].relation, Relation::Elsewhere);
        let out = standing_beside(&w, "me", Some("/home/ed/claude/wsp"));
        assert_eq!(out[0].relation, Relation::SameCwd);
    }

    #[test]
    fn a_pane_herdr_has_never_heard_of_still_gets_an_answer() {
        let w = world(vec![pane("b", "w2", "/home/ed/claude/wsp")]);
        let out = standing_beside(&w, "not-a-pane", Some("/home/ed/claude/wsp"));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].relation, Relation::SameCwd);
    }

    #[test]
    fn what_a_pane_is_called_is_the_best_thing_it_can_say() {
        let w = world(vec![pane("me", "w0", "/x"), pane("b", "w2", "/y")]);
        let mut s = standing_beside(&w, "me", None).remove(0);
        assert_eq!(s.name(), "b", "nothing to go on at all: the pane id");
        s.workspace = "Verb UI".into();
        assert_eq!(s.name(), "Verb UI", "the workspace label beats the pane id");
        s.title = "◐ Exploring".into();
        assert_eq!(s.name(), "(◐ Exploring)");
        s.task = Some(Task::new("Split panel.rs", "t-1"));
        assert_eq!(s.name(), "Split panel.rs");
    }
}
