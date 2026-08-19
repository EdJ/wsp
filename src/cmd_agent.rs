//! Agent binding, context resolution, and the views that join the store to
//! herdr's live panes.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::cmd_govern;
use crate::herdr;
use crate::message::{self, About, Ask, Hand, Kind, Message, Party, Shape, Waiting};
use crate::model::{Status, Task};
use crate::place::{Delivery, Place, Refusal, State};
use crate::place_herdr::Herdr;
use crate::resolve::{self, Index};
use crate::store::Store;
use crate::sync;
use crate::util::{self, Paint};
use crate::Args;

/// Every pane holding `task` that is not `me`, and that a live agent is
/// driving.
///
/// The one definition of "somebody else has this". `claim` refuses on it and
/// `next` declines to name it, and the two have to be the same rule: when they
/// disagreed, `next` named work `claim` would refuse, so an agent told to find
/// its own work was sent straight into a wall with nothing to try instead.
///
/// A *dead* pane's binding is not a holder — that is exactly the stale state a
/// re-claim exists to clear. Nor is a shell's: a person at a terminal can be
/// asked to move, and refusing on their behalf helps nobody.
pub fn live_holders<'a>(
    bindings: &std::collections::BTreeMap<String, serde_json::Value>,
    panes_now: &'a [herdr::Pane],
    task: &str,
    me: Option<&str>,
) -> Vec<&'a herdr::Pane> {
    panes_now
        .iter()
        .filter(|p| !p.agent.is_empty())
        .filter(|p| Some(p.pane_id.as_str()) != me)
        .filter(|p| {
            bindings
                .get(&p.pane_id)
                .and_then(|b| b.get("task_id"))
                .and_then(|x| x.as_str())
                == Some(task)
        })
        .collect()
}

/// Everything the standing-in chain reads, gathered into one value.
///
/// The chain itself — pin > binding > claim > mandate > cwd > label — is the
/// precedence every command in wsp inherits, and two of its six steps are
/// decided here rather than in [`resolve::resolve`]: where the mandate sits,
/// and the label lookup at the bottom. Those two had no test, because reading
/// them took a pane with the right environment, a store with the right
/// mandate, and a herdr answering with the right label at once.
pub(crate) struct Here {
    pub index: Index,
    pub pins: std::collections::BTreeMap<String, String>,
    pub bindings: std::collections::BTreeMap<String, serde_json::Value>,
    pub claims: std::collections::BTreeMap<String, serde_json::Value>,
    pub mandates: std::collections::BTreeMap<String, serde_json::Value>,
    pub tasks: Vec<Task>,
    /// Read for one field, and only at the bottom of the chain: the label of
    /// the workspace this pane stands in.
    pub workspaces: Vec<herdr::Workspace>,
    pub pane: Option<String>,
    pub workspace: Option<String>,
    pub cwd: Option<String>,
}

impl Here {
    pub(crate) fn live(store: &Store, index: &Index) -> Here {
        let env = herdr::Env::read();
        Here {
            index: Index::new(index.projects.clone()),
            pins: store.pins(),
            bindings: store.bindings(),
            claims: store.claims(),
            mandates: store.mandates(),
            tasks: store.tasks(),
            workspaces: match (&env.workspace_id, herdr::available()) {
                (Some(_), true) => herdr::workspaces().unwrap_or_default(),
                _ => Vec::new(),
            },
            // The seat, through the one reading of it, rather than herdr's
            // variable directly. A binding is keyed on a seat and a
            // supervisor's seats are not panes, so an agent hosted with no
            // terminal resolved to no binding at all: measured 2026-08-17, a
            // headless spawn whose `SessionStart` brief said "nothing claimed"
            // about the task the same command had just claimed for it.
            pane: my_pane(),
            workspace: env.workspace_id,
            cwd: std::env::current_dir().ok().map(|p| p.display().to_string()),
        }
    }
}

/// Walk the chain. Pure: everything it reads is in `h`.
pub(crate) fn standing_in(h: &Here) -> Option<String> {
    let bound_project = h.pane.as_ref().and_then(|pane| {
        h.bindings
            .get(pane)
            .and_then(|b| b.get("task_id"))
            .and_then(|t| t.as_str())
            .and_then(|id| h.tasks.iter().find(|t| t.id == id))
            .and_then(|t| t.project.clone())
    });

    // A mandate is a statement about what this workspace is *for*, so it beats
    // the directory the shell happens to be sitting in — but not a pin, which
    // is a statement about what the workspace *is*, and not a binding, which is
    // the work actually in hand. Checked here rather than inside `resolve` so
    // that the panel and `overlap` go on placing panes by where they stand:
    // standing direction says nothing about which tree a pane is in.
    let mandate = h
        .workspace
        .as_deref()
        .and_then(|ws| crate::cmd_mandate::from_map(&h.mandates, ws));

    let r = resolve::resolve(
        &h.index,
        &h.pins,
        resolve::Held {
            binding: bound_project,
            claim: resolve::claimed_project(&h.claims, &h.tasks, h.workspace.as_deref(), None),
        },
        h.workspace.as_deref(),
        None,
        h.cwd.as_deref(),
    );
    // A claim is work in hand, like a binding, so it stands with the binding
    // above the mandate: what this workspace is *doing* beats what it is *for*
    // for as long as it is holding it.
    if matches!(r.source, "pin" | "binding" | "claim") && r.project.is_some() {
        return r.project;
    }
    if mandate.is_some() {
        return mandate;
    }
    if r.project.is_some() {
        return r.project;
    }

    // Last resort: the workspace's own label, which is the only link herdr
    // supplies and the only one that goes away when it stops answering.
    let ws = h.workspace.as_deref()?;
    let w = h.workspaces.iter().find(|w| w.id == ws)?;
    h.index.project_for_label(&w.label)
}

/// The task this pane is holding, out of the two records that say so.
///
/// The binding is herdr's live answer and the claim is the durable one, and the
/// order is that: a binding names the pane in front of you, where a claim keyed
/// on the workspace survives a restart the daemon has not reconciled yet, so a
/// session that comes back early still knows what it was doing.
///
/// One rule, because there are now two readers of it — the brief, which prints
/// what you are on, and [`crate::cmd_govern`], whose routing walk starts at the
/// worklist this task is a member of. Two copies of a precedence rule is how
/// the two come to disagree about which task a pane is on, and a disagreement
/// there is a raised hand delivered to the wrong seat.
pub(crate) fn task_in_hand(
    bindings: &std::collections::BTreeMap<String, serde_json::Value>,
    claims: &std::collections::BTreeMap<String, serde_json::Value>,
    pane: Option<&str>,
    workspace: Option<&str>,
) -> Option<String> {
    pane.and_then(|p| bindings.get(p))
        .and_then(|b| b.get("task_id"))
        .and_then(|t| t.as_str())
        .map(str::to_string)
        .or_else(|| {
            let ws = workspace?;
            claims
                .iter()
                .find(|(_, c)| c.get("workspace_id").and_then(|x| x.as_str()) == Some(ws))
                .map(|(id, _)| id.clone())
        })
}

/// The project the caller is standing in. `-p` always wins; otherwise the
/// precedence chain is pin > binding > claim > mandate > cwd > label.
pub fn current_project(
    store: &Store,
    args: &Args,
    index: &Index,
) -> Result<Option<String>, i32> {
    if let Some(p) = args.get("project") {
        if p == "none" || p == "inbox" {
            return Ok(None);
        }
        return match index.find(&p) {
            Some(found) => Ok(Some(found.id.clone())),
            None => {
                eprintln!("wsp: no such project `{p}`");
                Err(1)
            }
        };
    }
    Ok(standing_in(&Here::live(store, index)))
}

fn pane_id(args: &Args) -> Option<String> {
    args.get("pane").or_else(my_pane)
}

/// Which seat this process is standing in, or none.
///
/// The one reading of it, and it is now the port's rather than herdr's:
/// `place::Place::here` is the verb, and which variable carries the answer is
/// the adapter's business. Every agent-side verb — `claim`, `say`, `release` —
/// is downstream of this, and so is `despawn`'s refusal to end the seat it is
/// running in.
///
/// A `String` because that is what the callers below hold — a claim's `pane`,
/// a binding's key, the `--pane` a person types are all the same string, and
/// converting them all is `place::Seat`'s own migration rather than this one's.
///
/// No socket is opened: [`Herdr::here`] reads what herdr set before this process
/// existed, which is a rule of the port and not an optimisation. Constructing
/// the backend costs four durations and a clock reference.
///
/// # Which backend is this process standing in
///
/// Two backends can seat an agent and each has its own name for the seat it
/// hands its occupant — `HERDR_PANE_ID` for a pane, `place::SEAT_ENV` for a
/// supervisor. Neither adapter is allowed to read the other's:
/// `place_herdr::here` says why in as many words, and the mirror image holds —
/// two answers to *which seat is this* is a way to be in two seats, and under
/// herdr a stale `WSP_SEAT_ID` would bind a task to a pane that backend never
/// issued.
///
/// So the choice is made here, one level up, and it is made by **which name the
/// seat arrived under** rather than by asking anything. A process started by a
/// supervisor has the supervisor's variable and nothing else; a shell in a pane
/// has herdr's. The order matters only for the case that should not exist —
/// both set — and the supervisor wins it because it is the backend that forked
/// this process, whatever multiplexer the shell it came from was in.
pub(crate) fn my_pane() -> Option<String> {
    let seat = match crate::place_super::Supervisor::new().here() {
        Some(s) => Some(s),
        None => Herdr::new().here(),
    };
    seat.map(|s| s.to_string())
}

/// The width every label wsp writes is cut to — the 44 characters `sync`
/// already gives the `task` token. A herdr sidebar is 26 columns and draws its
/// own ellipsis, so this is not about what fits: it is about not putting a
/// paragraph on the wire as a name.
///
/// It is the width of the *wire* and of nothing else. Every name below is
/// built whole and cut here, on the last line before it is sent, so a surface
/// with room — `panel --full` is a hundred columns of it — can read the whole
/// one back out of [`full_name`] and do its own cutting to its own width. The
/// rule this constant states was never the mistake; being the only copy was.
const LABEL_MAX: usize = 44;

/// The one cut, and the only place a name of ours is shortened for herdr.
fn on_the_wire(full: &str) -> String {
    util::truncate(full, LABEL_MAX)
}

/// What you would type to mean this task: its id, `render-109`, printed as it
/// is stored.
///
/// It used to be composed — the project the task is in *now*, a slash, the
/// number off the end of the id — and that composes a name the CLI rejects. An
/// id keeps the prefix it was numbered under when the task moves project, by
/// decision on robustness-002, because an id must never change under a claim, a
/// parent link or a log line: `wsp-077` can end up living in `batch`. The
/// label said `batch/077`, `wsp show batch-077` found nothing, and the thirty
/// or so tasks moved when `batch` and `fork` were made all displayed a name
/// that does not exist.
///
/// Worse than a plain mismatch, because it looks right: a project beside a
/// bare number is the pre-t-260815-004 ambiguity — the one that had Ed reading
/// the wrong `022` — wearing the new scheme's clothes, and the two agree only
/// by coincidence, for a task that has never moved.
///
/// Both halves are still here, which was the whole argument for composing one:
/// `render-109` beside `robustness-109` is two agents told apart in ten columns
/// where `109` beside `109` is not. They are simply the two halves the store
/// wrote down, rather than one of them and a guess.
pub fn task_scope(task: &Task) -> String {
    task.id.clone()
}

/// Every scope a label of ours might be wearing, today's first: the id, then
/// the composed shapes claims wrote before it.
///
/// Nothing renames the world. Every pane and workspace named before this
/// change carries `<project>/<n>`, and both ends of the naming pair ask one
/// question of a label — [`named_after_task`], to take the work's name back
/// off a released pane, and [`pane_rename`], to put the right one on. A
/// matcher that knew only the id form would answer no to both: a pane claimed
/// yesterday would keep its unresolvable name until somebody typed a new one,
/// and `release` would leave the work's name on an agent holding nothing,
/// which is the bug [`unname_after_task`] exists to prevent.
///
/// Two projects, because the shape was composed from wherever the task sat at
/// the time: where it is now, and where its id was numbered. The bare ident is
/// what a task with no project got.
///
/// The old shapes are ambiguous — that is the defect — so `batch/077` could be
/// two tasks. It costs nothing here: both callers ask this about a pane or a
/// workspace already bound to the task, so the only question is whether *this*
/// name was written for *this* binding, and a name never written by any claim
/// still matches none of the shapes.
fn scopes_of(task: &Task) -> Vec<String> {
    let ident = task.id.rsplit('-').next().unwrap_or(&task.id).to_string();
    let numbered_in = task.id.rsplit_once('-').map(|(p, _)| p.to_string());
    let mut out = vec![task_scope(task)];
    for p in [task.project.clone(), numbered_in].into_iter().flatten() {
        let legacy = format!("{p}/{ident}");
        if !p.is_empty() && !out.contains(&legacy) {
            out.push(legacy);
        }
    }
    if !out.contains(&ident) {
        out.push(ident);
    }
    out
}

/// The scope a label of ours is wearing, or `None` if the label is not one of
/// ours. Anchored at the front and taking the separator with it, so a scope
/// only ever matches where a claim would have written one.
fn scope_worn(label: &str, task: &Task) -> Option<String> {
    let label = label.trim();
    scopes_of(task).into_iter().find(|s| label.starts_with(&format!("{s} ·")))
}

/// The name a task lends the workspace and pane holding it, or `None` if it
/// lends none.
///
/// Scope first, sentence second, because the ellipsis falls on the right and a
/// collapsed sidebar is a rail a few columns wide. Three agents in one tree all
/// wearing a title that starts "let's add the …" are three agents you cannot
/// tell apart without widening the sidebar and reading to the end of each one;
/// `render-109` in front of it is the whole answer in ten columns, and it is
/// also what you would type to go and look at the work.
pub fn task_label(task: &Task) -> Option<String> {
    task_full(task).map(|full| on_the_wire(&full))
}

/// The same name with nothing taken out of it.
///
/// The store's own copy, and the reason a pane holding a task needs nothing
/// remembered anywhere: the title is in the task file, whole, and a surface
/// wide enough to draw it can rebuild the name from what the label was made of
/// rather than from the label.
pub(crate) fn task_full(task: &Task) -> Option<String> {
    said_full(task, &task.title)
}

/// The same name, with something an agent said in place of the task's title,
/// and every word of it.
///
/// The scope belongs to the pane for as long as it holds the task, not just
/// until the first `wsp say` — the sentence is what changed, and losing which
/// piece of work it is about was the cost of saying anything at all.
///
/// Nothing is cut here. Cutting the sentence to the room left by the scope and
/// cutting the finished string to [`LABEL_MAX`] come to the same characters —
/// the scope is short and survives either way — so the two-step form the label
/// used to be built in bought nothing and cost the only copy of the sentence.
/// One string is built here, and whoever sends it puts it [`on_the_wire`].
fn said_full(task: &Task, said: &str) -> Option<String> {
    let said = said.trim();
    if said.is_empty() {
        return None;
    }
    Some(format!("{} · {said}", task_scope(task)))
}

/// What a pane wears once the work has been taken back off it.
///
/// A word rather than nothing. A pane with no label of ours falls back to its
/// terminal title — the prompt an agent announced itself with and never
/// revises — so blanking a released pane leaves the agents panel saying,
/// confidently, that it is still on the thing it just handed back. This says
/// the one fact that is certainly true about it, and it is the fact you are
/// reading that list to find: nobody has given this agent anything.
pub(crate) const UNASSIGNED_LABEL: &str = "unassigned";

/// A name for a pane holding no task, which is the only kind that has to be
/// checked for a name of ours.
///
/// `wsp` and `wsp:view` are withheld. They are how the panel finds its own
/// panes, and `install` adopts a stray pane labelled `wsp` as a panel it lost
/// track of — an agent that said "wsp" would be adopted as furniture. Nothing
/// scoped can collide: a scope carries a `/` and those two do not.
///
/// Withheld here rather than after the cut: all three are short words, so the
/// check reads the same on either side of it, and this side is the string
/// somebody actually typed.
fn plain_full(said: &str) -> Option<String> {
    match said.trim() {
        "" | crate::panel::PANEL_LABEL | crate::panel::VIEW_LABEL | crate::panel::FULL_LABEL => {
            None
        }
        said => Some(said.to_string()),
    }
}

/// The whole of the name a pane is wearing a cut-down of, or `None` when what
/// it is wearing is already the whole of it.
///
/// The one reader of everything above, and the answer every wide surface wants.
/// `name` is what the surface would otherwise draw — a pane's label, or the
/// workspace's when the pane has none — and the two ways back to the long form
/// are asked in the order of how much they are worth:
///
/// 1. **What the store kept.** A sentence from `wsp say` exists nowhere else,
///    so `said.json` holds it beside the label it was cut to. It is used only
///    while the pane is still wearing that label: anything that renamed the
///    pane since — a person, a claim, another tool — means the sentence is not
///    what is on screen, and a long name that disagrees with the short one is
///    worse than the short one.
/// 2. **What the task says.** A pane named after its task needs nothing
///    remembered: the title is in the store, and rebuilding the name from it is
///    exactly what [`task_label`] did before cutting. Matched against the cut
///    form, so a title edited since the claim does not expand into a name the
///    pane is not wearing — that stale label is [`pane_rename`]'s trade, and
///    this is not the place to take a different one.
///
/// `None` when neither answers, which is most panes: a shell's own title, a
/// name somebody typed, `unassigned`. Nothing there was ever cut, so there is
/// nothing to give back.
pub(crate) fn full_name(
    said: &BTreeMap<String, Value>,
    pane: &str,
    name: &str,
    held: Option<&Task>,
) -> Option<String> {
    let str_at = |v: &Value, k: &str| v.get(k).and_then(|s| s.as_str()).unwrap_or("").to_string();
    let cached = said
        .get(pane)
        .filter(|v| str_at(v, "label") == name)
        .map(|v| str_at(v, "full"))
        .filter(|full| !full.is_empty());
    cached
        .or_else(|| match held {
            Some(t) if task_label(t).as_deref() == Some(name) => task_full(t),
            _ => None,
        })
        .filter(|full| full != name)
}

/// Put the task's name on the workspace and the pane that took it up. Returns
/// the workspace's new label, if the workspace took it.
///
/// herdr has no name of its own for a workspace nobody named — `workspace.list`
/// answers with the agent standing in it, or the folder leaf — so three agents
/// in one tree all read as `claude`, which is the one thing about them you
/// already knew. A claim is the moment wsp knows better.
///
/// It renames over a name typed by hand, by decision on wsp-016, and the
/// claim prints what it overwrote so `herdr workspace rename` can put it back.
/// It used to cost `resolve` its last resort — a workspace whose project was
/// inferred from a label like `Trance Video` lost that inference the moment the
/// label became a task title — and the scope on the front hands it back: the
/// label now leads with the project's own id, which is the first thing
/// [`Index::project_for_label`] looks for.
///
/// The one name it will not write over is a seat's. A workspace holding a
/// custodial slot is named after the position — see
/// [`cmd_govern::governor_of`] — and a custodian that claims a task to read it
/// would otherwise rename its own room after the task and leave the sidebar
/// saying what robustness-048 was filed about: `robustness/078`, with nothing
/// anywhere saying seat. The pane still takes the task's name, because a pane
/// answers "what is happening in there now" and that is what is happening.
fn name_after_task(pane: &str, workspace: &str, task: &Task, ws_label: &str) -> Option<String> {
    let label = task_label(task)?;
    if !herdr::available() {
        return None;
    }
    if !pane.is_empty() {
        let _ = herdr::rename_pane(pane, &label);
    }
    if workspace.is_empty() || cmd_govern::is_governor_label(ws_label) {
        return None;
    }
    herdr::rename_workspace(workspace, &label).ok().map(|_| label)
}

/// Whether a name is one this task's claim put there.
///
/// Two shapes wear the same scope and only one of them is the title. `claim`
/// writes [`task_label`], and every `wsp say` since has written a
/// [`said_label`] over it — the same `render-047 ·` in front, a different
/// sentence behind. Matching the title alone meant that an agent which had
/// said anything at all kept the work's name for ever after handing it back,
/// which is every agent that follows the brief.
///
/// The scope is what wsp owns and the only part of either shape that is not a
/// sentence somebody chose: it is the task's id, and nothing a person types by
/// hand looks like it. So the scope is what is recognised, and a name that
/// does not carry it is somebody else's and is left alone.
fn named_after_task(label: &str, task: &Task) -> bool {
    scope_worn(label, task).is_some()
}

/// What [`name_bound`] should rename a bound pane to, or `None` to leave the
/// name it is wearing.
///
/// The bug this exists to stop: `name_bound` used to rename whenever the label
/// differed from [`task_label`], and a sentence differs from the title by
/// construction — so every `reconcile` erased every agent's `wsp say`, on every
/// daemon start, silently. The status line each agent keeps for everyone else's
/// benefit was destroyed by a maintenance verb, and the cost lands on a person,
/// who has to interrupt an agent to ask what it is doing. That is the exact cost
/// `wsp say` was built to remove.
///
/// So the question is [`named_after_task`]'s, not equality's: *is this a name of
/// ours for this task*. That is the same question [`unname_after_task`] already
/// asks at the other end of the pair, and asking it here is what makes the two
/// halves of the mechanism agree. `name_bound` still does the whole job it was
/// written for — a pane wearing `claude`, or a name from before any of this
/// existed, or nothing, carries no scope and is renamed.
///
/// The trade: a pane wearing the *old* title after `wsp edit --title` also
/// carries the right scope, so reconcile no longer refreshes it. It cannot be
/// told from a sentence — one field holds both, which is the argument for moving
/// `say` off the label entirely (robustness-026). It is the cheaper half of the
/// trade: the workspace below is untouched and still takes the new title, and a
/// workspace is where "what is this work" is read; the pane answers "what is
/// happening in there now", and a sentence is a better answer to that than a
/// title. `claim` and `wsp say --clear` both write the new title back.
///
/// The name it answers with is the whole one; [`name_bound`] cuts it for the
/// wire. `full` is what the pane's label was cut from, when the store kept it —
/// carried in so a rename reads the sentence at full length and puts it back at
/// full length. Take it from the label instead and reconcile quietly cuts the
/// sentence a second time, wearing down what an agent said one rewrite at a
/// time, which is a slower version of the erasure this function was written to
/// stop.
fn pane_rename(pane_label: &str, full: Option<&str>, task: &Task) -> Option<String> {
    match scope_worn(pane_label, task) {
        Some(worn) if worn == task_scope(task) => None,
        // Ours, in a shape that names nothing. Only the scope is wrong, so
        // only the scope is rewritten and the sentence behind it is carried
        // across — it is the agent's, and it is newer than anything reconcile
        // knows. Writing the title back here would be the erasure this
        // function exists to stop, arriving by the other door.
        Some(worn) => {
            let wearing = full.unwrap_or(pane_label);
            let said = wearing.trim().strip_prefix(&format!("{worn} ·")).unwrap_or("").trim();
            said_full(task, said).or_else(|| task_full(task))
        }
        None => task_full(task),
    }
}

/// Take the task's name back off the pane and the workspace that held it.
///
/// The other half of [`name_after_task`], and the reason it is needed: a claim
/// writes the task's title over both, and until now nothing ever wrote it back.
/// An agent that handed its work in went on reading as that work — in the
/// sidebar, in the panel's rows, in `workspace.list` — so the one place you look
/// to find somebody free said the opposite.
///
/// Only what the claim wrote — see [`named_after_task`] for what that comes to.
/// A name without the task's scope on it is one somebody typed since, and
/// blanking that would be this function deciding something it was never asked
/// to.
///
/// The pane takes a word and the workspace takes nothing, because the two are
/// asked different questions. A pane is a person, and "unassigned" is the
/// answer the agents panel exists to give; a workspace is a room, and herdr
/// names an unnamed one after what is standing in it, which is true again the
/// moment the task stops being.
///
/// **Unless the pane is a governor's**, which is the one agent for which
/// "unassigned" is not merely unhelpful but false. Ed, 2026-08-17, looking at
/// this exact label after a custodian released the task it had borrowed: a
/// custodian holding no task is the most assigned thing on the panel — it holds
/// a project. The word above is right for a *worker* that has put its task
/// down, and the question is which of the two this pane is, so it is asked here
/// rather than answered once for both.
///
/// The workspace waits on the last binding in it. Two agents in one tree are
/// rare and one of them finishing is not a reason to unname the room they are
/// both standing in.
pub(crate) fn unname_after_task(store: &Store, pane: &str, task_id: &str) {
    if !herdr::available() {
        return;
    }
    let Some(task) = store.task(task_id) else { return };
    let Ok(panes) = herdr::panes() else { return };
    let Some(p) = panes.iter().find(|p| p.pane_id == pane) else { return };

    if named_after_task(&p.label, &task) {
        let governs = cmd_govern::governs(&store.governors(), &p.workspace_id);
        let _ = match &governs {
            Some(project) => herdr::rename_pane(pane, &cmd_govern::governor_of(project)),
            None => herdr::rename_pane(pane, UNASSIGNED_LABEL),
        };
    }
    if p.workspace_id.is_empty() {
        return;
    }
    let still_bound = store.bindings().keys().any(|other| {
        other != pane
            && panes.iter().any(|q| q.pane_id == *other && q.workspace_id == p.workspace_id)
    });
    if still_bound {
        return;
    }
    if herdr::workspaces()
        .unwrap_or_default()
        .iter()
        .any(|w| w.id == p.workspace_id && named_after_task(&w.label, &task))
    {
        let _ = herdr::rename_workspace(&p.workspace_id, "");
    }
}

/// `wsp flag <id> "why"` — an agent raises a hand about one task.
///
/// The thing an agent cannot do from inside its own pane is point. It can write
/// the task up, it can claim it, it can block it with a question on it — and all
/// of that lands somewhere a person has to go and look. What it could not do
/// was say *this row, now*, which is the difference between a backlog somebody
/// reads at the end of the day and a question that gets answered.
///
/// So a flag is one sentence about one task, and the panel is where it arrives.
/// A panel is already installed in every workspace and they all read one shared
/// view, so nothing here has to find a window or know which screen is in front
/// of anybody: it writes the flag, and the panel the person is looking at draws
/// it inside a tick, pinned at the foot where it cannot be scrolled away from.
///
/// Two things an agent wants and one primitive. "Can I take this?" is a flag
/// with a question in the sentence; "this exists and you should look at it" is
/// a flag with or without one. Both are a hand raised about a single row, and
/// the answer to both is a person putting their eyes on it — which is the one
/// thing the panel is good at and the one thing a sentence in a transcript
/// nobody is reading is not.
///
/// State, not store. A flag is true while somebody is at the machine and
/// meaningless a week later, so it sits beside the claims and the pins rather
/// than in the task file: no commit, nothing in the history of the work, and
/// nothing left behind when it is answered. `wsp note` is the verb for the half
/// of it that is worth keeping.
///
/// # A raised hand is a message, and it is no longer keyed by the task
///
/// `worklist-017`, and the answer is that the defect **dissolves** rather than
/// being repaired. Until this, a flag was a row in `flags.json` keyed by the
/// task it was about — so `Store::set_flag` inserted by task id, and an agent
/// raising a second hand on the task it was working **overwrote the first
/// without saying so**. A seat that did not happen to notice would have lost
/// the first raised hand and been told nothing, which is this codebase's house
/// fault in its sharpest form: the near side reporting success on its own
/// records while the far side lost the thing. `wsp flag` returned cleanly. The
/// agent believed two hands were up. One was. And the thing lost was *the
/// request for attention itself* — the mechanism whose entire job is to not be
/// lost.
///
/// The question that had to be answered before touching `set_flag` was whether
/// a flag is still keyed by a task at all, and it is not. [`crate::message`]
/// landed with ids of its own, deliberately outside the task id space, and a
/// flag is one of its three shapes. So a raised hand is a [`Message`]: keyed by
/// its own id, so **two are two**, with nothing left to overwrite. Repairing
/// `set_flag` — refusing the second, or keying it by task *and* raiser — would
/// have been a second keying scheme standing beside the record's, which is one
/// concept too many for a fault that a record wsp already has does not have.
///
/// What each field became, so the mapping is readable in one place:
///
/// | was | is |
/// |---|---|
/// | the task it was keyed under | [`About::Task`] — the *subject*, from which the address is derived |
/// | `said` / `--title` / `--body` | one `text`, [`message::compose`]d, headline first |
/// | `pane` + `workspace` | [`Party::Pane`] — the sender, structured |
/// | `ask: claim` | [`Ask::Claim`], **and** [`Shape::Question`] |
/// | `at`, `seen` | unchanged, and they mean the same thing |
///
/// Two things fall out that were not asked for and are worth naming. A flag
/// with `--ask` is now a **question**, so [`Shape::may`] refuses to let it be
/// cleared: `worklist-004`'s failure, where a seat answered by another route
/// and then lowered the hand so that *clearing looked like answering*, is now
/// a refusal rather than a convention. And `--body` with no `--title` can no
/// longer draw as nothing (`worklist-018`), because [`message::compose`] makes
/// the first line of `text` non-empty whenever anything at all was given.
///
/// The event is the seam for anything louder. `~/wsp/hooks/on-message-raised`
/// gets the JSON on stdin — the id, the shape, the sender, the subject and the
/// headline — so a desktop notification is an executable file. It replaces
/// `on-task-flagged`, which named the record rather than the act and could only
/// ever have fired for one of the three shapes.
pub fn flag(store: &Store, args: &Args) -> i32 {
    let p = Paint::new();
    let clearing = args.has("clear");
    let needle = args.rest.first().cloned();

    // Before anything reads or writes: any hand the *installed* binary raised
    // into `flags.json` is carried across into the record. The installed binary
    // is routinely not the tree, and a hand that stopped being drawn the moment
    // this landed would be `worklist-017` committed by the change that fixes
    // it. Once, then never again — see [`message::adopt_legacy_flags`].
    let carried = message::adopt_legacy_flags(store);
    if !carried.is_empty() {
        eprintln!(
            "{}",
            p.dim(&format!(
                "· carried {} raised hand{} across from flags.json into the message record",
                carried.len(),
                if carried.len() == 1 { "" } else { "s" }
            ))
        );
    }

    // No id and nothing to clear: what is raised. The question a person asks
    // from a shell is the same one the panel answers by drawing the section,
    // and it should not need a second verb.
    let Some(needle) = needle else {
        if clearing {
            eprintln!("usage: wsp flag --clear <id>");
            return 2;
        }
        return list_flags(store, args);
    };

    // `--seen` and `--clear` act on a *raised hand*, and a hand is now named by
    // an id of its own rather than by the task it is about — so they resolve
    // the needle against the record and not against the task table. Both still
    // take a task id, because that is what every surface has been passing them
    // and because it is what a person says; what they no longer do is guess
    // when it names more than one.
    if args.has("seen") || clearing {
        return dispose(store, args, &needle, clearing);
    }

    let task = match store.task_or_why(&needle) {
        Ok(t) => t,
        Err(why) => {
            eprintln!("{why}");
            return 1;
        }
    };

    // The sentence is optional on purpose: "look at this task" is a complete
    // thing to say, and a verb that refused without a reason would be a verb
    // nobody reached for in the case that matters most — a task somebody has
    // to see the existence of.
    // A lone `-` is not part of the sentence, it is the conventional name for
    // stdin — the same spelling `wsp edit <id> --overview -` uses. Without this
    // it read as a word and the receipt printed the dash on the end of what the
    // agent said, which is the sort of thing nobody reports and everybody sees.
    let from_stdin = args.rest.iter().skip(1).any(|a| a == "-");
    let said = args
        .rest
        .iter()
        .skip(1)
        .filter(|a| *a != "-")
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    let said = said.trim();
    // The sentence is a row on somebody's panel and stays one line, so it has
    // no `-` form — `--body -` below is where a paragraph goes. It gets the
    // check anyway: see [`crate::util::terminal_output`].
    if let Some(why) = util::terminal_output(said) {
        eprintln!("wsp: {why}");
        eprintln!("     `wsp flag {} --body -` reads a paragraph from a stream", task.id);
        return 2;
    }
    let env = herdr::Env::read();
    let pane = args.get("pane").or(env.pane_id).unwrap_or_default();
    let workspace = env.workspace_id.unwrap_or_default();

    // The card's three parts, and each is optional because the cheap version of
    // this has to stay cheap: `wsp flag <id>` on its own still works and the
    // card fills itself in from the task. A title where the task's own is
    // wrong for the moment — "the store is corrupt", on a task called something
    // else — a body for the paragraph a row cannot hold, and an ask for the one
    // thing a keypress can answer.
    let title = args.get("title").unwrap_or_default();
    // Where the paragraph comes from, and there are three spellings of it
    // because `worklist-036` is what having only two cost.
    //
    // `--from <path>` is what `note`, `block`, `park` and `decide` all take,
    // and **the house rule printed in every brief is to pass prose through a
    // file** — a shell evaluates backticks inside double quotes, so the one
    // habit the fleet is told to form was the habit that lost the message on
    // this verb. `flag` had `--body -` and no `--from`; the parser bound the
    // path to an option nothing read, and the hand went up with `"text": ""`
    // and a success receipt under it. `robustness-088`'s named failure — *a
    // panel full of flags nobody reads* — arriving through the verb built to
    // prevent it, and arriving silently, which is the half that matters
    // unattended: nobody reads the receipt at three in the morning.
    //
    // Two sources for one paragraph is refused rather than resolved. Picking
    // one and dropping the other is the same fault in a smaller hat, and there
    // is no reading of `--body X --from Y` where the caller knew what they were
    // asking for.
    if args.has("body") && args.has("from") {
        eprintln!("wsp: --body and --from are two sources for one paragraph — give one");
        return 2;
    }
    // A lone `--from`, or `--from -`, is stdin: a dash is not a value, it is
    // the conventional name for the stream, and `--body -` reaches the parser
    // as `true` the same way. `prose_source` reads it identically.
    let source = match (args.get("from"), args.get("body")) {
        (Some(path), _) => Some(match path.as_str() {
            "true" => "-".to_string(),
            other => other.to_string(),
        }),
        (None, Some(v)) if v == "true" || v == "-" => Some("-".to_string()),
        (None, None) if from_stdin => Some("-".to_string()),
        _ => None,
    };
    let body = match &source {
        Some(src) => match crate::cmd_task::read_source(src) {
            Ok(text) => text,
            Err(e) => {
                match src.as_str() {
                    "-" => eprintln!("wsp: cannot read stdin: {e}"),
                    path => eprintln!("wsp: cannot read {path}: {e}"),
                }
                return 1;
            }
        },
        None => args.get("body").unwrap_or_default(),
    };
    // A source that was given and came back empty is the same silence again by
    // another road: the file was the wrong path's worth of nothing, or the pipe
    // was, and the caller believes a paragraph went up. `wsp flag <id>` on its
    // own stays legal — *look at this task* is a complete thing to say — but a
    // caller that pointed at words and got none is told so instead of raising
    // an empty hand and exiting 0.
    if let Some(src) = &source {
        if body.trim().is_empty() && said.is_empty() && title.trim().is_empty() {
            match src.as_str() {
                "-" => eprintln!("wsp: nothing came in on stdin — the hand would go up empty"),
                path => eprintln!("wsp: {path} is empty — the hand would go up empty"),
            }
            return 2;
        }
    }
    let ask = args.get("ask").unwrap_or_default();
    // A closed vocabulary, because the answer is a key on somebody's panel and
    // that key runs a command. An agent naming its own argv would be an agent
    // choosing what a keystroke does — so it names a *question* the panel
    // already knows how to answer, and the panel decides what answering means.
    if !ask.is_empty() && ask != "claim" {
        eprintln!("wsp: `--ask {ask}` is not a question the panel can answer. Try: --ask claim");
        return 2;
    }
    // Nothing to hand it to. A claim needs a pane to claim *into*, and asking
    // from outside herdr would draw a `y` that could not do anything.
    if ask == "claim" && pane.is_empty() {
        eprintln!("wsp: --ask claim needs a pane to hand it to — run it inside a herdr pane");
        return 2;
    }

    // The three inputs become one text, headline first — see
    // [`message::compose`], which is where the invariant that keeps
    // `worklist-018` shut lives.
    let mut raised = Message::new(
        match pane.is_empty() {
            // No pane is a person at a shell, and that is the correct reading:
            // the one channel wsp does not control is a human keyboard, so
            // unattributed is a person rather than an unknown.
            true => Party::Human,
            false => Party::pane(&pane, &workspace),
        },
        // `wsp-095` Part 4: every verb's default is the quietest kind that verb
        // can honestly be, and `wsp flag` is `note`. A raised hand is read at
        // the reader's next turn boundary; nothing but `stop` interrupts.
        Kind::Note,
        &message::compose(said, &title, &body),
    );
    raised.about = About::Task(task.id.clone());
    if ask == "claim" {
        raised.set_ask(Ask::Claim);
        // An ask is a **question**, and the shape is what makes it one:
        // something is now waiting, so it may be answered or abandoned and may
        // never merely be cleared. A bare flag stays a notification, because a
        // question commits somebody to answering it and starts a clock, and
        // that is not a thing to acquire by accident.
        raised.set_shape(Shape::Question);
        // Who is sitting still. The pane, and the task that pane is holding —
        // *not* the task being asked about — because an answer goes home to
        // the asker and `message::replies_for` finds it by the asker's task.
        // An agent asking to take something usually holds nothing, and the
        // subject is then the only task there is to name; `homes_to` would
        // reach it either way, but `replies_for` would not, and an answer its
        // asker cannot find is the return path failing quietly.
        let mine = store
            .bindings()
            .get(&pane)
            .and_then(|b| b.get("task_id"))
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| task.id.clone());
        raised.waiting = Some(Waiting::new(&pane, &mine));
    }
    // The record refuses what it will not hold, and a verb whose whole effect
    // is on somebody else's screen has to say when nothing landed.
    if let Err(why) = message::raise(store, &raised) {
        eprintln!("wsp: {why}");
        return 1;
    }

    if args.json() {
        println!(
            "{}",
            json!({ "id": task.id, "flagged": true, "said": said, "pane": pane })
        );
    } else {
        // The rows the seat will see, and not a paraphrase of the arguments.
        // The receipt used to print the positional sentence, so an agent that
        // put its message in `--body` was shown an empty confirmation of a
        // message it had sent — the near side reporting success on its own
        // records. See [`hand_rows`].
        println!("{} {}  {}", p.red(glyph_flag()), p.bold(&task.id), task.title);
        for l in hand_rows(&raised, &p) {
            println!("{l}");
        }
        if ask == "claim" {
            println!("  {}", p.dim("asking to take it · y there hands it over"));
        }
        // Say where it went. A command whose whole effect is on somebody else's
        // screen has to name the screen, or it reads as having done nothing.
        println!("  {}", p.dim(&addressed(store, &task)));
    }
    0
}

/// `--seen` and `--clear`: the two things you can do to a raised hand without
/// answering it.
///
/// # Why this takes an id and may refuse
///
/// `worklist-017` from the reading side. Under `flags.json` a task named at
/// most one raised hand, so `wsp flag --clear <task>` was unambiguous by
/// construction — and it was unambiguous because the store was *losing* the
/// second hand, which is the fault this whole change is about. With the hands
/// kept, a task can name several, and a verb that quietly acted on one of them
/// would be the same fault wearing the other hat: an act on somebody's raised
/// hand that nobody was told about.
///
/// So a needle resolves to [`Hand`] and the ambiguous case comes back with the
/// hands listed. That is the cheapest of the three shapes this task's overview
/// offered — *one per task, but say so* — landing at the one place where the
/// ambiguity is real rather than at the store, where it would have been a rule
/// against holding two.
///
/// # What `--clear` means now, and the one thing it may not do
///
/// It **acknowledges**: *I have this and I am not passing it on*. That is a
/// real act rather than a dismissal, it is what makes the chain auditable, and
/// it costs the same one keystroke `x` always did.
///
/// It may not close a question. `Shape::may` refuses it, and the refusal is
/// `worklist-004`: a seat answered a flagged agent's question down a different
/// channel and then lowered the hand, so **clearing looked like answering** —
/// to every surface in the system the matter was closed while the asker sat
/// waiting. A question ends by being answered or abandoned, both of which take
/// a sentence and both of which reach the asker.
///
/// The record is then forgotten, and the two halves are deliberate: the
/// *disposition* is an event in `events.jsonl`, which is the history and is
/// append-only, and `messages.json` holds what is still standing. Keeping every
/// acknowledged hand in the standing set would grow a file that twenty-two
/// panels stat on every tick, to say something the log already says better.
fn dispose(store: &Store, args: &Args, needle: &str, clearing: bool) -> i32 {
    let p = Paint::new();
    let one = match message::hand(store, needle) {
        Hand::One(m) => *m,
        Hand::Nothing => {
            eprintln!("wsp: nothing raised on {needle}");
            return 1;
        }
        // Not a guess and not a silent pick. The ids are printed because they
        // are what the caller needs to say next, and this is the only place
        // most people will ever see one.
        Hand::Several(up) => {
            eprintln!("wsp: {} hands are up on {needle} — name the one you mean:", up.len());
            for m in &up {
                eprintln!("     {}  {}", m.id, util::truncate(m.title(), 60));
            }
            return 2;
        }
    };
    let task = one.about.task().unwrap_or(needle).to_string();

    // Read, and still up. The card is the interruption and the row is the
    // reminder, so putting the card away has to be a different act from taking
    // the hand down — `esc` says "not now", `x` says "dealt with", and a panel
    // that could only do the second would train you to clear things you had not
    // answered. Written into the record rather than kept per panel, because
    // there are twenty-two panels and the card would otherwise pop again on the
    // next one you switched to.
    if !clearing {
        if let Err(why) = message::see(store, &one.id) {
            eprintln!("wsp: {why}");
            return 1;
        }
        if args.json() {
            println!("{}", json!({ "id": one.id, "task": task, "seen": true }));
        } else {
            println!("{} {}  {}", p.dim("·"), p.bold(&task), p.dim("seen · still raised"));
        }
        return 0;
    }

    let env = herdr::Env::read();
    let by = match env.pane_id {
        Some(pane) if !pane.is_empty() => {
            Party::pane(&pane, &env.workspace_id.unwrap_or_default())
        }
        _ => Party::Human,
    };
    if let Err(why) = message::acknowledge(store, &one.id, &by) {
        eprintln!("wsp: {why}");
        // The sentence `Refused::WrongShape` prints is true and short; this is
        // the one that says what to do instead, and a person who has just been
        // stopped from taking a hand down is exactly who needs it.
        if one.shape() == Some(Shape::Question) {
            eprintln!("     {}", message::Refused::StillOpen);
            eprintln!("     wsp answer {} \"…\"  ·  wsp answer {} --abandon \"…\"", one.id, one.id);
        }
        return 1;
    }
    let _ = store.forget_message(&one.id);
    if args.json() {
        println!("{}", json!({ "id": one.id, "task": task, "flagged": false, "was": true }));
    } else {
        println!("{} {}  {}", p.dim("·"), p.bold(&task), p.dim("lowered"));
    }
    0
}

/// The most of one hand's detail a listing prints before it says how much is
/// left.
///
/// A guard against a pasted build log, not a decision about how much to show —
/// and the two are different. The card's budget is a *decision*: a knock on the
/// door is read over somebody else's tree and the rest is a keypress away. A
/// shell listing has nowhere else to send you, so it prints the message, and
/// this is only the wall a paste hits. Wide enough that no hand anybody writes
/// reaches it.
const HAND_LINES: usize = 40;

/// One raised hand, drawn the way it will be read.
///
/// **The receipt and the listing share this on purpose.** `worklist-018`: an
/// agent raised a hand carrying a paragraph that said *landed and NOT
/// INSTALLED, the retry hazard is live*; `wsp flag` printed a success receipt
/// with none of it in it, and the seat's listing drew the task's own title, a
/// pane and an age. Both ends reported a message that neither of them had
/// shown to anybody, and it was read four hours later out of `flags.json` by
/// hand. An agent handed back the rows its own message produced cannot put a
/// paragraph into an empty row and be told it worked — which is the same
/// repair `cmd_install::health` and `delivered` are, arriving at the one verb
/// whose entire effect is on somebody else's screen.
///
/// One text and no second field to prefer wrongly: the first line is the
/// headline and the rest is drawn under it. [`message::compose`] is what makes
/// the first line non-empty whenever anything at all was given.
fn hand_rows(m: &Message, p: &Paint) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if !m.title().is_empty() {
        out.push(format!("  {}", m.title()));
    }
    let rest: Vec<&str> = m.body().lines().collect();
    for l in rest.iter().take(HAND_LINES) {
        out.push(format!("    {}", p.dim(l)));
    }
    if rest.len() > HAND_LINES {
        let n = rest.len() - HAND_LINES;
        out.push(format!("    {}", p.dim(&format!("… {n} more line(s) · wsp flag --json has them"))));
    }
    out
}

/// The line under a hand: who raised it, how long ago, what a keypress would
/// answer, and whose it is.
///
/// **The ask is on it because it was nowhere in this listing.** The card said
/// *asks to take it* and `wsp watch` said *asking to claim*, and a seat reading
/// its own inbox — which is the surface a governor actually uses — could not
/// tell a question from a notice. That is `worklist-018`'s second owed check
/// and it failed: `--ask claim` raised with no sentence drew a task title, a
/// pane and an age, and nothing that said a keypress would answer it.
fn hand_aside(m: &Message, seat: Option<&cmd_govern::Seat>, held_here: bool) -> String {
    let mut out: Vec<String> = Vec::new();
    let who = m.from.byline();
    if !who.is_empty() {
        out.push(who);
    }
    let held = util::since(&m.at);
    if held > 0 {
        out.push(util::duration_human(held));
    }
    if m.ask() == Some(Ask::Claim) {
        out.push("asks to take it".into());
    }
    // Nothing when there is no seat above it, which is the ordinary case and
    // already reads as "this is yours" to the person looking at it.
    if let Some(s) = seat {
        out.push(match held_here {
            true => format!("{} yours · {}", glyph_seat(), s.scope),
            false => format!("{} {} · {}", glyph_seat(), cmd_govern::governor_of(&s.scope), s.workspace),
        });
    }
    out.join(" · ")
}

/// Who a raised hand is addressed to, in one line of receipt.
///
/// Addressing, not delivery. Nothing is pushed at the seat: the flag is written
/// where it always was and every panel still draws it, and what this adds is
/// the sentence saying whose it is. Pushing would mean prompting a live agent
/// mid-task — a whole context re-read per interruption, into a session that is
/// holding the thread on purpose — to save it a poll it was going to make
/// anyway. That is the gate this is not.
///
/// Derived rather than stored, so nothing goes stale. A seat taken after the
/// hand went up still answers for it, and a seat that stands down hands its
/// flags to whoever is above it without touching a single flag record.
fn addressed(store: &Store, task: &Task) -> String {
    let index = Index::new(store.projects());
    // The list first, and this is the whole of what a worklist changes about a
    // raised hand: a member of tonight's run is answered for by whoever is
    // running it, not by whoever governs the project it happens to live in.
    let lists = crate::worklist::Running::read(store);
    match cmd_govern::seat_for(
        &store.governors(),
        &index,
        lists.list_of(&task.id),
        task.project.as_deref(),
    ) {
        Some(s) => format!("raised to the {} governor · {} · x there lowers it", s.scope, s.workspace),
        None => "raised on every panel · x there lowers it".into(),
    }
}

/// The panel's mark, borrowed for the receipt so the two agree about what a
/// raised hand looks like.
fn glyph_flag() -> &'static str {
    crate::panel::glyph::FLAG
}

/// And the panel's mark for a governor's slot, borrowed for the same reason.
/// The words come from [`cmd_govern::governor_of`], which carries no mark of its
/// own — a label goes to herdr, which has no column to put one in.
fn glyph_seat() -> &'static str {
    crate::panel::glyph::SEAT
}

/// `wsp flag` with nothing to raise: what is already up.
///
/// Reads [`message::raised`] rather than `flags.json`, which is the whole of
/// `worklist-017` arriving at a listing: two hands on one task are two rows,
/// where before the second had already replaced the first by the time anything
/// could draw either.
///
/// Replies are not in it. An answer is addressed to whoever asked and already
/// has two deliveries of its own; see [`Message::is_reply`].
fn list_flags(store: &Store, args: &Args) -> i32 {
    // Newest first: a flag is an interruption, and the one raised while you
    // were reading the last one is the one you have not seen. The record is
    // kept oldest-first, because which end of a queue an interruption is read
    // from is a decision about a surface.
    let rows: Vec<Message> =
        message::raised(store).into_iter().filter(|m| !m.is_reply()).rev().collect();
    if args.json() {
        let out: Vec<serde_json::Value> = rows.iter().map(Message::to_json).collect();
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        return 0;
    }
    let p = Paint::new();
    if rows.is_empty() {
        println!("{}", p.dim("nothing raised"));
        return 0;
    }

    // Whose hands these are. The order stays newest-first — an interruption is
    // read in the order it arrived, and re-sorting by addressee would bury the
    // one raised while you were reading the last — so the seat is a mark on the
    // row instead. `--seat` is for the agent that only wants its own.
    let index = Index::new(store.projects());
    let governors = store.governors();
    let mine = herdr::Env::read()
        .workspace_id
        .and_then(|ws| cmd_govern::governs(&governors, &ws));
    let only_mine = args.has("seat");
    // Asked for an inbox from a pane that has no seat. Said plainly, because
    // "nothing raised for you" and "you are not the seat" look identical from
    // an empty list, and only one of them is worth acting on.
    if only_mine && mine.is_none() {
        println!("{}", p.dim("this workspace is nobody's seat — wsp flag alone shows them all"));
        return 0;
    }
    let mut shown = 0;
    // Whether any of them is a question, which decides the last line.
    let mut asked = false;

    // Read once for the whole list rather than per row: the answer is the same
    // handful of files however many hands are up.
    let lists = crate::worklist::Running::read(store);
    for m in &rows {
        let id = m.about.task().unwrap_or_default();
        let task = store.task(id);
        let seat = task.as_ref().and_then(|t| {
            cmd_govern::seat_for(&governors, &index, lists.list_of(&t.id), t.project.as_deref())
        });
        let held_here = seat.as_ref().map(|s| mine.as_deref() == Some(s.scope.as_str())).unwrap_or(false);
        if only_mine && !held_here {
            continue;
        }
        shown += 1;
        asked |= m.shape() == Some(Shape::Question);
        // The subject. A hand about a task that has since been retired is
        // still a hand raised, and it says the subject is gone rather than
        // drawing `(no such task)` where a title goes — those two words in the
        // title slot made a retired subject and an empty message render
        // identically, which is how three hands on Ed's panel on 2026-08-19
        // read as blank rows carrying nothing.
        let title = match &task {
            Some(t) => t.title.clone(),
            None => p.dim("— the task is gone; the hand is still up").to_string(),
        };
        println!("{} {}  {}", p.red(glyph_flag()), p.bold(id), title);
        // What was said, in full. The seat has nowhere else to read it: the
        // panel has a card and this has nothing behind it, which is what made
        // `flags.json` the surface of last resort.
        for l in hand_rows(m, &p) {
            println!("{l}");
        }
        let aside = hand_aside(m, seat.as_ref(), held_here);
        if !aside.is_empty() {
            println!("  {}", p.dim(&aside));
        }
    }
    if shown == 0 {
        println!("{}", p.dim("nothing raised to this seat"));
        return 0;
    }
    println!("{}", p.dim("wsp flag --clear <id> lowers one"));
    // And the other verb, only when there is something it applies to. A
    // question may not be cleared — `Shape::may` refuses it, because clearing
    // is what looked like answering in `worklist-004` — so a listing that
    // offered only the refused verb would send a reader at the one door that
    // is shut.
    if asked {
        println!("{}", p.dim("wsp answer <id> \"…\" closes one and reaches whoever asked"));
    }
    0
}

/// `wsp tell <id> "…"` — say something to an agent without ending it.
///
/// **The verb that was missing, and the reason a stalled agent was expensive
/// rather than merely annoying.** Until this, the only thing that reached an
/// ordinary agent was a spawn's work order; `wsp govern --tell` reached a
/// governor and nothing reached anybody else. So the whole repertoire for an
/// agent that had stopped was `wsp despawn` and `wsp spawn` — which is not a
/// repair, it is a demolition. On 2026-08-18 that discarded hours of unrecorded
/// reading across seven agents whose conversations were entirely intact; the
/// turn had ended, and nothing else had.
///
/// So this is the cheap half of the answer to [`quiet_note`]: the census says
/// which agents are not turning, and this says a sentence to one of them. It is
/// the same delivery `spawn` uses for a work order and `govern --tell` uses for
/// direction — [`crate::agent_commands`] over [`crate::place::Place::tell`] —
/// because how a sentence reaches an agent is a fact about the agent's kind,
/// and there should not be three answers to it.
///
/// **No `/clear` in front of it**, unlike the hand-overs in `panel::verbs`. A
/// work order is given to an agent that has just finished something else and
/// wants an empty context; this is a word to one that is in the middle of a
/// piece of work and whose context is the thing worth saving. Emptying it would
/// destroy exactly what makes this cheaper than a respawn.
///
/// Takes the message on stdin as well as in argv, from the first line rather
/// than after somebody is bitten. `render-080` recorded what argv-only costs on
/// this specific payload: a message to an agent is long prose full of file and
/// verb names, which is precisely the text that wants backticks, and inside the
/// double quotes a shell needs for it every backtick is command substitution.
/// Three phrases *executed* instead of being delivered, and the message arrived
/// fluent with the load-bearing nouns missing — silent at the receiving end,
/// which is worse than an error.
pub fn tell(store: &Store, args: &Args) -> i32 {
    let p = Paint::new();
    let Some(needle) = args.rest.first().cloned() else {
        eprintln!("usage: wsp tell <id> \"…\"   (or `-` to read it from stdin)");
        return 2;
    };
    let text = match message(args) {
        Ok(t) => t,
        Err(code) => return code,
    };
    if !herdr::available() {
        eprintln!("wsp: no herdr socket — nothing to say it to");
        return 1;
    }

    // Not `peek_target`, and the difference is worth a line rather than the
    // reuse. That resolver's whole value is that it also finds the panel, the
    // view and the board, which are wsp's own panes and hold no agent — the
    // right answer for "show me what this looks like" and a hazard for a verb
    // that types prose into whatever it lands on. This resolves the two things
    // that can hold an agent and nothing else.
    let (seat, what) = match target(store, &needle) {
        Some(v) => v,
        None => {
            eprintln!("wsp: nothing holds `{needle}` — `wsp wip` says who holds what");
            return 1;
        }
    };

    let place = Herdr::new();
    let Some(pane) = herdr::panes().ok().and_then(|ps| ps.into_iter().find(|x| x.pane_id == seat)) else {
        eprintln!("wsp: herdr does not list {seat}");
        return 1;
    };
    if pane.agent.trim().is_empty() {
        eprintln!("wsp: no agent in {seat} — the pane is alive and empty. `wsp spawn {needle}` puts one in it");
        return 1;
    }
    // The one state a message must not be sent into, and the reason it is
    // checked here rather than left to the backend. A blocked agent has a
    // permission dialog holding the keyboard, so the text does not queue behind
    // anything — it is typed *at the dialog*, where a sentence about what to do
    // next can select an answer nobody chose. herdr cannot refuse this for us:
    // from its side the prompt was delivered and the pane took the keys.
    if matches!(crate::place_herdr::state_of_pane(&pane), State::Blocked) {
        eprintln!("wsp: {what} is stopped on a prompt only a person can answer — answer that first");
        eprintln!("     `wsp peek {needle}` shows what it is asking");
        return 1;
    }

    let how = crate::agent_commands::of(&pane.agent);
    let sent = Sent::new(&what, &what, &seat, &needle, &text, args);
    if let Some(ago) = sent.already_sent(store) {
        if !args.has("again") {
            return twice(&sent, ago, &p);
        }
    }
    delivered(store, how.tell(&place, &crate::place::Seat::new(&seat), &text), &sent)
}

/// One sentence on its way to one pane: everything both `tell` verbs need to
/// recognise it, report it and record it.
///
/// It exists because there are two of them. `wsp tell <task>` and `wsp govern
/// <scope> --tell` end at the same [`crate::agent_commands::Kind::tell`] and
/// used to disagree about everything after it: one wrote an event and the other
/// wrote none, one read [`Refusal::NotTaken`] as a rescue and the other as a
/// failure, and only one of them was ever right. A governor's seat is the
/// address that has no task behind it, so the verb a governor is *obliged* to
/// use was the one with less of the machinery.
pub struct Sent {
    /// The addressee as an id: a task, or the scope a seat governs. What the
    /// JSON and the event carry.
    pub target: String,
    /// The same addressee as a person would say it — `worklist-002`, or `the
    /// wsp seat` — which is what makes a refusal read as a sentence.
    pub what: String,
    pub pane: String,
    /// What to type after `wsp peek` when the sentence needs rescuing.
    pub peek: String,
    pub chars: usize,
    /// This exact sentence to this exact pane, as a short handle.
    ///
    /// **Derived from the message and not minted**, which is the whole of its
    /// value: a resend of the same paragraph to the same seat carries the same
    /// id, so a retry is recognisable as a repeat by anything that has seen the
    /// first — [`Sent::already_sent`] below, and a person reading two lines of
    /// output. An id from a counter would be unique per attempt and would
    /// answer a question nobody asked.
    pub id: String,
    pub json: bool,
}

/// How long a sentence is still the same sentence.
///
/// Two minutes, and both bounds are the recorded harm rather than taste.
/// worklist-010's retries came seconds after a delivery falsely reported as a
/// failure — that is the whole population this is for, and it is measured in
/// seconds. Anything much longer starts refusing the deliberate repeat: a
/// governor typing `continue` at an agent twice in an evening means it twice,
/// and a channel that silently swallowed the second would stall a night to
/// prevent a duplicate paragraph.
const SAME_BREATH: i64 = 120;

impl Sent {
    pub fn new(target: &str, what: &str, pane: &str, peek: &str, text: &str, args: &Args) -> Sent {
        Sent {
            target: target.into(),
            what: what.into(),
            pane: pane.into(),
            peek: peek.into(),
            chars: text.len(),
            id: message_id(pane, text),
            json: args.json(),
        }
    }

    /// This same sentence, to this same pane, within the last [`SAME_BREATH`].
    ///
    /// The idempotence half of worklist-010, and it is the half that addresses
    /// the *harm* rather than the report. A verb that says "was not told" about
    /// a message it delivered turns one paragraph into three, and the fix for
    /// the report — `place_herdr::tell`, which no longer lies — cannot undo a
    /// retry that has already been typed. This can: the second send of the same
    /// text is recognised and refused, whatever made somebody type it.
    ///
    /// Off the event log, which is where the first send wrote itself. That is
    /// the only durable record either verb has ever left, and it is why
    /// [`delivered`] writes one from both paths rather than from one.
    pub fn already_sent(&self, store: &Store) -> Option<i64> {
        let now = util::epoch_secs();
        store
            .events_of("agent-told")
            .iter()
            .rev()
            .filter(|d| d.get("id").and_then(Value::as_str) == Some(&self.id))
            .filter_map(|d| d.get("at").and_then(Value::as_i64))
            .map(|at| now - at)
            .find(|ago| *ago <= SAME_BREATH)
    }
}

/// A sentence's short handle: the pane and the text, and nothing that changes
/// between two attempts to send it.
///
/// FNV-1a, written out rather than reached for, because `DefaultHasher` is
/// SipHash with keys that Rust does not promise across versions — an id that
/// changed under a compiler upgrade would silently stop recognising repeats,
/// which is the one thing this is for. Sixteen hex characters of a 64-bit
/// digest is not a claim about adversaries; two sentences would have to collide
/// *and* arrive at the same pane within two minutes to be confused.
fn message_id(pane: &str, text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in pane.as_bytes().iter().chain(b"\0").chain(text.as_bytes()) {
        h ^= *b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    format!("m-{h:016x}")
}

/// Refuse a sentence that has just been sent to this pane, and say so.
///
/// `--again` is the escape and it is deliberately a word rather than a
/// timeout: the case where somebody means it twice exists, and the case where
/// they are retrying a failure that never happened is the one worth an extra
/// keystroke.
pub fn twice(sent: &Sent, ago: i64, p: &Paint) -> i32 {
    if sent.json {
        println!(
            "{}",
            json!({ "target": sent.target, "pane": sent.pane, "id": sent.id, "told": false, "already": ago })
        );
    } else {
        println!(
            "{} {}  {}",
            p.dim("·"),
            p.bold(&sent.id),
            p.dim(&format!(
                "the same sentence reached {} {} ago — `--again` sends it anyway",
                sent.what,
                util::duration_human(ago)
            )),
        );
    }
    0
}

/// What became of a sentence, said honestly, recorded once, from both verbs.
///
/// **The rule this enforces is worklist-010's and it is a constraint rather
/// than a preference: never report failure for a send that happened.** The old
/// path printed `the wsp seat was not told` on a herdr wait timing out, and the
/// seat it was addressed to had already read the message — so the governor
/// retried, and one paragraph was delivered three times. `delivered, no turn
/// seen` is a true sentence about the same event, and it is what
/// [`Delivery::Unconfirmed`] prints.
///
/// The event is the other half. `wsp tell` has always written `agent-told` and
/// `govern --tell` wrote nothing at all, so the channel a governor is obliged
/// to use was the one channel with no forensic trace — and worklist-010 was
/// investigated for a day out of terminal scrollback because of it. Both write
/// it now, with the message's id in it, which is what makes a repeat
/// recognisable at all.
pub fn delivered(store: &Store, outcome: crate::place::Result<Delivery>, sent: &Sent) -> i32 {
    let p = Paint::new();
    let Sent { target, what, pane, peek, chars, id, json } = sent;
    match outcome {
        Ok(how) => {
            // Recorded as an event and not on the task's log, deliberately. The
            // log is injected into every future spawn on that task, so a line
            // per sentence would be paid for by every session afterwards; this
            // is forensics — who said what to whom, when — and belongs where
            // the claims and the releases already are.
            store.log_event(
                "agent-told",
                json!({
                    "target": target, "pane": pane, "chars": chars, "id": id,
                    "at": util::epoch_secs(),
                    "turn": matches!(how, Delivery::Started),
                }),
            );
            if *json {
                println!(
                    "{}",
                    json!({ "target": target, "pane": pane, "id": id, "told": true,
                            "turn": matches!(how, Delivery::Started) })
                );
            } else {
                // The tail is on the unconfirmed one only, and it is not a
                // hedge: a sentence queued behind a turn has not been read yet,
                // which is a different thing to know at a barrier than a turn
                // that started on it.
                let tail = match how {
                    Delivery::Started => String::new(),
                    Delivery::Unconfirmed => format!(" · {}", p.dim("delivered, no turn seen")),
                };
                println!("{}{}", p.dim(&format!("→ {what} · {pane}")), tail);
            }
            0
        }
        // `NotTaken` is delivered-and-unmoved, which for this verb is the
        // interesting answer rather than an error: the sentence is sitting in
        // the composer unsent, and it is still there to be rescued. Named as
        // such, because "failed" would make somebody send it twice.
        Err(Refusal::NotTaken) => {
            eprintln!("wsp: {what} took the text and started nothing — it is sitting in the composer");
            eprintln!("     `wsp peek {peek}` shows it; a return in the pane sends it");
            1
        }
        Err(e) => {
            eprintln!("wsp: {what} was not told: {e}");
            1
        }
    }
}

/// The pane holding an agent that a needle names: a pane id, or a task.
///
/// A workspace is not accepted, and that is a refusal rather than an omission.
/// A workspace can hold several panes with agents in them, and picking one for
/// the caller is how a sentence meant for one agent reaches its neighbour —
/// `govern --tell` may do it because a governorship *is* the workspace's, and a
/// task is held by a pane.
fn target(store: &Store, needle: &str) -> Option<(String, String)> {
    if needle.contains(':') {
        return Some((needle.to_string(), format!("pane {needle}")));
    }
    let t = store.find_task(needle)?;
    let pane = store.panes_for_task(&t.id).into_iter().next()?;
    Some((pane, format!("{} — {}", t.id, util::truncate(&t.title, 44))))
}

/// The message: the rest of the line, or a stream.
///
/// Not folded to one line, which is where this parts company with `wsp note`.
/// That one folds because the `## Log` it writes to is read line-by-line by
/// everything downstream. This writes to a composer, and the paragraph breaks
/// in a brief are what make it readable by the thing that has to act on it.
fn message(args: &Args) -> Result<String, i32> {
    prose(args, 1)
}

/// The same sentence, for the verbs that are not `tell`.
///
/// `wsp ask` and `wsp answer` take prose after an id exactly as this does, and
/// they must take it *identically*: the `-` form, the empty check and the
/// backtick refusal are one behaviour, and a second copy of them is a second
/// set of edge cases. `render-080` is why the refusal exists at all — three
/// phrases were executed by the shell instead of delivered, and the message
/// arrived fluent with the load-bearing nouns missing.
pub(crate) fn prose(args: &Args, skip: usize) -> Result<String, i32> {
    let rest = args.rest.get(skip..).unwrap_or_default();
    if !matches!(rest, [one] if one == "-") {
        return typed_message(&args.text(skip));
    }
    piped_message()
}

/// A message given on the command line, checked for the one thing that says it
/// is not one. See [`crate::util::terminal_output`] — the payload here is prose
/// about the CLI going through a shell, which is the exact shape that comes
/// back carrying a command's output instead of the command's name.
pub(crate) fn typed_message(raw: &str) -> Result<String, i32> {
    let text = raw.trim();
    if text.is_empty() {
        eprintln!("wsp: nothing to say");
        return Err(2);
    }
    if let Some(why) = util::terminal_output(text) {
        eprintln!("wsp: {why}");
        eprintln!("     a backtick inside double quotes runs a command. `-` reads the message from a stream, where a shell never sees it");
        return Err(2);
    }
    Ok(text.to_string())
}

/// The message on stdin. Shared with `govern --tell -`, because the two verbs
/// are one act — a sentence typed into an agent's composer — and a stream form
/// that behaved differently on one of them is a stream form nobody trusts.
pub(crate) fn piped_message() -> Result<String, i32> {
    if util::stdin_is_tty() {
        // The same failure `wsp note` names: a command that stops and silently
        // swallows keys is worse than one that refuses.
        eprintln!("wsp: nothing is piped in — `-` reads the message from a stream");
        return Err(2);
    }
    match crate::cmd_task::read_source("-") {
        Ok(raw) if raw.trim().is_empty() => {
            eprintln!("wsp: nothing on stdin — nothing sent");
            Err(2)
        }
        Ok(raw) => Ok(raw.trim().to_string()),
        Err(e) => {
            eprintln!("wsp: cannot read stdin: {e}");
            Err(1)
        }
    }
}

/// `wsp say "<what you are doing>"` — an agent says where it has got to.
///
/// The pane takes the sentence; the workspace keeps the task. That division is
/// the whole design: a workspace answers *what is this work*, which changes
/// when the task changes, and a pane answers *what is happening in there right
/// now*, which changes all the time. Putting both on the workspace would mean
/// the sidebar losing the name of the work every time somebody started a build.
///
/// `--clear`, or an empty sentence, puts the task's own name back — so there is
/// always a way home that does not need the agent to remember what it was
/// called. A claim resets it too, which is why `claim` names the pane as well
/// as the workspace rather than leaving it to this.
pub fn say(store: &Store, args: &Args) -> i32 {
    let Some(pane) = pane_id(args) else {
        eprintln!("wsp: no pane to name — run this inside a herdr pane, or pass --pane");
        return 2;
    };
    if !herdr::available() {
        eprintln!("wsp: no herdr socket at {}", herdr::socket_path().display());
        return 1;
    }

    let said = args.text(0);
    let said = said.trim();
    // Short by nature and so no `-` form: this is a status line in a sidebar,
    // and one that needed a stream would be one nobody ran. It gets the check
    // anyway, and here it is a rendering question rather than a record one —
    // an escape sequence reaching a pane label is drawn, not stored.
    if let Some(why) = util::terminal_output(said) {
        eprintln!("wsp: {why}");
        return 2;
    }

    // Home is the task this pane holds. Without one there is nothing to fall
    // back to, so the label is cleared outright and herdr goes back to naming
    // the pane whatever it named it before.
    let held = store
        .bindings()
        .get(&pane)
        .and_then(|b| b.get("task_id"))
        .and_then(|t| t.as_str())
        .and_then(|id| store.task(id));

    // A sentence said by a pane holding work is scoped to that work, so the
    // rail still says which of the agents this is while it talks. A pane
    // holding none has nothing to scope it by, and says only what it said.
    //
    // Built whole, and cut once below. The whole one is the sentence an agent
    // typed, which is the string this verb exists to publish and the only one
    // in the system with no other home — the task's title is in the store and
    // this is not.
    let full = match (said.is_empty() || args.has("clear"), &held) {
        (true, Some(t)) => task_full(t),
        (true, None) => None,
        (false, Some(t)) => said_full(t, said),
        (false, None) => plain_full(said),
    };
    let label = full.as_deref().map(on_the_wire);

    let r = match &label {
        Some(l) => herdr::rename_pane(&pane, l),
        None => herdr::call("pane.rename", json!({ "pane_id": pane, "label": null })).map(|_| ()),
    };
    if let Err(e) = r {
        eprintln!("wsp: {e}");
        return 1;
    }

    // Kept only once herdr has taken the name, and only when there is more of
    // it than went over: what is stored is a statement about the label the pane
    // is wearing, and a rename that failed leaves it wearing the old one.
    // `set_said` keeps nothing when the two agree, so `--clear` on a short
    // title and a pane whose label went away both end with no entry, which is
    // the state that means "what is on the wire is the whole of it".
    match (&label, &full) {
        (Some(l), Some(f)) => store.set_said(&pane, l, f),
        _ => {
            store.clear_said(&pane);
        }
    }

    if args.json() {
        println!("{}", json!({ "pane": pane, "label": label }));
    } else {
        let p = Paint::new();
        match &label {
            Some(l) => println!("{} {}", p.dim(&format!("{pane} ·")), l),
            None => println!("{}", p.dim(&format!("{pane} · cleared"))),
        }
    }
    0
}

/// What a pane wears while its agent is looking for a task.
///
/// State first, project second, because the ellipsis falls on the right: a
/// herdr sidebar draws 26 columns, and `looking for work in stra…` still says
/// the thing a person scanning a column of panes is scanning for. The project
/// is the half they can usually read off the workspace row anyway.
fn looking_label(project: Option<&str>, found: bool) -> String {
    match (found, project) {
        (true, Some(p)) => format!("looking for work in {p}"),
        (true, None) => "looking for work".into(),
        (false, Some(p)) => format!("nothing actionable in {p}"),
        (false, None) => "nothing actionable".into(),
    }
}

/// Say, on the pane, that this agent has nothing in hand and is looking.
///
/// The gap this fills is the one between being sent to find work and having
/// found it. `claim` names the pane after the task, so everything from the
/// claim onwards is visible from outside — but the minute or two before it, in
/// which an agent reads a backlog, writes an overview and decides, showed
/// whatever the pane was called last. From herdr that is indistinguishable
/// from an agent that read its instruction and did nothing.
///
/// Two conditions, and both are about the caller rather than the answer:
///
/// *An agent.* A person running `wsp next` in a shell is asking a question,
/// not reporting a state, and their pane is not ours to rename.
///
/// *Holding nothing.* A bound pane running `next` is peeking at what comes
/// after the thing it is in the middle of — and `next` deliberately keeps that
/// pane's own `doing` task in the running, so the answer is often the task it
/// already has. Renaming there would replace a true name with a false one. It
/// is also why this needs no separate hook for the panel's `f`: `find_work`
/// refuses on an agent that still holds a task, so an agent sent looking is
/// unbound by the time it asks.
///
/// Nothing here is worth failing a command over, so every failure is silent —
/// the same bargain `claim`'s rename makes.
pub fn say_looking(store: &Store, panes: &[herdr::Pane], project: Option<&str>, found: bool) {
    if !herdr::available() {
        return;
    }
    let Some(pane) = herdr::Env::read().pane_id else { return };
    let driven = panes.iter().any(|p| p.pane_id == pane && !p.agent.is_empty());
    if !driven {
        return;
    }
    let holds = store
        .bindings()
        .get(&pane)
        .and_then(|b| b.get("task_id"))
        .and_then(|t| t.as_str())
        .is_some_and(|id| store.task(id).is_some());
    if holds {
        return;
    }
    let _ = herdr::rename_pane(&pane, &looking_label(project, found));
}

/// `Trance Video · 3h12m` — a claim as one line.
///
/// Both this and `worked_line` join what they have and skip what they do not,
/// because every part is optional: a claim made outside herdr has no label, and
/// one made before the clock was recorded has no duration. Formatting them with
/// fixed separators left `" · 3s · to prio1-001"` hanging off nothing.
pub fn claim_line(c: &serde_json::Value) -> String {
    let get = |k: &str| c.get(k).and_then(|x| x.as_str()).unwrap_or("");
    let mut parts: Vec<String> = Vec::new();
    match (get("workspace_label"), get("workspace_id")) {
        ("", "") => {}
        ("", id) => parts.push(id.to_string()),
        (label, "") => parts.push(label.to_string()),
        (label, id) => parts.push(format!("{label} ({id})")),
    }
    let held = util::since(get("claimed_at"));
    if held > 0 {
        parts.push(util::duration_human(held));
    }
    parts.join(" · ")
}

/// `Trance Video · 3h12m · to render-004` — the claim that ended.
pub fn worked_line(w: &serde_json::Value) -> String {
    let get = |k: &str| w.get(k).and_then(|x| x.as_str()).unwrap_or("");
    let mut parts: Vec<String> = Vec::new();
    match (get("workspace_label"), get("workspace_id")) {
        ("", id) if !id.is_empty() => parts.push(id.to_string()),
        (label, _) if !label.is_empty() => parts.push(label.to_string()),
        _ => {}
    }
    let spent = w.get("seconds").and_then(|x| x.as_i64()).unwrap_or(0);
    if spent > 0 {
        parts.push(util::duration_human(spent));
    }
    match get("handed_to") {
        "" => parts.push(get("reason").to_string()),
        next => parts.push(format!("to {next}")),
    }
    parts.join(" · ")
}

/// The tier a claim was made at, as a clause on the line that records the
/// claim — and nothing at all when none was stated.
///
/// **A clause and not a line of its own**, which is the whole reason it is
/// shaped like this. `wsp brief` hands an arriving agent the last four log
/// lines and nothing more; a record that spent one of those four on the tier
/// would push whatever direction was written on the task out of the brief, on
/// every spawn, for ever. The claim line was already the one that says an
/// attempt started, so this is the same sentence finishing itself.
///
/// **`default` is a word here and an absence in the flag**, and the asymmetry
/// is deliberate. `--effort high` with no `--model` is a real thing to type,
/// so the pair has to be printable with one half missing; `default/high` says
/// *whatever the settings file says, at high effort*, which is precisely what
/// was asked for and reads back unambiguously. What it does **not** do is
/// name the model — because at the moment of the claim wsp does not know it,
/// and the record that does is [`crate::agent_commands::Ran`], read off the
/// transcript when the claim ends.
///
/// Empty for a spawn that stated neither, so the ordinary claim line is
/// byte-for-byte what it was — the same compatibility rule `--model` itself
/// keeps, and the reason nothing in the store needs migrating.
fn spawned_at(model: Option<&str>, effort: Option<&str>) -> String {
    match (model, effort) {
        (None, None) => String::new(),
        (m, e) => format!(
            " · spawned at {}/{}",
            m.unwrap_or("default"),
            e.unwrap_or("default")
        ),
    }
}

/// What served the attempt that is ending, as a clause on the line that ends
/// it — and nothing at all when it cannot be read.
///
/// This is the field wsp-060 was written for, and the argument for reading it
/// here rather than at report time is that **evidence has to be durable and a
/// transcript is not**. The task file is committed to git and reaches every
/// machine; `~/.claude/projects` is one machine's disk, is cleaned up on its
/// own schedule, and holds nothing at all for an agent that ran on the other
/// box. A report that read the transcript live would answer differently next
/// month, which is the one thing calibration data must not do. So the reading
/// happens once, at the moment the attempt closes, and what lands on the task
/// is the answer rather than the pointer to it.
///
/// Silent on every failure, and that is the rule rather than laziness: the
/// task said in as many words that a silently wrong label is worse than a
/// missing one. A transcript that cannot be found, a kind that keeps none, an
/// agent killed before it answered — each of those is *no clause*, and a
/// reader that sees a release line with nothing after it knows exactly that
/// much. There is no default to fall back on that would not be a guess.
///
/// It must never cost a release. Nothing here is fatal, nothing waits on a
/// socket, and the file read is a line-at-a-time scan of one file — see
/// `agent_commands::read_ran` for why it is not read whole.
fn ran_at(store: &Store, task_id: &str) -> String {
    let Some(thread) = crate::cmd_resume::thread_for_task(store, task_id) else {
        return String::new();
    };
    let Some(ran) = crate::agent_commands::of(&thread.kind).ran(&thread.session, &thread.cwd)
    else {
        return String::new();
    };
    format!(" · ran {} · {} turns", ran.label(), ran.turns)
}

/// End a claim and leave the trace behind.
///
/// An agent works several tasks in sequence, so every way a claim can end —
/// handed to the next task, released by hand, finished — has to answer what
/// becomes of the one being left. It keeps its status, because `doing` with
/// nobody on it is a true and useful state: it is the work that is underway
/// and waiting for you. What it loses is the claim, and what it gains is the
/// record of who had it and for how long.
///
/// Does nothing when there was no claim: `done` on a task nobody ever picked
/// up must not write a line saying it was released.
pub fn hand_off(store: &Store, task_id: &str, to: Option<&str>, reason: &str) {
    let Some(claim) = store.claims().get(task_id).cloned() else {
        return;
    };
    let get = |k: &str| claim.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let from = get("claimed_at");
    let secs = util::since(&from);

    // What the attempt was actually served at, asked for while the record that
    // answers it is still standing: `thread_for_task` reads the binding first
    // and the event log after it, and `release_pane` clears the binding before
    // it gets here — so the log fallback is not a nicety, it is the path the
    // commonest release takes.
    let ran = ran_at(store, task_id);

    store.set_worked(
        task_id,
        json!({
            "workspace_id": get("workspace_id"),
            "workspace_label": get("workspace_label"),
            "cwd": get("cwd"),
            "host": get("host"),
            "from": from,
            "to": util::now_iso(),
            "seconds": secs,
            "handed_to": to,
            "reason": reason,
        }),
    );
    store.clear_claim(task_id);

    if let Some(mut t) = store.task(task_id) {
        let spent = if secs > 0 { format!(" after {}", util::duration_human(secs)) } else { String::new() };
        // Why it ended, where the line does not already say. `release` and
        // `handoff` are the sentence itself and `done` is the `→ done` on the
        // next line; what is left is the reaper, and it is the one an attempt
        // must not be judged by. A workspace closed under an agent is not
        // evidence that its tier could not do the work — 23 of the 213 attempts
        // in this store had ended that way by 2026-08-18, and without this word
        // every one of them counts against whatever ran it.
        let why = match reason {
            "release" | "handoff" | "done" => String::new(),
            other => format!(" · {other}"),
        };
        match to {
            Some(next) => t.log(&format!("handed off to {next}{spent}{why}{ran}")),
            None => t.log(&format!("released{spent}{why}{ran}")),
        }
        t.touch();
        let _ = store.save_task(&t);
    }
    store.log_event(
        if to.is_some() { "task-handoff" } else { "task-released" },
        json!({ "id": task_id, "to": to, "reason": reason, "seconds": secs }),
    );
}

/// Every task this pane, or the workspace it sits in, is about to stop working.
///
/// Two lists rather than one, because a claim outlives the pane that made it:
/// after a herdr restart the binding is gone and only the claim says the
/// workspace was on that task, so migrating there without looking would leave
/// the old claim standing and `reconcile` would bind the agent straight back
/// to the task it had left.
fn leaving(store: &Store, pane: &str, workspace: &str, taking: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(prev) = store
        .bindings()
        .get(pane)
        .and_then(|b| b.get("task_id"))
        .and_then(|x| x.as_str())
    {
        if prev != taking {
            out.push(prev.to_string());
        }
    }
    if !workspace.is_empty() {
        for (task, c) in store.claims() {
            if task == taking || out.contains(&task) {
                continue;
            }
            if c.get("workspace_id").and_then(|x| x.as_str()) == Some(workspace) {
                out.push(task);
            }
        }
    }
    out
}

/// The question a `wsp block` left behind, if it can still be found.
///
/// `block` records it as a `blocked: …` line in the task's log and nowhere
/// else, and until now nothing read it back. The refusal below is where it is
/// worth the most: "someone owes you an answer" is only actionable if it says
/// what was asked, and the alternative is making the reader open the task to
/// find out why the command they just ran did nothing.
///
/// The last one wins — a task blocked twice is waiting on the second question.
/// The date is stepped over rather than matched, because `Task::log` is what
/// writes it; anything that is not `- <stamp> blocked: …` is somebody else's
/// log line and is not treated as one.
fn blocked_question(t: &Task) -> Option<String> {
    t.section("Log")?.lines().rev().find_map(|l| {
        let (_stamp, rest) = l.trim().strip_prefix("- ")?.split_once(' ')?;
        let q = rest.strip_prefix("blocked:")?.trim();
        (!q.is_empty()).then(|| q.to_string())
    })
}

pub fn claim(store: &Store, args: &Args) -> i32 {
    let Some(needle) = args.rest.first().cloned() else {
        eprintln!("usage: wsp claim <id>   (inside a herdr pane)");
        return 2;
    };
    let t = match store.task_or_why(&needle) {
        Ok(t) => t,
        Err(why) => {
            eprintln!("{why}");
            return 1;
        }
    };
    let Some(pane) = pane_id(args) else {
        eprintln!("wsp: no pane to bind — run this inside a herdr pane, or pass --pane");
        return 2;
    };

    let env = herdr::Env::read();

    // Claiming on behalf of another pane — from the panel, say — means the
    // environment describes the caller, not the target. Ask herdr what that
    // pane actually belongs to, or the claim records a workspace that nothing
    // can later resolve.
    let panes_now = herdr::panes().unwrap_or_default();
    let target = panes_now.iter().find(|p| p.pane_id == pane).cloned();
    let workspace = target
        .as_ref()
        .map(|p| p.workspace_id.clone())
        .filter(|w| !w.is_empty())
        .or_else(|| env.workspace_id.clone())
        .unwrap_or_default();

    // The session in the seat, from the party that knows. This read
    // `CLAUDE_SESSION_ID` out of the caller's own environment until 2026-08-17,
    // and Claude Code does not set that variable — so every binding this store
    // had ever written carried `""`, and nothing noticed because nothing read
    // it. The same objection as the workspace above applies twice over: a claim
    // made from the panel would have recorded the *panel's* session for
    // somebody else's seat. herdr reports `agent_session` on a `pane.list` row
    // (recorded against 0.7.5, protocol 17), so the answer is already in hand.
    //
    // Empty here is the honest answer and the ordinary one: `spawn` claims
    // before it starts the agent, so at this instant the seat is usually still
    // a shell. [`learn_sessions`] is what fills it once there is a session.
    let session = target.as_ref().map(|p| p.session_id.clone()).unwrap_or_default();
    let cwd = match &target {
        Some(p) if !p.cwd.is_empty() => p.cwd.clone(),
        _ => std::env::current_dir().map(|c| util::contract(&c)).unwrap_or_default(),
    };

    // The other direction: a task taken off another agent. Two panes bound to
    // one task is not a state anything downstream can read — the tree hangs a
    // pane under the task it is bound to and takes the first it finds, so the
    // second would simply not be drawn.
    let displaced: Vec<String> =
        store.panes_for_task(&t.id).into_iter().filter(|p| p != &pane).collect();

    let bindings_now = store.bindings();

    // Claiming finished work reopens it, silently: the status goes back to
    // `doing` and the task rejoins every open list on the machine. That is
    // occasionally what you want — work comes back — but never by accident,
    // and a bare suffix like `005` resolving to something already done is
    // exactly how the accident happens. Found the hard way: this command was
    // pointed at a completed task while testing the refusal below, and quietly
    // undid somebody else's finished work.
    if !t.status().is_open() && !args.has("force") {
        let p = Paint::new();
        eprintln!("{} {}  {}", p.yellow("✗"), p.bold(&t.id), t.title);
        eprintln!("  {}", p.dim(&format!("already {} — claiming it would reopen it", t.status_raw)));
        eprintln!("  {}", p.dim(&format!("wsp claim {} --force   to pick it back up", t.id)));
        return 1;
    }

    // `blocked` is open — `is_open` is `!Done` — so the guard above never
    // covered it, and a claim on a blocked task ran to the end, where the
    // status is set to `doing`. The status is the only structured record that a
    // decision is owed: `block` writes its question to the log and nothing
    // reads it back, so the block was cleared by the claim and what remained
    // was one log line no list, count or panel row shows. The whole content of
    // `block` is "stop, someone owes you an answer", and this is the command
    // that was walking past it — twice over from the panel, which claims a task
    // and then types "work it" into the agent's pane.
    //
    // Refused rather than warned about, for the same reason as the two guards
    // around it: a line above the work still leaves the block gone.
    if t.status() == Status::Blocked && !args.has("force") {
        let p = Paint::new();
        eprintln!("{} {}  {}", p.yellow("✗"), p.bold(&t.id), t.title);
        match blocked_question(&t) {
            Some(q) => eprintln!("  {}", p.dim(&format!("blocked, waiting on an answer: {q}"))),
            None => eprintln!("  {}", p.dim("blocked — a decision is owed before it is worked")),
        }
        eprintln!("  {}", p.dim(&format!("wsp claim {} --force   to work it anyway", t.id)));
        return 1;
    }

    // Taking work off a *live* agent is almost always a mistake, and it used
    // to be silent: the binding was cleared, the other agent went on editing
    // files for a task the store said was ours, and the first anyone knew was
    // two commits fighting over the same lines. A dead pane's binding is
    // another matter — that is exactly the stale state a re-claim is for.
    //
    // Refused before anything is written, so a refusal costs nothing: this
    // agent has not yet let go of whatever it was holding.
    let held_by = live_holders(&bindings_now, &panes_now, &t.id, Some(&pane));
    if !held_by.is_empty() && !args.has("force") {
        let held = store
            .claims()
            .get(&t.id)
            .and_then(|c| c.get("claimed_at"))
            .and_then(|c| c.as_str())
            .map(|c| util::duration_human(util::since(c)))
            .unwrap_or_default();
        if args.json() {
            println!(
                "{}",
                json!({
                    "error": "held",
                    "task": t.id,
                    "held_by": held_by.iter().map(|p| json!({
                        "pane": p.pane_id, "agent": p.agent, "state": p.agent_status,
                    })).collect::<Vec<_>>(),
                    "held_for": held,
                })
            );
        } else {
            let p = Paint::new();
            eprintln!("{} {}  {}", p.yellow("✗"), p.bold(&t.id), t.title);
            for h in &held_by {
                eprintln!(
                    "  {}",
                    p.dim(&format!(
                        "held by {} in {} · {}{}",
                        if h.agent.is_empty() { "a shell" } else { &h.agent },
                        h.pane_id,
                        h.agent_status,
                        if held.is_empty() { String::new() } else { format!(" · {held}") }
                    ))
                );
            }
            eprintln!("  {}", p.dim(&format!("wsp claim {} --force   to take it anyway", t.id)));
        }
        return 1;
    }

    // Whatever this agent was on, it is not on it after this. The claim moves
    // with the agent; the trace stays with the task.
    let left = leaving(store, &pane, &workspace, &t.id);
    for task in &left {
        hand_off(store, task, Some(&t.id), "handoff");
    }

    let ws_label = herdr::workspaces()
        .unwrap_or_default()
        .into_iter()
        .find(|w| w.id == workspace)
        .map(|w| w.label)
        .unwrap_or_default();

    // Name the workspace and the pane after the work, before the claim is
    // written: the claim records the label the workspace is to be *found* by
    // when its id is gone, so it has to record the name it is about to have and
    // not the one it is losing.
    let named = name_after_task(&pane, &workspace, &t, &ws_label);

    // One lock around the state files a claim touches, so a claim
    // arriving in the middle of this one cannot read a half-made state: a
    // binding cleared and not yet replaced, or a claim recorded against a task
    // whose binding has not landed. The task file and its git commit are
    // outside it deliberately — a commit can take longer than any other agent
    // should be made to wait, and `wsp reconcile` already rebuilds bindings
    // from claims if the two ever part company.
    store.locked(|| {
        for other in &displaced {
            store.clear_binding(other);
        }
        store.set_binding(
            &pane,
            json!({
                "task_id": t.id,
                "pane_id": pane,
                "workspace_id": workspace,
                "agent_session_id": session,
                "cwd": cwd,
                "started_at": util::now_iso(),
            }),
        );

        // The durable half. A pane id is worthless the moment the pane dies, so
        // record the workspace instead — by id, and by the label and cwd herdr
        // keeps in its own session file. Those two are not decoration: a
        // workspace id above the one that survived a restart is handed out
        // again (`robustness-084`), so the id alone cannot tell the workspace
        // this claim meant from the one that took its name, and the label and
        // cwd are what can. The label is looked up before the lock: asking herdr
        // is a socket round-trip, and nothing else should wait on it.
        store.set_claim(
            &t.id,
            json!({
                "workspace_id": workspace,
                "workspace_label": named.clone().unwrap_or_else(|| ws_label.clone()),
                "cwd": cwd,
                // The same session the binding above gets, on the record that
                // outlives the pane. A binding is per-seat and is cleared the
                // moment an agent lets go — `release_pane` and `done` both drop
                // it *before* the claim ends — so a reader that only had the
                // binding could not answer what had just been running at the one
                // moment anybody asks: the end of the attempt. See `ran_at`.
                //
                // Usually empty here and filled by `learn_sessions`, because
                // `spawn` claims before it starts the agent. Not always: a claim
                // made in a pane that already holds one — the panel's `c`, an
                // agent claiming at its own shell — reads it off the pane row
                // right here, and that path writes no `session-learned` event at
                // all, which is how the event log turned out not to be a
                // fallback worth having.
                "agent_session_id": session,
                "host": util::hostname(),
                "claimed_at": util::now_iso(),
            }),
        );

        // A hand raised about this task has been answered by somebody taking
        // it. That is the rule the card's `y` is built on: it runs a plain
        // `claim`, and the flag comes down because the thing it asked for has
        // happened — rather than the panel running a second command to tidy up
        // after the first, which is a tidy-up that a failure between the two
        // would skip.
        //
        // It holds whoever claims and however: from the card, from `c` in the
        // tree, or by the agent itself at a shell. A flag on work now in
        // somebody's hands is a question about the past.
        store.clear_flag(&t.id);
    });

    // The panes this claim displaced lose the name along with the binding.
    // What displaced them is that they were bound to *this* task, so the name
    // they are wearing is this task's and comes off by the same rule `release`
    // uses. Their workspace keeps its name if the claiming pane is standing in
    // it, because the binding written just above is one that counts.
    //
    // Outside the lock: this is socket round-trips, and the next claim in
    // should not be made to wait on a rename any more than on a commit.
    for other in &displaced {
        unname_after_task(store, other, &t.id);
    }

    // `hand_off` wrote to the tasks it released, and one of them may be this
    // one — a re-claim of work the same agent put down. Re-read rather than
    // saving the copy taken before all that.
    let mut t = store.task(&t.id).unwrap_or(t);
    if t.status() != Status::Doing {
        t.set_status(Status::Doing);
    }
    for other in &displaced {
        t.log(&format!("taken over from pane {other}"));
    }
    let asked = spawned_at(args.get("model").as_deref(), args.get("effort").as_deref());
    match left.first() {
        Some(prev) => t.log(&format!("claimed by pane {pane}, taken up from {prev}{asked}")),
        None => t.log(&format!("claimed by pane {pane}{asked}")),
    }
    let _ = store.save_task(&t);
    store.log_event(
        "task-claimed",
        json!({ "id": t.id, "pane": pane, "from": left, "took_over": displaced }),
    );
    store.git_commit(&format!("wsp: claim {} — {}", t.id, t.title));

    // Reflect it in the sidebar immediately rather than waiting for the daemon.
    let mut cache = sync::Cache::default();
    let _ = sync::sync(store, &mut cache, true);

    if args.json() {
        println!(
            "{}",
            json!({
                "task": t.json(),
                "pane": pane,
                "from": left,
                "took_over": displaced,
                "named": named,
                "was": ws_label,
            })
        );
    } else {
        let p = Paint::new();
        println!("{} {}  {}", p.cyan("▸"), p.bold(&t.id), t.title);
        println!("  {}", p.dim(&format!("bound to {pane}")));
        // A rename is not free to the person who typed the old name, so say
        // what it was: that line is the whole of the undo.
        match &named {
            Some(l) if *l == ws_label => {}
            Some(_) if ws_label.is_empty() => println!("  {}", p.dim(&format!("named {workspace}"))),
            Some(_) => println!("  {}", p.dim(&format!("named {workspace} · was {ws_label}"))),
            None => {}
        }
        // Naming what was put down is the whole point of a migration being one
        // command: the agent moved, and you can see what it moved off.
        for prev in &left {
            let title = store.task(prev).map(|x| x.title).unwrap_or_default();
            println!("  {}", p.dim(&format!("left {prev}  {}", util::truncate(&title, 48))));
        }
        for other in &displaced {
            println!("  {}", p.dim(&format!("taken from {other}")));
        }
        // A title and nothing under it. The brief asks every agent to write the
        // work down because a bare title is unreadable a day later, and then the
        // claim hands it exactly that and says nothing — so the first move is to
        // reconstruct an overview from a sentence, silently, and get it wrong.
        // Said here because this is the one moment it is still cheap: the task
        // is in hand, nothing has been built on a guess about it yet, and the
        // person who knows what it meant may still be at the keyboard.
        if t.section("Overview").is_none() {
            println!(
                "  {}",
                p.dim(&format!(
                    "no overview — please write one before you start: wsp edit {} --overview -",
                    t.id
                ))
            );
        }
        // Claiming is the moment an agent commits to a tree, and the moment it
        // is cheapest to be told it is not alone in one. On 2026-08-15 two
        // agents worked this repo for twenty minutes without knowing, and this
        // line is where the first of them would have found out.
        let w = crate::overlap::World::live(store);
        let cwd = std::env::current_dir().ok().map(|p| p.display().to_string());
        let near: Vec<_> = crate::overlap::standing_beside(&w, &pane, cwd.as_deref())
            .into_iter()
            .filter(|s| s.relation.is_near())
            .collect();
        if !near.is_empty() {
            println!();
            println!(
                "  {}",
                p.yellow(&format!(
                    "{} else in this tree — commit with explicit paths",
                    near.len()
                ))
            );
            for s in near.iter().take(4) {
                println!(
                    "  {}",
                    p.dim(&format!("{}  {}", util::pad(&s.pane, 7), util::truncate(&s.name(), 52)))
                );
            }
            if near.len() > 4 {
                println!("  {}", p.dim(&format!("{} more · wsp overlap", near.len() - 4)));
            }
        }
    }
    0
}

/// What a backend reading teaches about the sessions in seats already bound.
///
/// **A session id cannot be recorded when the claim is made, and that is the
/// whole reason this exists rather than one line in [`claim`].** `spawn`'s order
/// is workspace, claim, agent, and the claim has to land first because the agent
/// reads it in the `SessionStart` brief on its way in. So at the instant the
/// binding is written the seat is still a shell, there is no session, and every
/// binding written by a spawn is written inside that window. Fixing where
/// `claim` reads from is necessary — it was reading the *caller's* environment —
/// and on its own it would have left the field empty for every spawned agent,
/// which is all of them.
///
/// The two rules, and both are the point.
///
/// **Silence is not a correction.** A row carrying no session leaves the
/// recorded one alone. A pane whose agent has died reports no `agent_session`,
/// and that is the moment the id is worth the most: it is what `claude --resume`
/// needs to bring the same session back, which is what `wsp-073` buys a remote
/// agent for the price of a dropped ssh. Writing `""` over it would delete the
/// fact at the instant it became useful. Same judgement `sync` already makes
/// about a machine that did not answer, one field further in.
///
/// **A different session is a correction, and is taken.** `/clear`, a restart by
/// hand and a resume that mints a fresh id all leave a seat holding a session
/// that is not the recorded one, and the backend is the party that knows. A
/// recorded id that no longer names the agent in the seat is worse than none:
/// it resumes the wrong conversation.
fn sessions_learned<'a>(
    bindings: &BTreeMap<String, Value>,
    seen: impl Iterator<Item = (&'a str, &'a str)>,
) -> Vec<(String, String)> {
    seen.filter(|(_, session)| !session.trim().is_empty())
        .filter_map(|(seat, session)| {
            // An unbound pane is not this function's business — a session in a
            // seat nobody has claimed anything in is `reconcile`'s to notice, if
            // it is anybody's. A binding with no such key at all reads as empty
            // and is therefore learned rather than skipped: `adopt` writes one
            // of those every time, so that is a live shape and not a defence
            // against an imagined store.
            let b = bindings.get(seat)?;
            let had = b.get("agent_session_id").and_then(|v| v.as_str()).unwrap_or("");
            (had != session).then(|| (seat.to_string(), session.to_string()))
        })
        .collect()
}

/// Record against each bound seat the session the backend says is sitting in it.
///
/// Called from wherever a backend reading is already in hand — `sync`, which has
/// the pane list every tick, and `spawn`, which asks once the moment its agent
/// is ready. Neither pays a round-trip for it. Returns how many bindings
/// changed, which is zero on every tick after the one where an agent started.
///
/// The event is logged because it is the only place a *history* of sessions
/// against a task can come from: a binding holds one at a time, and an agent
/// that is cleared and resumed twice leaves three. `render-061` wants exactly
/// that list, and this is where it accrues without being built.
pub fn learn_sessions<'a>(store: &Store, seen: impl Iterator<Item = (&'a str, &'a str)>) -> usize {
    // Read unlocked, because the expensive half is the caller's backend reading
    // and this is only deciding whether there is anything to write at all.
    let learned = sessions_learned(&store.bindings(), seen);
    if learned.is_empty() {
        return 0;
    }
    store.locked(|| {
        // Re-read inside the lock: a claim landing between the two readings has
        // rebound this pane to another task, and the session belongs to the
        // agent in the seat either way — but the rest of the record is now
        // somebody else's and must not be written back from the stale copy.
        let bindings = store.bindings();
        for (seat, session) in &learned {
            let Some(mut b) = bindings.get(seat).cloned() else { continue };
            let task = b.get("task_id").and_then(|t| t.as_str()).unwrap_or("").to_string();
            let Some(o) = b.as_object_mut() else { continue };
            o.insert("agent_session_id".to_string(), json!(session));
            store.set_binding(seat, b);
            // And on the claim, which is the copy that is still there when the
            // attempt ends. Only where there is one: a session learned for a
            // seat holding nothing has no task to be recorded against.
            if let Some(mut c) = store.claims().get(&task).cloned() {
                if let Some(o) = c.as_object_mut() {
                    o.insert("agent_session_id".to_string(), json!(session));
                    store.set_claim(&task, c);
                }
            }
            store.log_event(
                "session-learned",
                json!({ "id": task, "pane": seat, "session": session }),
            );
        }
    });
    learned.len()
}

/// Rebuild bindings from claims and whatever herdr currently has open.
///
/// This is what makes a binding disposable. herdr restores workspaces, their
/// layouts and their agent sessions across a restart, but pane ids are not
/// stable across it and nothing outside wsp knows which task a pane was on.
/// So: for every claim belonging to this host, find the workspace it names —
/// by id, or failing that by the label and cwd herdr persists — and bind its
/// most plausible pane.
///
/// What one pass put right.
#[derive(Debug, Default, Clone, Copy)]
pub struct Reconciled {
    /// Bindings re-established from claims.
    pub bound: usize,
    /// Workspaces and panes given the name of the task they hold.
    pub named: usize,
    /// Claims dropped because the workspace holding them is gone.
    pub reaped: usize,
    /// Seats dropped for the same reason. Separate from `reaped` because they
    /// are not the same loss: a reaped claim frees work for somebody else to
    /// take, and a swept seat means raised hands stop being addressed to a
    /// workspace nobody is in.
    pub stood_down: usize,
    /// Panel records forgotten for the same reason, and separate again: this
    /// one costs nobody any work, it only stops `panels.json` pointing at panes
    /// that belong to whoever took the workspace id next.
    pub forgotten: usize,
}

/// Returns what it put right.
///
/// `reap` additionally ends every claim whose workspace herdr no longer knows —
/// the other half of retiring a set of workspaces, because a claim naming one
/// that has been closed goes on saying work is being done in it, and herdr
/// hands ids out again. Asked for rather than automatic: a claim outliving its
/// *pane* is an accident of process lifetime and must stand, and a daemon
/// starting before herdr has finished restoring a session would otherwise read
/// a half-built world as a mass closure.
/// The machine an id sits on, as a map key: `""` for this seat, the `@mb2`
/// suffix for anywhere else.
///
/// A thin skin on [`herdr::split_host`], which is the one place that suffix
/// means anything. Here so the two sides of the guard below are keyed the same
/// way without either of them spelling out the rule.
///
/// The *id* and not the claim's `host` field, deliberately. `host` is
/// `hostname()` as the process that wrote the claim saw it — and an agent on
/// an executor runs a `wsp` shim that executes on the seat, so its claims
/// record the seat's hostname and `host` cannot tell the machines apart. The
/// workspace id can: it comes back from that machine's own herdr already
/// qualified. The id is the routing key everywhere else in this design; it is
/// the routing key here too.
fn machine_of(id: &str) -> &str {
    herdr::host_of(id).unwrap_or("")
}

/// Which machines actually answered, and with how many workspaces.
///
/// **The judgement the whole reap turns on: unreachable is not empty.**
///
/// `reconcile --reap` ends a claim whose workspace is gone, and a machine that
/// is merely unreachable reports no workspaces — which looks exactly like a
/// machine with nothing running on it. Reaped on that basis, the first network
/// blip hands back every task an executor is holding, kills the claims, and
/// leaves real agents working on tasks that no longer know about them.
///
/// So a machine must have been *heard from* before anything it holds is
/// touched: present in this map, which it only ever is by having reported a
/// workspace. Absent covers all three ways of saying nothing — unreachable,
/// answering-but-empty, and never asked — and none of them is evidence that
/// the work stopped.
///
/// This generalises the check that used to sit at the top of the reap as
/// `if reap && !workspaces.is_empty()`, which made exactly this judgement, for
/// exactly this reason, on the one machine there was. Widening it rather than
/// adding a second test beside it: two rules for "is this machine answering"
/// is how they drift apart, and the one that drifts is the one that reaps.
///
/// Today every workspace is local, so this is one entry keyed `""` and the
/// behaviour is unchanged. When `workspaces()` fans out across machines
/// (wsp-047) the ids come back `@machine`-qualified and this partitions
/// itself, with no further change here.
///
/// Takes ids rather than workspaces because `sync` needs the same judgement one
/// layer over, about panes and bindings (robustness-015). Two rules for "is this
/// machine answering" is how they drift apart, and the one that drifts is the
/// one that reaps — so there is one, and each caller brings whatever it has
/// actually heard.
///
/// **This is the rule and the wrong source, and its successor is
/// [`crate::place::Census`]** (robustness-059). Partitioning the ids works only
/// while wsp knows one backend's shape of id — the `@mb2` the `Remote` decorator
/// exists to make private — and it reads *answered* off the rows that came back,
/// so a machine that answered holding nothing is filed under silence, and for as
/// long as it holds nothing its claims cannot be swept at all. Here that is the
/// conservative reading rather than a defect — an empty answer is what a herdr
/// restoring a session gives for a second or two — but it is a reading nobody
/// chose, because the two facts arrive as one. A census reports who answered as
/// its own fact, and leaves the choice to the caller that reaps.
/// The move is not made here because nothing in this reap goes through the port
/// yet, and it needs the other half with it: a census can only speak about seats
/// it can see, and a seat being reaped is by definition in no census, so the
/// record has to carry the machine it was opened on. Both halves land together,
/// with robustness-058's seat record, and this becomes one comparison of two
/// names with nothing parsed.
pub(crate) fn answered_by_machine<'a>(
    ids: impl IntoIterator<Item = &'a str>,
) -> std::collections::BTreeMap<&'a str, usize> {
    let mut out = std::collections::BTreeMap::new();
    for id in ids {
        *out.entry(machine_of(id)).or_insert(0) += 1;
    }
    out
}

/// Whether the machine this claim sits on has said enough to be believed.
///
/// The one line standing between a network blip and every task an executor
/// holds being handed back. Silence — unreachable, answering with nothing, or
/// never asked — is not the same as "the work stopped", and only the machine
/// that spoke gets its claims examined.
pub(crate) fn may_reap(answered: &std::collections::BTreeMap<&str, usize>, id: &str) -> bool {
    answered.contains_key(machine_of(id))
}

pub fn reconcile(store: &Store, reap: bool) -> Reconciled {
    let mut out = Reconciled::default();
    let claims = store.claims();
    let governors = store.governors();
    // A store with nothing claimed and no seat in it still has panel records to
    // sweep, so the fast path out is only for the pass that is not reaping.
    if claims.is_empty() && governors.is_empty() && !reap {
        return out;
    }
    let Ok(panes) = herdr::panes() else { return out };
    let workspaces = herdr::workspaces().unwrap_or_default();
    let host = util::hostname();

    if reap {
        let answered = answered_by_machine(workspaces.iter().map(|w| w.id.as_str()));

        // `panels.json` is keyed on the workspace too, and until
        // `robustness-090` nothing swept it at all: 147 entries and not one of
        // them a workspace herdr still listed. Same guard, same evidence, one
        // file over — and `crate::panel::reap_panels` is where the argument for
        // sweeping it *here* rather than in `sync` is written down.
        out.forgotten = crate::panel::reap_panels(
            store,
            &workspaces.iter().map(|w| w.id.clone()).collect::<Vec<_>>(),
            &answered,
        );

        // A seat whose workspace has gone is worse than no seat: every raised
        // hand under it goes on being addressed to a workspace nobody is
        // sitting in, and the brief goes on naming it. Swept here rather than
        // checked at every read, so the cost lands on the daemon's reap and not
        // on the panel's tick — and behind the same `may_reap` guard as the
        // claims, because an empty answer from a machine that did not reply is
        // what once reaped every binding in the store.
        //
        // Vacated rather than removed. The occupancy is what the closed
        // workspace disproves; the position is the project's and outlives every
        // agent that ever sat in it — see [`cmd_govern::vacate`]. Before that
        // distinction existed, a night ending took the governor off `wsp`
        // entirely, and the morning had no way to tell "nobody is in the seat"
        // from "this project never had one".
        for (project, g) in &governors {
            let get = |k: &str| g.get(k).and_then(|v| v.as_str()).unwrap_or("");
            if !get("host").is_empty() && get("host") != host {
                continue;
            }
            if !may_reap(&answered, get("workspace")) {
                continue;
            }
            if workspaces.iter().any(|w| w.id == get("workspace")) {
                continue;
            }
            if cmd_govern::vacate(store, project) {
                out.stood_down += 1;
            }
        }

        for (task_id, c) in &claims {
            let get = |k: &str| c.get(k).and_then(|v| v.as_str()).unwrap_or("");
            if !get("host").is_empty() && get("host") != host {
                continue;
            }
            if !may_reap(&answered, get("workspace_id")) {
                continue;
            }
            let alive = workspaces.iter().any(|w| {
                w.id == get("workspace_id")
                    || (!get("workspace_label").is_empty() && w.label == get("workspace_label"))
            });
            if alive {
                continue;
            }
            hand_off(store, task_id, None, "workspace closed");
            out.reaped += 1;
        }
        // Same reason as `release`: the log lines are written here, so the
        // commit that carries them is this one. Nothing is written when
        // nothing was reaped, and `git_commit` is a no-op on that.
        store.git_commit(&format!("wsp: release {} task(s) whose workspace closed", out.reaped));
    }
    // Nothing claimed leaves nothing to bind or rename. Checked here rather
    // than at the top so a store whose only live record is a seat still gets
    // its dead seats swept.
    if claims.is_empty() {
        return out;
    }
    let claims = if out.reaped > 0 { store.claims() } else { claims };

    let bindings = store.bindings();
    let already: Vec<String> = bindings
        .values()
        .filter_map(|b| b.get("task_id").and_then(|t| t.as_str()).map(|s| s.to_string()))
        .collect();

    // A pane holds one task, here as everywhere else. Two claims naming the
    // same workspace used to pick the same pane and the second quietly
    // overwrote the first — and claims are walked in id order, so an agent came
    // back from a restart bound to the *older* task, the one it had left.
    let mut taken: Vec<String> = bindings.keys().cloned().collect();

    for (task_id, c) in &claims {
        if already.iter().any(|t| t == task_id) {
            continue;
        }
        let get = |k: &str| c.get(k).and_then(|v| v.as_str()).unwrap_or("");
        // A claim made on another machine says nothing about this one.
        if !get("host").is_empty() && get("host") != host {
            continue;
        }

        let want_id = get("workspace_id");
        let want_label = get("workspace_label");
        let want_cwd = get("cwd");

        // The id if it still names a workspace; otherwise the label and cwd,
        // which is what survives a workspace being rebuilt under a new id.
        let ws = workspaces
            .iter()
            .find(|w| w.id == want_id)
            .or_else(|| {
                workspaces.iter().find(|w| !want_label.is_empty() && w.label == want_label)
            })
            .map(|w| w.id.clone());
        let Some(ws) = ws else { continue };

        // Prefer a pane running an agent, then any pane that is not one of our
        // own panels. A workspace normally has exactly one candidate.
        let mut candidates: Vec<&herdr::Pane> = panes
            .iter()
            .filter(|p| p.workspace_id == ws && p.label != crate::panel::PANEL_LABEL)
            .collect();
        candidates.sort_by_key(|p| u8::from(p.agent.is_empty()));
        let Some(pane) = candidates.iter().find(|p| !taken.contains(&p.pane_id)) else { continue };
        taken.push(pane.pane_id.clone());

        store.set_binding(
            &pane.pane_id,
            json!({
                "task_id": task_id,
                "pane_id": pane.pane_id,
                "workspace_id": ws,
                "agent_session_id": pane.session_id,
                "cwd": if want_cwd.is_empty() { pane.cwd.clone() } else { want_cwd.to_string() },
                "started_at": util::now_iso(),
                "reconciled": true,
            }),
        );
        store.log_event(
            "task-reconciled",
            json!({ "id": task_id, "pane": pane.pane_id, "workspace": ws }),
        );
        out.bound += 1;
    }

    out.named = name_bound(store, &panes, &workspaces);
    out
}

/// Give every bound pane, and the workspace it stands in, the name of the task
/// it is holding.
///
/// `claim` does this at the moment it happens, which covers everything claimed
/// since — but only that. A pane that took its task up before any of this
/// existed keeps whatever herdr called it, and a claim whose rename was
/// dropped on a slow socket keeps it too, silently, because the rename is not
/// worth failing a claim over. This is where both are put right.
///
/// Deliberately not in `sync`: that runs every tick, and a name reasserted
/// every tick is a name you cannot change by hand. Here it runs when the
/// daemon starts and when somebody asks — so a name you type survives until
/// the next reconcile, which is the trade the user picked on wsp-016.
fn name_bound(
    store: &Store,
    panes: &[herdr::Pane],
    workspaces: &[herdr::Workspace],
) -> usize {
    let tasks = store.tasks();
    let said = store.said();
    let mut named = 0;
    for (pane_id, b) in store.bindings() {
        let Some(task) = b
            .get("task_id")
            .and_then(|t| t.as_str())
            .and_then(|id| tasks.iter().find(|t| t.id == id))
        else {
            continue;
        };
        let Some(label) = task_label(task) else { continue };
        let Some(pane) = panes.iter().find(|p| p.pane_id == pane_id) else { continue };

        let mut touched = false;
        let worn = full_name(&said, &pane_id, &pane.label, Some(task));
        if let Some(name) = pane_rename(&pane.label, worn.as_deref(), task) {
            let wire = on_the_wire(&name);
            if herdr::rename_pane(&pane.pane_id, &wire).is_ok() {
                touched = true;
                // The entry follows the label it describes. It is keyed on what
                // the pane wears, so leaving the old one behind would leave a
                // long name matching nothing — harmless, and it would also mean
                // the sentence we just carried across being read as gone.
                store.set_said(&pane_id, &wire, &name);
            }
        }
        // A workspace nobody named reads back as the agent or the folder, so
        // the comparison can never say "already right" — it says "not the task
        // title", which is the same answer and the one that matters. A seat's
        // own name is the exception, for the reason [`name_after_task`] gives:
        // the room belongs to the position, and reconcile runs on every daemon
        // start, so this is where a nightly custodian would silently lose it.
        if workspaces
            .iter()
            .any(|w| w.id == pane.workspace_id && w.label != label && !cmd_govern::is_governor_label(&w.label))
        {
            touched |= herdr::rename_workspace(&pane.workspace_id, &label).is_ok();
        }
        if touched {
            named += 1;
        }
    }
    named
}

/// Put work down: this pane's, or the named task's wherever it is held.
///
/// The id form exists because the bare form used to swallow one. `wsp release
/// render-040` unbound *this* pane and printed `released w1X:p1`, which is a
/// pane id and the word "released" — close enough to success to walk away
/// from, while the claim it actually ended was the one the caller was standing
/// in. Every other verb in this CLI takes an id, so the token was typed in good
/// faith and dropped on the floor.
///
/// A task rather than a pane, because a claim is the thing being ended and a
/// pane is only where one usually sits. Claims outlive panes, so the id form
/// also reaches the case the pane form cannot: a claim left standing by a
/// workspace that is gone.
pub fn release(store: &Store, args: &Args) -> i32 {
    match args.rest.first().cloned() {
        Some(needle) => release_task(store, args, &needle),
        None => release_here(store, args),
    }
}

fn release_here(store: &Store, args: &Args) -> i32 {
    let Some(pane) = pane_id(args) else {
        eprintln!("wsp: no pane — pass --pane or run inside herdr");
        return 2;
    };
    let (removed, _task) = release_pane(store, &pane);
    if args.json() {
        println!("{}", json!({ "pane": pane, "released": removed }));
    } else if removed {
        println!("released {pane}");
    } else {
        println!("nothing bound to {pane}");
    }
    0
}

/// `wsp release <id>`: end that task's claim, whoever is holding it.
///
/// Two targets are refused rather than ranked. `--pane` names one seat and an
/// id names whatever seat holds a task; a line carrying both is a person who
/// believes one of them is being read, and picking either is how this command
/// earned its defect in the first place.
///
/// Not guarded by `--force` the way `claim` is. Stealing work is silent damage
/// — the other agent goes on editing files for a task the store says is yours —
/// while releasing one is a claim ending, which is what the caller typed the
/// task's id to ask for, and a re-claim undoes it.
fn release_task(store: &Store, args: &Args, needle: &str) -> i32 {
    if let Some(pane) = args.get("pane") {
        eprintln!("wsp: name a task or a pane, not both");
        eprintln!("  wsp release {needle} — the claim on that task, wherever it is");
        eprintln!("  wsp release --pane {pane} — whatever that pane is holding");
        return 2;
    }
    let t = match store.task_or_why(needle) {
        Ok(t) => t,
        Err(why) => {
            eprintln!("{why}");
            return 1;
        }
    };

    // Every pane bound to it, not the first: two bindings on one task is a
    // state `claim` works to prevent and `reconcile` can still leave behind
    // after a herdr restart, and releasing half of it would leave the task
    // looking held by a pane that no longer has a claim to stand on.
    let panes = store.panes_for_task(&t.id);
    for pane in &panes {
        if store.clear_binding(pane) {
            unname_after_task(store, pane, &t.id);
        }
    }

    // The claim is read before `hand_off` clears it: `where` is the only part
    // of this the caller cannot already see, and a released claim leaves
    // nothing behind that says where it had been.
    let where_held = store.claims().get(&t.id).map(|c| claim_where(c));
    if where_held.is_some() {
        end_claim(store, &t.id);
    }
    if !panes.is_empty() || where_held.is_some() {
        let mut cache = sync::Cache::default();
        let _ = sync::sync(store, &mut cache, true);
    }

    if args.json() {
        println!(
            "{}",
            json!({
                "task": t.id,
                "released": !panes.is_empty() || where_held.is_some(),
                "panes": panes,
                "claim": where_held,
            })
        );
    } else if !panes.is_empty() {
        println!("released {} from {}", t.id, panes.join(", "));
    } else if let Some(where_held) = where_held {
        // A claim with no pane under it is the ordinary aftermath of a herdr
        // restart, and saying so is the difference between "that worked" and
        // "did it find the right thing?".
        println!("released {} — the claim in {where_held}, with no pane bound", t.id);
    } else {
        println!("nothing holds {}", t.id);
    }
    0
}

/// Where a claim says the work is, for one line of output.
///
/// The id rather than the label, which is the opposite of what a list wants.
/// A workspace holding a task is *named after* it — `002 · the probe task
/// claimed onto anot…` — so the label restates the id already in the sentence
/// and arrives elided, with the ellipsis landing mid-clause. The id is the part
/// the reader cannot reconstruct: it says which screen to go and look at.
pub(crate) fn claim_where(claim: &serde_json::Value) -> String {
    let get = |k: &str| claim.get(k).and_then(|v| v.as_str()).unwrap_or("");
    let (label, id) = (get("workspace_label"), get("workspace_id"));
    match (id.is_empty(), label.is_empty()) {
        (false, _) => id.to_string(),
        (true, false) => label.to_string(),
        (true, true) => "somewhere it did not record".to_string(),
    }
}

/// End the durable claim on a task, and commit the trace of it.
///
/// Shared by both forms of `release`, which differ only in how they find the
/// task: a released claim has to leave the same record whichever end it was
/// asked for.
fn end_claim(store: &Store, task_id: &str) {
    // Releasing is a decision, so it clears the durable claim too — unlike a
    // pane exiting, which is only ever an accident of process lifetime and must
    // leave the intent standing. It ends the same way a migration does, and
    // leaves the same record behind.
    hand_off(store, task_id, None, "release");
    // `hand_off` writes the release into the task's log, and until commits were
    // scoped to what a command wrote, that line waited for some later command
    // to sweep it up — under that command's message. It belongs to this one.
    store.git_commit(&format!("wsp: release {task_id}"));
}

/// Drop a pane's binding and, with it, the claim it stood for.
///
/// The mechanism of `release` with no printing in it, because there are two
/// callers and they say different things: this verb, and `wsp despawn`, which
/// ends the seat first and then has to end the claim through exactly this path
/// rather than a second copy of it. Returns whether there was a binding, and the
/// task whose claim was ended.
pub(crate) fn release_pane(store: &Store, pane: &str) -> (bool, Option<String>) {
    let had = store.bindings().get(pane).cloned();
    let mut released: Option<String> = None;
    let removed = store.clear_binding(pane);
    if removed {
        if let Some(task_id) = had.as_ref().and_then(|b| b.get("task_id")).and_then(|t| t.as_str()) {
            released = Some(task_id.to_string());
            // The name goes back before the log line does, and while the task
            // is still readable: `unname_after_task` needs the title to know
            // whether the label it is looking at is one we wrote.
            unname_after_task(store, pane, task_id);
            end_claim(store, task_id);
        }
        let mut cache = sync::Cache::default();
        let _ = sync::sync(store, &mut cache, true);
    }
    (removed, released)
}

pub fn pin(store: &Store, args: &Args) -> i32 {
    // `--top` marks a workspace as belonging to no project on purpose: the
    // home for whatever runs the whole space, and for terminals that are not
    // work. Without it, "no project" only ever means "nothing resolved", and
    // the two are not the same thing.
    if args.has("top") {
        let Some(ws) = args.get("workspace").or_else(|| herdr::Env::read().workspace_id) else {
            eprintln!("wsp: no workspace — pass -w, or run inside herdr");
            return 2;
        };
        store.set_pin(&ws, crate::resolve::TOP_LEVEL);
        if args.json() {
            println!("{}", json!({ "workspace": ws, "project": null, "top": true }));
        } else {
            println!("workspace {ws} pinned outside the project tree");
        }
        return 0;
    }
    let Some(needle) = args.rest.first().cloned() else {
        eprintln!("usage: wsp pin <project> [-w workspace] | wsp pin --top");
        return 2;
    };
    let index = Index::new(store.projects());
    let Some(proj) = index.find(&needle) else {
        eprintln!("wsp: no such project `{needle}`");
        return 1;
    };
    let Some(ws) = args.get("workspace").or_else(|| herdr::Env::read().workspace_id) else {
        eprintln!("wsp: no workspace — pass -w, or run inside herdr");
        return 2;
    };

    store.set_pin(&ws, &proj.id);
    let mut cache = sync::Cache::default();
    let _ = sync::sync(store, &mut cache, true);

    if args.json() {
        println!("{}", json!({ "workspace": ws, "project": proj.id }));
    } else {
        println!("workspace {ws} pinned to {}", proj.id);
    }
    0
}

pub fn unpin(store: &Store, args: &Args) -> i32 {
    let Some(ws) = args.get("workspace").or_else(|| herdr::Env::read().workspace_id) else {
        eprintln!("wsp: no workspace — pass -w, or run inside herdr");
        return 2;
    };
    let removed = store.clear_pin(&ws);
    let mut cache = sync::Cache::default();
    let _ = sync::sync(store, &mut cache, true);
    if args.json() {
        println!("{}", json!({ "workspace": ws, "unpinned": removed }));
    } else if removed {
        println!("workspace {ws} unpinned");
    } else {
        println!("workspace {ws} was not pinned");
    }
    0
}

/// Everything `wsp where` reads, gathered into one value.
///
/// The subject here is the resolution chain — pin > binding > mandate > cwd >
/// workspace label — and the chain is already pure in [`resolve::resolve`].
/// What was not pure is *gathering the five things it compares*, which took a
/// pane with the right environment, a store with the right pins, and a herdr
/// answering with the right label. So the one question worth asking of this
/// command — which link won, and what it beat — had no test.
pub(crate) struct Whereabouts {
    pub index: Index,
    pub pins: std::collections::BTreeMap<String, String>,
    pub bindings: std::collections::BTreeMap<String, serde_json::Value>,
    pub claims: std::collections::BTreeMap<String, serde_json::Value>,
    pub tasks: Vec<Task>,
    /// For the label of the workspace this pane stands in, which is both a
    /// link in the chain and how a claim is matched to a workspace.
    pub workspaces: Vec<herdr::Workspace>,
    /// The seat this process is standing in, and the workspace herdr's
    /// environment names — the second of which only a multiplexer has.
    pub pane: Option<String>,
    pub workspace: Option<String>,
    /// The process's own directory rather than the pane's: herdr reports the
    /// shell's cwd, which is stale the moment anyone `cd`s.
    pub cwd: Option<String>,
}

impl Whereabouts {
    pub(crate) fn live(store: &Store) -> Whereabouts {
        let env = herdr::Env::read();
        Whereabouts {
            index: Index::new(store.projects()),
            pins: store.pins(),
            bindings: store.bindings(),
            claims: store.claims(),
            tasks: store.tasks(),
            workspaces: match (&env.workspace_id, herdr::available()) {
                (Some(_), true) => herdr::workspaces().unwrap_or_default(),
                _ => Vec::new(),
            },
            pane: my_pane(),
            workspace: env.workspace_id,
            cwd: std::env::current_dir().ok().map(|p| p.display().to_string()),
        }
    }
}

/// Where this pane sits, once every link has been compared.
pub(crate) struct Located {
    pub project: Option<String>,
    pub source: &'static str,
    pub tags: Vec<String>,
    /// What cwd alone would have said. Worth carrying, because a claimed pane
    /// keeps its project after you `cd` somewhere else, and the two disagreeing
    /// is the state a person is trying to understand when they ask.
    pub by_cwd: Option<String>,
    pub label: Option<String>,
    pub task: Option<Task>,
}

pub(crate) fn locate(w: &Whereabouts) -> Located {
    let task = w
        .pane
        .as_ref()
        .and_then(|p| w.bindings.get(p))
        .and_then(|b| b.get("task_id"))
        .and_then(|t| t.as_str())
        .and_then(|id| w.tasks.iter().find(|t| t.id == id).cloned());

    let label = w
        .workspace
        .as_ref()
        .and_then(|ws| w.workspaces.iter().find(|x| &x.id == ws).map(|x| x.label.clone()));

    let r = resolve::resolve(
        &w.index,
        &w.pins,
        resolve::Held {
            binding: task.as_ref().and_then(|t| t.project.clone()),
            claim: resolve::claimed_project(
                &w.claims,
                &w.tasks,
                w.workspace.as_deref(),
                label.as_deref(),
            ),
        },
        w.workspace.as_deref(),
        label.as_deref(),
        w.cwd.as_deref(),
    );

    Located {
        tags: r.project.as_ref().map(|p| w.index.effective_tags(p)).unwrap_or_default(),
        by_cwd: w.cwd.as_deref().and_then(|c| w.index.project_for_cwd(c)),
        project: r.project,
        source: r.source,
        label,
        task,
    }
}

fn where_json(w: &Whereabouts, at: &Located) -> serde_json::Value {
    json!({
        "project": at.project,
        "source": at.source,
        "tags": at.tags,
        "by_cwd": at.by_cwd,
        "workspace_id": w.workspace,
        "workspace_label": at.label,
        "pane_id": w.pane,
        "cwd": w.cwd,
        "task": at.task.as_ref().map(|t| t.json()),
    })
}

fn where_lines(at: &Located, p: &Paint) -> Vec<String> {
    let mut out = Vec::new();
    match &at.project {
        Some(proj) => {
            out.push(format!("{}  {}", p.bold(proj), p.dim(&format!("via {}", at.source))));
            if !at.tags.is_empty() {
                out.push(p.dim(&at.tags.join(" ")));
            }
        }
        None => out.push(p.dim("no project resolved for this pane")),
    }
    if let Some(t) = &at.task {
        out.push(String::new());
        out.push(format!("{} {}  {}", p.cyan("▸"), p.bold(&t.id), t.title));
    }
    // Only when they disagree: saying "cwd would also say wsp" is a line that
    // never means anything.
    if let Some(c) = &at.by_cwd {
        if Some(c) != at.project.as_ref() {
            out.push(String::new());
            out.push(p.dim(&format!("cwd alone would say {c} — `wsp release` to follow the directory instead")));
        }
    }
    out
}

pub fn where_am_i(store: &Store, args: &Args) -> i32 {
    let w = Whereabouts::live(store);
    let at = locate(&w);
    match args.json() {
        true => println!("{}", serde_json::to_string_pretty(&where_json(&w, &at)).unwrap_or_default()),
        false => {
            for l in where_lines(&at, &Paint::new()) {
                println!("{l}");
            }
        }
    }
    0
}

pub fn wip(store: &Store, args: &Args) -> i32 {
    let w = Wip::live(store);
    match args.json() {
        true => println!("{}", serde_json::to_string_pretty(&wip_json(&w)).unwrap_or_default()),
        false => {
            for l in wip_lines(&w, &Paint::new(), args.terse()) {
                println!("{l}");
            }
        }
    }
    0
}

/// Everything `wsp wip` reads, gathered into one value.
///
/// The same bargain [`crate::panel::Snapshot`] and [`crate::cmd_machine::Fleet`]
/// make. This view's whole subject is the join — an agent that has stopped on
/// a task that is still `doing` is a person being the blocker — and while it
/// read the store and asked herdr mid-print, seeing that state took two live
/// agents in the states you wanted and a task in the right status underneath
/// them. There was nowhere to put one.
pub(crate) struct Wip {
    pub tasks: Vec<Task>,
    pub index: Index,
    pub bindings: std::collections::BTreeMap<String, serde_json::Value>,
    pub claims: std::collections::BTreeMap<String, serde_json::Value>,
    pub pins: std::collections::BTreeMap<String, String>,
    /// project id -> seat. Read once here rather than per row: the map is small
    /// and every row asks it the same question.
    pub governors: std::collections::BTreeMap<String, serde_json::Value>,
    /// Only the panes running an agent: `wip` is about who is working, and a
    /// shell is a person at a terminal rather than work in progress.
    pub agents: Vec<herdr::Pane>,
    /// For naming only — a pane's workspace label, which is also one link in
    /// the chain that resolves which project it is standing in.
    pub workspaces: Vec<herdr::Workspace>,
}

impl Wip {
    pub(crate) fn live(store: &Store) -> Wip {
        let up = herdr::available();
        Wip {
            tasks: store.tasks(),
            index: Index::new(store.projects()),
            bindings: store.bindings(),
            claims: store.claims(),
            pins: store.pins(),
            governors: store.governors(),
            agents: if up { herdr::agents().unwrap_or_default() } else { Vec::new() },
            workspaces: if up { herdr::workspaces().unwrap_or_default() } else { Vec::new() },
        }
    }
}

/// One agent, as `wip` wants it.
///
/// `pub(crate)` for `cmd_watch`, and that is not a convenience. `needs_you` is
/// a *published* field — `wsp wip --json` has carried it beside `turning` and
/// `seat` all along — and a watcher that recomputed the conjunction would be
/// the fourth hand-rolled copy of [`cmd_govern::needs_a_person`] rather than
/// the first subscriber to it. Two callers of one predicate is fine; three
/// definitions of it is how the exception for seats gets quietly lost.
pub(crate) struct WipRow {
    pub(crate) project: String,
    pub(crate) task: String,
    pub(crate) task_id: String,
    pub(crate) pane: String,
    pub(crate) workspace: String,
    pub(crate) state: String,
    /// Whether a turn is actually running in it. Beside `state` rather than
    /// derived from it at the point of drawing, because it is the answer to the
    /// question the heading asks — and because reading it off the word is the
    /// thing four censuses each got wrong in their own way.
    pub(crate) turning: bool,
    pub(crate) needs_you: bool,
    /// The project this pane's workspace is the governor of, if any — which is
    /// no project for every ordinary agent, and that is nearly all of them.
    pub(crate) seat: Option<String>,
}

/// The agents, resolved and in reading order: by project, then by pane.
pub(crate) fn wip_rows(w: &Wip) -> Vec<WipRow> {
    let mut rows: Vec<WipRow> = Vec::new();
    for a in &w.agents {
        let bound = w
            .bindings
            .get(&a.pane_id)
            .and_then(|b| b.get("task_id"))
            .and_then(|t| t.as_str())
            .and_then(|id| w.tasks.iter().find(|t| t.id == id));

        let label = w.workspaces.iter().find(|x| x.id == a.workspace_id).map(|x| x.label.clone());
        let r = resolve::resolve(
            &w.index,
            &w.pins,
            resolve::Held {
                binding: bound.and_then(|t| t.project.clone()),
                claim: resolve::claimed_project(
                    &w.claims,
                    &w.tasks,
                    Some(&a.workspace_id),
                    label.as_deref(),
                ),
            },
            Some(&a.workspace_id),
            label.as_deref(),
            Some(&a.cwd),
        );

        // Not `agent_status == "idle"`, which was three words short: see
        // `cmd_govern::needs_a_person`. Running and not turning is the whole
        // predicate, and it is asked of the port rather than of herdr's
        // spelling so the next word herdr adds does not read as work.
        let stopped = crate::place_herdr::state_of_pane(a).stopped();
        let doing = bound.map(|t| t.status() == Status::Doing).unwrap_or(false);
        let seat = cmd_govern::governs(&w.governors, &a.workspace_id);
        let needs_you = cmd_govern::needs_a_person(stopped, doing, seat.is_some());

        rows.push(WipRow {
            project: r.project.unwrap_or_else(|| "—".into()),
            task: bound
                .map(|t| t.title.clone())
                .unwrap_or_else(|| if a.title.is_empty() { "(unbound)".into() } else { format!("({})", a.title) }),
            task_id: bound.map(|t| t.id.clone()).unwrap_or_default(),
            pane: a.pane_id.clone(),
            workspace: label.unwrap_or_default(),
            state: a.agent_status.clone(),
            turning: crate::place_herdr::state_of_pane(a).turn_in_flight(),
            needs_you,
            seat,
        });
    }
    rows.sort_by(|a, b| a.project.cmp(&b.project).then(a.pane.cmp(&b.pane)));
    rows
}

/// The three lists under the agents: work waiting on a person, in the order it
/// is drawn.
fn wip_queues(w: &Wip) -> (Vec<&Task>, Vec<&Task>, usize) {
    (
        w.tasks.iter().filter(|t| t.status() == Status::Blocked).collect(),
        w.tasks.iter().filter(|t| t.status() == Status::Review).collect(),
        w.tasks.iter().filter(|t| t.project.is_none() && t.status().is_open()).count(),
    )
}

fn wip_json(w: &Wip) -> serde_json::Value {
    let rows = wip_rows(w);
    let (blocked, in_review, inbox) = wip_queues(w);
    json!({
        "agents": rows.iter().map(|r| json!({
            "project": r.project, "task": r.task, "task_id": r.task_id,
            "pane": r.pane, "workspace": r.workspace, "state": r.state,
            "turning": r.turning, "needs_you": r.needs_you, "seat": r.seat,
        })).collect::<Vec<_>>(),
        "turning": rows.iter().filter(|r| r.turning).count(),
        "needs_you": rows.iter().filter(|r| r.needs_you).count(),
        "blocked": blocked.iter().map(|t| t.json()).collect::<Vec<_>>(),
        "review": in_review.iter().map(|t| t.json()).collect::<Vec<_>>(),
        "inbox": inbox,
    })
}

fn wip_lines(w: &Wip, p: &Paint, terse: bool) -> Vec<String> {
    let rows = wip_rows(w);
    let (blocked, in_review, inbox) = wip_queues(w);
    let needs = rows.iter().filter(|r| r.needs_you).count();
    let mut out = Vec::new();

    if rows.is_empty() {
        out.push(p.dim("no agents running"));
    } else {
        // Not `all busy`, which is what this said and was the same sentence
        // `doctor`'s `herdr up, 12 agents` was saying on the night seven of
        // them had stopped. `needs you` is a narrower question — stopped *on a
        // task that is still `doing`* — so a machine full of agents that had
        // finished and were holding their slots answered it truthfully with
        // nought, and printed `all busy` over five panes running no turn.
        // Measured here on 2026-08-19: 12 agents, 7 turning, `all busy`.
        //
        // Both counts, because they answer different questions and the second
        // one does not imply the first. Nothing needing you is genuinely good
        // news; five agents idle is a slot problem whether or not anybody is
        // owed an answer.
        let turning = rows.iter().filter(|r| r.turning).count();
        let mut head = format!(
            "{}  ·  {} agents  ·  {}",
            p.bold("WIP"),
            rows.len(),
            p.dim(&format!("{turning} running a turn"))
        );
        if needs > 0 {
            head.push_str(&format!("  ·  {}", p.yellow(&format!("{needs} need you"))));
        }
        out.push(head);
        out.push(String::new());
        let pw = rows.iter().map(|r| r.project.chars().count()).max().unwrap_or(7).max(7);
        let tw = 46;
        out.push(format!(
            "{}  {}  {}  {}",
            p.dim(&util::pad("PROJECT", pw)),
            p.dim(&util::pad("TASK", tw)),
            p.dim(&util::pad("PANE", 7)),
            p.dim("STATE")
        ));
        for r in &rows {
            let state = match r.state.as_str() {
                "working" => p.green(&util::pad("working", 8)),
                "idle" => p.dim(&util::pad("idle", 8)),
                other => p.dim(&util::pad(other, 8)),
            };
            // A seat says so instead of saying it needs you. The two can never
            // both be true — [`cmd_govern::needs_a_person`] is what makes them
            // exclusive — and the column is the same width either way.
            let flag = match (&r.seat, r.needs_you) {
                (Some(proj), _) => p.cyan(&format!("{} {}", glyph_seat(), cmd_govern::governor_of(proj))),
                (None, true) => p.yellow("← needs you"),
                (None, false) => String::new(),
            };
            out.push(format!(
                "{}  {}  {}  {} {}",
                util::pad(&r.project, pw),
                util::pad(&util::truncate(&r.task, tw), tw),
                p.dim(&util::pad(&r.pane, 7)),
                state,
                flag
            ));
        }
    }

    // One task under a heading, for the two named queues below.
    let named = |out: &mut Vec<String>, t: &Task| {
        out.push(format!(
            "  {}  {}  {}",
            p.dim(&t.id),
            util::pad(&t.project.clone().unwrap_or_else(|| "—".into()), 8),
            util::truncate(&t.title, 56)
        ));
    };

    // Blocked work, named. Under `--terse` the count alone: `wip` is asked
    // repeatedly through a session to see who is free, and the answer to that
    // moves every few minutes while this list does not — a task is blocked
    // because it is waiting on a person, which is the slowest thing here.
    // Still a line, because a count going up is the reason you would go and
    // read it.
    if !blocked.is_empty() {
        out.push(String::new());
        if terse {
            out.push(format!("{}  {}   {}", p.red(&util::pad("BLOCKED", 8)), blocked.len(), p.dim("wsp ls -s blocked")));
        } else {
            out.push(format!("{}  {}", p.red(&util::pad("BLOCKED", 8)), blocked.len()));
            for t in &blocked {
                named(&mut out, t);
            }
        }
    }

    // Work an agent has finished with. `review` is the agent's terminal verb —
    // it stops there and says so, and only a person says `done` — so this is
    // the list of things waiting on you rather than on anybody working.
    if !in_review.is_empty() {
        out.push(String::new());
        out.push(format!("{}  {}   {}", p.yellow(&util::pad("REVIEW", 8)), in_review.len(), p.dim("wsp done <id> · wsp reopen <id>")));
        for t in &in_review {
            named(&mut out, t);
        }
    }
    if inbox > 0 {
        out.push(String::new());
        out.push(format!("{}  {}   {}", p.dim(&util::pad("INBOX", 8)), inbox, p.dim("wsp inbox")));
    }
    out
}

/// `wsp overlap` — who else is standing in this tree.
///
/// The near set first and by itself, because that is the answer to the question
/// worth asking: who can reach the files under my hands. Everyone else follows
/// under a rule, as context — they cannot clobber this checkout, but knowing
/// the tree is busy is worth a line.
pub fn overlap(store: &Store, args: &Args) -> i32 {
    let w = crate::overlap::World::live(store);
    let me = pane_id(args).unwrap_or_default();
    let cwd = std::env::current_dir().ok().map(|p| p.display().to_string());
    let all = crate::overlap::standing_beside(&w, &me, cwd.as_deref());
    let (near, far): (Vec<_>, Vec<_>) = all.iter().partition(|s| s.relation.is_near());

    if args.json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "pane": me,
                "cwd": cwd,
                "near": near.iter().map(|s| s.json()).collect::<Vec<_>>(),
                "elsewhere": far.iter().map(|s| s.json()).collect::<Vec<_>>(),
            }))
            .unwrap_or_default()
        );
        return 0;
    }

    let p = Paint::new();
    if all.is_empty() {
        println!("{}", p.dim("nobody else is standing anywhere"));
        return 0;
    }

    let row = |s: &crate::overlap::Standing, warn: bool| {
        let who = util::truncate(&s.name(), 44);
        let name = if warn { p.yellow(&util::pad(&who, 44)) } else { p.dim(&util::pad(&who, 44)) };
        let held = match s.since {
            Some(secs) => util::duration_human(secs),
            None => String::new(),
        };
        println!(
            "  {} {}  {}  {}  {}{}",
            // A shell is marked rather than described: nobody is driving it,
            // but a person at one can overwrite a file as thoroughly as an
            // agent can, and the panel already means this by `▫`.
            p.dim(if s.agent { " " } else { crate::panel::glyph::SHELL }),
            p.dim(&util::pad(&s.pane, 7)),
            name,
            p.dim(&util::pad(s.relation.as_str(), 13)),
            p.dim(&held),
            if s.needs_you() { p.yellow("  ← needs you") } else { String::new() }
        );
    };

    if near.is_empty() {
        println!("{}", p.dim("nobody else is standing in this tree"));
    } else {
        println!(
            "{}  {}",
            p.yellow(&format!("{} in this tree", near.len())),
            p.dim("— they can reach the files you are editing")
        );
        for s in &near {
            row(s, true);
        }
    }

    // Elsewhere is context, not a warning, and most of it is idle shells that
    // have been sitting in a directory since Tuesday. Name the ones doing
    // something; count the rest, because twenty rows of pane id would bury the
    // two lines above that actually matter.
    let (busy, quiet): (Vec<_>, Vec<_>) =
        far.into_iter().partition(|s| s.agent || s.task.is_some());
    if !busy.is_empty() {
        println!("\n{}", p.dim(&format!("{} elsewhere", busy.len())));
        for s in &busy {
            row(s, false);
        }
    }
    if !quiet.is_empty() {
        println!(
            "\n{}",
            p.dim(&format!(
                "{} idle {} in other trees · wsp wip",
                quiet.len(),
                if quiet.len() == 1 { "shell" } else { "shells" }
            ))
        );
    }
    0
}

/// The pane's content out of a `pane.read` reply.
///
/// It arrives wrapped — `{"type": "pane_read", "read": {…}}` — which the
/// schema's own `PaneReadResult` does not say, and reading it flat returns an
/// empty string rather than an error: the command worked, the pane looked
/// blank, and nothing said why. So unwrap it if the wrapper is there and read
/// it flat if it is not, and keep working whichever shape a later herdr sends.
fn read_body(r: &serde_json::Value) -> &serde_json::Value {
    r.get("read").unwrap_or(r)
}

/// `wsp peek [<target>]` — read what is actually on a pane.
///
/// Every interface bug on 2026-08-15 cost a round trip through a person's eyes:
/// a key that did nothing because the binary was stale, a cursor scrolled off
/// the pane, a closed column still showing a shell prompt. In each case the
/// pane was displaying the answer and the agent that wrote the code could not
/// see it, so the loop was guess, build, install, ask, wait.
///
/// herdr has been able to answer this all along — `pane.read` returns the
/// characters on a pane. What was missing is a way to name the pane you mean.
/// Nobody reaches for it under pressure if reaching for it starts with listing
/// every pane on the machine and picking through the JSON.
///
/// So the target resolves the way everything else in wsp resolves: by what you
/// call the thing, not by the id herdr filed it under.
/// What a needle named, once it has been read against the panes herdr
/// reported.
///
/// Reading the pane is herdr's and cannot be anything else. *Naming* it is
/// ours, and it is the half with rules in it — which label, preferring this
/// workspace, falling back to any; a pane id only if a pane wears it; a task
/// only if the store knows it. Those rules were inside `peek`, between a
/// socket check and a `pane.read`, so none of them could be asked about
/// without a herdr and two panels open in different workspaces.
pub(crate) enum Peeked {
    /// A pane to read, and what to call it in the header.
    At(String, String),
    /// The panel, when the panel is not a pane: `wsp surface`, drawn into
    /// herdr's sidebar rect by the fork. It carries no pane id because it has
    /// none — that is the property the fork exists to get — so the frame comes
    /// off disk rather than out of `pane.read`.
    Surface,
    /// The needle named something real that nothing is currently on. Carries
    /// the hint, which names what is *missing* rather than what was asked for:
    /// "no view pane" is a fact you can act on, "nothing to peek at for view"
    /// is not.
    Nothing(&'static str),
    /// The needle named nothing at all.
    Unknown,
}

/// The task half of a needle, resolved by the store: its id, its title, and
/// the panes bound to it. Supplied by the caller because the fuzzy id search is
/// the store's business and has its own tests.
pub(crate) struct Holder {
    pub id: String,
    pub title: String,
    pub panes: Vec<String>,
}

/// Read a needle against what herdr reported.
///
/// `here` is the workspace to prefer — the caller's, or whatever `--workspace`
/// said. A label is preferred within it and accepted outside it, because there
/// is a panel in every workspace and the one you mean is almost always yours,
/// but peeking at another workspace's is a real thing to want.
///
/// `surface` is whether a `wsp surface` is drawing the sidebar on this
/// machine. A fact about processes rather than about panes, so it is handed in
/// for the same reason the rest of this is a function: what "the panel" means
/// under the fork is a rule with a test, and asking a process list is not.
///
/// The task lookup arrives as a closure rather than a value because it is only
/// reached by the last arm. `find_task("")` matches every open task by suffix,
/// so resolving it up front to hand in would be a fuzzy search over the whole
/// store on every `wsp peek` — the commonest call, which names no task at all.
pub(crate) fn peek_target(
    panes: &[herdr::Pane],
    here: Option<&str>,
    me: Option<&str>,
    surface: bool,
    needle: &str,
    holder: impl FnOnce(&str) -> Option<Holder>,
) -> Peeked {
    let mine = |label: &str| {
        panes
            .iter()
            .find(|p| p.label == label && Some(p.workspace_id.as_str()) == here)
            .or_else(|| panes.iter().find(|p| p.label == label))
            .map(|p| p.pane_id.clone())
    };
    let named = |pane: Option<String>, what: &str, hint: &'static str| match pane {
        Some(id) => Peeked::At(id, what.to_string()),
        None => Peeked::Nothing(hint),
    };
    const NOTHING_HOLDS: &str = "nothing holds that — `wsp wip` says who holds what";

    match needle {
        // The common case, and the reason there is a default at all: "what
        // does the sidebar look like right now".
        //
        // A surface wins outright rather than being tried second. Under the
        // fork the sidebar *is* the surface — a child of herdr, in no
        // workspace — and a panel pane standing beside one is either a husk an
        // older wsp left or somebody deliberately comparing the two, which is
        // what `panel install` says when it makes one. Either way it is not
        // what the person is looking at, and it stays reachable by its pane id.
        "" | "panel" if surface => Peeked::Surface,
        "" | "panel" => named(
            mine(crate::panel::PANEL_LABEL),
            "the panel",
            "no panel in this workspace — `wsp panel install`",
        ),
        "view" | "detail" => named(
            mine(crate::panel::VIEW_LABEL),
            "the view",
            "no view pane open — `↵` on a row in the panel opens one",
        ),
        "board" | "kanban" => named(mine(crate::panel::BOARD_LABEL), "the board", NOTHING_HOLDS),
        "full" | "fullscreen" => {
            named(mine(crate::panel::FULL_LABEL), "the whole tree", NOTHING_HOLDS)
        }
        "me" => named(me.map(str::to_string), "this pane", NOTHING_HOLDS),
        // A pane id, given as herdr writes them.
        n if n.contains(':') && panes.iter().any(|p| p.pane_id == n) => {
            Peeked::At(n.to_string(), format!("pane {n}"))
        }
        // Otherwise a task: whichever pane holds it. This is how you look at
        // what another agent is doing without knowing where it is sitting.
        n => match holder(n) {
            Some(t) => named(
                t.panes.into_iter().next(),
                &format!("{} — {}", t.id, util::truncate(&t.title, 44)),
                NOTHING_HOLDS,
            ),
            None => Peeked::Unknown,
        },
    }
}

pub fn peek(store: &Store, args: &Args) -> i32 {
    if !herdr::available() {
        eprintln!("wsp: no herdr socket");
        return 1;
    }
    let env = herdr::Env::read();
    let here = args.get("workspace").or(env.workspace_id.clone());
    let panes = herdr::panes().unwrap_or_default();
    let needle = args.rest.first().cloned().unwrap_or_default();

    let (pane, what) = match peek_target(
        &panes,
        here.as_deref(),
        env.pane_id.as_deref(),
        crate::daemon::surface_drawing(&store.state),
        &needle,
        |n| {
            store.find_task(n).map(|t| Holder {
                panes: store.panes_for_task(&t.id),
                id: t.id,
                title: t.title,
            })
        },
    ) {
        Peeked::At(pane, what) => (pane, what),
        Peeked::Surface => return peek_surface(store, args),
        Peeked::Nothing(hint) => {
            eprintln!("wsp: {hint}");
            return 1;
        }
        Peeked::Unknown => {
            eprintln!("wsp: no pane, task or project matching `{needle}`");
            return 1;
        }
    };

    // `visible` is what is on the pane now, which is what "what does it look
    // like" means. `recent` reaches back through what has scrolled past, for
    // when the question is what happened rather than what is showing.
    let source = args.get("source").unwrap_or_else(|| "visible".into());
    let mut params = json!({ "pane_id": pane, "source": source, "format": "text" });
    if let Some(n) = args.get("lines").and_then(|l| l.parse::<u64>().ok()) {
        params["lines"] = json!(n);
    }
    let Ok(r) = herdr::call("pane.read", params) else {
        eprintln!("wsp: herdr would not read {pane}");
        return 1;
    };
    let body = read_body(&r);
    let text = body.get("text").and_then(|t| t.as_str()).unwrap_or("");

    if args.json() {
        println!("{}", serde_json::to_string_pretty(&json!({
            "pane": pane, "what": what, "source": source, "text": text,
            "truncated": body.get("truncated").and_then(|t| t.as_bool()).unwrap_or(false),
        })).unwrap_or_default());
        return 0;
    }

    let p = Paint::new();
    println!("{}", p.dim(&format!("{what}  ({pane})")));
    if text.trim().is_empty() {
        println!("{}", p.dim("(nothing on it)"));
    } else {
        println!("{}", text.trim_end());
    }
    0
}

/// The sidebar, when the sidebar is a surface.
///
/// The whole of `peek`'s job is to put the picture a person is looking at in
/// front of an agent that cannot see it, and every other target reaches that
/// picture by asking herdr to read a pane. A surface has no pane, so it leaves
/// each frame it draws beside the bindings instead, and this prints that — see
/// `panel::surface_frame`, which carries the argument for why it is the frame
/// the host was handed rather than a fresh one drawn here.
///
/// Two things go in the header that a pane's does not, and both are about
/// whether to trust what follows. The **cells** it was built for, because half
/// of what this is used to find is a row costing more of them than it should;
/// and the **age**, because a surface writes only frames that changed, so a
/// stamp from ten minutes ago means the picture has not moved — which is
/// either a quiet machine or exactly the bug being looked for, and the reader
/// is the one who can tell those apart.
///
/// `--source` and `--lines` are not read here. They reach back through what has
/// scrolled past a pane, and there is no scrollback behind a surface: one
/// frame is all there has ever been.
fn peek_surface(store: &Store, args: &Args) -> i32 {
    let Some(frame) = crate::panel::surface_frame(&store.state) else {
        // A running surface with no frame on disk is a wsp older than this
        // build, or one that has not painted yet — a tick settles either — and
        // saying so is more use than falling back to a pane nobody is looking
        // at.
        eprintln!(
            "wsp: a surface is drawing the sidebar but has left no frame — an older wsp, or it has not painted yet"
        );
        return 1;
    };
    let age = util::since(&frame.at);
    let text = frame.lines.join("\n");

    if args.json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                // Null rather than absent: a reader that keys on `text` should
                // not have to know which kind of panel answered, and one that
                // does care is told outright.
                "pane": Value::Null,
                "surface": true,
                "what": "the surface",
                "source": "frame",
                "cols": frame.cols,
                "rows": frame.rows,
                "at": frame.at,
                "age": age,
                "text": text,
                // A frame is the whole picture by construction: there is no
                // scrollback behind a surface to have cut anything out of.
                "truncated": false,
            }))
            .unwrap_or_default()
        );
        return 0;
    }

    let p = Paint::new();
    println!(
        "{}",
        p.dim(&format!(
            "the surface  ({}×{}, drawn {} ago)",
            frame.cols,
            frame.rows,
            util::duration_human(age)
        ))
    );
    if text.trim().is_empty() {
        println!("{}", p.dim("(nothing on it)"));
    } else {
        println!("{}", text.trim_end());
    }
    0
}

pub fn sync_once(store: &Store, args: &Args) -> i32 {
    if !herdr::available() {
        eprintln!("wsp: no herdr socket at {}", herdr::socket_path().display());
        return 1;
    }
    // A one-shot sync always forces: there is no warm cache to trust.
    let mut cache = sync::Cache::default();
    match sync::sync(store, &mut cache, true) {
        Ok(r) => {
            if args.json() {
                println!(
                    "{}",
                    json!({ "workspaces": r.workspaces, "panes": r.panes, "reaped": r.reaped })
                );
            } else {
                println!(
                    "synced {} workspaces, {} panes{}",
                    r.workspaces,
                    r.panes,
                    if r.reaped > 0 { format!(", reaped {} stale binding(s)", r.reaped) } else { String::new() }
                );
            }
            0
        }
        Err(e) => {
            eprintln!("wsp: sync failed: {e}");
            1
        }
    }
}

/// Entry point for herdr `[[events]]` hooks. The event name arrives as an
/// argument, its payload in `HERDR_PLUGIN_EVENT_JSON`.
pub fn hook(store: &Store, args: &Args) -> i32 {
    let event = args
        .rest
        .first()
        .cloned()
        .or_else(|| std::env::var("HERDR_PLUGIN_EVENT").ok())
        .unwrap_or_default();
    let env = herdr::Env::read();

    match event.as_str() {
        "pane.exited" | "pane.closed" | "pane_exited" | "pane_closed" => {
            // Only the binding. The claim survives, because a pane exiting is
            // an accident of process lifetime and says nothing about whether
            // the work is still yours — this is precisely the cascade that
            // once cleared every binding on the machine at a stroke.
            if let Some(pane) = env.pane_id.clone() {
                if store.clear_binding(&pane) {
                    store.log_event("pane-exited", json!({ "pane": pane }));
                }
            }
        }
        "workspace.created" | "workspace_created" => {
            if let Some(ws) = env.workspace_id.clone() {
                crate::panel::install_if_adopted(store, &ws);
            }
        }
        "workspace.closed" | "workspace_closed" => {
            if let Some(ws) = env.workspace_id.clone() {
                store.clear_pin(&ws);
            }
        }
        _ => {}
    }

    if !herdr::available() {
        return 0;
    }
    let mut cache = sync::Cache::default();
    let _ = sync::sync(store, &mut cache, false);
    0
}

/// Lines the index would drop that HEAD has and the files on disk still have.
///
/// Both arguments are `git diff --numstat` output: the first HEAD against the
/// index, the second HEAD against the working tree. Per path, the index's
/// deletions minus the disk's are what staging that path would throw away and
/// nothing would still hold — the disk's own deletions are somebody's edit,
/// which is theirs to make.
///
/// That subtraction is what keeps this quiet for the ordinary case. An agent
/// halfway through the commit procedure has staged what it wrote, so the index
/// and the disk delete the same lines and the difference is zero; it stays zero
/// while they carry on editing, because everything the index drops the disk
/// drops too. Only an index holding something *older* than both — a `read-tree`
/// or a `reset` that landed on the shared file — deletes what the disk still
/// has.
fn index_loss(staged: &str, disk: &str) -> u64 {
    let deletions = |numstat: &str| -> std::collections::BTreeMap<String, u64> {
        numstat
            .lines()
            .filter_map(|l| {
                let mut f = l.split('\t');
                let (_add, del, path) = (f.next()?, f.next()?, f.next()?);
                // `-` for a binary file, where git counts no lines at all.
                Some((path.to_string(), del.parse::<u64>().ok()?))
            })
            .collect()
    };
    let disk = deletions(disk);
    deletions(staged)
        .iter()
        .map(|(path, del)| del.saturating_sub(disk.get(path).copied().unwrap_or(0)))
        .sum()
}

/// The same question, asked of a working tree.
///
/// `GIT_INDEX_FILE` is stripped deliberately. The commit procedure has every
/// agent working through a private index, so a `doctor` run from inside one
/// would otherwise inspect that agent's own staging and pronounce the shared
/// index — the one that is actually loaded — healthy.
///
/// `--no-renames` for the same reason a rename would break it: numstat writes
/// a rename as `old => new` in one field, which matches nothing in the other
/// listing and would count the whole file as loss.
fn tree_index_loss(root: &std::path::Path) -> Option<u64> {
    let git = |args: &[&str]| -> Option<String> {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env_remove("GIT_INDEX_FILE")
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    };
    // One call answers three questions at once: a directory that is not a git
    // repo fails here, and so does a repo with no commit to be behind.
    let staged = git(&["diff", "--cached", "--numstat", "--no-renames"])?;
    if staged.trim().is_empty() {
        return Some(0);
    }
    let disk = git(&["diff", "--numstat", "--no-renames", "HEAD"])?;
    Some(index_loss(&staged, &disk))
}

/// What is sitting in the store that no command wrote.
///
/// Every `wsp` command commits the files it writes, so whatever is left is a
/// hand edit — `agents.md` is the usual one, a hook script the other. That
/// used to be invisible because the next command along swept it into its own
/// commit; now it waits, correctly, for the person who made it, and the only
/// thing missing is anybody being told.
fn store_uncommitted(root: &std::path::Path) -> Vec<String> {
    let Ok(out) = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["status", "--porcelain"])
        .env_remove("GIT_INDEX_FILE")
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.get(3..).map(|p| p.trim().to_string()))
        .filter(|p| !p.is_empty())
        .collect()
}

/// What asking herdr got us, as `doctor` needs to distinguish it.
///
/// Three answers, not two, and telling them apart is the whole of what this
/// part of `doctor` is for. A socket that is not there is a normal machine. A
/// socket that is there and will not answer is broken. A socket that answers
/// with nothing is a herdr running with no agents in it — which reads exactly
/// like the second one if you only look at the length of the list, and is the
/// confusion the reap guard exists to survive.
///
/// `Up` carries both listings, because neither alone can tell a pane that is
/// gone from a pane whose agent is. `agent.list` drops a pane the moment the
/// process inside it exits; `pane.list` keeps it, because the pane is still
/// there holding the shell the agent was started from. Asking only the first
/// is what made `doctor` call an emptied pane a dead one — see [`Bound`].
pub(crate) enum Probe {
    Down,
    Unreachable(String),
    Up { agents: Vec<herdr::Pane>, panes: Vec<herdr::Pane> },
}

impl Probe {
    pub(crate) fn live() -> Probe {
        if !herdr::available() {
            return Probe::Down;
        }
        match herdr::agents() {
            // A pane listing that fails leaves `panes` empty, which reads below
            // as "no machine answered" and says nothing about any binding.
            // Silence is not evidence, and this is the judgement `sync` already
            // makes before it reaps — the two have to agree, or `doctor` is back
            // to advising a sweep that will not happen.
            Ok(agents) => Probe::Up { agents, panes: herdr::panes().unwrap_or_default() },
            Err(e) => Probe::Unreachable(e.to_string()),
        }
    }
}

/// Which of five states a bound pane is in, and the two in the middle are the
/// ones a census kept confusing with work.
///
/// `doctor` used to ask a single question — "is this pane in `agent.list`" —
/// and report every `No` as a dead pane needing `wsp sync`. `sync` reaps
/// against `pane.list`, so for the commoner `No` of the two it correctly reapt
/// nothing, and `doctor` printed the same line on the next run, and the run
/// after that, indefinitely. The fault was the diagnosis rather than the sweep.
///
/// So [`Bound::Emptied`] was the first name for a state that had none: the pane
/// is alive and the agent inside it exited. That is not an absent pane, it is an
/// *emptied* one, and it is worth showing rather than reaping — it means an
/// agent stopped mid-task with its claim still held and its worktree still on
/// disk.
///
/// **[`Bound::Quiet`] is the second, and it is the one robustness-083 was
/// opened for.** Every `Yes` to that single question was called running,
/// because a row in `agent.list` was taken for work happening. It is not: on
/// 2026-08-18 an API overload left seven agents with their processes alive,
/// their conversations intact and their turns abandoned, and `doctor` counted
/// all seven and said `herdr up, 12 agents`. Nothing had been written to a task
/// in hours. The reading that separates them was on the socket the whole time
/// (`place::State::turn_in_flight`), and this asks it.
pub(crate) enum Bound {
    /// A turn is in flight. This, and only this, is work happening.
    Turning,
    /// An agent is in the pane and no turn is running. Finished, stopped, or
    /// waiting on a person — see [`State::turn_in_flight`] for why this does not
    /// try to say which, and [`quiet_note`] for what makes it worth reporting.
    Quiet(State),
    /// The pane is listed and no agent is in it. `despawn`, then decide.
    Emptied,
    /// The pane is not listed, and its machine did answer. `sync` reaps it.
    Gone,
    /// The machine this pane is on said nothing, so nothing is known.
    Unheard,
}

pub(crate) fn bound_state(
    pane: &str,
    agents: &[herdr::Pane],
    panes: &[herdr::Pane],
    answered: &std::collections::BTreeMap<&str, usize>,
) -> Bound {
    if let Some(a) = agents.iter().find(|a| a.pane_id == pane) {
        // The status alone. See `place_herdr::state_of_pane` for why a census
        // must not qualify this by readiness.
        let state = crate::place_herdr::state_of_pane(a);
        return match state.turn_in_flight() {
            true => Bound::Turning,
            false => Bound::Quiet(state),
        };
    }
    if panes.iter().any(|p| p.pane_id == pane) {
        return Bound::Emptied;
    }
    match may_reap(answered, pane) {
        true => Bound::Gone,
        false => Bound::Unheard,
    }
}

/// How long a claim may sit with no turn running before it is worth a line.
///
/// An hour, and it is the task's own number rather than a tuned one. The point
/// of the conjunction below is that neither half needs a fine threshold: an
/// agent between turns is quiet for seconds, and an agent that has written
/// nothing for an hour is normally deep in a turn. It takes both to be wrong.
const QUIET: i64 = 60 * 60;

/// What to say, if anything, about a bound pane with no turn in it.
///
/// **The conjunction is the whole design, and it is why this needs no screen
/// and no new state.** Either half alone is ordinary:
///
/// - *no turn in flight* is every agent between turns, and every agent that has
///   finished and is waiting to be told the next thing;
/// - *nothing written to the task for an hour* is every agent halfway through a
///   long read, which is exactly when it should be left alone.
///
/// Together they are never ordinary. An agent that is running no turn will not
/// start one on its own — whatever ended the last one, nothing in this system
/// resumes it — so a claim in that state is a slot spending money and producing
/// nothing, and it stays that way until a person looks. That is the fact wsp
/// already had in two files it already reads, and never put side by side.
///
/// [`State::Blocked`] skips the hour, because it is the one case where the
/// repair is a keystroke and the report is the only thing that can summon
/// somebody to press it. Waiting an hour to mention a permission prompt would
/// be waiting an hour to say a word.
///
/// **What this does not do is guess why the turn ended.** Finished, stalled and
/// overloaded look identical from here, and a diagnosis wsp cannot support
/// would be worth less than the fact it can state: the agent is not working,
/// and nothing has been written since *then*. A person reads that in a second.
///
/// The *status* it can read, and that is what picks the verb rather than the
/// sentence. A task still `doing` is work that stopped in the middle, and the
/// thing to try first is a word — the conversation is intact, and it was
/// despawn-and-respawn that threw hours of unrecorded reading away on
/// 2026-08-18. A task already in `review` is work that finished; the agent is
/// not stalled, it is *done*, and what is wrong is that it is still holding a
/// claim and a slot. Measured on this machine the moment this check first ran:
/// four claims quiet for between five and nine hours, and every one of them the
/// second kind. A check that had offered `wsp tell` for all four would have
/// been ignored by the second afternoon.
fn quiet_note(state: State, status: Status, since: Option<&str>) -> Option<String> {
    if matches!(state, State::Blocked) {
        return Some("stopped on a prompt only a person can answer — `wsp peek <id>` shows what it is asking".into());
    }
    let quiet = util::since(since?);
    if quiet < QUIET {
        return None;
    }
    let for_ = util::duration_human(quiet);
    Some(match status {
        // Finished, and still in the chair. Nothing is broken and a slot is
        // gone, so the verb is the one that gives it back.
        Status::Review | Status::Done => {
            format!("finished {for_} ago and still holding the claim — `wsp release <id>` frees the slot")
        }
        // Waiting on somebody, which is a fact about the work rather than about
        // the agent. Sitting idle is the correct thing for it to be doing.
        Status::Blocked | Status::Parked => return None,
        _ => format!(
            "no turn running, and nothing written for {for_} — `wsp tell <id> …` reaches it without ending it"
        ),
    })
}

/// A few of the names in a line, with the rest counted.
///
/// A machine that lost eight agents at once is exactly when this line matters,
/// and exactly when naming all eight would bury the sentence after it.
fn few(named: &[String]) -> String {
    const SHOWN: usize = 4;
    let head = named.iter().take(SHOWN).cloned().collect::<Vec<_>>().join(", ");
    match named.len() > SHOWN {
        true => format!("{head} and {} more", named.len() - SHOWN),
        false => head,
    }
}

/// What `doctor` says about herdr, and about the bindings that outlived it.
///
/// `tasks` is here for one reason: an agent's last turn is a fact about herdr
/// and its last *output* is a fact about the store, and the check this function
/// gained needs both in the same place. See [`quiet_note`].
fn herdr_health(
    probe: &Probe,
    bindings: &std::collections::BTreeMap<String, serde_json::Value>,
    tasks: &[Task],
    problems: &mut Vec<String>,
    notes: &mut Vec<String>,
) {
    match probe {
        Probe::Up { agents, panes } => {
            let answered = answered_by_machine(panes.iter().map(|p| p.pane_id.as_str()));
            let (mut emptied, mut gone) = (Vec::new(), Vec::new());
            let (mut quiet, mut turning) = (Vec::new(), 0usize);
            for (pane, b) in bindings {
                // Named by the work rather than by the pane, because the work
                // is what the reader has to decide about and what the verb
                // underneath takes.
                let task = b.get("task_id").and_then(|x| x.as_str()).unwrap_or("");
                let named = match task.is_empty() {
                    true => pane.clone(),
                    false => format!("{task} ({pane})"),
                };
                match bound_state(pane, agents, panes, &answered) {
                    Bound::Emptied => emptied.push(named),
                    Bound::Gone => gone.push(named),
                    Bound::Turning => turning += 1,
                    Bound::Quiet(state) => {
                        // The later of the two, because either one moving is
                        // the work advancing: `started_at` covers an agent put
                        // on a task that already had a long history, and
                        // `updated` covers everything after that.
                        let started = b.get("started_at").and_then(|x| x.as_str());
                        let touched = tasks.iter().find(|t| t.id == task).map(|t| t.updated.as_str());
                        let last = [started, touched].into_iter().flatten().max();
                        let status = tasks
                            .iter()
                            .find(|t| t.id == task)
                            .map(|t| t.status())
                            .unwrap_or(Status::Doing);
                        if let Some(why) = quiet_note(state, status, last) {
                            quiet.push(format!("{named} — {why}"));
                        }
                    }
                    Bound::Unheard => {}
                }
            }
            if !emptied.is_empty() {
                notes.push(format!(
                    "{} pane(s) alive with the agent gone — {} — claim and worktree \
                     still held, and `wsp sync` will not touch these. `wsp despawn <id>` \
                     gives one up; `wsp spawn <id>` puts a fresh agent on it",
                    emptied.len(),
                    few(&emptied)
                ));
            }
            if !gone.is_empty() {
                notes.push(format!(
                    "{} binding(s) on panes herdr no longer lists — `wsp sync` reaps them",
                    gone.len()
                ));
            }
            // A problem rather than a note, which is the whole point of the
            // task this came from: it was a note-shaped failure — everything
            // reporting healthy — that cost a night. The conversation in each
            // of these is intact, so `wsp tell` is the first thing to try and
            // `wsp despawn` the last.
            problems.extend(quiet.iter().cloned());
            // Counted rather than listed, and the count is the correction. The
            // old line said "12 agents" of a machine on which five were turning
            // and seven had stopped, and read as health.
            notes.push(format!(
                "herdr up, {} agents — {turning} of {} claimed pane(s) running a turn",
                agents.len(),
                bindings.len()
            ));
        }
        Probe::Unreachable(e) => problems.push(format!("herdr socket present but unreachable: {e}")),
        // Not a problem. A machine with no herdr on it is a machine wsp works
        // on, and calling that broken is how a check nobody can act on gets
        // ignored along with the ones they can.
        Probe::Down => notes
            .push("herdr socket not found (CLI still works, sidebar tokens will not update)".into()),
    }
}

pub fn doctor(store: &Store, args: &Args) -> i32 {
    let mut problems: Vec<String> = Vec::new();
    let mut notes: Vec<String> = Vec::new();

    if !store.exists() {
        problems.push(format!("no store at {} — run `wsp init`", util::contract(&store.root)));
    } else {
        notes.push(format!("store {}", util::contract(&store.root)));
    }
    if !store.root.join(".git").exists() {
        notes.push("store is not a git repo (history disabled)".into());
    } else {
        let loose = store_uncommitted(&store.root);
        if !loose.is_empty() {
            let named: Vec<&str> = loose.iter().take(3).map(|s| s.as_str()).collect();
            let rest = match loose.len() > named.len() {
                true => format!(" and {} more", loose.len() - named.len()),
                false => String::new(),
            };
            notes.push(format!(
                "uncommitted in the store: {}{rest} — wsp commits what it writes, so these are yours to commit",
                named.join(", ")
            ));
        }
    }

    let index = Index::new(store.projects());
    let tasks = store.tasks();
    notes.push(format!("{} projects, {} tasks", index.projects.len(), tasks.len()));

    for p in &index.projects {
        if let Some(parent) = &p.parent {
            if index.get(parent).is_none() {
                problems.push(format!("project {} has missing parent `{}`", p.id, parent));
            }
        }
        // Cycle detection: walking ancestors returns to self.
        if index.ancestors(&p.id).contains(&p.id) {
            problems.push(format!("project {} is in a parent cycle", p.id));
        }
        for r in &p.roots {
            if !util::expand(r).exists() {
                problems.push(format!("project {} root does not exist: {}", p.id, r));
            }
        }
    }

    // A loaded index in a tree agents share.
    //
    // `git add` writes to one `.git/index` for everybody standing in a tree,
    // and whoever commits takes whatever is in it — which the commit procedure
    // answers by having each agent work through a private `GIT_INDEX_FILE`. The
    // failure that leaves behind is silent: one `read-tree` or `reset` run
    // without that variable set puts an *older* tree in the shared index, where
    // it sits looking like nothing at all until somebody who skipped the
    // procedure — or a person at a shell — runs a plain `git commit` and takes
    // it. On 2026-08-15 that was 4,962 lines behind HEAD in `~/claude/wsp` for
    // most of an afternoon, and every fact needed to say so was already here.
    //
    // Declared roots only. wsp knows where a project's work lives because
    // somebody said so, and a check that went hunting for git repositories
    // would be reporting on trees nobody asked it about.
    let mut seen: Vec<std::path::PathBuf> = Vec::new();
    // Read once, not once per root: it walks the task directory and the archive.
    let closed = crate::cmd_checkout::finished(store);
    // And the other evidence a tree can be finished with on, read once for the
    // same reason. `doctor` reports rather than removes, but it has to be
    // answering the same question the sweep acts on — a tree called litter here
    // and left alone there is how eighteen of them came to be sitting in one
    // repository with nobody able to say which were safe to take.
    let passed = crate::worklist::passed_by_running(store);
    for p in &index.projects {
        for r in &p.roots {
            let root = util::real(r);
            if seen.contains(&root) || !root.exists() {
                continue;
            }
            seen.push(root.clone());
            // A tree per task leaks the same way a tree per build did, and for
            // the same reason: landing no longer removes anything, `--rm` is a
            // command somebody has to remember, and this is what is left when
            // nobody did. A note rather than a problem — nothing is broken, it
            // is disk — and never removed from here, because `doctor` looks at
            // every declared root and a command that deletes directories should
            // only ever act on the one you are standing in.
            for s in crate::cmd_checkout::stale(&root, &closed, &|t| passed.contains(t)) {
                // A seat still bound to the task changes the advice rather than
                // the finding: the tree is litter either way, but a `--rm`
                // there leaves the agent and the workspace behind it.
                let seated = !store.panes_for_task(&s.task).is_empty();
                notes.push(format!(
                    "{}: the tree for {} is finished with — {} — {}",
                    util::contract(&root),
                    s.task,
                    s.note,
                    s.why.fix(&s.task, seated)
                ));
            }
            match tree_index_loss(&root) {
                Some(lost) if lost > 0 => {
                    problems.push(format!(
                        "{}: the staged index would drop {lost} line(s) that HEAD has and the files still have",
                        util::contract(&root)
                    ));
                    problems.push(
                        "  a plain `git commit` there takes it as it stands — `git read-tree HEAD`, with no GIT_INDEX_FILE set, puts it back".into(),
                    );
                }
                _ => {}
            }
        }
    }

    // The fifth piece of shared mutable state, and the last one nothing was
    // watching: `refs/stash`, one stack per repository however many worktrees
    // stand in it. The guard that refuses a new one lives in git rather than
    // here — `crate::guard` says why — so what is left for doctor is whether
    // each root has it, and whether anything is sitting on the stack now.
    crate::guard::health(
        &seen,
        crate::guard::state,
        crate::guard::stashed,
        &crate::guard::git_version(),
        &mut problems,
        &mut notes,
    );

    // And the binary those trees were built into, which is the one file none
    // of the checks above can see: the stamp travels in the bytes, so it is
    // asked of the installed wsp rather than worked out from a tree.
    crate::cmd_install::health(&seen, &mut notes);

    for t in &tasks {
        if let Some(proj) = &t.project {
            if index.get(proj).is_none() {
                problems.push(format!("task {} references unknown project `{}`", t.id, proj));
            }
        }
        if t.title.trim().is_empty() {
            problems.push(format!("task {} has an empty title", t.id));
        }
        if let Some(parent) = &t.parent {
            if !tasks.iter().any(|x| &x.id == parent) {
                problems.push(format!("task {} references unknown parent `{}`", t.id, parent));
            }
            if parent == &t.id {
                problems.push(format!("task {} is its own parent", t.id));
            }
        }
        // A loop resolves at every step, so the "unknown parent" check above
        // sees nothing wrong while the tree hangs a row beneath itself. The
        // one-step case is already named above, and naming it twice reads as
        // two faults.
        let mut walk = t.parent.clone().filter(|p| p != &t.id);
        let mut seen: Vec<String> = vec![t.id.clone()];
        while let Some(id) = walk {
            if seen.contains(&id) {
                if id == t.id {
                    problems.push(format!("task {} is in a parent cycle", t.id));
                }
                break;
            }
            seen.push(id.clone());
            walk = tasks.iter().find(|x| x.id == id).and_then(|x| x.parent.clone());
        }
    }

    // One name, two pieces of work. Nothing downstream can tell them apart —
    // `show` answers with the live one while the log, the claim and any
    // `parent` pointing at it describe both.
    let archived = store.archived_ids();
    let archived_tasks = store.archived_tasks();
    let claims = store.claims();
    let worked = store.worked();
    for t in &tasks {
        if !archived.contains(&t.id) {
            continue;
        }
        problems.push(format!(
            "task {} has the same id as an archived task — one of them needs renumbering",
            t.id
        ));
        // The blast radius, spelled out. Everything keyed on an id inherits
        // whatever else wore it: a claim made by the archived task now names
        // the live one, and nothing in the record says which it meant.
        let mut carried: Vec<&str> = Vec::new();
        if claims.contains_key(&t.id) {
            carried.push("a claim");
        }
        if worked.contains_key(&t.id) {
            carried.push("a worked record");
        }
        if !carried.is_empty() {
            problems.push(format!(
                "  …and {} still keyed on it, which may belong to either",
                carried.join(" and ")
            ));
        }
    }

    // Two projects numbering in one space is the same failure a step earlier:
    // nothing has collided yet, but the next `wsp add` in either of them will
    // reach for a number the other has already used.
    let mut codes: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for p in &index.projects {
        codes.entry(p.code().to_string()).or_default().push(p.id.clone());
    }
    for (code, owners) in &codes {
        if owners.len() > 1 {
            problems.push(format!(
                "projects {} all number under `{code}-` — `wsp code <project> <new>` separates them",
                owners.join(", ")
            ));
        }
        if code == crate::store::INBOX_CODE {
            problems.push(format!(
                "project {} numbers under `{code}-`, which the inbox already uses",
                owners.join(", ")
            ));
        }
    }

    // A counter that has fallen behind costs one directory scan and is not
    // worth alarming anybody about; one that has run *ahead* silently skips
    // numbers for ever, and is the only direction worth reporting.
    for p in &index.projects {
        let top = tasks
            .iter()
            .chain(archived_tasks.iter())
            .filter_map(|t| t.id.strip_prefix(&format!("{}-", p.code())))
            .filter_map(|n| n.parse::<usize>().ok())
            .max()
            .unwrap_or(0);
        if p.seq > top {
            notes.push(format!(
                "{} counts from {} but its highest id is {top} — {} number(s) will be skipped",
                p.id,
                p.seq,
                p.seq - top
            ));
        }
    }

    // Ids still in the dated scheme, which is a note and not a problem: a
    // part-migrated store works, it is only harder to read.
    let dated = tasks.iter().filter(|t| crate::cmd_migrate::is_dated(&t.id)).count();
    if dated > 0 {
        notes.push(format!(
            "{dated} task(s) still carry dated ids — `wsp migrate -n` shows what they would become"
        ));
    }

    // The bridge from old ids to new is only a bridge while both ends stand.
    for (from, to) in store.renamed_ids() {
        if store.task(&to).is_none() && !archived.contains(&to) {
            problems.push(format!(
                "ids.json says {from} became {to}, and there is no {to} — every old reference to it now resolves to nothing"
            ));
        }
    }

    let bindings = store.bindings();
    for (pane, b) in &bindings {
        let id = b.get("task_id").and_then(|x| x.as_str()).unwrap_or("");
        if store.task(id).is_none() {
            problems.push(format!("binding on {pane} points at missing task `{id}`"));
        }
    }

    section_damage(store, args, &tasks, &index.projects, &mut problems, &mut notes);

    let probe = Probe::live();
    herdr_health(&probe, &bindings, &tasks, &mut problems, &mut notes);
    // Detection rules herdr is taking from this machine instead of from
    // upstream. Silent on a machine with none, which is every machine until
    // somebody has a fault worth shadowing upstream's fixes to fix.
    crate::detect_override::health(
        &crate::detect_override::dirs(),
        crate::detect_override::ask,
        &mut problems,
        &mut notes,
    );
    // Whether the machine has the one daemon it should have. The probe is passed
    // because a machine with no herdr on it wants no daemon either, and a check
    // that said "no daemon running" there is a check that gets ignored along
    // with the ones that mean something.
    crate::daemon::health(
        store.daemon_holder().map(|(pid, _)| pid),
        crate::daemon::running(&store.state).as_deref(),
        matches!(probe, Probe::Up { .. }),
        &mut problems,
        &mut notes,
    );
    // And whether anything that said it was reporting still is. The only check
    // here whose subject is a *reporter* rather than a piece of work, and the
    // only place in wsp that can tell a fleet with nothing to say from a
    // watcher that stopped saying it.
    crate::cmd_watch::health(store, &mut problems);
    // And whether the one thing that reports with nobody looking has anywhere
    // to report *to*. The pass is the fix for "nothing notices unattended" and
    // an empty `hooks/` would make it a fix that notices and tells no one.
    crate::attention::health(store, &mut notes);

    if args.json() {
        println!("{}", json!({ "problems": problems, "notes": notes }));
        return if problems.is_empty() { 0 } else { 1 };
    }

    let p = Paint::new();
    for n in &notes {
        println!("{} {}", p.dim("·"), n);
    }
    for prob in &problems {
        println!("{} {}", p.red("✗"), prob);
    }
    if problems.is_empty() {
        println!("{} no problems", p.green("✓"));
        0
    } else {
        1
    }
}

/// Bodies carrying headings the schema does not know about — reported always,
/// repaired with `--fix`.
///
/// `wsp edit --overview` used to store a payload's own `## ` headings verbatim,
/// and the next read took them for sibling sections. Three things follow, in
/// order of how much they cost: the body is split into sections nobody wrote;
/// rewriting the same brief adds a second copy of all of them, because
/// `split_sections` rightly keeps every heading it does not recognise; and once
/// one of them sorts after `## Log`, every entry appended since has gone under
/// it, where nothing will ever read it.
///
/// The write no longer does that. This is for the bodies written before it
/// stopped — and there is no other way back, since no command deletes a section
/// it does not know about.
fn section_damage(
    store: &Store,
    args: &Args,
    tasks: &[Task],
    projects: &[crate::model::Project],
    problems: &mut Vec<String>,
    notes: &mut Vec<String>,
) {
    let fix = args.has("fix");
    let mut swallowing: Vec<String> = Vec::new();
    let mut split: Vec<String> = Vec::new();
    let mut fixed: Vec<String> = Vec::new();

    let bodies: Vec<(&str, &str, &str)> = tasks
        .iter()
        .map(|t| ("task", t.id.as_str(), t.body.as_str()))
        .chain(projects.iter().map(|p| ("project", p.id.as_str(), p.body.as_str())))
        .collect();

    for (what, id, body) in bodies {
        if crate::model::stray_sections(body).is_empty() {
            continue;
        }
        // A displaced log is the half that is actively losing entries; the rest
        // is a body that reads oddly and would duplicate on the next rewrite.
        let heads = crate::model::headings(body);
        match heads.iter().any(|h| h == "Log") && heads.last().map(|h| h.as_str()) != Some("Log") {
            true => swallowing.push(id.to_string()),
            false => split.push(id.to_string()),
        }
        if !fix {
            continue;
        }
        // Folded from the copy on disk rather than the one scanned above. The
        // store is shared and a repair pass over thirty files takes long enough
        // for a note or a claim to land in the middle of it; folding what was
        // read at the start would write that away again.
        let fresh = match what {
            "project" => store.project(id).map(|p| p.body),
            _ => store.task(id).map(|t| t.body),
        };
        let Some(fresh) = fresh else {
            problems.push(format!("{what} {id} disappeared while repairing it"));
            continue;
        };
        // Nothing left to fold means somebody else got there first.
        let Some(folded) = crate::model::fold_stray_sections(&fresh) else {
            continue;
        };
        let saved = match what {
            "project" => store.project(id).map(|mut p| {
                p.body = folded;
                store.save_project(&p)
            }),
            _ => store.task(id).map(|mut t| {
                t.body = folded;
                // The fold moves prose, and lifts swallowed entries back into
                // the log. A change to a body that nobody can see afterwards is
                // the failure this whole task is about, so it says so in the
                // one place that is read for that.
                t.log("sections folded back under the prose they were written in (`wsp doctor --fix`)");
                store.save_task(&t)
            }),
        };
        match saved {
            Some(Ok(())) => fixed.push(id.to_string()),
            Some(Err(e)) => problems.push(format!("{what} {id}: write failed: {e}")),
            None => problems.push(format!("{what} {id} disappeared while repairing it")),
        }
    }

    let list = |ids: &[String]| ids.join(", ");
    if !fixed.is_empty() {
        store.log_event("sections-folded", json!({ "ids": fixed }));
        store.git_commit("wsp: fold stray sections back into the prose they belong to");
        notes.push(format!("folded {} item(s): {}", fixed.len(), list(&fixed)));
        return;
    }
    if !swallowing.is_empty() {
        problems.push(format!(
            "{} task(s) have a heading sorted after `## Log` and are swallowing log entries: {} — `wsp doctor --fix` folds them back",
            swallowing.len(),
            list(&swallowing)
        ));
    }
    if !split.is_empty() {
        notes.push(format!(
            "{} item(s) carry headings outside the schema — harmless until the next `--overview` rewrite duplicates them; `wsp doctor --fix` folds them back",
            split.len()
        ));
    }
}

/// Turn live herdr workspaces into tasks the store knows about.
///
/// The old workspaces carry their meaning in a hand-typed label — "Trance
/// Video", "TET -> EIN" — and nowhere else. Closing them without reading them
/// first throws away the only record that the work exists. So: for every
/// workspace with no claim on it, propose a task in whichever project the
/// label or the cwd points at, and claim it there.
///
/// Prints a plan and does nothing unless `--yes`.
pub fn adopt(store: &Store, args: &Args) -> i32 {
    if !herdr::available() {
        eprintln!("wsp: no herdr socket");
        return 1;
    }
    let index = Index::new(store.projects());
    let pins = store.pins();
    let claims = store.claims();
    let workspaces = herdr::workspaces().unwrap_or_default();
    let panes = herdr::panes().unwrap_or_default();
    let apply = args.has("yes");

    // A label that is only the folder's name says nothing the cwd does not.
    let uninformative = |label: &str, cwd: &str| -> bool {
        let leaf = cwd.trim_end_matches('/').rsplit('/').next().unwrap_or("");
        label.eq_ignore_ascii_case(leaf) || label.is_empty()
    };

    let mut plan: Vec<(String, String, Option<String>, String)> = Vec::new();
    for w in &workspaces {
        let ws_panes: Vec<&herdr::Pane> = panes
            .iter()
            .filter(|p| p.workspace_id == w.id && p.label != crate::panel::PANEL_LABEL)
            .collect();
        let Some(pane) = ws_panes.first() else { continue };
        if claims.values().any(|c| c.get("workspace_id").and_then(|x| x.as_str()) == Some(&w.id)) {
            continue;
        }
        if uninformative(&w.label, &pane.cwd) {
            continue;
        }
        // A workspace deliberately outside the tree is not work to adopt.
        if pins.get(&w.id).map(|p| p == crate::resolve::TOP_LEVEL).unwrap_or(false) {
            continue;
        }
        // Label first here, deliberately: the folder is shared by ten
        // workspaces and the label is the only thing that separates them.
        let project = index
            .project_for_label(&w.label)
            .or_else(|| index.project_for_cwd(&pane.cwd))
            .or_else(|| {
                resolve::resolve(
                    &index,
                    &pins,
                    resolve::Held::default(),
                    Some(&w.id),
                    Some(&w.label),
                    Some(&pane.cwd),
                )
                    .project
            });
        plan.push((w.id.clone(), w.label.clone(), project, pane.pane_id.clone()));
    }

    if plan.is_empty() {
        println!("nothing to adopt — every workspace is claimed or unnamed");
        return 0;
    }

    let p = Paint::new();
    for (ws, label, project, _) in &plan {
        println!(
            "{}  {}  {}",
            p.dim(&util::pad(ws, 4)),
            util::pad(label, 22),
            p.dim(project.as_deref().unwrap_or("(inbox)"))
        );
    }
    if !apply {
        println!("\n{} workspace(s). Re-run with --yes to create the tasks and claim them.", plan.len());
        return 0;
    }

    let mut made = 0;
    for (ws, label, project, pane) in &plan {
        let Ok(id) = store.alloc_task_id(project.as_deref()) else { continue };
        let mut t = crate::model::Task::new(label, &id);
        t.project = project.clone();
        t.set_status(Status::Doing);
        t.log(&format!("adopted from herdr workspace {ws}"));
        if store.save_task(&t).is_err() {
            continue;
        }
        store.set_claim(
            &t.id,
            json!({
                "workspace_id": ws,
                "workspace_label": label,
                "cwd": panes.iter().find(|p| &p.pane_id == pane).map(|p| p.cwd.clone()).unwrap_or_default(),
                "host": util::hostname(),
                "claimed_at": util::now_iso(),
            }),
        );
        store.set_binding(
            pane,
            json!({
                "task_id": t.id,
                "pane_id": pane,
                "workspace_id": ws,
                "cwd": "",
                "started_at": util::now_iso(),
                "adopted": true,
            }),
        );
        store.log_event("task-adopted", json!({ "id": t.id, "workspace": ws, "label": label }));
        made += 1;
    }
    store.git_commit(&format!("wsp: adopt {made} workspace(s)"));
    println!("adopted {made} workspace(s)");
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store of our own, with the state beside it — a raised hand lives in
    /// state, and a test that only moved `WSP_HOME` would put its flags on the
    /// machine's real panel.
    fn scratch(tag: &str) -> (crate::util::Isolated, Store) {
        let env = crate::util::isolated(tag);
        let store = Store::open();
        store.ensure_dirs().unwrap();
        (env, store)
    }

    fn a_task(store: &Store, id: &str) -> Task {
        let mut t = Task::new("the panel work, in order", id);
        t.project = Some("wsp".into());
        store.save_task(&t).unwrap();
        t
    }

    fn raise_one(store: &Store, args: &[&str], flags: &[(&str, &str)]) -> i32 {
        flag(store, &Args::synth("flag", args, flags))
    }

    /// **The fault this whole change exists for**, driven through the verb
    /// rather than through the record.
    ///
    /// `worklist-017`, found live: `store.set_flag` inserted by task id, so an
    /// agent that raised a second hand on the task it was working overwrote the
    /// first and was told nothing. `wsp flag` returned cleanly, the agent
    /// believed two hands were up, and one was. The thing lost was the request
    /// for attention itself.
    ///
    /// Asserted on the *first* hand rather than on the count alone, because a
    /// count of two would also pass if the second had replaced the first and
    /// something else had arrived.
    #[test]
    fn a_second_hand_on_one_task_does_not_replace_the_first() {
        let (_env, store) = scratch("flag-two");
        a_task(&store, "wsp-001");

        for said in ["the index is behind HEAD", "and the tree is shared"] {
            assert_eq!(raise_one(&store, &["wsp-001", said], &[]), 0);
        }

        let up = crate::message::about_task(&store, "wsp-001");
        assert_eq!(up.len(), 2, "the second raised hand replaced the first");
        assert_eq!(up[0].title(), "the index is behind HEAD", "and the one lost was the older");
        assert_eq!(up[1].title(), "and the tree is shared");
        assert_ne!(up[0].id, up[1].id, "two hands sharing an identity is the same fault again");
    }

    /// **The fault `worklist-018` exists for**, driven through the verb and
    /// asserted where the words are actually read.
    ///
    /// An agent raised a hand carrying *landed and NOT INSTALLED — the retry
    /// hazard is live* in `--body`, with no positional sentence. `wsp flag`
    /// stored it, returned success, and printed a receipt with none of it in;
    /// the seat's listing drew the task's own title, a pane and an age. Both
    /// ends reported a message neither of them had shown to anybody, and it was
    /// read four hours later out of `flags.json` by hand.
    ///
    /// Two halves, and both are needed. The writer joins the three inputs into
    /// one text so there is no empty headline to draw ([`message::compose`]);
    /// this is the other half — every surface reads that one text, first line
    /// as the headline and the rest under it, so there is no second field to
    /// prefer wrongly.
    #[test]
    fn a_hand_raised_with_only_a_body_is_drawn_where_it_is_read() {
        let (_env, store) = scratch("flag-body-only");
        a_task(&store, "wsp-001");
        let body = "landed and NOT INSTALLED — the retry hazard is live\n\nuntil somebody runs `wsp install`, every seat is on the old binary";
        assert_eq!(raise_one(&store, &["wsp-001"], &[("body", body)]), 0);

        let up = crate::message::raised(&store);
        let rows = hand_rows(&up[0], &Paint::new());
        assert!(
            rows[0].contains("landed and NOT INSTALLED"),
            "the first line of the message is the headline, wherever it was typed: {rows:?}",
        );
        assert!(
            rows.iter().any(|r| r.contains("every seat is on the old binary")),
            "and the rest of it is drawn under that: {rows:?}",
        );
    }

    /// **The fault `worklist-036` exists for**, driven through the verb.
    ///
    /// `wsp flag <id> --from FILE` wrote a record with `"text": ""`, printed a
    /// success receipt and raised a hand with the words gone. `flag` took a
    /// positional sentence, `-`, `--title` and `--body` and had no `--from`, so
    /// the parser bound the path to an option nothing read — and `--from` is
    /// the spelling `note`, `block`, `park` and `decide` all take and the one
    /// every brief tells an agent to use, because a shell evaluates backticks
    /// inside double quotes. The habit the fleet is told to form was the habit
    /// that lost the message, on the one verb whose entire job is to not lose
    /// one.
    ///
    /// Asserted on the words rather than on the exit code, because the exit
    /// code was already 0 when this was broken.
    #[test]
    fn a_hand_raised_from_a_file_carries_the_words_the_file_held() {
        let (env, store) = scratch("flag-from-file");
        a_task(&store, "wsp-001");
        let path = env.path("finding.txt");
        std::fs::write(&path, "the index is behind HEAD\n\nand the tree is shared\n").unwrap();

        assert_eq!(raise_one(&store, &["wsp-001"], &[("from", path.to_str().unwrap())]), 0);

        let up = crate::message::raised(&store);
        assert_eq!(up.len(), 1);
        let rows = hand_rows(&up[0], &Paint::new());
        assert!(
            rows[0].contains("the index is behind HEAD"),
            "the file's first line is the headline: {rows:?}",
        );
        assert!(
            rows.iter().any(|r| r.contains("and the tree is shared")),
            "and the rest of the file is under it: {rows:?}",
        );
    }

    /// A source that was given and came back empty is told about, not raised.
    ///
    /// The other road to the same silence: the path was wrong in a way the
    /// filesystem does not mind — an empty file, a pipe with nothing in it —
    /// and the caller believes a paragraph went up. `wsp flag <id>` on its own
    /// stays legal, because *look at this task* is a complete thing to say;
    /// what is refused is pointing at words and raising a hand without them.
    #[test]
    fn a_source_that_came_back_empty_is_refused_rather_than_raised_with_nothing_on_it() {
        let (env, store) = scratch("flag-from-empty");
        a_task(&store, "wsp-001");
        let path = env.path("nothing.txt");
        std::fs::write(&path, "\n").unwrap();

        assert_eq!(
            raise_one(&store, &["wsp-001"], &[("from", path.to_str().unwrap())]),
            2,
            "an empty source reported success",
        );
        assert!(
            crate::message::raised(&store).is_empty(),
            "and a hand went up with nothing behind it",
        );
    }

    /// Two sources for one paragraph is a caller who does not know what they
    /// asked for, and choosing one of them is this task's own fault in a
    /// smaller hat — the message that lost is lost silently.
    #[test]
    fn a_paragraph_given_twice_is_refused_rather_than_one_of_them_dropped() {
        let (env, store) = scratch("flag-from-and-body");
        a_task(&store, "wsp-001");
        let path = env.path("finding.txt");
        std::fs::write(&path, "out of the file\n").unwrap();

        assert_eq!(
            raise_one(
                &store,
                &["wsp-001"],
                &[("body", "on the command line"), ("from", path.to_str().unwrap())],
            ),
            2,
        );
        assert!(crate::message::raised(&store).is_empty(), "one of the two went up anyway");
    }

    /// `wsp flag <id>` on its own draws no words, because there are none.
    ///
    /// The cheap version has to stay cheap — *look at this task, it exists* is a
    /// complete thing for an agent to say — and the row above these carries the
    /// task's own title, so the reader is not left with a blank. What must not
    /// happen is the surface inventing a sentence to fill the space.
    #[test]
    fn a_hand_with_nothing_written_on_it_draws_nothing_rather_than_something_invented() {
        let (_env, store) = scratch("flag-bare");
        a_task(&store, "wsp-001");
        assert_eq!(raise_one(&store, &["wsp-001"], &[]), 0);

        let up = crate::message::raised(&store);
        assert!(hand_rows(&up[0], &Paint::new()).is_empty(), "words appeared from nowhere");
    }

    /// A hand that a keypress would answer says so in the listing.
    ///
    /// `worklist-018`'s second owed check, and it failed when it was made: the
    /// card said *asks to take it*, `wsp watch` said *asking to claim*, and
    /// `wsp flag --seat` — a governor's actual inbox — said neither. So a
    /// question raised with no sentence was a row carrying a task title, a pane
    /// and an age, indistinguishable from a notice.
    #[test]
    fn the_listing_says_when_a_hand_is_a_question_a_keypress_would_answer() {
        let (_env, store) = scratch("flag-ask-shown");
        a_task(&store, "wsp-001");
        assert_eq!(
            raise_one(&store, &["wsp-001"], &[("ask", "claim"), ("pane", "w4:p2")]),
            0,
        );

        let up = crate::message::raised(&store);
        assert!(
            hand_aside(&up[0], None, false).contains("asks to take it"),
            "a question read as a notice: {}",
            hand_aside(&up[0], None, false),
        );

        // And a hand that asks for nothing does not pretend to: the phrase is
        // only worth its width when it is true.
        raise_one(&store, &["wsp-001", "just look at this"], &[]);
        let up = crate::message::raised(&store);
        assert!(!hand_aside(&up[1], None, false).contains("asks to"), "a notice claimed to be a question");
    }

    /// And the ambiguity that follows is said out loud rather than guessed at.
    ///
    /// The verb that could take a task id and be certain could only be certain
    /// because the store was losing the second hand. With both kept, a task id
    /// naming two of them is a question, and the answer is to print them and
    /// refuse — an exit of 2, which is what wsp says when an argument did not
    /// name one thing.
    #[test]
    fn clearing_by_task_refuses_while_two_hands_are_up_and_works_once_one_is_down() {
        let (_env, store) = scratch("flag-ambiguous");
        a_task(&store, "wsp-001");
        raise_one(&store, &["wsp-001", "the index is behind HEAD"], &[]);
        raise_one(&store, &["wsp-001", "and the tree is shared"], &[]);

        assert_eq!(
            raise_one(&store, &["wsp-001"], &[("clear", "true")]),
            2,
            "a keypress disposed of one of two raised hands without saying which",
        );

        // Named, it goes — and only it.
        let first = crate::message::about_task(&store, "wsp-001")[0].id.clone();
        assert_eq!(raise_one(&store, &[&first], &[("clear", "true")]), 0);
        let left = crate::message::about_task(&store, "wsp-001");
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].title(), "and the tree is shared");

        // With one left, the task names it again and the shorthand works.
        assert_eq!(raise_one(&store, &["wsp-001"], &[("clear", "true")]), 0);
        assert!(crate::message::about_task(&store, "wsp-001").is_empty());
    }

    /// A hand outlives its subject, and a person must still be able to put it
    /// down.
    ///
    /// Found live by `worklist-018` while cleaning up after itself: `flag`
    /// resolved the needle through `task_or_why` *before* it looked at
    /// `--clear`, so a hand on a task that no longer existed exited 1 with the
    /// hand still up, and the panel's `x` built the same argv and failed the
    /// same way. The file had to be edited by hand. `list_flags` had been
    /// drawing `(no such task)` on those rows the whole time, saying *the fix
    /// is to lower it* — which was the one thing that did not work.
    ///
    /// It is structural rather than a slip: a raised hand is identified by what
    /// is raised, and `about` is a subject rather than a foreign key. So the
    /// resolution goes to the record first and the task table never gets a
    /// chance to refuse.
    #[test]
    fn a_hand_about_a_task_that_no_longer_exists_can_still_be_lowered() {
        let (_env, store) = scratch("flag-orphan");
        a_task(&store, "wsp-001");
        raise_one(&store, &["wsp-001", "the store will not parse"], &[]);
        std::fs::remove_file(store.task_path("wsp-001")).unwrap();
        assert!(store.task("wsp-001").is_none(), "the subject is gone");

        assert_eq!(
            raise_one(&store, &["wsp-001"], &[("clear", "true")]),
            0,
            "a hand about a retired task could not be lowered by the verb that exists to lower it",
        );
        assert!(crate::message::raised(&store).is_empty());
    }

    /// `--ask` makes it a question, and a question may not simply be taken
    /// down.
    ///
    /// `worklist-004`: a seat answered a flagged agent's question down a
    /// different channel and then cleared the flag, so **clearing looked like
    /// answering** — every surface read the matter as closed while the asker
    /// sat waiting. Under the record that is not a convention anybody has to
    /// remember, it is `Shape::may` refusing, and the hand stays up.
    #[test]
    fn an_ask_is_a_question_and_x_will_not_close_one() {
        let (_env, store) = scratch("flag-ask");
        a_task(&store, "wsp-001");
        assert_eq!(
            raise_one(&store, &["wsp-001", "this is next"], &[("ask", "claim"), ("pane", "w4:p2")]),
            0
        );

        let up = crate::message::about_task(&store, "wsp-001");
        assert_eq!(up[0].shape(), Some(crate::message::Shape::Question));
        assert_eq!(up[0].ask(), Some(crate::message::Ask::Claim));
        // Who is sitting still, so the answer can find its way back. Nothing
        // was claimed in this pane, so the subject is the only task to name —
        // and `message::replies_for` reads exactly this field.
        assert_eq!(
            up[0].waiting.as_ref().map(|w| (w.pane.as_str(), w.task.as_str())),
            Some(("w4:p2", "wsp-001")),
        );

        assert_eq!(raise_one(&store, &["wsp-001"], &[("clear", "true")]), 1);
        assert_eq!(
            crate::message::about_task(&store, "wsp-001").len(),
            1,
            "the question was closed by a keystroke that said nothing",
        );
    }

    /// The body-only flag, which drew as nothing.
    ///
    /// `worklist-018`: a hand raised with `--body` and no sentence showed the
    /// task's own title and no message, because the field the surfaces drew was
    /// the empty one. The record has one text and a first line, so there is no
    /// empty field left to draw — which is the repair belonging at the writer,
    /// where there is one of it.
    #[test]
    fn a_hand_with_only_a_paragraph_still_has_a_headline() {
        let (_env, store) = scratch("flag-body");
        a_task(&store, "wsp-001");
        raise_one(
            &store,
            &["wsp-001"],
            &[("body", "Landed on master, at review — and NOT INSTALLED.\nThe retry hazard is live.")],
        );

        let up = crate::message::about_task(&store, "wsp-001");
        assert_eq!(up[0].title(), "Landed on master, at review — and NOT INSTALLED.");
        assert_eq!(up[0].body(), "The retry hazard is live.");
    }

    /// A row of `agent.list`, and deliberately without `interactive_ready`:
    /// that is how a plugin-reported agent arrives, and a census that could not
    /// read one would be blind to a whole class of agent.
    fn listed(pane: &str, status: &str) -> herdr::Pane {
        herdr::parse_pane(&json!({ "pane_id": pane, "agent": "claude", "agent_status": status }))
    }

    /// The failure this file's census was rebuilt for, at the one line that
    /// decides it.
    ///
    /// Seven agents on 2026-08-18 were listed by `agent.list`, alive, and doing
    /// nothing at all — the turns had been abandoned by an API overload. Being
    /// listed was the whole of what `doctor` asked, so all seven counted as work
    /// in progress. A row in that listing is a process, and a process is not a
    /// turn.
    #[test]
    fn an_agent_that_is_listed_is_not_thereby_working() {
        let answered = answered_by_machine(["w1:p1", "w2:p1", "w3:p1"]);
        let panes = vec![listed("w1:p1", "working"), listed("w2:p1", "idle"), listed("w3:p1", "blocked")];
        let bound = |p: &str| bound_state(p, &panes, &panes, &answered);

        assert!(matches!(bound("w1:p1"), Bound::Turning), "a turn in flight is the only working");
        assert!(matches!(bound("w2:p1"), Bound::Quiet(State::Idle)), "alive, and nothing happening");
        assert!(
            matches!(bound("w3:p1"), Bound::Quiet(State::Blocked)),
            "waiting on a person is not waiting on the model"
        );

        // And the two states that were already named stay where they were: this
        // split the `Yes` arm and left the `No` arms alone.
        let empty = herdr::parse_pane(&json!({ "pane_id": "w4:p1" }));
        assert!(matches!(bound_state("w4:p1", &[], &[empty], &answered), Bound::Emptied));
        assert!(matches!(bound_state("w9:p1", &[], &[], &answered), Bound::Gone));
    }

    /// herdr answers `done` for an agent that went idle while nobody was
    /// looking at its workspace, which is the *majority* reading on a machine
    /// running agents in the background — four of twelve on this one, live, on
    /// 2026-08-19. Read as `Unknown` it was a seat wsp could not see and would
    /// not speak to.
    ///
    /// It is `Idle` and not a state of its own because the difference between
    /// the two words is `seen` (herdr `pane/state.rs:8`) — whether a person has
    /// looked at the pane — and a census whose answer changes when somebody
    /// switches tabs is not a census.
    #[test]
    fn an_agent_nobody_has_looked_at_is_idle_and_not_a_mystery() {
        let answered = answered_by_machine(["w1:p1"]);
        let panes = vec![listed("w1:p1", "done")];
        assert!(matches!(
            bound_state("w1:p1", &panes, &panes, &answered),
            Bound::Quiet(State::Idle)
        ));
    }

    /// Neither half of the test is a fault on its own, which is the whole
    /// reason it can be run against every claim on the machine without crying
    /// wolf.
    #[test]
    fn a_quiet_agent_is_only_worth_reporting_once_the_work_has_stopped_too() {
        let recently = util::now_iso();
        let long_ago = "2020-01-01T00:00:00Z";
        assert_eq!(quiet_note(State::Idle, Status::Doing, Some(&recently)), None, "between turns");

        let said = quiet_note(State::Idle, Status::Doing, Some(long_ago)).expect("an hour of both");
        assert!(said.contains("no turn running"), "{said}");
        assert!(said.contains("wsp tell"), "the repair that keeps the conversation: {said}");

        // Blocked skips the clock: the repair is one keystroke by one person,
        // and the only thing that can fetch them is being told now.
        let now = quiet_note(State::Blocked, Status::Doing, Some(&recently)).expect("said at once");
        assert!(now.contains("only a person can answer"), "{now}");

        // Nothing to measure against is not evidence of a stall. A binding with
        // no timestamp and no task predates this and says nothing either way.
        assert_eq!(quiet_note(State::Idle, Status::Doing, None), None);
    }

    /// The status picks the verb, and getting this wrong is how the check gets
    /// ignored. All four quiet claims on this machine when it first ran were
    /// finished work, not stalled work — offering `wsp tell` for those would
    /// have trained somebody to skip the line.
    #[test]
    fn an_agent_that_finished_is_holding_a_slot_and_not_stalled() {
        let long_ago = Some("2020-01-01T00:00:00Z");

        let done = quiet_note(State::Idle, Status::Review, long_ago).expect("still a claim held");
        assert!(done.contains("finished"), "{done}");
        assert!(done.contains("wsp release"), "the slot is the thing to give back: {done}");
        assert!(!done.contains("wsp tell"), "nothing is stuck here: {done}");

        // And work that is waiting on a person is *supposed* to be sitting
        // still. An agent idle on a blocked task is doing the right thing, and
        // a census that nagged about it would be nagging about the answer.
        for waiting in [Status::Blocked, Status::Parked] {
            assert_eq!(quiet_note(State::Idle, waiting, long_ago), None, "{waiting:?}");
        }
    }

    /// `wsp tell` refuses a workspace on purpose, and takes the two things that
    /// can hold exactly one agent.
    #[test]
    fn a_message_is_addressed_to_a_pane_or_to_the_work_and_never_to_a_room() {
        let env = crate::util::isolated("tell-target");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let t = Task::new("a task with an agent on it", "x-001");
        store.save_task(&t).unwrap();
        store.set_binding("w1:p1", json!({ "task_id": t.id, "pane_id": "w1:p1" }));

        let (pane, what) = target(&store, &t.id).expect("the task's pane");
        assert_eq!(pane, "w1:p1");
        assert!(what.starts_with(&t.id), "{what}");

        assert_eq!(target(&store, "w1:p1").map(|(p, _)| p), Some("w1:p1".into()), "a pane id, as given");
        assert_eq!(target(&store, "w1"), None, "a workspace may hold two agents and names neither");
        assert_eq!(target(&store, "nothing-like-this"), None);
    }

    /// **The retry is the harm, and this is what recognises one.**
    ///
    /// worklist-010's cost was not the false failure report; it was what the
    /// report caused. `wsp govern wsp --tell` said `the wsp seat was not told`
    /// about a message that had arrived, so the seat retried, and one paragraph
    /// was delivered to the `wsp` seat two or three times in a day — a governor
    /// sending the same direction three times to an agent that acted on the
    /// first. `place_herdr::tell` no longer lies, which stops the retries this
    /// channel *caused*; this stops the ones anything else causes.
    ///
    /// The id does the work and it is derived rather than minted: the same text
    /// to the same pane is the same id, so the second attempt recognises the
    /// first without either of them carrying a record of the other. A different
    /// pane or a different word is a different message, which is the whole of
    /// what "the same message" can honestly mean from the sending end.
    #[test]
    fn the_same_sentence_to_the_same_pane_is_one_message_and_knows_it() {
        let env = crate::util::isolated("tell-twice");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let args = crate::Args::parse(vec!["wsp".into(), "tell".into()]);
        let sent = |pane: &str, text: &str| Sent::new("x-001", "x-001", pane, "x-001", text, &args);

        let first = sent("w1:p6", "worklist-005 touches panel/rows.rs");
        assert_eq!(first.already_sent(&store), None, "nothing has been said to anybody");

        // What `delivered` writes on the way past, which is the only record
        // either verb has ever left — and until this task, `govern --tell` left
        // none at all.
        store.log_event(
            "agent-told",
            json!({ "target": "x-001", "pane": "w1:p6", "chars": 12,
                    "id": first.id, "at": util::epoch_secs() }),
        );

        assert_eq!(first.already_sent(&store), Some(0), "the retry after a lie");
        assert_eq!(
            sent("w1:p6", "worklist-005 touches panel/rows.rs").id,
            first.id,
            "a resend has to arrive at the same id or it recognises nothing",
        );
        assert_eq!(sent("w3R:p1", "worklist-005 touches panel/rows.rs").already_sent(&store), None,
                   "the same words to a different seat are a different message");
        assert_eq!(sent("w1:p6", "worklist-005 touches worklist.rs").already_sent(&store), None,
                   "and a different sentence to the same seat is too");

        // Old enough to be somebody meaning it twice rather than retrying a
        // failure that never happened — see `SAME_BREATH`.
        store.log_event(
            "agent-told",
            json!({ "target": "x-001", "pane": "w2:p1", "chars": 8,
                    "id": sent("w2:p1", "continue").id, "at": util::epoch_secs() - SAME_BREATH - 1 }),
        );
        assert_eq!(sent("w2:p1", "continue").already_sent(&store), None, "not a duplicate any more");
    }

    /// The correctness hazard the executor design is built around, at the one
    /// line that decides it.
    ///
    /// A machine that is merely unreachable reports no workspaces, which reads
    /// exactly like a machine with nothing running on it. Reaped on that
    /// basis, one dropped link hands back every task the executor is holding
    /// while its agents carry on working on tasks that no longer know about
    /// them. So a machine has to have been heard from first.
    #[test]
    fn a_machine_that_said_nothing_keeps_its_claims() {
        // The seat answered; mb2 did not — an offline executor, or one whose
        // tunnel is down, or one nobody asked.
        let answered = answered_by_machine(["w0", "w1"]);

        assert!(may_reap(&answered, "w9"), "a workspace gone from a machine that answered");
        assert!(!may_reap(&answered, "w0@mb2"), "silence from mb2 is not mb2 being empty");

        // And once it does answer, its claims are examined like anyone else's.
        let both = answered_by_machine(["w0", "w0@mb2"]);
        assert!(may_reap(&both, "w9@mb2"));
        assert!(may_reap(&both, "w9"));

        // The same judgement over pane ids, which is what `sync` brings to it:
        // one rule, and each caller hands it what it has actually heard.
        let panes = answered_by_machine(["w0:p1", "w0:p2"]);
        assert!(may_reap(&panes, "w3:p9"));
        assert!(!may_reap(&panes, "w0:p1@mb2"));
    }

    /// The generalisation, not a second rule beside the old one: `reap` used to
    /// refuse outright when herdr answered with nothing at all, for exactly
    /// this reason, on the one machine there was. That case still holds.
    #[test]
    fn a_herdr_answering_with_nothing_still_reaps_nothing() {
        let answered = answered_by_machine(std::iter::empty());
        assert!(!may_reap(&answered, "w0"));
        assert!(!may_reap(&answered, ""), "a claim too old to carry a workspace id either");
    }

    /// The claim's `host` field cannot key this. An agent on an executor runs a
    /// `wsp` shim that executes on the seat, so its claims record the *seat's*
    /// hostname; the workspace id is the only thing that came back from the far
    /// machine, and it comes back qualified.
    #[test]
    fn the_machine_is_read_off_the_id_and_a_bare_id_is_this_one() {
        assert_eq!(machine_of("w0:p3@mb2"), "mb2");
        assert_eq!(machine_of("w0:p3"), "", "a local id stays bare, and keys as this seat");
        assert_eq!(machine_of(""), "");
        // A colon is inside a pane id, not a separator — hence `@`.
        assert_eq!(herdr::split_host("w0:p3@mb2"), ("w0:p3", Some("mb2")));
        assert_eq!(herdr::split_host("w0:p3"), ("w0:p3", None));
        assert_eq!(herdr::split_host("w0@"), ("w0@", None), "a trailing @ names no machine");
    }

    /// The shape that cost the first half hour of this command: a wrapped
    /// payload read flat gives `""`, which looks exactly like a pane with
    /// nothing on it. Both shapes have to work, and neither may be silent.
    #[test]
    fn a_pane_read_is_unwrapped_whichever_shape_it_arrives_in() {
        let wrapped = json!({ "type": "pane_read", "read": { "text": "on the pane" } });
        assert_eq!(read_body(&wrapped).get("text").unwrap(), "on the pane");

        let flat = json!({ "text": "on the pane" });
        assert_eq!(read_body(&flat).get("text").unwrap(), "on the pane");

        // And an empty pane is still an empty pane, not a missing wrapper.
        let empty = json!({ "type": "pane_read", "read": { "text": "" } });
        assert_eq!(read_body(&empty).get("text").unwrap(), "");
    }

    /// The two names an unscoped pane may not take. `install` treats any pane
    /// labelled `wsp` as a panel it lost track of, so an agent that said "wsp"
    /// while holding nothing would be adopted as furniture and dropped from
    /// the tree — the panel filters its own panes out of it.
    #[test]
    fn a_pane_never_takes_the_panels_own_name() {
        assert_eq!(plain_full("wsp"), None);
        assert_eq!(plain_full("  wsp  "), None);
        assert_eq!(plain_full("wsp:view"), None);
        assert_eq!(plain_full(""), None);

        // Anything else is itself, whole — and cut to the width `sync` already
        // uses for the same title only on the way to herdr.
        assert_eq!(plain_full("wsp panel"), Some("wsp panel".to_string()));
        let long = "Agents should rename as they pick up new tasks, and say so";
        assert_eq!(plain_full(long), Some(long.to_string()));
        assert_eq!(on_the_wire(long).chars().count(), LABEL_MAX);
        assert!(on_the_wire(long).ends_with('…'));
    }

    /// What the sidebar has to answer in the first ten columns: which of these
    /// agents is this. The title is the same shape on every row of a project
    /// being worked through — the scope is not.
    #[test]
    fn a_pane_holding_work_wears_the_scope_of_it() {
        let mut t = Task::new("let's add the task scope to the agent window", "render-109");
        t.project = Some("render".into());

        assert_eq!(
            task_label(&t),
            Some("render-109 · let's add the task scope to th…".to_string())
        );
        // Still the one width, prefix and all: it is a name on a wire, not a
        // paragraph.
        assert_eq!(task_label(&t).unwrap().chars().count(), LABEL_MAX);

        // A sentence keeps the scope the title had, so saying something does
        // not cost the pane its place in the list.
        assert_eq!(
            said_full(&t, "reading the claim guard"),
            Some("render-109 · reading the claim guard".to_string())
        );
        assert_eq!(said_full(&t, "   "), None);

        // An unfiled task is numbered in the inbox, and that is an id like any
        // other — so it is printed like any other.
        let loose = Task::new("something nobody has filed", "inbox-004");
        assert_eq!(task_label(&loose), Some("inbox-004 · something nobody has filed".to_string()));
    }

    /// The whole name, given back to a surface that has room for it — and
    /// withheld the moment it would be a name the pane is not wearing.
    ///
    /// `render-066`: the label is cut on the wire and the cut copy was the only
    /// copy, so `panel --full` drew 44 characters into a hundred columns. The
    /// two ways back are worth different things and are asked in that order:
    /// what the store kept for a sentence that lives nowhere else, then the
    /// task's own title, which was never lost in the first place.
    #[test]
    fn a_name_cut_for_the_wire_can_be_had_whole_again() {
        let mut t = Task::new(
            "Labels are truncated on the wire, so the full name is gone",
            "render-066",
        );
        t.project = Some("render".into());

        let full = said_full(&t, "reading cmd_agent.rs for where the cut happens").unwrap();
        let wire = on_the_wire(&full);
        assert!(wire.ends_with('…'), "the case this is about is a label that was cut");

        let mut said = BTreeMap::new();
        said.insert("w1:p1".to_string(), json!({ "label": wire, "full": full }));

        // The sentence, which exists in the store and nowhere else.
        assert_eq!(full_name(&said, "w1:p1", &wire, Some(&t)), Some(full.clone()));

        // The same entry against a pane wearing something else: a person
        // renamed it, or a claim did. What is on screen wins, always — a long
        // name that disagrees with herdr's is a worse answer than a short one.
        assert_eq!(full_name(&said, "w1:p1", "claude", Some(&t)), None);
        assert_eq!(full_name(&said, "w9:p9", &wire, Some(&t)), None, "another pane's sentence");

        // The title needs nothing kept: the store has it, so a pane wearing the
        // name a claim wrote expands out of the task itself. This is every
        // pane claimed before any of this shipped.
        let claimed = task_label(&t).unwrap();
        assert_eq!(full_name(&BTreeMap::new(), "w1:p1", &claimed, Some(&t)), task_full(&t));

        // And nothing is invented. A short name, a shell's own title, a pane
        // holding no task — none of them were ever cut.
        assert_eq!(full_name(&BTreeMap::new(), "w1:p1", "claude", Some(&t)), None);
        assert_eq!(full_name(&BTreeMap::new(), "w1:p1", &claimed, None), None);
        let mut short = Task::new("short enough", "render-002");
        short.project = Some("render".into());
        let whole = task_label(&short).unwrap();
        assert_eq!(
            full_name(&BTreeMap::new(), "w1:p1", &whole, Some(&short)),
            None,
            "a name that fits is its own whole, and saying so twice helps nobody"
        );
    }

    /// The defect: the label was composed from the project the task is in
    /// *now*, so the thirty-odd tasks moved when `batch` and `fork` were made
    /// went on wearing a name that resolves to nothing. Ed read one off a pane
    /// and typed it, and `wsp show` said no such task.
    #[test]
    fn a_task_that_moved_project_is_labelled_by_the_id_it_kept() {
        let mut moved = Task::new("a pane's label names a task that does not exist", "wsp-077");
        moved.project = Some("batch".into());

        // The id, whole, and nothing reconstructed from where it sits today.
        assert_eq!(task_scope(&moved), "wsp-077");
        assert!(task_label(&moved).unwrap().starts_with("wsp-077 ·"));
        assert!(
            !task_label(&moved).unwrap().contains("batch/077"),
            "the composed name is the one the CLI rejects"
        );
    }

    /// What a release is entitled to take back. The bug it fixes: `u` on an
    /// agent that had ever run `wsp say` left the pane wearing the work — the
    /// name matched the sentence, not the title, so the old test for "is this
    /// ours" said no and the agents panel went on showing a task and a project
    /// for somebody holding neither.
    #[test]
    fn a_release_takes_back_the_scope_and_nothing_else() {
        let mut t = Task::new("When unassigning an agent via u", "render-047");
        t.project = Some("render".into());

        // Both shapes a claim leaves behind: the title it wrote, and whatever
        // has been said over it since.
        assert!(named_after_task(&task_label(&t).unwrap(), &t));
        assert!(named_after_task(
            &on_the_wire(&said_full(&t, "reading how release names panes").unwrap()),
            &t
        ));

        // Somebody else's name, and another task's. Neither is ours to take.
        assert!(!named_after_task("Trance Video", &t));
        assert!(!named_after_task("", &t));
        let mut other = Task::new("something else entirely", "render-030");
        other.project = Some("render".into());
        assert!(!named_after_task(&task_label(&other).unwrap(), &t));

        // The scope is the whole id: the same number under another project is
        // a different piece of work.
        let mut elsewhere = Task::new("same number, another project", "robustness-047");
        elsewhere.project = Some("robustness".into());
        assert!(!named_after_task(&task_label(&t).unwrap(), &elsewhere));
    }

    /// What a release has to take back off panes named before the label was
    /// the id — which is every pane and workspace standing when this shipped.
    /// Missing them would leave an agent holding nothing wearing the work,
    /// which is the whole reason [`unname_after_task`] exists.
    #[test]
    fn a_release_still_recognises_the_names_written_before_the_id_was_the_label() {
        let mut moved = Task::new("a pane's label names a task that does not exist", "wsp-077");
        moved.project = Some("batch".into());

        // Composed where it is now, and composed where it was numbered: the
        // same claim wrote both, on either side of the move.
        assert!(named_after_task("batch/077 · a pane's label names a task…", &moved));
        assert!(named_after_task("wsp/077 · a pane's label names a task…", &moved));
        // And the bare form an unfiled task's claim wrote.
        assert!(named_after_task("077 · a pane's label names a task…", &moved));

        // Still nobody else's. A number that is not this task's is not ours by
        // any of the shapes.
        assert!(!named_after_task("batch/078 · something else", &moved));
        assert!(!named_after_task("Trance Video", &moved));
    }

    /// The other half of the same transition: reconcile has to *replace* the
    /// old shape, or a name that resolves to nothing outlives the fix. What it
    /// must not do is take the sentence with it — the scope is what was wrong.
    #[test]
    fn reconcile_rewrites_a_composed_name_and_keeps_what_the_agent_said() {
        let mut moved = Task::new("a pane's label names a task that does not exist", "wsp-077");
        moved.project = Some("batch".into());

        assert_eq!(
            pane_rename("batch/077 · reading rows.rs for ident_of", None, &moved),
            Some("wsp-077 · reading rows.rs for ident_of".to_string())
        );
        // Nothing said yet, so the title is what there is to put back.
        assert_eq!(pane_rename("batch/077 · ", None, &moved), task_full(&moved));
        // Already the id. Reconcile has nothing to do, sentence or not.
        assert_eq!(pane_rename("wsp-077 · reading rows.rs", None, &moved), None);
    }

    /// And it rewrites the scope on the sentence the store kept, not on the
    /// forty-four characters of it herdr was given.
    ///
    /// The defect this is the guard for is a slow one: reconcile read the
    /// sentence off the label, so every rewrite put back what was already cut
    /// and the ellipsis crept left. An agent's status line would wear down to
    /// nothing over a week of daemon restarts, and nothing would look broken at
    /// any single step.
    #[test]
    fn reconcile_rewrites_the_scope_on_the_whole_sentence() {
        let mut moved = Task::new("a pane's label names a task that does not exist", "wsp-077");
        moved.project = Some("batch".into());

        let said = "reading rows.rs for what places a pane against a project";
        let full = format!("batch/077 · {said}");
        let wire = on_the_wire(&full);
        assert!(wire.ends_with('…'), "the label herdr holds is the cut one");

        assert_eq!(
            pane_rename(&wire, Some(&full), &moved),
            Some(format!("wsp-077 · {said}")),
            "the scope is what was wrong, and the sentence comes through whole"
        );
    }

    /// The defect: `reconcile` renamed on any label that differed from the
    /// title, and a sentence always differs — so a routine maintenance verb
    /// wiped every agent's `wsp say`, silently, and the panel went back to
    /// reading as a row of titles nobody had said anything about.
    #[test]
    fn reconcile_leaves_a_sentence_where_the_agent_put_it() {
        let mut t = Task::new("The three-module partition: core records", "robustness-083");
        t.project = Some("robustness".into());

        // What an agent is saying right now. Reconcile has no business here:
        // the sentence is newer than anything reconcile knows.
        let said = said_full(&t, "reading place.rs for what the port owes say").unwrap();
        assert_eq!(pane_rename(&on_the_wire(&said), Some(&said), &t), None);

        // The name the claim wrote, and still wearing it. Nothing to do.
        assert_eq!(pane_rename(&task_label(&t).unwrap(), None, &t), None);

        // The cases name_bound exists for, all intact: a pane from before any
        // of this, one whose claim-time rename was dropped on a slow socket,
        // and one still wearing the task it was moved off.
        assert_eq!(pane_rename("claude", None, &t), task_full(&t));
        assert_eq!(pane_rename("", None, &t), task_full(&t));
        let mut old = Task::new("something else entirely", "robustness-030");
        old.project = Some("robustness".into());
        let stale = said_full(&old, "still on the old one").unwrap();
        assert_eq!(pane_rename(&stale, None, &t), task_full(&t));
    }

    /// The compatibility rule `--model` itself keeps, at the other end of it: a
    /// spawn that states no tier writes no clause, so every claim line in the
    /// store that came before this is still exactly the line it was and nothing
    /// needs migrating.
    ///
    /// And `default` is a word rather than an omission when *one* half was
    /// stated. `--effort high` with no `--model` is an ordinary thing to type —
    /// it is the cheaper knob and the one to reach for first — and the record
    /// of it has to say which half was left to the settings file, or a reader
    /// cannot tell `high effort on whatever is configured` from `high effort on
    /// something nobody wrote down`.
    #[test]
    fn a_claim_that_states_no_tier_says_nothing_about_one() {
        assert_eq!(spawned_at(None, None), "");
        assert_eq!(spawned_at(Some("opus[1m]"), Some("high")), " · spawned at opus[1m]/high");
        assert_eq!(spawned_at(None, Some("high")), " · spawned at default/high");
        assert_eq!(spawned_at(Some("haiku"), None), " · spawned at haiku/default");
    }

    /// The subtraction that separates a loaded index from an agent halfway
    /// through the commit procedure. Deletions the disk makes too are somebody
    /// editing; deletions only the index makes are work nothing else holds.
    #[test]
    fn only_the_index_deleting_what_the_disk_keeps_counts_as_loss() {
        // The incident: an old tree in the shared index. The disk has the file
        // as HEAD left it, and staging it would drop four lines.
        let staged = "0\t4\tsrc/panel.rs\n";
        assert_eq!(index_loss(staged, ""), 4);

        // An agent that staged what it wrote: both listings say the same
        // thing, because the index holds exactly what is on disk.
        assert_eq!(index_loss("2\t1\tsrc/panel.rs\n", "2\t1\tsrc/panel.rs\n"), 0);

        // …and carried on editing afterwards. The disk has moved further from
        // HEAD than the index has, which is the normal direction of travel.
        assert_eq!(index_loss("2\t1\tsrc/panel.rs\n", "9\t3\tsrc/panel.rs\n"), 0);

        // A deletion made on purpose — the file is gone from the disk as well,
        // so the index is not holding the only copy of anything.
        assert_eq!(index_loss("0\t40\tsrc/old.rs\n", "0\t40\tsrc/old.rs\n"), 0);

        // Per path, not in total: one file legitimately staged does not pay
        // for another the index would revert.
        let staged = "2\t1\tsrc/a.rs\n0\t30\tsrc/b.rs\n";
        let disk = "2\t1\tsrc/a.rs\n";
        assert_eq!(index_loss(staged, disk), 30);

        // A binary file counts no lines at all, and `-` is not a number.
        assert_eq!(index_loss("-\t-\tdoc/screenshot.png\n", ""), 0);

        // A clean index is the common answer and has no listing to read.
        assert_eq!(index_loss("", ""), 0);
    }

    /// The two states an agent with nothing in hand can be in, and the reason
    /// the project is not what the label leads with: a herdr sidebar draws 26
    /// columns and cuts the right-hand end, so a project-first label loses the
    /// state — which is the half a person scanning for a free agent is reading.
    #[test]
    fn a_looking_pane_says_the_state_before_the_project() {
        assert_eq!(looking_label(Some("render"), true), "looking for work in render");
        assert_eq!(looking_label(Some("render"), false), "nothing actionable in render");

        // Unscoped — `wsp next` outside any project. Still the two states.
        assert_eq!(looking_label(None, true), "looking for work");
        assert_eq!(looking_label(None, false), "nothing actionable");

        // What survives herdr's cut is the state, for the longest slug we have.
        let long = looking_label(Some("strata-prototype"), true);
        assert!(long.chars().take(26).collect::<String>().starts_with("looking for work"));

        // And nothing here can ever be a name the panel hunts for.
        for l in [looking_label(Some("wsp"), true), looking_label(Some("wsp"), false)] {
            assert_ne!(l, crate::panel::PANEL_LABEL);
            assert_ne!(l, crate::panel::VIEW_LABEL);
        }
    }

    /// The refusal says what is owed, and the log is the only place that has
    /// ever recorded it. Anything that is not a `blocked:` line written by
    /// `Task::log` is somebody else's note and must not be read as the
    /// question — a claim that answers "blocked, waiting on an answer: claimed
    /// by pane w0:p3" is worse than one that says nothing.
    #[test]
    fn the_refusal_finds_the_question_the_block_left_behind() {
        let mut t = Task::new("Per-project id counters", "t-024");
        t.log("claimed by pane w0:p3");
        assert_eq!(blocked_question(&t), None);

        t.log("blocked: wsp-014 or t-260815-014?");
        assert_eq!(blocked_question(&t), Some("wsp-014 or t-260815-014?".into()));

        // Blocked twice: the second question is the one still waiting.
        t.log("blocked: and what happens to the ids already written?");
        assert_eq!(
            blocked_question(&t),
            Some("and what happens to the ids already written?".into())
        );

        // A line that only mentions one is not one.
        let mut mentions = Task::new("…", "t-025");
        mentions.log("hand-off — t-024 is blocked: waiting on Ed");
        assert_eq!(blocked_question(&mentions), None);

        // And a block with no reason falls back rather than showing nothing.
        let mut bare = Task::new("…", "t-026");
        bare.log("blocked:");
        assert_eq!(blocked_question(&bare), None);
    }

    // ---- wip, offline ------------------------------------------------------

    fn wip_agent(pane: &str, ws: &str, state: &str, title: &str) -> herdr::Pane {
        herdr::Pane {
            pane_id: pane.to_string(),
            workspace_id: ws.to_string(),
            agent: "claude".into(),
            agent_status: state.to_string(),
            title: title.to_string(),
            ..Default::default()
        }
    }

    fn wip_task(id: &str, title: &str, project: Option<&str>, status: &str) -> Task {
        let mut t = Task::new(title, id);
        t.project = project.map(str::to_string);
        t.status_raw = status.to_string();
        t
    }

    /// Two agents on two tasks, one of them stopped on work that is still
    /// `doing` — plus the three queues waiting on a person underneath.
    ///
    /// The state this view exists to show is a join of a herdr fact (an agent
    /// is idle) with a store fact (its task is still open), and standing it up
    /// live means two real agents in the states you want with the right rows
    /// underneath them. This is the fixture there was nowhere to hang.
    fn wip_world() -> Wip {
        let mut bindings = std::collections::BTreeMap::new();
        bindings.insert("w1:p1".to_string(), json!({ "task_id": "t-001" }));
        bindings.insert("w2:p1".to_string(), json!({ "task_id": "t-002" }));

        Wip {
            tasks: vec![
                wip_task("t-001", "the seam under the panel", Some("wsp"), "doing"),
                wip_task("t-002", "a board over the same facts", Some("wsp"), "doing"),
                wip_task("t-003", "waiting on a decision", Some("wsp"), "blocked"),
                wip_task("t-004", "finished, waiting on you", Some("wsp"), "review"),
                wip_task("t-005", "unfiled", None, "todo"),
            ],
            index: Index::new(vec![crate::model::Project::new("wsp")]),
            bindings,
            claims: std::collections::BTreeMap::new(),
            pins: std::collections::BTreeMap::new(),
            governors: std::collections::BTreeMap::new(),
            agents: vec![
                wip_agent("w1:p1", "w1", "working", "wsp"),
                // Stopped, on a task that is still doing: the ← this view is for.
                wip_agent("w2:p1", "w2", "idle", "wsp"),
                // An agent holding nothing, which is its own kind of row.
                wip_agent("w3:p1", "w3", "working", "reading the README"),
            ],
            workspaces: vec![
                herdr::Workspace { id: "w1".into(), label: "wsp".into(), ..Default::default() },
                herdr::Workspace { id: "w2".into(), label: "wsp".into(), ..Default::default() },
                herdr::Workspace { id: "w3".into(), label: "elsewhere".into(), ..Default::default() },
            ],
        }
    }

    /// An idle agent on a task that is still `doing` is a person being the
    /// blocker, and saying so is what this view is for. One of the three
    /// agents is in that state and exactly one row carries the mark.
    #[test]
    fn wip_names_the_agent_that_is_waiting_on_you() {
        let w = wip_world();
        let rows = wip_rows(&w);
        let flagged: Vec<&str> = rows.iter().filter(|r| r.needs_you).map(|r| r.pane.as_str()).collect();
        assert_eq!(flagged, ["w2:p1"], "idle on doing, and only that");

        let text = wip_lines(&w, &Paint::new(), false).join("\n");
        assert!(text.contains("1 need you"), "the heading counts it:\n{text}");
        assert!(text.contains("← needs you"), "and the row carries it:\n{text}");
        // And the heading says how many are working, which is the fact the old
        // `all busy` asserted without having. Three agents, one of them stopped.
        assert!(text.contains("2 running a turn"), "{text}");

        // A working agent on the same kind of task is not waiting on anybody.
        let mut busy = wip_world();
        busy.agents[1].agent_status = "working".into();
        assert!(wip_rows(&busy).iter().all(|r| !r.needs_you));
        let text = wip_lines(&busy, &Paint::new(), false).join("\n");
        assert!(!text.contains("need you"), "{text}");
        assert!(
            !text.contains("all busy"),
            "the sentence this printed over five stopped agents on 2026-08-19:\n{text}"
        );
        assert!(text.contains("3 running a turn"), "{text}");
    }

    /// The other two words herdr has for a pane with no turn running, and this
    /// view called both of them busy.
    ///
    /// `done` is not rare and this is the measurement that says so: on this
    /// machine on 2026-08-19, **four of twelve agents were answering it** —
    /// every agent that had finished a turn while nobody was looking at its
    /// workspace. `wip` read "not idle" and printed `all busy` over four agents
    /// that had stopped, which is the report the night of 2026-08-18 was lost
    /// under.
    #[test]
    fn an_agent_that_finished_unwatched_or_stopped_on_a_dialog_is_not_busy() {
        for word in ["idle", "done", "blocked"] {
            let mut w = wip_world();
            w.agents[1].agent_status = word.into();
            let rows = wip_rows(&w);
            let flagged: Vec<&str> =
                rows.iter().filter(|r| r.needs_you).map(|r| r.pane.as_str()).collect();
            assert_eq!(flagged, ["w2:p1"], "`{word}` is a pane running no turn");
        }

        // And a word herdr has never sent is still not evidence of work. An
        // absence stays an absence — the rule the whole port is built on — so
        // it is not called stopped either.
        let mut w = wip_world();
        w.agents[1].agent_status = "something-new".into();
        assert!(wip_rows(&w).iter().all(|r| !r.needs_you), "not knowing is not knowing it stopped");
    }

    /// A pane holding nothing still appears — `wip` is who is running, not who
    /// has claimed — and says what it can about itself rather than nothing.
    #[test]
    fn an_agent_holding_nothing_is_still_an_agent() {
        let rows = wip_rows(&wip_world());
        let bare = rows.iter().find(|r| r.pane == "w3:p1").expect("a pane with no binding is still a row");
        assert_eq!(bare.task, "(reading the README)", "its terminal title is the best on offer");
        assert!(bare.task_id.is_empty());
        assert!(!bare.needs_you, "holding nothing cannot be blocked on you");

        // With not even a title, it says so rather than leaving the cell blank.
        let mut w = wip_world();
        w.agents[2].title = String::new();
        let rows = wip_rows(&w);
        assert_eq!(rows.iter().find(|r| r.pane == "w3:p1").unwrap().task, "(unbound)");
    }

    /// The row that was wrong for a whole night. `w2:p1` is idle on a task that
    /// is still `doing`, which for a worker means a person has become the
    /// blocker — and for a seat means it is between the agents it is
    /// sequencing, which is where a seat spends most of its time. The same
    /// pane, the same herdr reading, and the opposite answer.
    #[test]
    fn a_seat_idle_between_its_agents_does_not_read_as_one_that_has_stalled() {
        let mut w = wip_world();
        assert!(wip_rows(&w).iter().find(|r| r.pane == "w2:p1").unwrap().needs_you);

        w.governors.insert(
            "wsp".into(),
            json!({ "workspace": "w2", "host": util::hostname() }),
        );
        let row = wip_rows(&w).into_iter().find(|r| r.pane == "w2:p1").unwrap();
        assert!(!row.needs_you, "a seat waiting on its agents is not a person being the blocker");
        assert_eq!(row.seat.as_deref(), Some("wsp"), "and the row says which project it governs");

        let text = wip_lines(&w, &Paint::new(), false).join("\n");
        assert!(text.contains("governor · wsp"), "{text}");
        assert!(!text.contains("needs you"), "nobody else here has stopped:\n{text}");
    }

    /// A seat on the laptop is not a seat the desktop can reach: the record
    /// names a herdr workspace, and herdr's ids are per machine. Read the other
    /// way, every agent in that workspace here would stop reading as stalled.
    #[test]
    fn a_seat_recorded_on_another_machine_changes_no_row_here() {
        let mut w = wip_world();
        w.governors
            .insert("wsp".into(), json!({ "workspace": "w2", "host": "somewhere-else" }));
        assert!(
            wip_rows(&w).iter().find(|r| r.pane == "w2:p1").unwrap().needs_you,
            "still a stalled agent, because that seat is not here"
        );
    }

    /// herdr silent costs the view its agents and nothing else. The queues
    /// below are the store's, and they are what you read `wip` for when
    /// nothing is running.
    #[test]
    fn no_herdr_is_wip_without_the_agents() {
        let mut w = wip_world();
        w.agents.clear();
        w.workspaces.clear();
        let text = wip_lines(&w, &Paint::new(), false).join("\n");

        assert!(text.contains("no agents running"), "{text}");
        assert!(text.contains("BLOCKED"), "blocked work is the store's:\n{text}");
        assert!(text.contains("waiting on a decision"), "{text}");
        assert!(text.contains("REVIEW"), "{text}");
        assert!(text.contains("finished, waiting on you"), "{text}");
        assert!(text.contains("INBOX"), "unfiled work is still unfiled:\n{text}");
    }

    /// `--terse` cuts the blocked list to its count and nothing else. `wip` is
    /// asked repeatedly through a session to see who is free; that answer moves
    /// every few minutes while the blocked list does not.
    #[test]
    fn terse_keeps_the_blocked_count_and_drops_the_names() {
        let w = wip_world();
        let full = wip_lines(&w, &Paint::new(), false).join("\n");
        let terse = wip_lines(&w, &Paint::new(), true).join("\n");

        assert!(terse.contains("BLOCKED"), "the line survives — a count going up is why you go and read it");
        assert!(!terse.contains("waiting on a decision"), "…without the names:\n{terse}");
        assert!(full.contains("waiting on a decision"));

        // Review is not cut: it is work an agent has finished with, and every
        // line of it is something only a person can move.
        assert!(terse.contains("finished, waiting on you"), "{terse}");
    }

    /// The json is the same reckoning as the text, not a second one. When they
    /// were built in two passes over the same data that was a thing to keep
    /// true by hand; asserted here so it stays one.
    #[test]
    fn the_json_says_what_the_text_says() {
        let w = wip_world();
        let v = wip_json(&w);
        assert_eq!(v["needs_you"], json!(1));
        assert_eq!(v["agents"].as_array().unwrap().len(), 3);
        assert_eq!(v["blocked"].as_array().unwrap().len(), 1);
        assert_eq!(v["review"].as_array().unwrap().len(), 1);
        assert_eq!(v["inbox"], json!(1));

        let agents = v["agents"].as_array().unwrap();
        let waiting: Vec<&str> = agents
            .iter()
            .filter(|a| a["needs_you"] == json!(true))
            .map(|a| a["pane"].as_str().unwrap())
            .collect();
        assert_eq!(waiting, ["w2:p1"]);
    }

    // ---- where, offline ----------------------------------------------------

    fn where_index() -> Index {
        let mut wsp = crate::model::Project::new("wsp");
        wsp.roots = vec!["/home/ed/claude/wsp".into()];
        wsp.tags = vec!["rust".into()];
        let mut strata = crate::model::Project::new("strata");
        strata.roots = vec!["/home/ed/claude/strata".into()];
        Index::new(vec![wsp, strata])
    }

    /// A pane in wsp's tree, bound to a task filed under strata — which is the
    /// state worth asking about, because the two links disagree.
    fn standing() -> Whereabouts {
        let mut bindings = std::collections::BTreeMap::new();
        bindings.insert("w1:p1".to_string(), json!({ "task_id": "t-001" }));
        let mut t = Task::new("a task filed somewhere else", "t-001");
        t.project = Some("strata".into());
        t.status_raw = "doing".into();

        Whereabouts {
            index: where_index(),
            pins: std::collections::BTreeMap::new(),
            bindings,
            claims: std::collections::BTreeMap::new(),
            tasks: vec![t],
            workspaces: vec![herdr::Workspace {
                id: "w1".into(),
                label: "strata/001 · a task filed somewhere else".into(),
                ..Default::default()
            }],
            pane: Some("w1:p1".into()),
            workspace: Some("w1".into()),
            cwd: Some("/home/ed/claude/wsp".into()),
        }
    }

    /// The whole point of the command: which link won, and what it beat. A
    /// binding outranks the directory, so a pane that claimed strata work keeps
    /// saying strata after you cd into the wsp tree — and the command says so
    /// out loud rather than leaving you to wonder.
    #[test]
    fn where_says_which_link_won_and_what_it_beat() {
        let w = standing();
        let at = locate(&w);
        assert_eq!(at.project.as_deref(), Some("strata"));
        assert_eq!(at.source, "binding");
        assert_eq!(at.by_cwd.as_deref(), Some("wsp"), "and what the directory alone would have said");

        let text = where_lines(&at, &Paint::new()).join("\n");
        assert!(text.contains("strata  via binding"), "{text}");
        assert!(text.contains("cwd alone would say wsp"), "the disagreement is the answer:\n{text}");
        assert!(text.contains("wsp release"), "…and what to do about it:\n{text}");
        assert!(text.contains("a task filed somewhere else"), "the task it is holding:\n{text}");
    }

    /// When the two links agree there is nothing to say, and saying "cwd would
    /// also say wsp" is a line that never means anything.
    #[test]
    fn a_pane_standing_where_its_work_lives_gets_no_second_opinion() {
        let mut w = standing();
        w.tasks[0].project = Some("wsp".into());
        let at = locate(&w);
        assert_eq!(at.project.as_deref(), Some("wsp"));

        let text = where_lines(&at, &Paint::new()).join("\n");
        assert!(!text.contains("cwd alone"), "nothing disagrees:\n{text}");
        assert!(text.contains("rust"), "the project's tags are what a pane inherits:\n{text}");
    }

    /// A pin is the top of the chain and beats everything under it, which is
    /// what makes it worth having: it is the one link a person sets by hand.
    #[test]
    fn a_pin_beats_the_binding_and_the_directory_both() {
        let mut w = standing();
        w.pins.insert("w1".to_string(), "wsp".to_string());
        let at = locate(&w);
        assert_eq!((at.project.as_deref(), at.source), (Some("wsp"), "pin"));
    }

    /// herdr silent costs the chain one link — the workspace label — and the
    /// rest of it answers as it did. A pane with nothing else to go on falls
    /// through to the directory rather than to nothing.
    #[test]
    fn no_herdr_costs_the_chain_its_label_and_no_more() {
        let mut w = standing();
        w.workspaces.clear();
        w.bindings.clear();
        let at = locate(&w);
        assert_eq!((at.project.as_deref(), at.source), (Some("wsp"), "cwd"));
        assert!(at.label.is_none());
        assert!(at.task.is_none(), "no binding, so nothing in hand");

        // And with not even a directory it says so, rather than guessing.
        w.cwd = None;
        let at = locate(&w);
        assert_eq!(at.project, None);
        assert_eq!(at.source, "none");
        assert!(where_lines(&at, &Paint::new()).join("\n").contains("no project resolved"));
    }

    /// The json is the same reckoning as the text, and carries the two ids a
    /// caller needs to act on it.
    #[test]
    fn where_json_carries_the_pane_it_answered_for() {
        let w = standing();
        let v = where_json(&w, &locate(&w));
        assert_eq!(v["project"], json!("strata"));
        assert_eq!(v["source"], json!("binding"));
        assert_eq!(v["by_cwd"], json!("wsp"));
        assert_eq!(v["pane_id"], json!("w1:p1"));
        assert_eq!(v["workspace_id"], json!("w1"));
        assert_eq!(v["task"]["id"], json!("t-001"));
    }


    // ---- doctor and peek, offline ------------------------------------------

    /// Three answers, not two. A machine with no herdr is normal; a socket
    /// that will not answer is broken; a herdr answering with nothing is a
    /// herdr with no agents in it — and the third reads exactly like the
    /// second if you only look at the length of the list.
    #[test]
    fn doctor_tells_no_herdr_from_a_broken_one_from_an_empty_one() {
        let mut bindings = std::collections::BTreeMap::new();
        bindings.insert("w1:p1".to_string(), json!({ "task_id": "t-001" }));

        let say = |probe: Probe| {
            let (mut problems, mut notes) = (Vec::new(), Vec::new());
            herdr_health(&probe, &bindings, &[], &mut problems, &mut notes);
            (problems, notes)
        };

        // Not there. A note, never a problem: wsp works on a machine with no
        // herdr on it, and calling that broken is how a check nobody can act
        // on gets ignored along with the ones they can.
        let (problems, notes) = say(Probe::Down);
        assert!(problems.is_empty(), "{problems:?}");
        assert!(notes.iter().any(|n| n.contains("socket not found")), "{notes:?}");

        // There and not answering. A problem, and it says what herdr said.
        let (problems, notes) = say(Probe::Unreachable("connection refused".into()));
        assert!(problems.iter().any(|p| p.contains("connection refused")), "{problems:?}");
        assert!(notes.is_empty(), "{notes:?}");

        // Answering with nothing. Not a problem either — a herdr with no panes
        // in it is a herdr. And it says nothing about the binding, because a
        // machine that listed no panes is a machine that has not been heard
        // from: that is the judgement `sync` makes before it reaps, and the two
        // have to agree or `doctor` advises a sweep that will not happen.
        let (problems, notes) = say(Probe::Up { agents: Vec::new(), panes: Vec::new() });
        assert!(problems.is_empty(), "an empty herdr is a running herdr: {problems:?}");
        assert!(!notes.iter().any(|n| n.contains("binding")), "silence is not evidence: {notes:?}");
        assert!(notes.iter().any(|n| n.starts_with("herdr up, 0 agents")), "{notes:?}");

        // And answering with the pane the binding names: nothing stale.
        let busy = wip_agent("w1:p1", "w1", "working", "");
        let (problems, notes) = say(Probe::Up { agents: vec![busy.clone()], panes: vec![busy] });
        assert!(!notes.iter().any(|n| n.contains("binding")), "{notes:?}");
        assert!(!notes.iter().any(|n| n.contains("agent gone")), "{notes:?}");
        assert!(problems.is_empty(), "a turn in flight is the healthy case: {problems:?}");
        // The correction robustness-083 made to this line. "12 agents" was
        // read as twelve agents working on the night seven of them had
        // stopped, and the count that answers that question is the second one.
        assert!(
            notes.iter().any(|n| n == "herdr up, 1 agents — 1 of 1 claimed pane(s) running a turn"),
            "{notes:?}"
        );
    }

    /// The one note that mentions a thing, or the whole list to read when there
    /// is none — the useful half of a failure here is what `doctor` said instead.
    fn note_about<'a>(notes: &'a [String], word: &str) -> &'a str {
        match notes.iter().find(|n| n.contains(word)) {
            Some(n) => n,
            None => panic!("nothing about `{word}` in {notes:?}"),
        }
    }

    /// The bug the whole distinction exists for. `doctor` asked `agent.list`
    /// alone and called every miss a dead pane needing `wsp sync`; `sync` reaps
    /// against `pane.list`, found the pane alive, and reapt nothing — so the
    /// line printed again on every run, indefinitely.
    ///
    /// Asserted as three states rather than as "not the old wording", because
    /// what went wrong was the diagnosis: a pane that is gone and a pane whose
    /// agent is gone need different sentences and different verbs, and the
    /// emptied one must not be sent to the sweep that ignores it.
    #[test]
    fn a_pane_whose_agent_exited_is_not_reported_as_a_dead_pane() {
        let mut bindings = std::collections::BTreeMap::new();
        for (pane, task) in [("w1:p1", "t-001"), ("w2:p1", "t-002"), ("w3:p1", "t-003")] {
            bindings.insert(pane.to_string(), json!({ "task_id": task }));
        }

        // w1:p1 has an agent in it. w2:p1 is the emptied one — herdr still
        // lists the pane, holding the shell the agent was started from, and
        // `agent.list` has dropped it. w3:p1 herdr does not list at all.
        let busy = wip_agent("w1:p1", "w1", "working", "");
        let emptied = labelled("w2:p1", "w2", "");
        let (mut problems, mut notes) = (Vec::new(), Vec::new());
        let probe = Probe::Up { agents: vec![busy.clone()], panes: vec![busy, emptied] };
        herdr_health(&probe, &bindings, &[], &mut problems, &mut notes);
        assert!(problems.is_empty(), "{problems:?}");

        let gone = note_about(&notes, "no longer lists");
        assert!(gone.starts_with("1 binding(s)"), "{gone}");
        assert!(gone.contains("wsp sync"), "the sweep is right for a pane that is gone: {gone}");

        let empty = note_about(&notes, "agent gone");
        assert!(empty.starts_with("1 pane(s)"), "{empty}");
        assert!(empty.contains("t-002 (w2:p1)"), "named by the work to decide about: {empty}");
        assert!(!empty.contains("t-001"), "an agent that is working is not emptied: {empty}");
        assert!(!empty.contains("t-003"), "an absent pane is the other state: {empty}");
        assert!(empty.contains("wsp despawn"), "the instruction has to be one that works: {empty}");
        assert!(empty.contains("wsp spawn"), "and the decision after it is spawn, or not: {empty}");
        assert!(empty.contains("claim"), "nothing here throws a claim away silently: {empty}");
    }

    /// The same states read off a socket rather than handed in, which is where
    /// the fault actually was: a probe that asks herdr once could pass every
    /// assertion above and still report an emptied pane as a dead one, because
    /// nothing above makes the two listings come from the same server.
    ///
    /// The fake carries the asymmetry as a live herdr gave it on 2026-08-17 —
    /// an agent that has gone leaves its pane in `pane.list`, holding the shell
    /// it was started from, and drops out of `agent.list`.
    #[test]
    fn a_probe_over_the_socket_sees_the_pane_the_agent_listing_dropped() {
        let env = util::isolated("doctor-emptied");
        let stage = crate::fake::Stage::of(vec![
            crate::fake::Spot::agent("w1:p1", "claude", "t-001", crate::place::State::Working),
            crate::fake::Spot::agent("w2:p1", "claude", "t-002", crate::place::State::Gone),
        ]);
        let fake = crate::fake::Fake::bind(env.path("herdr.sock"), stage).expect("a socket");
        let (k, v) = fake.socket_env();
        std::env::set_var(k, v);

        let mut bindings = std::collections::BTreeMap::new();
        for (pane, task) in [("w1:p1", "t-001"), ("w2:p1", "t-002"), ("w3:p1", "t-003")] {
            bindings.insert(pane.to_string(), json!({ "task_id": task }));
        }

        let (mut problems, mut notes) = (Vec::new(), Vec::new());
        herdr_health(&Probe::live(), &bindings, &[], &mut problems, &mut notes);
        assert!(problems.is_empty(), "{problems:?}");
        assert!(notes.iter().any(|n| n.starts_with("herdr up, 1 agents")), "{notes:?}");

        let empty = note_about(&notes, "agent gone");
        assert!(empty.contains("t-002 (w2:p1)"), "{empty}");
        assert!(empty.contains("wsp despawn"), "{empty}");
        // The line the bug printed forever, and the reason it never came true.
        assert!(!empty.contains("wsp sync` reaps"), "{empty}");

        let gone = note_about(&notes, "no longer lists");
        assert!(gone.starts_with("1 binding(s)"), "only w3:p1 is actually gone: {gone}");
    }

    /// A machine that said nothing is not a machine whose agents stopped — the
    /// reap guard, asked one layer up. `doctor` has to make the same call as
    /// `sync` or it goes back to advising a sweep that correctly refuses.
    #[test]
    fn a_binding_on_a_machine_that_did_not_answer_is_not_reported_at_all() {
        let mut bindings = std::collections::BTreeMap::new();
        bindings.insert("w0:p1@mb2".to_string(), json!({ "task_id": "t-001" }));
        bindings.insert("w9:p9".to_string(), json!({ "task_id": "t-002" }));

        // Only this machine listed panes, so only this machine's bindings are
        // examined — and `w9:p9` is not among the panes it listed.
        let here = wip_agent("w1:p1", "w1", "working", "");
        let (mut problems, mut notes) = (Vec::new(), Vec::new());
        let probe = Probe::Up { agents: vec![here.clone()], panes: vec![here] };
        herdr_health(&probe, &bindings, &[], &mut problems, &mut notes);
        assert!(problems.is_empty(), "{problems:?}");

        let gone = note_about(&notes, "no longer lists");
        assert!(gone.starts_with("1 binding(s)"), "mb2 was never heard from: {gone}");
        assert!(!gone.contains("mb2"), "{gone}");
    }

    fn labelled(pane: &str, ws: &str, label: &str) -> herdr::Pane {
        herdr::Pane {
            pane_id: pane.to_string(),
            workspace_id: ws.to_string(),
            label: label.to_string(),
            ..Default::default()
        }
    }

    fn no_task(_: &str) -> Option<Holder> {
        None
    }

    fn at(p: Peeked) -> (String, String) {
        match p {
            Peeked::At(pane, what) => (pane, what),
            Peeked::Surface => panic!("the surface, which has no pane"),
            Peeked::Nothing(hint) => panic!("nothing: {hint}"),
            Peeked::Unknown => panic!("unknown"),
        }
    }

    /// There is a panel in every workspace, so "the panel" has to mean yours —
    /// and peeking at another workspace's is still a real thing to want, which
    /// is why the fallback exists rather than a refusal.
    #[test]
    fn peek_prefers_the_panel_in_your_own_workspace_and_takes_any_other() {
        let panes = vec![
            labelled("w1:p9", "w1", crate::panel::PANEL_LABEL),
            labelled("w2:p9", "w2", crate::panel::PANEL_LABEL),
            labelled("w2:p8", "w2", crate::panel::VIEW_LABEL),
        ];
        assert_eq!(at(peek_target(&panes, Some("w2"), None, false, "", no_task)).0, "w2:p9");
        assert_eq!(at(peek_target(&panes, Some("w1"), None, false, "panel", no_task)).0, "w1:p9");

        // A workspace with no panel of its own falls through to one that has.
        assert_eq!(at(peek_target(&panes, Some("w7"), None, false, "panel", no_task)).0, "w1:p9");
        // …and so does a caller with no workspace at all, which is what a
        // shell outside herdr is.
        assert_eq!(at(peek_target(&panes, None, None, false, "panel", no_task)).0, "w1:p9");

        // The view is a different surface with a different label, and asking
        // for it from a workspace that has none finds the one that exists.
        assert_eq!(at(peek_target(&panes, Some("w1"), None, false, "view", no_task)).0, "w2:p8");
    }

    /// Missing is named as what is missing, not as what was asked for: "no view
    /// pane open" is a fact you can act on, "nothing to peek at for view" is
    /// not — and the two surfaces are opened in different ways, so the hint has
    /// to differ too.
    #[test]
    fn a_target_that_is_not_open_says_how_to_open_it() {
        let none: Vec<herdr::Pane> = Vec::new();
        let hint = |needle: &str| match peek_target(&none, Some("w1"), None, false, needle, no_task) {
            Peeked::Nothing(h) => h,
            _ => panic!("expected nothing for {needle}"),
        };
        assert!(hint("").contains("wsp panel install"));
        assert!(hint("panel").contains("wsp panel install"));
        assert!(hint("view").contains("↵` on a row in the panel"));
        assert!(hint("board").contains("wsp wip"));

        // A needle nothing recognises is a different answer again: the target
        // does not exist, rather than existing and being empty.
        assert!(matches!(peek_target(&none, None, None, false, "banana", no_task), Peeked::Unknown));
    }

    /// Under the fork the sidebar is not a pane, so `peek panel` has to mean
    /// the thing on the screen rather than the thing wearing the label. A
    /// panel pane standing beside a running surface is a husk or a deliberate
    /// comparison, and neither is what the person is looking at.
    #[test]
    fn a_running_surface_is_what_the_panel_means_and_a_pane_only_without_one() {
        let panes = vec![labelled("w1:p9", "w1", crate::panel::PANEL_LABEL)];
        let panel = |surface| peek_target(&panes, Some("w1"), None, surface, "panel", no_task);
        assert!(matches!(panel(true), Peeked::Surface));
        assert_eq!(at(panel(false)).0, "w1:p9");

        // The bare needle is the same question, and the ordinary state under
        // the fork is that there is no panel pane to fall back to at all —
        // where the answer used to be advice to install one.
        let none: Vec<herdr::Pane> = Vec::new();
        assert!(matches!(peek_target(&none, Some("w1"), None, true, "", no_task), Peeked::Surface));

        // Nothing else moves. A surface is one panel, not a new way to name
        // panes, and the view and the board are panes as they were.
        assert_eq!(at(peek_target(&panes, None, None, true, "w1:p9", no_task)).1, "pane w1:p9");
        assert!(matches!(
            peek_target(&none, Some("w1"), None, true, "view", no_task),
            Peeked::Nothing(_)
        ));
    }

    /// A pane id is taken literally, but only if a pane wears it — otherwise it
    /// falls through to the task search, where `w9:p1` is a needle like any
    /// other rather than a confident answer that is wrong.
    #[test]
    fn a_pane_id_is_only_a_pane_id_if_a_pane_has_it() {
        let panes = vec![labelled("w1:p3", "w1", "")];
        assert_eq!(at(peek_target(&panes, None, None, false, "w1:p3", no_task)).1, "pane w1:p3");
        assert!(matches!(peek_target(&panes, None, None, false, "w9:p1", no_task), Peeked::Unknown));

        // `me` is this pane, whatever herdr reported — and a caller herdr has
        // not told about itself has no `me` to look at.
        assert_eq!(at(peek_target(&panes, None, Some("w1:p3"), false, "me", no_task)).0, "w1:p3");
        assert!(matches!(peek_target(&panes, None, None, false, "me", no_task), Peeked::Nothing(_)));
    }

    /// A task names whichever pane is holding it, which is how you look at what
    /// another agent is doing without knowing where it is sitting. A task
    /// nobody is holding is not a failure to find the task.
    #[test]
    fn a_task_resolves_to_whoever_is_holding_it() {
        let panes = vec![labelled("w4:p1", "w4", "")];
        let held = |_: &str| {
            Some(Holder {
                id: "t-001".into(),
                title: "the seam under the panel".into(),
                panes: vec!["w4:p1".into()],
            })
        };
        let (pane, what) = at(peek_target(&panes, None, None, false, "001", held));
        assert_eq!(pane, "w4:p1");
        assert!(what.starts_with("t-001 — the seam under the panel"), "{what}");

        let unheld = |_: &str| {
            Some(Holder { id: "t-002".into(), title: "nobody is on it".into(), panes: Vec::new() })
        };
        assert!(matches!(peek_target(&panes, None, None, false, "002", unheld), Peeked::Nothing(_)));
    }


    // ---- the standing-in chain, offline ------------------------------------

    fn here() -> Here {
        let mut wsp = crate::model::Project::new("wsp");
        wsp.roots = vec!["/home/ed/claude/wsp".into()];
        let mut strata = crate::model::Project::new("strata");
        strata.roots = vec!["/home/ed/claude/strata".into()];
        let mut music = crate::model::Project::new("music");
        music.name = "Trance Video".into();

        Here {
            index: Index::new(vec![wsp, strata, music]),
            pins: std::collections::BTreeMap::new(),
            bindings: std::collections::BTreeMap::new(),
            claims: std::collections::BTreeMap::new(),
            mandates: std::collections::BTreeMap::new(),
            tasks: vec![wip_task("t-001", "somebody else's tree", Some("strata"), "doing")],
            workspaces: vec![herdr::Workspace {
                id: "w1".into(),
                label: "Trance Video".into(),
                ..Default::default()
            }],
            pane: Some("w1:p1".into()),
            workspace: Some("w1".into()),
            cwd: Some("/home/ed/claude/wsp".into()),
        }
    }

    /// The two steps this chain decides for itself rather than leaving to
    /// `resolve`. A mandate says what a workspace is *for* and beats the
    /// directory the shell happens to be sitting in; work actually in hand
    /// beats both, for as long as it is held.
    #[test]
    fn a_mandate_beats_the_directory_and_loses_to_work_in_hand() {
        let mut h = here();
        // Nothing but a directory: the directory wins.
        assert_eq!(standing_in(&h).as_deref(), Some("wsp"));

        // A mandate over the top of it.
        h.mandates.insert("w1".to_string(), json!({ "project": "music" }));
        assert_eq!(standing_in(&h).as_deref(), Some("music"), "what this workspace is for");

        // And a binding over the top of that: what it is *doing* beats what it
        // is *for*, for as long as it is holding it.
        h.bindings.insert("w1:p1".to_string(), json!({ "task_id": "t-001" }));
        assert_eq!(standing_in(&h).as_deref(), Some("strata"), "work in hand wins");

        // A pin is the person's own word and beats everything under it.
        h.pins.insert("w1".to_string(), "wsp".to_string());
        assert_eq!(standing_in(&h).as_deref(), Some("wsp"));
    }

    /// The bottom of the chain is the only link herdr supplies, so it is the
    /// only one that goes away when herdr stops answering. Everything above it
    /// is the store's and answers the same either way.
    #[test]
    fn the_workspace_label_is_the_last_resort_and_the_only_herdr_link() {
        let mut h = here();
        h.cwd = None;
        assert_eq!(standing_in(&h).as_deref(), Some("music"), "matched by its name");

        // herdr silent: the label link is gone and nothing else is.
        h.workspaces.clear();
        assert_eq!(standing_in(&h), None);
        h.cwd = Some("/home/ed/claude/wsp".into());
        assert_eq!(standing_in(&h).as_deref(), Some("wsp"), "the directory is the store's");

        // A label that matches no project resolves to nothing rather than to
        // the label — a workspace called after a film is not a project.
        let mut h = here();
        h.cwd = None;
        h.workspaces[0].label = "something nobody named a project".into();
        assert_eq!(standing_in(&h), None);
    }

    /// A mandate names a machine as well as a workspace: workspace ids are
    /// herdr's and mean nothing on another host, which is why a claim carries
    /// one too. Somebody else's mandate must not place this pane.
    #[test]
    fn a_mandate_from_another_machine_places_nobody() {
        let mut h = here();
        h.cwd = None;
        h.workspaces.clear();
        h.mandates.insert(
            "w1".to_string(),
            json!({ "project": "music", "host": "some-other-box" }),
        );
        assert_eq!(standing_in(&h), None, "a workspace id from another host names nothing here");

        // The same record with this machine's name on it does place it.
        h.mandates.insert(
            "w1".to_string(),
            json!({ "project": "music", "host": crate::util::hostname() }),
        );
        assert_eq!(standing_in(&h).as_deref(), Some("music"));
    }

    /// A pane herdr has told us nothing about — a shell outside it, or a hook
    /// with no environment — falls through to the directory, which is the one
    /// link that needs neither a pane nor a workspace.
    #[test]
    fn a_pane_herdr_never_named_still_has_a_directory() {
        let mut h = here();
        h.pane = None;
        h.workspace = None;
        h.bindings.insert("w1:p1".to_string(), json!({ "task_id": "t-001" }));
        assert_eq!(standing_in(&h).as_deref(), Some("wsp"), "the binding is not this pane's");

        h.cwd = Some("/tmp".into());
        assert_eq!(standing_in(&h), None, "and nowhere is an answer");
    }

    /// A store of bindings, in the shape `claim`, `reconcile` and `adopt`
    /// actually write them — including `adopt`'s, which carries no
    /// `agent_session_id` key at all.
    fn bound(rows: &[(&str, &str, Option<&str>)]) -> BTreeMap<String, Value> {
        rows.iter()
            .map(|(pane, task, session)| {
                let mut b = json!({ "task_id": task, "pane_id": pane });
                if let Some(s) = session {
                    b["agent_session_id"] = json!(s);
                }
                (pane.to_string(), b)
            })
            .collect()
    }

    /// The defect, at the line that decides it: every binding this store has
    /// ever written carried `""`, because a claim is made before the agent that
    /// would have a session exists. So the empty field is not a failure to
    /// record — it is the state a seat is in between the claim and the agent,
    /// and this is the step that ends it.
    #[test]
    fn a_seat_claimed_before_its_agent_existed_learns_the_session_when_one_appears() {
        let store = bound(&[("w1:p1", "robustness-060", Some(""))]);

        // Between claim and start: herdr has a pane and nothing in it.
        let nothing = sessions_learned(&store, [("w1:p1", "")].into_iter());
        assert!(nothing.is_empty(), "no agent, no session, nothing to write");

        // The agent comes up and herdr says which session it is.
        let learned = sessions_learned(&store, [("w1:p1", "309b2e2c")].into_iter());
        assert_eq!(learned, vec![("w1:p1".to_string(), "309b2e2c".to_string())]);

        // An `adopt`ed binding has no such key and is in exactly the same
        // position, so absence has to read as empty rather than as "skip".
        let adopted = bound(&[("w1:p1", "robustness-060", None)]);
        assert_eq!(sessions_learned(&adopted, [("w1:p1", "309b2e2c")].into_iter()).len(), 1);
    }

    /// **Silence is not a correction**, and this is the reading that would
    /// destroy the whole point of the field. A pane whose agent has died carries
    /// no `agent_session`, and that is precisely when the id matters: it is what
    /// `claude --resume` needs to bring the same session back after an ssh drops
    /// (`wsp-073`). Writing the empty string over it would delete the fact at the
    /// instant it became useful — the same shape as `sync` reaping every binding
    /// on one pane list that timed out.
    #[test]
    fn a_seat_whose_agent_died_keeps_the_session_it_was_last_seen_with() {
        let store = bound(&[("w1:p1", "robustness-060", Some("309b2e2c"))]);

        assert!(
            sessions_learned(&store, [("w1:p1", "")].into_iter()).is_empty(),
            "a dead agent's silence must not erase the id a resume is keyed on"
        );
        assert!(
            sessions_learned(&store, [("w1:p1", "   ")].into_iter()).is_empty(),
            "and a blank one is silence too"
        );
    }

    /// A *different* session is a correction and is taken. `/clear` mints a new
    /// id inside the same process, and a restart by hand mints one in the same
    /// pane — so a recorded id can stop naming the agent that is sitting there,
    /// and a stale one is worse than none because it resumes the wrong
    /// conversation.
    #[test]
    fn a_seat_now_holding_another_session_is_corrected_to_it() {
        let store = bound(&[("w1:p1", "robustness-060", Some("309b2e2c"))]);

        let learned = sessions_learned(&store, [("w1:p1", "f1487db7")].into_iter());
        assert_eq!(learned, vec![("w1:p1".to_string(), "f1487db7".to_string())]);

        // And the ordinary tick, where nothing has changed, writes nothing:
        // `sync` runs this every 20s against every pane on the machine.
        let same = sessions_learned(&store, [("w1:p1", "309b2e2c")].into_iter());
        assert!(same.is_empty(), "a store write per tick per pane is not a projection");
    }

    /// Only seats wsp has bound. herdr reports a session for every agent on the
    /// machine — a colleague's pane, the governor, a `claude` somebody started
    /// by hand — and none of those is a binding this may invent.
    #[test]
    fn a_session_in_a_seat_wsp_has_not_bound_teaches_it_nothing() {
        let store = bound(&[("w1:p1", "robustness-060", Some(""))]);
        let seen = [("w1:p1", "309b2e2c"), ("w9:p1", "77fa398b")];

        let learned = sessions_learned(&store, seen.into_iter());
        assert_eq!(learned, vec![("w1:p1".to_string(), "309b2e2c".to_string())]);
    }

    /// A store on disk with one pane holding one task.
    ///
    /// The cases below stop before anything reaches herdr on purpose: a real
    /// herdr is usually up on the machine running these tests, and `sync`
    /// writes metadata to whatever it finds there. What is under test is which
    /// claim the command *selects*, which is decided before any of that.
    fn holding(tag: &str, pane: &str, task: &str) -> Store {
        let root = std::env::temp_dir().join(format!("wsp-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = Store::at(root.clone(), root.join("state"));
        store.ensure_dirs().unwrap();
        for id in [task, "probe-040"] {
            store.save_task(&Task::new("Retune the early reflections", id)).unwrap();
        }
        store.set_binding(pane, json!({ "task_id": task, "pane_id": pane }));
        store.set_claim(task, json!({ "workspace_id": "w1", "claimed_at": util::now_iso() }));
        store
    }

    /// The defect, at the line that decides it. `wsp release probe-040` used to
    /// drop the id on the floor and unbind the calling pane, then print
    /// `released w1:p1` — a word and a pane id that read enough like success to
    /// move on from, while the claim it ended was the one you were holding.
    #[test]
    fn a_release_that_names_a_task_never_ends_the_callers_own_claim() {
        let store = holding("release-names-a-task", "w1:p1", "batch-043");

        // Named, unheld, and this pane is where the command was typed.
        let code = release(&store, &Args::synth("release", &["probe-040"], &[]));
        assert_eq!(code, 0, "asking for a claim that is not there is not an error");
        assert_eq!(
            store.bindings().get("w1:p1").and_then(|b| b["task_id"].as_str()),
            Some("batch-043"),
            "the caller's own claim is not what `release <id>` was asked about"
        );
        assert!(store.claims().contains_key("batch-043"));

        // An id that names nothing stops before it touches anything at all,
        // rather than falling back to this pane — the fallback *is* the bug.
        let code = release(&store, &Args::synth("release", &["nosuch-999"], &[]));
        assert_eq!(code, 1);
        assert!(store.bindings().contains_key("w1:p1"));
    }

    /// Two targets on one line, refused rather than ranked: `--pane` names a
    /// seat and an id names whatever seat holds a task, and choosing between
    /// them silently is how this command earned its defect.
    #[test]
    fn a_release_given_both_a_task_and_a_pane_does_neither() {
        let store = holding("release-two-targets", "w1:p1", "batch-043");

        let args = Args::synth("release", &["probe-040"], &[("pane", "w1:p1")]);
        assert_eq!(release(&store, &args), 2);
        assert!(store.bindings().contains_key("w1:p1"), "a refusal costs nothing");
        assert!(store.claims().contains_key("batch-043"));
    }
}
