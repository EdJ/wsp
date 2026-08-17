//! What is running, in wsp's own words.
//!
//! One indirection, and it is the whole of this file: a view asks *here* what
//! panes exist, and [`crate::herdr`] is on this side of the answer rather than
//! inside the view's own struct.
//!
//! Before it, all three views carried the runner's types — `panel::Snapshot`
//! held `Vec<herdr::Workspace>` and `Vec<herdr::Pane>`, `detail::Ctx` and
//! `kanban::Ctx` a pane list each. So a renderer handed a view was handed a
//! herdr type on every surface, and `story.rs` opened by fabricating
//! `herdr::Pane`s to draw pictures that have nothing to do with a runner.
//! `draw.rs` reported that while the renderer port was being written, because a
//! renderer is handed a view and a canvas renderer for a phone would therefore
//! have had to link wsp's herdr socket client to compile.
//!
//! # [`AgentRef`] is not a new type
//!
//! It is the projection the panel had already written *inside* `collect()`,
//! moved up to where the socket call is. The three view and render layers were
//! measured at 96b37a4 and read eight fields off herdr's two types —
//! `pane_id`, `workspace_id`, `label`, `title`, `agent`, `agent_status`, `cwd`
//! and `focused`. Seven of them are what a row here carries; the eighth is
//! below, and does not travel in a view at all.
//!
//! Two of the row's fields — `task` and `project` — are the store's half of the
//! join and are left empty here, because the runner cannot know either. The
//! view that has the store fills them in; see `panel::rows::collect`, which is
//! the only place that resolves a pane to a project.
//!
//! # Why this is not `place::Seated`
//!
//! `place.rs` is the port for *starting and stopping* work, and its [`Seated`]
//! covers five of the seven: seat, label, cwd, agent, state. The two it does
//! not are `title` — a pane's terminal title, the second fallback in
//! [`pane_name`] — and `workspace_id`, which is where a row sends you. Both are
//! drawing facts, so the view's pane row is this and not that, and the two
//! ports stay apart for the reason `place.rs` and `arrange.rs` already gave:
//! they have different backends, lifetimes and failure modes.
//!
//! [`Seated`]: crate::place::Seated
//!
//! # Focus does not travel inside a view
//!
//! herdr answers two focus questions — which workspace is on screen, and which
//! pane in it has the keyboard — and neither is drawn. Their only readers are
//! in the panel's event loop: the cadence it schedules itself on (250ms for the
//! workspace being looked at, 30s for the twenty-one that are not) and what a
//! click means in a pane that did not already have the keyboard.
//!
//! So they are carried on [`Live`], beside the rows rather than inside them.
//! `draw.rs` states the rule for renderers — focus is not an input, and holds
//! by there being nowhere to read it from — and a scheduling input travelling
//! in a view is one every renderer is handed and none of them can use.

use std::collections::BTreeMap;

use crate::herdr;

/// A pane, as a view knows one.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct AgentRef {
    pub(crate) pane: String,
    pub(crate) workspace: String,
    /// What that workspace is called. Carried on the row rather than the view
    /// holding a second list to join against: it is read to name the pane when
    /// nothing better exists, and to place a pane whose cwd resolves nowhere.
    pub(crate) workspace_label: String,
    /// herdr's own label for the pane, which `claim` and `wsp say` keep
    /// current. Kept raw beside [`AgentRef::where_`] because the panel decides
    /// what is *furniture* by it — our own panels wear a label we wrote, and a
    /// pane that merely happens to be titled `wsp` is not one of ours.
    pub(crate) label: String,
    /// What the agent called itself when it started, and never revises.
    pub(crate) title: String,
    /// Where the pane's shell started. Not where the work is: an agent
    /// launched from the directory above every checkout stands in no project by
    /// this measure, which is why the panel can end up having to ask.
    pub(crate) cwd: String,
    /// herdr's word for what the agent is doing — `working`, `idle`, and the
    /// empty string for a pane that has not said. Read through
    /// [`crate::panel::agent_state`], which joins it with what the store holds.
    pub(crate) state: String,
    /// Whether an agent is running here, or it is just a shell.
    pub(crate) agent: bool,
    /// Which agent, as herdr spells it — `claude`, `codex`, `gemini` — and the
    /// empty string for a shell. Carried because one thing a verb does to a
    /// pane is not the same sentence at every kind: emptying a context is
    /// `/clear` at Claude Code and something else, or nothing, everywhere else.
    pub(crate) kind: String,
    /// The task claimed to this pane, if it holds one. Carried so a verb aimed
    /// at the pane can refuse on the ground that matters — it already has work
    /// — rather than on where the row happens to sit.
    ///
    /// The store's half of the row: empty as [`read`] leaves it, filled by the
    /// view that joins.
    pub(crate) task: Option<String>,
    /// The project this pane would take work from: its mandate if it has one,
    /// else wherever it is standing. Deliberately not the same question as
    /// which branch of the tree the row is drawn under — standing direction
    /// says what a pane is *for*, and the tree places it by where it *is*.
    ///
    /// The store's half of the row, like `task` above.
    pub(crate) project: Option<String>,
}

impl AgentRef {
    /// What to call it — see [`pane_name`].
    pub(crate) fn where_(&self) -> String {
        pane_name(&self.label, &self.title, &self.workspace_label)
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
///
/// One function for every surface: the tree, the board and the detail view name
/// the same panes, and a second answer to "what is this terminal called" is how
/// two surfaces come to disagree about which of the three strings is current.
pub(crate) fn pane_name(label: &str, title: &str, workspace: &str) -> String {
    [label, title, workspace]
        .into_iter()
        .map(str::trim)
        .find(|s| !s.is_empty())
        .unwrap_or_default()
        .to_string()
}

/// One reading of the runner: the rows a view draws, and the two facts about
/// focus that no view draws.
#[derive(Debug, Clone, Default)]
pub(crate) struct Live {
    /// Every pane herdr reports, ours included. Filtering the furniture out is
    /// the panel's business and not the same rule on the board — see
    /// `panel::rows::collect`.
    pub(crate) panes: Vec<AgentRef>,
    /// pane id -> whether it has the keyboard. Read through
    /// [`Live::has_keyboard`], which carries what an absent answer means.
    keyboard: BTreeMap<String, bool>,
    /// workspace id -> whether herdr is showing it. Read through
    /// [`Live::on_screen`]. A map rather than "the focused one", because panes
    /// can arrive from more than one machine and each of them is showing
    /// something.
    screen: BTreeMap<String, bool>,
}

impl Live {
    /// Does this pane have the keyboard?
    ///
    /// A pane herdr has never heard of counts as focused, and so does a panel
    /// that cannot name itself: both mean there is no focus to lose — a panel
    /// running outside herdr, or one whose census arrived empty because the
    /// socket was not answering — and a panel that read either as "somebody
    /// else has the keyboard" would swallow every click it was sent.
    pub(crate) fn has_keyboard(&self, pane: Option<&str>) -> bool {
        pane.and_then(|p| self.keyboard.get(p)).copied().unwrap_or(true)
    }

    /// Is this the workspace on screen?
    ///
    /// Unknown counts as on screen for the same reason, and the cost of the
    /// default is the other way round here: a panel that wrongly believed
    /// nobody was looking would drop to the thirty-second cadence in front of
    /// somebody who is.
    pub(crate) fn on_screen(&self, workspace: Option<&str>) -> bool {
        workspace.and_then(|w| self.screen.get(w)).copied().unwrap_or(true)
    }
}

/// Ask herdr what is running, and answer in wsp's types.
///
/// A herdr that is not answering degrades to an empty list rather than an
/// error, so every surface still shows the durable half. Two socket calls,
/// because a pane carries the name of the workspace it stands in and only the
/// workspace list has it.
pub(crate) fn read() -> Live {
    let workspaces = herdr::workspaces().unwrap_or_default();
    let panes = herdr::panes().unwrap_or_default();
    let label = |id: &str| -> String {
        workspaces.iter().find(|w| w.id == id).map(|w| w.label.clone()).unwrap_or_default()
    };
    Live {
        keyboard: panes.iter().map(|p| (p.pane_id.clone(), p.focused)).collect(),
        screen: workspaces.iter().map(|w| (w.id.clone(), w.focused)).collect(),
        panes: panes
            .iter()
            .map(|p| AgentRef {
                pane: p.pane_id.clone(),
                workspace: p.workspace_id.clone(),
                workspace_label: label(&p.workspace_id),
                label: p.label.clone(),
                title: p.title.clone(),
                cwd: p.cwd.clone(),
                state: p.agent_status.clone(),
                agent: !p.agent.is_empty(),
                kind: p.agent.clone(),
                task: None,
                project: None,
            })
            .collect(),
    }
}

/// The rows alone, for the two views whose loops schedule on nothing.
pub(crate) fn panes() -> Vec<AgentRef> {
    read().panes
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole of what this file bought, and the one check that can say so.
    ///
    /// Read the way `panel/run.rs` reads its own source: the assertion is over
    /// what the binary is built from, rather than a restatement of it that can
    /// drift. Three views and a storyboard, named one at a time rather than by
    /// directory, because the rest of `panel/` and `detail/` talks to herdr
    /// legitimately — installing panes, sending text, and the event loop — and
    /// a check over the whole directory would fail for the work that is
    /// supposed to be there.
    ///
    /// It fails the moment somebody reaches past the seam for a ninth field,
    /// which is the cheap version of the thing it protects: a renderer under
    /// `crate::draw` is handed a view, so a view that names herdr makes every
    /// target link wsp's socket client, including the ones that have no socket.
    #[test]
    fn no_view_names_the_runner() {
        let views = [
            ("panel/rows.rs", include_str!("panel/rows.rs")),
            ("detail/render.rs", include_str!("detail/render.rs")),
            ("kanban/mod.rs", include_str!("kanban/mod.rs")),
            ("story.rs", include_str!("story.rs")),
        ];
        for (name, src) in views {
            assert!(
                !src.contains(concat!("herdr", "::")),
                "{name} names a herdr type again — the projection is `live::AgentRef`, \
                 and if it is missing a field, add the field here",
            );
        }
    }

    /// One pane in the census has the keyboard, and a click means one thing in
    /// that pane and another in every other. This is where the panel's loop
    /// starts from and where it recovers to when the event stream has been
    /// away — so what an *absent* answer means is the part worth pinning: a
    /// panel that read "herdr did not mention you" as "somebody else is typing"
    /// would swallow every click it was ever sent.
    #[test]
    fn a_pane_nobody_reported_still_holds_its_own_keyboard() {
        let mut live = Live::default();
        live.keyboard.insert("w1:p1".into(), false);
        live.keyboard.insert("w0:p3".into(), true);
        live.screen.insert("w1".into(), false);

        assert!(!live.has_keyboard(Some("w1:p1")));
        assert!(live.has_keyboard(Some("w0:p3")));
        assert!(live.has_keyboard(Some("w9:p9")), "a pane herdr has never heard of");
        assert!(live.has_keyboard(None), "and a panel running outside herdr");

        assert!(!live.on_screen(Some("w1")));
        assert!(live.on_screen(Some("w9")));
        assert!(live.on_screen(None));
    }
}
