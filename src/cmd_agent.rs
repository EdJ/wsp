//! Agent binding, context resolution, and the views that join the store to
//! the live seats.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use crate::cmd_govern;
use crate::cmd_task;
use crate::message::{self, About, Ask, Hand, Kind, Message, Party, Shape, Waiting};
use crate::model::{Status, Task};
use crate::place::{Delivery, Place, Refusal, State};
pub(crate) use crate::place::my_pane;
use crate::resolve::{self, Index};
use crate::store::Store;
use crate::util::{self, Paint};
use crate::Args;

/// One pane row, as the views in this file read one. Nothing fills it live
/// (`wsp-120`).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Pane {
    pub pane_id: String,
    pub label: String,
    pub workspace_id: String,
    pub agent: String,
    pub agent_status: String,
    pub cwd: String,
    pub title: String,
    pub focused: bool,
}

/// One workspace row, as [`Pane`].
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Workspace {
    pub id: String,
    pub label: String,
}

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
    panes_now: &'a [Pane],
    task: &str,
    me: Option<&str>,
) -> Vec<&'a Pane> {
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
/// mandate, and a host answering with the right label at once.
pub(crate) struct Here {
    pub index: Index,
    pub pins: std::collections::BTreeMap<String, String>,
    pub bindings: std::collections::BTreeMap<String, serde_json::Value>,
    pub claims: std::collections::BTreeMap<String, serde_json::Value>,
    pub mandates: std::collections::BTreeMap<String, serde_json::Value>,
    /// The agent sitting in `pane`, if one has ever claimed there — read for
    /// the mandate lookup below, which tries this before the legacy workspace
    /// key (`compound-092` stage B).
    pub agent: Option<String>,
    pub tasks: Vec<Task>,
    /// Read for one field, and only at the bottom of the chain: the label of
    /// the workspace this pane stands in.
    pub workspaces: Vec<Workspace>,
    pub pane: Option<String>,
    pub workspace: Option<String>,
    pub cwd: Option<String>,
}

impl Here {
    pub(crate) fn live(store: &Store, index: &Index) -> Here {
        let pane = my_pane();
        Here {
            index: Index::new(index.projects.clone()),
            pins: store.pins(),
            bindings: store.bindings(),
            claims: store.claims(),
            mandates: store.mandates(),
            agent: pane.as_deref().and_then(|p| store.agent_in_seat(p)),
            // Front matter, and `## Verification` for `verifying_project`:
            // all a `Here` reads of a task (`wsp-320`).
            tasks: store.tasks_with_sections(&[crate::verification::SECTION]),
            workspaces: Vec::new(),
            // The seat, through the one reading of it, rather than a host's
            // variable directly. A binding is keyed on a seat and a
            // supervisor's seats are not panes, so an agent hosted with no
            // terminal resolved to no binding at all: measured 2026-08-17, a
            // headless spawn whose `SessionStart` brief said "nothing claimed"
            // about the task the same command had just claimed for it.
            pane,
            workspace: None,
            cwd: std::env::current_dir().ok().map(|p| p.display().to_string()),
        }
    }
}

/// Walk the chain. Pure: everything it reads is in `h`.
/// The project of the member whose open pass names this seat.
fn verifying_project(tasks: &[Task], seat: &str) -> Option<String> {
    let member = crate::verification::seats(tasks).remove(seat)?;
    tasks.iter().find(|t| t.id == member).and_then(|t| t.project.clone())
}

pub(crate) fn standing_in(h: &Here) -> Option<String> {
    let bound_project = h.pane.as_ref().and_then(|pane| {
        h.bindings
            .get(pane)
            .and_then(|b| b.get("task_id"))
            .and_then(|t| t.as_str())
            .and_then(|id| h.tasks.iter().find(|t| t.id == id))
            .and_then(|t| t.project.clone())
            // A verifier's seat is bound to nothing, and is named on its
            // member's open pass instead (`wsp-188`). Read as a binding, so
            // its brief and `wip` place it where the member is — it read as
            // `compound` while verifying a wsp member.
            .or_else(|| verifying_project(&h.tasks, pane))
    });

    // A mandate is a statement about what this workspace is *for*, so it beats
    // the directory the shell happens to be sitting in — but not a pin, which
    // is a statement about what the workspace *is*, and not a binding, which is
    // the work actually in hand. Checked here rather than inside `resolve` so
    // that the panel and `overlap` go on placing panes by where they stand:
    // standing direction says nothing about which tree a pane is in.
    let mandate = crate::cmd_mandate::from_map(&h.mandates, h.agent.as_deref(), h.workspace.as_deref());

    let r = resolve::resolve(
        &h.index,
        &h.pins,
        resolve::Held {
            binding: bound_project,
            claim: resolve::claimed_project(&h.claims, &h.tasks, h.agent.as_deref(), h.workspace.as_deref(), None),
        },
        h.agent.as_deref(),
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

    // Last resort: the workspace's own label, which is the only link a host
    // supplies and the only one that goes away when it stops answering.
    let ws = h.workspace.as_deref()?;
    let w = h.workspaces.iter().find(|w| w.id == ws)?;
    h.index.project_for_label(&w.label)
}

/// The task this pane is holding, out of the two records that say so.
///
/// The binding is the live answer and the claim is the durable one, and the
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
    match named_project(args, index) {
        Some(Ok(p)) => Ok(p),
        Some(Err(said)) => {
            eprintln!("{said}");
            Err(1)
        }
        None => Ok(standing_in(&Here::live(store, index))),
    }
}

/// What `-p` names, or why it cannot: `None` when it was not given. Split from
/// [`current_project`] so the refusal is a string a test can read — the verbs
/// that print it are all `eprintln!` and an exit code.
pub(crate) fn named_project(args: &Args, index: &Index) -> Option<Result<Option<String>, String>> {
    let p = args.get("project")?;
    if p == "none" || p == "inbox" {
        return Some(Ok(None));
    }
    Some(match index.find(&p) {
        Some(found) => Ok(Some(found.id.clone())),
        None => Err(crate::util::no_such("project", &p)),
    })
}

fn pane_id(args: &Args) -> Option<String> {
    args.get("pane").or_else(my_pane)
}

/// The width every label wsp writes is cut to — 44 characters. A sidebar is
/// 26 columns and draws its
/// own ellipsis, so this is not about what fits: it is about not putting a
/// paragraph on the wire as a name.
///
/// It is the width of the *wire* and of nothing else. Every name below is
/// built whole and cut here, on the last line before it is sent, so a surface
/// with room — `panel --full` is a hundred columns of it — can read the whole
/// one back out of [`full_name`] and do its own cutting to its own width. The
/// rule this constant states was never the mistake; being the only copy was.
const LABEL_MAX: usize = 44;

/// The one cut, and the only place a name of ours is shortened for the wire.
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

/// A name for a pane holding no task, which is the only kind that has to be
/// checked for a name of ours.
///
/// `wsp` and `wsp:view` are withheld. They are how the panel finds its own
/// panes, and the tree drops every pane wearing one as furniture — an agent
/// that said "wsp" would vanish from it. Nothing scoped can collide: a scope
/// carries a `/` and those two do not.
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

/// The sentence a seat's agent last said about itself, while it is still
/// about the work the seat holds.
///
/// Not [`full_name`], which answers *what is the whole of this label* and is
/// invalidated by a rename. This answers *what did the agent say it is doing*,
/// for a surface drawing no label of ours — a compound pane header, `wsp wip`
/// — and what makes it stale is the seat's work changing under it: a sentence
/// said on one task is not a statement about the next one, or about none. The
/// task is compared rather than trusted to have been cleared, for the reason
/// `full_name` compares the label: every claim, release and handoff would
/// otherwise have to remember this record, and the one that forgot would put
/// a sentence about old work over new work.
pub(crate) fn sentence(said: &BTreeMap<String, Value>, pane: &str, held: Option<&str>) -> Option<String> {
    let entry = said.get(pane)?;
    let task = entry.get("task").and_then(|t| t.as_str());
    let said = entry.get("said").and_then(|s| s.as_str()).filter(|s| !s.is_empty())?;
    (task == held).then(|| said.to_string())
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
    // Who is raising it: `whoami`'s reading, the one `wsp ask` already makes —
    // the post a seat holds, else the pane, else a person at a shell. It read
    // one host's pane variable alone, which a compound-hosted agent does not
    // have, so every hand one raised was recorded as the person's (`wsp-272`:
    // three of the eight up on 2026-10-10 were agents' and seats', reading
    // `from: human`). `--pane` still names one outright.
    let (me, here) = match args.get("pane") {
        Some(p) => (Party::pane(&p, ""), Some(p)),
        None => crate::cmd_message::whoami(store),
    };
    let pane = here.unwrap_or_default();

    // The card's three parts, and each is optional because the cheap version of
    // this has to stay cheap: `wsp flag <id>` on its own still works and the
    // card fills itself in from the task. A title where the task's own is
    // wrong for the moment — "the store is corrupt", on a task called something
    // else — a body for the paragraph a row cannot hold, and an ask for the one
    // thing a keypress can answer.
    let title = args.get("title").unwrap_or_default();
    // A heading has no `-` form for the same reason the row text has none — it
    // is one line, and `--body -` is where a paragraph goes — and it gets the
    // same check for the same reason. The heading is what is quoted onto
    // somebody's panel, so a `--title` that came back carrying a command's
    // output instead of the command's name is the one part of the card nobody
    // can read past. See [`crate::util::terminal_output`].
    if let Some(why) = util::terminal_output(&title) {
        eprintln!("wsp: --title {why}");
        eprintln!("     a backtick inside double quotes runs a command. `wsp flag {} --from FILE` reads a paragraph out of a file", task.id);
        return 2;
    }
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
        Some(src) => match crate::util::read_source(src) {
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
    // The line the flight view names the hand by (`wsp-276`): `--caption`, or
    // the `--title` a card already draws as its heading, or — nothing said at
    // all — the row's own, since *look at this* is about the row. Decided
    // here, because `compose` is about to join the three into one text.
    let caption = match crate::caption::flag(args, false) {
        Ok(Some(c)) if !c.is_empty() => Some(crate::caption::Caption { text: c, from: "authored" }),
        Ok(_) if !title.trim().is_empty() => Some(crate::caption::Caption { text: crate::caption::title(&title), from: "title" }),
        Ok(_) if said.is_empty() && body.trim().is_empty() => {
            Some(crate::caption::Caption { text: crate::caption::of_task(&task).text, from: "task" })
        }
        Ok(_) => None,
        Err(code) => return code,
    };
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
    // from outside a seat would draw a `y` that could not do anything.
    if ask == "claim" && pane.is_empty() {
        eprintln!("wsp: --ask claim needs a pane to hand it to — run it inside a seat, or pass --pane");
        return 2;
    }

    // The three inputs become one text, headline first — see
    // [`message::compose`], which is where the invariant that keeps
    // `worklist-018` shut lives.
    let mut raised = Message::new(
        // No pane is a person at a shell, and that is the correct reading: the
        // one channel wsp does not control is a human keyboard, so unattributed
        // is a person rather than an unknown.
        me.clone(),
        // `wsp-095` Part 4: every verb's default is the quietest kind that verb
        // can honestly be, and `wsp flag` is `note`. A raised hand is read at
        // the reader's next turn boundary; nothing but `stop` interrupts.
        Kind::Note,
        &message::compose(said, &title, &body),
    );
    raised.about = About::Task(task.id.clone());
    raised.caption = caption;
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

    // **Into the seat's spool, now, and `wsp-146`'s second half of it.** A hand
    // used to reach a governor only as a `flag` *level*, which
    // `Line::disposition` spools rather than wakes — on purpose, because
    // `hooks/on-attention-raised` already reaches a person for free and a flag
    // is not worth 208k on its own. That reasoning is about a person with a
    // panel in front of them, and this row's finding is that an agent raising a
    // hand to its governor is addressing a *seat*, and the seat was not on the
    // hook. So the words go in the spool directly, where
    // `Class::Message` dispositions them to a wake, and the flag level still
    // goes everywhere else it always did.
    // Not to the seat raising it: a seat's own hand is for the person, and
    // spooling it back to the seat that typed it is a wake to read itself.
    let seat = cmd_govern::answering_seat(store, &task).filter(|s| me.scope() != Some(s.scope.as_str()));
    // `map`, not `and_then`: `wake::say` no longer answers "is there a seat
    // here" — it spools and reports, because a vacancy is later, not nowhere.
    let report = seat.as_ref().map(|s| {
        crate::wake::say(store, &s.scope, &for_a_governor(&raised, &task), Some(&raised.id))
    });

    if args.json() {
        println!(
            "{}",
            json!({ "id": task.id, "flagged": true, "said": said, "pane": pane, "message": raised.id, "caption": raised.caption().json() })
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
        // …and whether the seat has it yet, because `wsp-146` made *later* an
        // honest answer for the first time and an agent that cannot tell later
        // from never is how a hand stays up for a night. Nothing when there is
        // no seat: the line above already said *every panel*, and that is the
        // whole of where it went.
        if let Some(r) = &report {
            println!("  {}", r.said(&format!("the {} seat", r.scope), &p));
        }
    }
    0
}

/// A notice the run writes on a task, for the seat that answers for it.
///
/// **Not `wsp flag` with made-up arguments, which is what it was** (`wsp-272`).
/// [`crate::cycle::tell`] fell back to synthesising a `flag` call when a run
/// had no governor of its own — a list just closed, a seat stood down — and the
/// verb did what it does for a caller with no pane: wrote `from: human` and no
/// `for`. So every barrier pass that reached a project's seat this way also
/// went up on the person's panel and in compound's hand, reading as something
/// a person typed and still owed, after the seat it was for had it.
///
/// Now it says who wrote it ([`Party::wsp`]) and who it is for — the scope
/// [`cmd_govern::answering_seat`] reaches, the same walk `flag` hands to — and
/// the words go into that seat's spool with the `Sent` hop, as `flag`'s do.
/// With no seat anywhere the person is the only reader there is, so it is
/// theirs, as an unaddressed hand always was.
///
/// Returns the record, or `None` when the task is gone or the record refused.
pub(crate) fn notice(store: &Store, task: &str, text: &str) -> Option<Message> {
    let task = store.task(task)?;
    let seat = cmd_govern::answering_seat(store, &task);
    let mut m = Message::new(Party::wsp(), Kind::Note, text);
    m.about = About::Task(task.id.clone());
    m.for_seat = seat.as_ref().map(|s| s.scope.clone());
    message::raise(store, &m).ok()?;
    if let Some(s) = &seat {
        crate::wake::say(store, &s.scope, &for_its_seat(&m, &task), Some(&m.id));
    }
    Some(m)
}

/// A run notice as its seat reads it: the words, and that it is this seat's
/// to dispose of rather than the person's. Shorter than [`for_a_governor`]
/// because nobody raised a hand — wsp is telling the seat about its own run.
fn for_its_seat(m: &Message, task: &Task) -> String {
    format!(
        "{text}\n\n\
         wsp's notice on {task} ({id}), for this seat and not on the person's panel · \
         `wsp flag {id} --clear` takes it down · `wsp flag {task} \"…\"` puts it in front of the person",
        text = m.text,
        task = task.id,
        id = m.id,
    )
}

/// The hand as it goes to the seat, in the words its reader has to act on.
///
/// **The same shape [`crate::cmd_message::ask`] sends, and the difference is the
/// last line.** An ask is owed an answer and says so by naming `wsp answer`; a
/// notification is not, and claiming otherwise would put a governor in a seat
/// expecting to reply. What is owed — a disposition — is named instead, so the
/// sentence reads the same and the obligation is not a guess.
fn for_a_governor(m: &Message, task: &Task) -> String {
    format!(
        "{by} raised a hand on {task} ({id}):\n\n{body}\n\n\
         nothing is owed back — `wsp flag {task} --seen` marks it read, `--clear` takes it down\n\
         if it was already seen · `wsp ask {task} \"…\"` is the same thing with a return path",
        by = m.from.byline(),
        task = task.id,
        id = m.id,
        body = m.title(),
    )
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

    // `whoami`, as the raising side reads it: a compound-hosted seat taking a
    // hand down was recorded as the person doing it.
    let (by, _) = crate::cmd_message::whoami(store);
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
fn hand_aside(
    governors: &std::collections::BTreeMap<String, serde_json::Value>,
    m: &Message,
    seat: Option<&crate::governors::Seat>,
    held_here: bool,
) -> String {
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
    // Said on the row, because the text listing shows every hand and a seat's
    // notice must not read as one the person owes (`wsp-272`).
    if let Some(scope) = &m.for_seat {
        out.push(format!("for the {scope} seat, not the person"));
    }
    // Nothing when there is no seat above it, which is the ordinary case and
    // already reads as "this is yours" to the person looking at it.
    if let Some(s) = seat {
        out.push(match held_here {
            true => format!("{} yours · {}", glyph_seat(), s.scope),
            false => format!(
                "{} {} · {}",
                glyph_seat(),
                cmd_govern::governor_of(&s.scope),
                cmd_govern::room_of(governors, &s.scope)
            ),
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
    let governors = store.governors();
    // The walk, asked of one function — [`crate::cmd_govern::answering_seat`]'s
    // reason. It used to be spelled here as well, and this receipt and
    // `cmd_message`'s were already two of them.
    match cmd_govern::answering_seat(store, task) {
        Some(s) => format!(
            "raised to the {} governor · {} · x there lowers it",
            s.scope,
            cmd_govern::whereabouts(&governors, &s)
        ),
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
/// own — a label goes to a host, which has no column to put one in.
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
    //
    // `--person` is the person's open set and nothing else: what a panel or
    // compound's raised hand should draw, asked of wsp rather than guessed from
    // `via` or the wording (`wsp-272`). Its questions are `wsp yours`'s items,
    // because both read [`message::raised_to_the_person`] (`wsp-275`). Not `--for person` after the JSON
    // field, because the spell check would then offer `--for` for `--form`,
    // which is the slip `--from` is there to catch.
    let rows: Vec<Message> = match args.has("person") {
        true => message::raised_to_the_person(store),
        false => message::raised(store).into_iter().filter(|m| !m.is_reply()).collect(),
    }
    .into_iter()
    .rev()
    .collect();
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
    // Off `my_pane()` and not a host's workspace variable, `whoami`'s reason
    // (`compound-095`) and `compound-105`'s: a compound-hosted agent has no
    // workspace to read, only the seat, and `my_pane()` already picks the
    // right one either way.
    let mine =
        my_pane().and_then(|pane| crate::place::governs(&governors, &crate::place::Seat::new(pane)));
    let only_mine = args.has("seat");
    // Asked for an inbox from a pane that has no seat. Said plainly, because
    // "nothing raised for you" and "you are not the seat" look identical from
    // an empty list, and only one of them is worth acting on.
    if only_mine && mine.is_none() {
        println!("{}", p.dim("this seat is nobody's — wsp flag alone shows them all"));
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
        let seat = task.as_ref().and_then(|t| cmd_govern::seat_for_task(&governors, &index, lists.list_for(t), t, t.project.as_deref()));
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
        let aside = hand_aside(&governors, m, seat.as_ref(), held_here);
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

/// The log line naming the run a start belongs to, or `None` when nobody is
/// running this row.
///
/// **Read from the environment rather than from the store**, because the run is
/// known at the moment the spawn is issued and nowhere else: the claim records
/// *who* holds a task, not which group of which list asked for the agent on it.
/// `cycle::start` sets both variables and `cycle::owned_rows` reads the marker
/// this writes, so the two halves are named once each and cannot disagree.
fn run_owner() -> Option<String> {
    let list = std::env::var(crate::cycle::OWNED_LIST).ok()?;
    let group = std::env::var(crate::cycle::OWNED_GROUP).ok()?;
    (!list.is_empty() && !group.is_empty())
        .then(|| format!("{} {} group {group}", crate::cycle::OWNED, list))
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
///
/// **And it is not how a governor is reached, which `wsp-146` had to make
/// explicit.** This verb addresses a *task* and types at the pane that task is
/// held in, and a seat holds no task — so there is nothing here for it to reach
/// a governor with, and the reason it keeps its own delivery rather than
/// [`crate::wake`]'s is not sentiment: `twice` and
/// [`crate::place::Refusal::NotTaken`] are a contract with the person at the
/// keyboard (*you have it and you did not submit it*), and a governor is not
/// standing at one. `wsp ask <id> "…"` is the verb that reaches one — it is named
/// in the brief's seat line and in the work order.
pub fn tell(store: &Store, args: &Args) -> i32 {
    let p = Paint::new();
    if args.has("log") {
        return match args.get("log").filter(|a| a != "true").or_else(|| args.rest.first().cloned()) {
            Some(address) => crate::tell_log::show(store, &address, args.has("full"), args.json()),
            None => {
                eprintln!("usage: wsp tell --log <address|id> [--full] [--json]");
                2
            }
        };
    }
    let Some(needle) = args.rest.first().cloned() else {
        eprintln!("usage: wsp tell <id|address> \"…\"   (or `-` to read it from stdin)");
        eprintln!("       an address: primary, governor:<list|project>, seat:<row>, verifier:<row>, check:<list>, list:<name>");
        return 2;
    };
    let text = match message(args) {
        Ok(t) => t,
        Err(code) => return code,
    };
    // `--re <id>` says which message this answers, and an id nothing here sent
    // or raised is refused before anything is typed (`wsp-521`).
    let re = match args.get("re").filter(|r| r != "true") {
        Some(r) => match known_message(store, &r) {
            Ok(id) => Some(id),
            Err(why) => {
                eprintln!("wsp: --re `{r}`: {why}");
                return 1;
            }
        },
        None => match args.has("re") {
            true => {
                eprintln!("wsp: --re takes the id of the message this answers");
                return 2;
            }
            false => None,
        },
    };
    let from = crate::cmd_message::sender(store);
    // A reply to the person is an item on their list, not keystrokes in a pane.
    if needle.trim() == "person" {
        return tell_the_person(store, args, &text, re.as_deref());
    }

    // Not `peek_target`, and the difference is worth a line rather than the
    // reuse. That resolver's whole value is that it also finds the panel, the
    // view and the board, which are wsp's own panes and hold no agent — the
    // right answer for "show me what this looks like" and a hazard for a verb
    // that types prose into whatever it lands on. This resolves the two things
    // that can hold an agent and nothing else.
    // An address (`primary`, `governor:<scope>`, `seat:<row>`, …) names a
    // post rather than a row, and resolves first; a bare word is a row or a
    // seat id, as it always was. See [`crate::address`].
    let addressed = crate::address::parse(&needle);
    if let Some(crate::address::Address::List(name)) = &addressed {
        return tell_list(store, args, &from, re.as_deref(), &text, &needle, name);
    }
    let (seat, what) = match &addressed {
        Some(addr) => match addressed_seat(store, addr) {
            Ok(v) => v,
            Err(why) => {
                eprintln!("wsp: `{needle}`: {why}");
                return 1;
            }
        },
        None => match target(store, &needle, &compound_holds) {
            Some(v) => v,
            None => {
                eprintln!("wsp: nothing holds `{needle}` — `wsp wip` says who holds what");
                return 1;
            }
        },
    };
    tell_seat(store, args, &p, &from, re.as_deref(), &text, &needle, addressed.is_some(), None, seat, what)
}

/// `wsp tell list:<name> "…"` — the sentence to every live seat on the list,
/// each as its own message with its own id (the id is of the pane and the
/// text), and the sender's `cycle.log` gets one line for the lot with its count
/// (`wsp-523`). The sender's own seat is not told its own sentence.
fn tell_list(store: &Store, args: &Args, from: &str, re: Option<&str>, text: &str, needle: &str, name: &str) -> i32 {
    let p = Paint::new();
    let seats = match crate::address::resolve(&crate::address::Address::List(name.into()), &crate::address::Book::read(store)) {
        Ok(v) => v,
        Err(why) => {
            eprintln!("wsp: `{needle}`: {why}");
            return 1;
        }
    };
    let own = crate::place::seat_from_env().map(|s| s.to_string());
    let seats: Vec<_> = seats.into_iter().filter(|n| own.as_deref() != Some(n.pane.as_str())).collect();
    if seats.is_empty() {
        eprintln!("wsp: `{needle}`: nobody else is seated on {name}");
        return 1;
    }
    let (mut told, mut failed) = (0usize, 0usize);
    // One key for the lot, so `wsp tell --log` can draw the seats' events as one
    // fan-out: the list, the sentence and the moment it went.
    let fan = format!("{}@{}", message_id(needle, text), util::epoch_secs());
    for n in &seats {
        match tell_seat(store, args, &p, from, re, text, needle, true, Some((fan.as_str(), seats.len())), n.pane.clone(), n.what.clone()) {
            0 => told += 1,
            _ => failed += 1,
        }
    }
    crate::store::log_line(
        store,
        &format!("told {needle}: fan-out to {} seats ({told} told, {failed} not) from {from}", seats.len()),
    );
    store.log_event(
        "agent-told-list",
        json!({ "list": name, "from": from, "seats": seats.len(), "told": told, "at": util::epoch_secs() }),
    );
    if !args.json() {
        println!("{}", p.dim(&format!("{needle}: {} seats, {told} told", seats.len())));
    }
    match failed {
        0 => 0,
        _ => 1,
    }
}

/// The rest of a tell once its seat is known: the checks on the seat, the
/// footer, the repeat guard and the delivery. One seat, whichever address or
/// fan-out found it.
#[allow(clippy::too_many_arguments)]
fn tell_seat(
    store: &Store,
    args: &Args,
    p: &Paint,
    from: &str,
    re: Option<&str>,
    text: &str,
    needle: &str,
    addressed: bool,
    fan: Option<(&str, usize)>,
    seat: String,
    what: String,
) -> i32 {
    // What `wsp peek` and `--anyway` are told to name: the seat, for an
    // address, since neither verb reads one.
    let peek = match addressed {
        true => seat.clone(),
        false => needle.to_string(),
    };

    // Asked of every backend wsp can spawn onto (`compound-077`), not one
    // alone: only the census that made a seat can say whose it is. This is
    // the same fold `Wip::live` already does — `wsp tell` had its own copy of
    // "one backend or nothing" until this row, which is why a headless or
    // `compound`-hosted agent could not be told anything at all.
    // `--anyway` is a person saying they have looked at the seat and it is
    // waiting; see `place_compound::Compound::insist`.
    // Under `WSP_HOST=manager` the host is the manager's; one that is silent
    // holds the tell, for a seat this machine's records say exists.
    let said = dressed(from, &message_id(&seat, text), text, !args.has("no-reply"));
    let held = |why: &str| -> i32 {
        let n = crate::wake::hold_tell(store, &seat, &said);
        // Written as held, so the log shows it before the link returns and
        // `flush_held_tells` settles it after (`wsp-522`).
        store.log_event(
            "agent-told",
            json!({
                "target": addressee_id(store, &needle, &what), "pane": seat, "chars": text.len(),
                "id": message_id(&seat, &text), "from": from, "re": re, "text": text, "held": true,
                "at": util::epoch_secs(), "turn": false,
            }),
        );
        let word = format!("{what} is {why} — held ({n} waiting), delivered when the link is back and the seat is ready");
        match args.json() {
            true => println!("{}", json!({ "held": true, "pane": seat, "waiting": n })),
            false => println!("{} {word}", p.yellow("…")),
        }
        0
    };
    let backends = match crate::cmd_spawn::told_backends(args.has("anyway")) {
        Ok(b) => b,
        Err(e) => {
            if crate::place_compound::Compound::new().holds(&crate::place::Seat::new(&seat)) {
                return held("unreachable");
            }
            eprintln!("wsp: {e}");
            return 1;
        }
    };
    let Some((place, row)) = locate_seat(&backends, &seat) else {
        eprintln!("wsp: nothing answers for {seat} — `wsp wip` says who holds what");
        return 1;
    };
    // A seat on a machine that has gone silent is held, not gone, and a tell to
    // it waits in the wake spool for the link: `docs/remote-agents.md` section 3.
    // Only a seat that was found gets here; one nothing answers for was refused
    // above and nothing was kept for it.
    if row.state == State::Unreachable {
        return held("unreachable");
    }
    if row.agent.kind.is_empty() {
        match addressed {
            true => eprintln!("wsp: no agent in {seat} — the pane is alive and empty"),
            false => eprintln!("wsp: no agent in {seat} — the pane is alive and empty. `wsp spawn {needle}` puts one in it"),
        }
        return 1;
    }
    // The one state a message must not be sent into: a blocked agent has a
    // permission dialog holding the keyboard, so the text would be typed at it.
    if let Some(wait) = crate::waiting::on_screen(row.state) {
        eprintln!("wsp: {what} is {} — answer that first", wait.sentence());
        eprintln!("     `wsp peek {peek}` shows what it is asking");
        return 1;
    }

    // **A busy seat is queued for, not refused** (`wsp-659`). A tell to a seat
    // mid-turn was answered "look first, then `--anyway`" and delivered
    // nothing, so the primary's — the person's — sentence was lost unless
    // somebody retried it by hand. It waits in the seat's held-tell spool and
    // [`crate::wake::flush_held_tells`] types it at the seat's next idle, in
    // order; a seat that ends first has it said back to the sender. And a tell
    // to a seat that already has tells queued queues behind them, whatever the
    // seat reads now, so the order they were sent in is the order they land.
    // **Under `WSP_ORDERS=store` a tell is an order** (wsp-124): `order.send`,
    // and the daemon's delivery worker types it under `deliver:<seat>` when
    // the seat reads idle. The machine's queue is the queue, so a busy seat's
    // tell is an order at once and not a line in the held-tell spool.
    // `--anyway` (a person who has looked at the seat) keeps the files path;
    // wsp-130, the deletion row of s2-state-core-2, decides what it becomes.
    let by_order = !args.has("anyway") && crate::orders::mode() == crate::orders::Mode::Store;
    if by_order && queues_for_idle(row.state) {
        if let Some(code) = queue_as_order(store, args, p, from, re, text, needle, &said, &seat, &what, row.state) {
            return code;
        }
    }
    let queue = crate::wake::held_tells(store, &seat);
    let queued = queue.len();
    if !args.has("anyway") && (queues_for_idle(row.state) || (queued > 0 && row.state != State::Unknown)) {
        // The same sentence already waiting is not queued again: this path
        // returns before [`Sent::already_sent`], so a retry of a tell that was
        // queued would otherwise land twice. `--again` queues it anyway, as it
        // sends anyway below.
        if !args.has("again") && queue.iter().any(|(_, h)| *h == said) {
            match args.json() {
                true => println!(
                    "{}",
                    json!({ "held": true, "queued": true, "pane": seat, "waiting": queued, "already": true })
                ),
                false => println!(
                    "{} {what} has that sentence queued already ({queued} waiting) — not queued twice; `--again` queues it anyway",
                    p.dim("·")
                ),
            }
            return 0;
        }
        let row_id = store
            .bindings()
            .get(&seat)
            .and_then(|b| b.get("task_id").and_then(Value::as_str).map(str::to_string))
            .filter(|t| !t.is_empty())
            .or_else(|| store.find_task(needle).filter(|_| !addressed).map(|t| t.id));
        let n = crate::wake::hold_tell_for(store, &seat, &said, row_id.as_deref());
        store.log_event(
            "agent-told",
            json!({
                "target": addressee_id(store, needle, &what), "pane": seat, "chars": text.len(),
                "id": message_id(&seat, text), "from": from, "re": re, "text": text, "held": true,
                "queued": true, "at": util::epoch_secs(), "turn": false,
            }),
        );
        crate::store::log_line(store, &format!("told {what}: queued for its next idle ({n} waiting), from {from}"));
        let word = format!(
            "{what} is {} — queued ({n} waiting), typed at its next idle; if the seat ends first you are told",
            row.state.as_str()
        );
        match args.json() {
            true => println!("{}", json!({ "held": true, "queued": true, "pane": seat, "waiting": n })),
            false => println!("{} {word}", p.yellow("…")),
        }
        return 0;
    }

    // A seat that cannot vouch for itself refuses rather than typing into
    // whatever is on screen (`compound-097`); `--anyway` is the way past it.
    if !row.state.will_take_a_prompt() && !args.has("anyway") {
        eprintln!(
            "wsp: {what} cannot say whether it is at a prompt — it reads {}",
            row.state.as_str()
        );
        eprintln!("     look first, then `wsp tell {peek} --anyway -` if it is waiting");
        return 1;
    }

    let how = crate::agent_commands::of(&row.agent.kind);
    let addressee = match addressed {
        true => needle.to_string(),
        false => addressee_id(store, &needle, &what),
    };
    let sent = Sent::new(&addressee, &what, &seat, &peek, text, args).by(from, re, !args.has("no-reply")).in_fan(fan);
    if let Some(ago) = sent.already_sent(store) {
        if !args.has("again") {
            return twice(&sent, ago, p);
        }
    }
    if by_order {
        match tell_as_order(store, &seat, &said, from, crate::orders::TELL_WAIT) {
            Ok((_, Some(outcome))) => return delivered(store, outcome, &sent),
            Ok((id, None)) => return left_for_the_daemon(store, args, p, &sent, &id),
            // No API, or a store that holds no such seat yet: the files path
            // takes it, and cycle.log says so.
            Err(e) => crate::store::log_line(store, &format!("told {what}: not sent as an order ({e}); typed the files way")),
        }
    }
    match how.tell(place.as_ref(), &crate::place::Seat::new(&seat), &said) {
        // The host went silent between the census and the order: held, as a
        // seat found unreachable is, and asked again under the same order id.
        Err(Refusal::Unreachable(_)) => held("unreachable"),
        outcome => delivered(store, outcome, &sent),
    }
}

// ---- tells as orders (wsp-124) ---------------------------------------------

/// `said` to `seat` as an order: sent, its answer waited for up to `wait`, and
/// what it came to by then ([`crate::orders::said`]; `None` is left for the daemon).
/// An `Err` is an order that was not sent, which the caller types the files way.
pub(crate) fn tell_as_order(
    store: &Store,
    seat: &str,
    said: &str,
    from: &str,
    wait: std::time::Duration,
) -> Result<(wsp_store_contract::OrderId, Option<crate::place::Result<Delivery>>), crate::orders::Error> {
    let id = crate::orders::send(store, &wsp_store_contract::SeatId(seat.into()), said, &crate::orders::principal(from), false)?;
    let now = match crate::orders::answer(store, &id, wait)? {
        Some(o) => Some(o),
        None => crate::orders::order(store, &id)?,
    };
    Ok((id, now.as_ref().and_then(crate::orders::said)))
}

/// `seat`'s orders the daemon has still to type: `queued` and `typed`.
fn orders_waiting(store: &Store, seat: &str) -> Vec<wsp_store_contract::Order> {
    use wsp_store_contract::OrderState as S;
    crate::orders::orders_of(store, &wsp_store_contract::SeatId(seat.into()), &[S::Queued, S::Typed]).unwrap_or_default()
}

/// A tell whose order had not settled when the wait ran out: the daemon types
/// it, and the receipt is the files path's for a tell it holds.
fn left_for_the_daemon(store: &Store, args: &Args, p: &Paint, sent: &Sent, id: &wsp_store_contract::OrderId) -> i32 {
    let n = orders_waiting(store, &sent.pane).len().max(1);
    store.log_event(
        "agent-told",
        json!({
            "target": sent.target, "pane": sent.pane, "chars": sent.chars, "id": sent.id, "from": sent.from,
            "re": sent.re, "text": sent.text, "held": true, "order": id.0, "at": util::epoch_secs(), "turn": false,
        }),
    );
    crate::store::log_line(store, &format!("told {}: order {} left for the daemon ({n} waiting), from {}", sent.what, id.0, sent.from));
    match args.json() {
        true => println!("{}", json!({ "held": true, "pane": sent.pane, "waiting": n })),
        false => println!(
            "{} {} has not answered yet — left for the daemon as order {} ({n} waiting), typed under deliver:{}",
            p.yellow("…"),
            sent.what,
            id.0,
            sent.pane
        ),
    }
    0
}

/// A tell to a busy seat under `store`: an order at once, which the daemon
/// types at the seat's next idle, with the files path's queued receipt.
/// `None` when the order could not be sent, and the held-tell spool takes it.
#[allow(clippy::too_many_arguments)]
fn queue_as_order(
    store: &Store,
    args: &Args,
    p: &Paint,
    from: &str,
    re: Option<&str>,
    text: &str,
    needle: &str,
    said: &str,
    seat: &str,
    what: &str,
    state: State,
) -> Option<i32> {
    let waiting = orders_waiting(store, seat);
    if !args.has("again") && waiting.iter().any(|o| o.text == said) {
        let n = waiting.len();
        match args.json() {
            true => println!("{}", json!({ "held": true, "queued": true, "pane": seat, "waiting": n, "already": true })),
            false => println!(
                "{} {what} has that sentence queued already ({n} waiting) — not queued twice; `--again` queues it anyway",
                p.dim("·")
            ),
        }
        return Some(0);
    }
    let id = match crate::orders::send(store, &wsp_store_contract::SeatId(seat.into()), said, &crate::orders::principal(from), false) {
        Ok(id) => id,
        Err(e) => {
            crate::store::log_line(store, &format!("told {what}: not sent as an order ({e}); held the files way"));
            return None;
        }
    };
    let n = orders_waiting(store, seat).len().max(1);
    store.log_event(
        "agent-told",
        json!({
            "target": addressee_id(store, needle, what), "pane": seat, "chars": text.len(),
            "id": message_id(seat, text), "from": from, "re": re, "text": text, "held": true,
            "queued": true, "order": id.0, "at": util::epoch_secs(), "turn": false,
        }),
    );
    crate::store::log_line(store, &format!("told {what}: order {} queued for its next idle ({n} waiting), from {from}", id.0));
    let word = format!(
        "{what} is {} — queued as order {} ({n} waiting), typed by the daemon at its next idle",
        state.as_str(),
        id.0
    );
    match args.json() {
        true => println!("{}", json!({ "held": true, "queued": true, "pane": seat, "waiting": n })),
        false => println!("{} {word}", p.yellow("…")),
    }
    Some(0)
}

/// The sentence as the seat reads it (`wsp-521`): it opens by saying who
/// speaks, and — unless `--no-reply` — ends with a footer naming the reply.
///
/// **One line, and on the same line as the sentence's last word.** A seat's
/// argv refuses control characters, so the footer is the one part that is never
/// split, and it is appended after a space rather than a break. The id is the
/// one [`message_id`] makes of the sentence as typed, so the footer, the
/// `agent-told` event and `--re` all name the same thing.
pub(crate) fn dressed(from: &str, id: &str, text: &str, reply: bool) -> String {
    let opened = format!("{from} says: {}", text.trim_end());
    match reply {
        true => format!("{opened} [from {from} · reply: wsp tell {from} --re {id} -]"),
        false => opened,
    }
}

/// Who a [`dressed`] sentence says is speaking: the word before its first
/// ` says: `, read back so a tell held and never read can be said back to its
/// sender (`wsp-659`). `None` for text that was not dressed.
pub(crate) fn speaker(said: &str) -> Option<&str> {
    let (who, _) = said.split_once(" says: ")?;
    (!who.is_empty() && !who.contains(char::is_whitespace)).then_some(who)
}

/// A [`dressed`] sentence's own words: the opener and the reply footer taken
/// off, for saying back to its sender what it was.
pub(crate) fn undressed(said: &str) -> &str {
    let body = match speaker(said) {
        Some(_) => said.split_once(" says: ").map_or(said, |(_, b)| b),
        None => said,
    };
    match body.rfind(" [from ") {
        Some(i) if body.ends_with(" -]") => &body[..i],
        _ => body,
    }
}

/// Whether a seat reading this state is busy with something that ends of
/// itself — a turn, or the agent coming up — so a tell to it is queued for its
/// next idle rather than refused (`wsp-659`). Not `Blocked` (a person must
/// answer first) and not `Unknown` (nothing says it will ever read idle).
pub(crate) fn queues_for_idle(state: State) -> bool {
    matches!(state, State::Working | State::Starting)
}

/// The id a `--re` names, if this store knows the message: one a tell left on
/// the event log, or a record the message store holds. An unambiguous prefix is
/// enough, as it is for `wsp answer`.
fn known_message(store: &Store, needle: &str) -> Result<String, String> {
    let needle = needle.trim();
    if needle.is_empty() {
        return Err("no id given".into());
    }
    let mut ids: Vec<String> = store
        .events_of("agent-told")
        .iter()
        .filter_map(|d| d.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();
    ids.extend(message::all(store).into_iter().map(|m| m.id));
    ids.sort();
    ids.dedup();
    if ids.iter().any(|i| i == needle) {
        return Ok(needle.to_string());
    }
    let hits: Vec<&String> = ids.iter().filter(|i| i.starts_with(needle)).collect();
    match hits.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err("no message by that id — a footer carries it, and `wsp ask` lists what is open".into()),
        many => Err(format!("names {} messages — give more of it", many.len())),
    }
}

/// `wsp tell person "…"`: what a seat says back to the person lands on their
/// list, by [`crate::cmd_message`]'s road for a question only they answer
/// (`wsp-233`), and is not typed into any pane.
///
/// A shell is the person, so telling the person from one is refused rather
/// than raised: nobody is waiting, and the record would be an item whose
/// author is its reader.
fn tell_the_person(store: &Store, args: &Args, text: &str, re: Option<&str>) -> i32 {
    let (me, pane) = crate::cmd_message::whoami(store);
    let from = crate::cmd_message::sender(store);
    tell_the_person_as(store, args, text, re, me, pane, &from)
}

/// [`tell_the_person`] with the caller read already, so the item is proved
/// without exporting a pane into the process every other test shares.
fn tell_the_person_as(
    store: &Store,
    args: &Args,
    text: &str,
    re: Option<&str>,
    me: Party,
    pane: Option<String>,
    from: &str,
) -> i32 {
    let Some(pane) = pane else {
        eprintln!("wsp: that is the person, and this is a shell — there is nobody here to tell them from");
        return 1;
    };
    let held = store
        .bindings()
        .get(&pane)
        .and_then(|b| b.get("task_id").and_then(Value::as_str).map(str::to_string))
        .filter(|id| store.task(id).is_some())
        .unwrap_or_default();
    let about = match held.is_empty() {
        true => About::Nothing,
        false => About::Task(held.clone()),
    };
    let q = Message::question(me, Kind::Note, text, Waiting::new(&pane, &held)).about(about).for_the_person();
    if let Err(e) = message::raise(store, &q) {
        eprintln!("wsp: {e}");
        return 1;
    }
    if !held.is_empty() {
        crate::cmd_message::asked_of_the_person(store, &q, &held);
    }
    let id = message_id("person", text);
    store.log_event(
        "agent-told",
        json!({
            "target": "person", "pane": "", "chars": text.len(), "id": id, "from": from,
            "re": re, "item": q.id, "text": text, "at": util::epoch_secs(), "turn": false,
        }),
    );
    match args.json() {
        true => println!("{}", json!({ "target": "person", "id": id, "item": q.id, "from": from, "re": re, "told": true })),
        false => {
            let p = Paint::new();
            println!("{}", p.dim(&format!("→ the person · item {} · `wsp yours` lists it", q.id)));
        }
    }
    0
}

/// The one seat an address names, as `target` answers: the pane and the post.
///
/// `list:` names every seat on a list, and delivering to more than one is not
/// built yet; a list with one seat on it is that seat, and one with more is
/// refused with the seats named, so the person can tell each.
fn addressed_seat(store: &Store, addr: &crate::address::Address) -> Result<(String, String), String> {
    let mut found = crate::address::resolve(addr, &crate::address::Book::read(store))?;
    if found.len() > 1 {
        let seats: Vec<String> = found.iter().map(|n| format!("{} ({})", n.pane, n.what)).collect();
        return Err(format!("names {} seats and a tell goes to one — tell each: {}", found.len(), seats.join(", ")));
    }
    let n = found.remove(0);
    Ok((n.pane, n.what))
}

/// The addressee of a direct tell as an id — the task's id, or the seat or pane
/// as the person named it — and not the display string [`target`] makes of it
/// (`wsp-491 — <title>`), so the line the tell leaves in cycle.log names the
/// same thing the wake path's does.
fn addressee_id(store: &Store, needle: &str, what: &str) -> String {
    if what.starts_with("pane ") {
        return needle.to_string();
    }
    if let Ok(Some(a)) = crate::pieces::addressed(store, needle) {
        return a.id();
    }
    store.find_task(needle).map(|t| t.id).unwrap_or_else(|| needle.to_string())
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
    /// The sentence itself, for the event `wsp tell --log --full` reads.
    pub text: String,
    /// This exact sentence to this exact pane, as a short handle.
    ///
    /// **Derived from the message and not minted**, which is the whole of its
    /// value: a resend of the same paragraph to the same seat carries the same
    /// id, so a retry is recognisable as a repeat by anything that has seen the
    /// first — [`Sent::already_sent`] below, and a person reading two lines of
    /// output. An id from a counter would be unique per attempt and would
    /// answer a question nobody asked.
    pub id: String,
    /// Who spoke, as an address, and which message this answers (`wsp-521`).
    /// Empty and `None` for the paths that do not say, which write the event as
    /// they always did.
    pub from: String,
    pub re: Option<String>,
    pub reply: bool,
    /// Set when this is one seat of a `list:` fan-out (`wsp-523`): the fan-out's
    /// key and how many seats it went to, which the event carries.
    pub fan: Option<(String, usize)>,
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
            text: text.to_string(),
            id: message_id(pane, text),
            from: String::new(),
            re: None,
            reply: false,
            fan: None,
            json: args.json(),
        }
    }

    pub fn in_fan(mut self, fan: Option<(&str, usize)>) -> Sent {
        self.fan = fan.map(|(k, n)| (k.to_string(), n));
        self
    }

    /// Who said it, what it answers, and whether it carried a reply footer.
    pub fn by(mut self, from: &str, re: Option<&str>, reply: bool) -> Sent {
        self.from = from.into();
        self.re = re.map(str::to_string);
        self.reply = reply;
        self
    }

    /// This same sentence, to this same pane, within the last [`SAME_BREATH`].
    ///
    /// The idempotence half of worklist-010, and it is the half that addresses
    /// the *harm* rather than the report. A verb that says "was not told" about
    /// a message it delivered turns one paragraph into three, and the fix for
    /// the report — a backend `tell` that no longer lies — cannot undo a
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
/// path printed `the wsp seat was not told` on a backend wait timing out, and the
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
    let Sent { target, what, pane, peek, chars, text, id, from, re, reply, fan, json } = sent;
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
                    "from": from, "re": re, "reply": reply, "text": text,
                    "fan": fan.as_ref().map(|f| f.0.as_str()), "fan_of": fan.as_ref().map(|f| f.1),
                    "at": util::epoch_secs(),
                    "turn": matches!(how, Delivery::Started | Delivery::Acked),
                }),
            );
            // The seat's supervisor acknowledged it (wsp-477): the same line the
            // wake path writes for a governor scope, so a direct tell to a
            // compound seat leaves its acknowledgement in cycle.log too.
            if matches!(how, Delivery::Acked) {
                crate::store::log_line(store, &format!("told {target}: delivered, acknowledged by the seat"));
            }
            if *json {
                println!(
                    "{}",
                    json!({ "target": target, "pane": pane, "id": id, "told": true, "from": from, "re": re,
                            "turn": matches!(how, Delivery::Started | Delivery::Acked) })
                );
            } else {
                // The tail is on the unconfirmed one only, and it is not a
                // hedge: a sentence queued behind a turn has not been read yet,
                // which is a different thing to know at a barrier than a turn
                // that started on it.
                let tail = match how {
                    Delivery::Started => String::new(),
                    Delivery::Acked => format!(" · {}", p.dim("delivered")),
                    Delivery::Unconfirmed => format!(" · {}", p.dim("delivered, no turn seen")),
                };
                println!("{}{}", p.dim(&format!("→ {what} · {pane}")), tail);
            }
            0
        }
        // `NotTaken` is delivered-and-unmoved, which for this verb is the
        // interesting answer rather than an error. Where the text now sits is
        // said as the unknown it is: the keystrokes were written at the pane,
        // and whether the TUI is still holding them in the composer or dropped
        // them is inside the application, which wsp cannot read. The first
        // wording asserted "sitting in the composer" flat out; the spawn
        // failures of 2026-08-25 showed composers empty after deliveries that
        // had been reported the same way, so the assertion was a guess that
        // had already been wrong. The rescue differs by half, and peek tells
        // the halves apart — which is why it is named before the keystroke:
        // resending blind would duplicate a composer still holding the text.
        Err(Refusal::NotTaken) => {
            eprintln!("wsp: {what} took the text and started nothing — it may be sitting in the composer or may have been dropped");
            eprintln!("     `wsp peek {peek}` shows which; if it is sitting there, a return in the pane sends it");
            1
        }
        // Read off the screen, so said flat: it is there (`wsp-252`). The
        // return is the repair; a second `wsp tell` also works, and leaves
        // the sentence in the prompt twice.
        Err(Refusal::Unsubmitted(_)) => {
            eprintln!("wsp: {what} has the text typed in its prompt and not submitted — wsp pressed submit again and it stayed");
            eprintln!("     a return in {pane} sends it; `wsp peek {peek}` shows it waiting");
            1
        }
        Err(e) => {
            // The supervisor's own answer after its retries: the wake path's
            // line for it, so a failed direct tell is not only on stderr.
            if let Refusal::Failed(why) = &e {
                crate::store::log_line(store, &format!("told {target}: delivery failed — {why}"));
            }
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
/// Which of `backends` answers for `seat`, and the row it answered with —
/// `wsp tell`'s own join, pulled out so it can be proven without a real
/// `Place::tell` to receive anything (`compound-077`). The same "ask every
/// backend, take whichever one's census names this seat" `Wip::live` already
/// does, one seat at a time instead of the whole list.
pub(crate) fn locate_seat<'a>(
    backends: &'a [Box<dyn Place>; 1],
    seat: &str,
) -> Option<(&'a Box<dyn Place>, crate::place::Seated)> {
    backends.iter().find_map(|b| {
        b.census().ok().and_then(|c| c.seats().find(|s| s.seat.as_str() == seat).cloned()).map(|row| (b, row))
    })
}

/// Whether compound minted `needle` and still holds it — the seat id read by
/// who made it rather than by how one is spelled (`wsp-156`).
///
/// `target` and `peek_target` both used to take "has a colon" to mean "is a
/// seat", which was one host's spelling and nobody else's: a compound seat is
/// `cpd-N`, so `wsp peek cpd-347` and `wsp tell cpd-347` fell through to the
/// task search and answered "nothing holds" for a seat that was live and
/// claimed. A backend's own record is the only thing that can say what it
/// minted, and asking is one stat. Passed in rather than called, like the task
/// lookup, so the resolvers stay provable without a compound directory.
fn compound_holds(needle: &str) -> bool {
    crate::place_compound::Compound::new().holds(&crate::place::Seat::new(needle))
}

fn target(store: &Store, needle: &str, seated: &dyn Fn(&str) -> bool) -> Option<(String, String)> {
    if needle.contains(':') || seated(needle) {
        return Some((needle.to_string(), format!("pane {needle}")));
    }
    // A piece id reaches the piece's seat, from its entry on the row.
    if let Ok(Some(a)) = crate::pieces::addressed(store, needle) {
        let piece = a.piece.as_ref()?;
        return Some((piece.pane.clone()?, format!("{} — {}", a.id(), util::truncate(&piece.title, 44))));
    }
    let t = store.find_task(needle)?;
    // The seat holding the row's claim, and named as that: a row started twice
    // in one tree has two, and a tell to the other reaches nobody who owns it
    // (`wsp-603`).
    let panes = store.panes_for_task(&t.id);
    let held = store.claims().get(&t.id).and_then(|c| crate::address::claim_holder(c, &panes, &store.agents_held()));
    let title = util::truncate(&t.title, 44);
    match held {
        Some(pane) => Some((pane.clone(), format!("{} — {title} · {pane} holds the claim", t.id))),
        None => Some((panes.into_iter().next()?, format!("{} — {title}", t.id))),
    }
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
    from_source(args, &args.text(skip))
}

/// The paragraph, out of the three places a paragraph comes from: typed on the
/// line, a stream, or a file named by `--from`.
///
/// **The file form is `worklist-036` beyond the row it was found on.** `flag`
/// lost the message because it had no `--from` and the parser bound the path to
/// an option nothing read — and `flag` was not alone in the first half of that:
/// `tell`, `ask`, `answer` and `govern --tell` all took `-` and none of them
/// took a file, while the house rule printed in every brief is *give a wsp verb
/// its prose through a file*, because a shell evaluates backticks inside double
/// quotes. Four verbs told agents to use a spelling four verbs did not have.
/// The dropped-word guard in `main` now says so out loud instead of losing it
/// (see [`crate::Args::dropped`]); this is the other half, which is that the
/// spelling should simply work, and it works the same way on all of them
/// because they are one act and share this function.
///
/// Two sources is refused rather than resolved: `--from` beside a typed
/// sentence is a caller who does not know which one is being sent, and picking
/// either is the same silent loss in a smaller hat.
pub(crate) fn from_source(args: &Args, typed: &str) -> Result<String, i32> {
    let Some(path) = args.get("from") else {
        return match typed.trim() == "-" {
            true => piped_message(),
            false => typed_message(typed),
        };
    };
    if !typed.trim().is_empty() && typed.trim() != "-" {
        eprintln!("wsp: a sentence and --from are two messages — send one");
        return Err(2);
    }
    // A lone `--from`, or `--from -`, is the stream: a dash is not a value, it
    // is the conventional name for stdin, and `prose_source` reads it the same
    // way for `note` and its neighbours.
    if matches!(path.as_str(), "true" | "-") {
        return piped_message();
    }
    match crate::util::read_source(&path) {
        Ok(text) if text.trim().is_empty() => {
            eprintln!("wsp: {path} is empty — nothing sent");
            Err(2)
        }
        Ok(text) => Ok(text.trim().to_string()),
        Err(e) => {
            eprintln!("wsp: cannot read {path}: {e}");
            Err(1)
        }
    }
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
    match crate::util::read_source("-") {
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
///
/// **The record is the act; the pane label is one drawing of it** (wsp-115).
/// The sentence goes into `said.json` first and unconditionally, keyed on the
/// seat [`my_pane`] resolved, and a surface with no label of ours to wear —
/// `wsp wip`, a compound pane header — reads it back through [`sentence`].
/// A host's rename is decoration applied afterwards, and only to a seat that
/// host issued. This verb used to open on a host check and return 1, which
/// on a machine whose seats are all compound's was every call from every
/// agent, and the one verb the brief tells them to use as they go.
pub fn say(store: &Store, args: &Args) -> i32 {
    let Some(pane) = pane_id(args) else {
        eprintln!("wsp: no seat to name — run this inside a seat wsp spawned, or pass --pane");
        return 2;
    };

    // A stream form, for the reason `note` has one: the handbook tells every
    // agent to give a wsp verb its prose through `-` or `--from`, and `say`
    // was the exception that took the `-` as the status line itself and said
    // nothing. Short by nature is still true — this is a sidebar label — but
    // "short" is a fact about the text, not about where it came from, and the
    // thing an agent pipes in is usually short and always shell-hostile.
    let streamed = cmd_task::payload_source(args.rest.get(0..).unwrap_or_default());
    let said = match &streamed {
        Some(src) => match cmd_task::prose_from_source(src) {
            Ok((text, _)) => text,
            Err(code) => return code,
        },
        None => args.text(0).trim().to_string(),
    };
    let said = said.trim();
    // Kept for both forms, and here it is a rendering question rather than a
    // record one — an escape sequence reaching a pane label is drawn, not
    // stored.
    if let Some(why) = util::terminal_output(said) {
        eprintln!("wsp: {why}");
        return 2;
    }

    // Home is the task this pane holds. Without one there is nothing to fall
    // back to, so the label is cleared outright and the host goes back to
    // naming the pane whatever it named it before.
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
    //
    // From a piece's pane the scope is the piece's id, `wsp-501-2`
    // (`docs/pieces.md`, *Seen and told*): the pane is bound to the row, and the
    // binding's `piece` says which of the row's seats it is.
    let piece_id = held.as_ref().and_then(|t| {
        let n = store.bindings().get(&pane)?.get("piece")?.as_u64()?;
        Some(format!("{}-{n}", t.id))
    });
    let scoped = |t: &Task, said: &str| match &piece_id {
        Some(id) => Some(format!("{id} · {}", said.trim())).filter(|_| !said.trim().is_empty()),
        None => said_full(t, said),
    };
    let full = match (said.is_empty() || args.has("clear"), &held) {
        (true, Some(t)) => scoped(t, &t.title),
        (true, None) => None,
        (false, Some(t)) => scoped(t, said),
        (false, None) => plain_full(said),
    };
    let label = full.as_deref().map(on_the_wire);

    // The record first, and whatever happens to the rename after. `said` is
    // the sentence the agent typed and `task` the work it was said about, so a
    // reader can tell when it has gone stale without anybody clearing it. The
    // `label`/`full` half is written unconditionally too: it is only read
    // while the pane is wearing `label`, so a rename that failed, or a seat
    // no host drew, leaves a half that matches nothing rather than one
    // that lies. `--clear`, and a sentence [`plain_full`] withholds, take the
    // whole entry away — the agent withdrew what it said.
    let spoken = !(said.is_empty() || args.has("clear")) && full.is_some();
    match (&label, &full) {
        (Some(l), Some(f)) if spoken => {
            store.set_sentence(&pane, said, held.as_ref().map(|t| t.id.as_str()));
            store.set_said(&pane, l, f);
        }
        (Some(l), Some(f)) => {
            store.clear_said(&pane);
            store.set_said(&pane, l, f);
        }
        _ => {
            store.clear_said(&pane);
        }
    }

    if args.json() {
        println!("{}", json!({ "pane": pane, "label": label, "said": spoken.then_some(said) }));
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
/// sidebar draws 26 columns, and `looking for work in stra…` still says
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
/// whatever the pane was called last. From outside that is indistinguishable
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
///
/// Said the way [`say`] says things, since wsp-115: into the record first, so
/// a seat no host draws still says it is looking. *An agent* is asked of the
/// backend that seated it — a supervisor sets its seat variable only on a
/// process it spawned as one, so a seat that arrived by that name is driven by
/// construction.
pub fn say_looking(store: &Store, project: Option<&str>, found: bool) {
    let Some(pane) = my_pane() else { return };
    if crate::place::seat_from_env().is_none() {
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
    let label = looking_label(project, found);
    store.set_sentence(&pane, &label, None);
}

/// `Trance Video · 3h12m` — a claim as one line.
///
/// Both this and `worked_line` join what they have and skip what they do not,
/// because every part is optional: a claim made outside a seat has no label, and
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
/// and the record that does is [`crate::agent_kind::Ran`], read off the
/// transcript when the claim ends.
///
/// Empty for a spawn that stated neither, so the ordinary claim line is
/// byte-for-byte what it was — the same compatibility rule `--model` itself
/// keeps, and the reason nothing in the store needs migrating.
pub(crate) fn spawned_at(model: Option<&str>, effort: Option<&str>) -> String {
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
/// It must never cost a release. Nothing here is fatal and nothing waits on a
/// socket. For Claude Code it is a line-at-a-time scan of one file — see
/// `agent_commands::read_ran` for why it is not read whole; for opencode it is
/// a subprocess, ~1.5s, which is the price of a record that runtime publishes
/// and wsp would otherwise have to derive. Paid once, when an attempt ends.
///
/// **And it does not have to be taken before the seat is ended.** `core-039`
/// began as that suspicion — `wsp despawn` stops the agent and only then
/// reaches the claim through [`release_pane`], and the first real opencode row
/// closed with no clause. Both endings were then driven against a live opencode
/// in a sandbox and **both wrote the clause**, while the row that had lost one
/// lost it again on a replay through `wsp release`, with no agent left to kill.
/// The reading is of a record on disk in both kinds, the agent is not asked
/// anything, and the order this is called in is not the hazard. What was
/// actually wrong is one file down, on `agent_commands::captured` — which is
/// where a missing clause should be read about next, before this is suspected
/// again.
///
/// # And what it cost, which is the clause `core-029` added
///
/// Same line, same reading, one more clause: `242M cache 4.7M in 875k out`,
/// and a price where the runtime states one. The argument for the units is on
/// [`crate::agent_kind::Spend`] and the argument for writing it here
/// rather than reading it at report time is the one above — evidence has to be
/// durable and a transcript is not.
///
/// The window is the claim rather than the session, because one session serves
/// several attempts in sequence and the second would otherwise be billed for
/// the first. That is a token count read twice, which is a bill.
fn ran_at(store: &Store, task_id: &str, since: &str) -> String {
    let Some(thread) = crate::cmd_resume::thread_for_task(store, task_id) else {
        return String::new();
    };
    ran_clause(&thread.kind, &thread.session, &thread.cwd, since)
}

/// [`ran_at`] for a leg's seat. A leg holds no claim and two siblings share one
/// task, so the session is the seat's own binding's and not the task's.
pub(crate) fn ran_in_seat(store: &Store, seat: &str, since: &str) -> String {
    ran_of_seat(store, seat, since).map(|r| ran_text(&r)).unwrap_or_default()
}

/// What a seat's session served since `since`, from the seat's own binding. A
/// verifier's seat is read the same way, and its pass keeps the reading.
pub(crate) fn ran_of_seat(store: &Store, seat: &str, since: &str) -> Option<crate::agent_kind::Ran> {
    let (kind, session, cwd) = session_of_seat(store, seat)?;
    crate::agent_commands::of(&kind).ran(&session, &cwd, util::epoch_of(since))
}

/// What the seat's agent did since `since` (`wsp-325`): [`ran_of_seat`]'s
/// reading of the same session, for the lines a verdict carries.
///
/// The seat's `seat.did` digests as folded ([`crate::fold::did_since`]), and no
/// transcript is read (wsp-028).
pub(crate) fn did_of_seat(store: &Store, seat: &str, since: &str) -> Option<crate::did::Did> {
    crate::fold::did_since(store, seat, util::epoch_of(since))
}

/// The stalled shell commands in the end of a seat's transcript (`wsp-397`),
/// open ones counted to `now`: the manager's `seat.stuck` as folded
/// ([`crate::fold::stall`]), its start kept at the `recorded` one when the
/// command is the same (wsp-028). No transcript is read.
pub(crate) fn stuck_of_seat(store: &Store, seat: &str, now: i64, recorded: Option<(&str, i64)>) -> Vec<crate::stuck::Stall> {
    let Some(r) = crate::fold::stall(store, seat, now, recorded) else { return Vec::new() };
    let kind = crate::stuck::classify(&r.command).unwrap_or(crate::stuck::Kind::TimedOut);
    vec![crate::stuck::Stall { kind, command: r.command, at: r.at, secs: r.secs, open: r.open }]
}

/// The kind, session and cwd a seat's binding records, when it has a session.
fn session_of_seat(store: &Store, seat: &str) -> Option<(String, String, String)> {
    let bindings = store.bindings();
    let b = bindings.get(seat)?;
    let get = |k: &str| b.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let session = get("agent_session_id");
    if session.is_empty() {
        return None;
    }
    Some((crate::cmd_spawn::kind_or_default(&get("agent_kind")), session, get("cwd")))
}

fn ran_clause(kind: &str, session: &str, cwd: &str, since: &str) -> String {
    // From the claim, not from the start of the session. One session serves
    // several attempts whenever an agent is handed the next task from the same
    // seat, and without the window the second attempt reports the first one's
    // turns and its bill — see [`crate::agent_commands::Kind::ran`].
    let ran = crate::agent_commands::of(kind).ran(
        session,
        cwd,
        util::epoch_of(since),
    );
    ran.map(|r| ran_text(&r)).unwrap_or_default()
}

/// The clauses a release line and a leg's end line carry: ` · ran opus-5/high ·
/// 96 turns · peak 180k · 242M cache 4.7M in 875k out`. The peak is left out
/// where the runtime read none, so a line written before it reads as it did.
fn ran_text(ran: &crate::agent_kind::Ran) -> String {
    let peak = match ran.peak {
        0 => String::new(),
        n => format!(" · peak {}", util::count_human(n)),
    };
    let spent = match ran.spend.clause() {
        Some(c) => format!(" · {c}"),
        None => String::new(),
    };
    format!(" · ran {} · {} turns{peak}{spent}", ran.label(), ran.turns)
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
    let ran = ran_at(store, task_id, &from);

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
/// after a restart the binding is gone and only the claim says the
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
pub(crate) fn blocked_question(t: &Task) -> Option<String> {
    t.blocked_question()
}

/// Why claiming finished work is refused, in one sentence.
///
/// Said twice over — the pretty block for a person at a shell, and as
/// `"reason"` beside `"error"` under `--json` — and they must not drift. The
/// surfaces that run this command with `--force` in their pocket (`c`, on the
/// tree or the board) quote the reason as the question the force is the answer
/// to, and a banner naming only id and title would make that question
/// unanswerable.
fn done_reason(t: &Task) -> String {
    format!("already {} — claiming it would reopen it", t.status_raw)
}

pub fn claim(store: &Store, args: &Args) -> i32 {
    let Some(needle) = args.rest.first().cloned() else {
        eprintln!("usage: wsp claim <id>   (inside a seat)");
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
        eprintln!("wsp: no seat to bind — run this inside one, or pass --pane");
        return 2;
    };

    // What is in the seat, from wsp's own port — compound. Asked of another
    // host's list, a seat that host does not draw is not on it, so a
    // worklist spawn's claim came back with no room, no session and the tree
    // the detached `worklist advance` happened to be standing in — the
    // previous member's, or the trunk (`wsp-142`).
    //
    // Claiming on behalf of another seat — from the panel, say — means this
    // process's environment describes the caller, not the target, which is
    // why the target is looked up rather than read off `env`.
    let place = crate::cmd_spawn::backend(args);
    let seats: Vec<crate::place::Seated> =
        place.census().map(|c| c.seats().cloned().collect()).unwrap_or_default();
    let target = seats.iter().find(|r| r.seat.as_str() == pane).cloned();
    // `spawn` knows where it put the seat and says so; otherwise the port's
    // room for it, which on compound is the seat itself.
    let workspace = args
        .get("workspace")
        .filter(|w| !w.is_empty())
        .or_else(|| place.room(&crate::place::Seat::new(&pane)))
        .unwrap_or_default();

    // The session in the seat, from the party that knows. This read
    // `CLAUDE_SESSION_ID` out of the caller's own environment until 2026-08-17,
    // and Claude Code does not set that variable — so every binding this store
    // had ever written carried `""`, and nothing noticed because nothing read
    // it. A claim made from the panel would also have recorded the *panel's*
    // session for somebody else's seat.
    //
    // Empty here is the honest answer and the ordinary one: `spawn` claims
    // before it starts the agent, so at this instant the seat is usually still
    // a shell. [`learn_sessions`] is what fills it once there is a session.
    let session = target.as_ref().map(|r| r.session.clone()).unwrap_or_default();
    // The tree a spawn opened, then the port's reading of the seat, and this
    // process's own directory last — right for an agent claiming at its own
    // shell, and the answer that was wrong for every spawn.
    let cwd = match (args.get("cwd").filter(|c| !c.is_empty()), &target) {
        (Some(c), _) => util::contract(&util::expand(&c)),
        (None, Some(r)) if !r.cwd.is_empty() => util::contract(&util::expand(&r.cwd)),
        _ => std::env::current_dir().map(|c| util::contract(&c)).unwrap_or_default(),
    };
    // What `spawn` is about to start in the seat, so the record says it from
    // the first moment rather than whenever a census next reports it.
    let kind = args.get("kind").filter(|k| !k.trim().is_empty());

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
        let why = done_reason(&t);
        if args.json() {
            println!("{}", json!({ "error": "done", "task": t.id, "reason": why }));
            return 1;
        }
        let p = Paint::new();
        eprintln!("{} {}  {}", p.yellow("✗"), p.bold(&t.id), t.title);
        eprintln!("  {}", p.dim(&why));
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
        let why = match blocked_question(&t) {
            Some(q) => format!("blocked, waiting on an answer: {q}"),
            None => "blocked — a decision is owed before it is worked".into(),
        };
        if args.json() {
            println!("{}", json!({ "error": "blocked", "task": t.id, "reason": why }));
            return 1;
        }
        let p = Paint::new();
        eprintln!("{} {}  {}", p.yellow("✗"), p.bold(&t.id), t.title);
        eprintln!("  {}", p.dim(&why));
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
    //
    // Read off the same census as the target. It was one host's pane list,
    // on which no compound seat appears, so under compound this never refused.
    let held_by: Vec<&crate::place::Seated> = seats
        .iter()
        .filter(|r| !r.agent.kind.is_empty() && r.state.is_running())
        .filter(|r| r.seat.as_str() != pane)
        .filter(|r| {
            bindings_now.get(r.seat.as_str()).and_then(|b| b.get("task_id")).and_then(|x| x.as_str())
                == Some(t.id.as_str())
        })
        .collect();
    if !held_by.is_empty() && !args.has("force") {
        let held = store
            .claims()
            .get(&t.id)
            .and_then(|c| c.get("claimed_at"))
            .and_then(|c| c.as_str())
            .map(|c| util::duration_human(util::since(c)))
            .unwrap_or_default();
        if args.json() {
            // The reason in one line, for the same reader as the two above:
            // whoever is holding this task, said where the question can quote
            // it.
            let who: Vec<String> = held_by
                .iter()
                .map(|h| {
                    format!(
                        "{} in {} · {}{}",
                        h.agent.kind,
                        h.seat,
                        h.state.as_str(),
                        if held.is_empty() { String::new() } else { format!(" · {held}") },
                    )
                })
                .collect();
            println!(
                "{}",
                json!({
                    "error": "held",
                    "task": t.id,
                    "reason": format!("held — {}", who.join("; ")),
                    "held_by": held_by.iter().map(|r| json!({
                        "pane": r.seat.as_str(), "agent": r.agent.kind, "state": r.state.as_str(),
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
                        h.agent.kind,
                        h.seat,
                        h.state.as_str(),
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

    // The seat's own label, as the port reports it. A host's workspace was
    // renamed after the task here so a restart could find it by name; a seat
    // wsp opens is named by wsp and found by its id.
    let ws_label = target.as_ref().map(|r| r.label.clone()).unwrap_or_default();

    // One lock around the state files a claim touches, so a claim
    // arriving in the middle of this one cannot read a half-made state: a
    // binding cleared and not yet replaced, or a claim recorded against a task
    // whose binding has not landed. The task file and its git commit are
    // outside it deliberately — a commit can take longer than any other agent
    // should be made to wait, and `wsp reconcile` already rebuilds bindings
    // from claims if the two ever part company.
    // The agent's own id, minted once and kept for as long as the agent is
    // (`compound-092`). REUSED where this seat already holds one, because Ed's
    // decision is that an id survives an agent's sessions — a restart, a
    // `wsp resume`, a move between backends — and a second claim by the same
    // agent is the same agent. A seat that has been handed on gets a new id
    // because `agent_in_seat` answers with the newest row and this writes one.
    let agent_id = store
        .agent_in_seat(&pane)
        .unwrap_or_else(crate::util::new_agent_id);

    store.locked(|| {
        for other in &displaced {
            store.clear_binding(other);
        }
        // The agent, under the id wsp minted, with the backend's own names for
        // it as attributes. Written inside the lock with the claim it belongs
        // to, so no reader can see a claim naming an agent that has no row.
        store.set_agent(
            &agent_id,
            json!({
                "seat": pane,
                "session": session,
                "cwd": cwd,
                "started": util::now_iso(),
            }),
        );
        let mut binding = json!({
            "task_id": t.id,
            "pane_id": pane,
            "workspace_id": workspace,
            "agent_session_id": session,
            "cwd": cwd,
            "started_at": util::now_iso(),
        });
        if let Some(k) = &kind {
            binding["agent_kind"] = json!(k);
        }
        store.set_binding(&pane, binding);

        // The durable half: the binding goes when the seat does, and this is
        // what is left saying which room and which tree the work was in.
        let mut claim = json!({
            // The identity that replaces the three below it
            // (`compound-092`). Written now and read by nothing yet: the
            // store is durable, so the id has to exist on records for a
            // while before anything is allowed to depend on it.
            "agent_id": agent_id,
            "workspace_id": workspace,
            "workspace_label": ws_label,
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
        });
        if let Some(k) = &kind {
            claim["agent_kind"] = json!(k);
        }
        store.set_claim(&t.id, claim);

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
    let mut asked = spawned_at(args.get("model").as_deref(), args.get("effort").as_deref());
    if args.has("packet") {
        asked.push_str(&format!(" · {}", crate::packet::clause(args.get("packet").and_then(|b| b.parse().ok()))));
    }
    match left.first() {
        Some(prev) => t.log(&format!("claimed by pane {pane}, taken up from {prev}{asked}")),
        None => t.log(&format!("claimed by pane {pane}{asked}")),
    }
    // **Which run opened this, when one did.** `wsp-158`: a helper a governor
    // spawned mid-group is a member of nothing and a verifier of nothing, and
    // the only way a pass ends it is to have written down that the run is what
    // put it there. Taken from the environment `cycle::start` sets, so it costs
    // a spawn nothing and is written on the same line as the claim it belongs
    // with — the one place a row's history is assembled.
    if let Some(owner) = run_owner() {
        t.log(&owner);
    }
    let _ = store.save_task(&t);
    store.log_event(
        "task-claimed",
        json!({ "id": t.id, "pane": pane, "from": left, "took_over": displaced }),
    );
    store.git_commit(&format!("wsp: claim {} — {}", t.id, t.title));

    if args.json() {
        println!(
            "{}",
            json!({
                "task": t.json(),
                "pane": pane,
                "from": left,
                "took_over": displaced,
            })
        );
    } else {
        let p = Paint::new();
        println!("{} {}  {}", p.cyan("▸"), p.bold(&t.id), t.title);
        println!("  {}", p.dim(&format!("bound to {pane}")));
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

/// What a backend reading teaches about the agents in seats already bound.
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
///
/// # And the kind, on the same rows and under the same two rules
///
/// `core-031`. `--kind` was read by `spawn` and never written down, so every
/// reader that later asked what is in this seat was answered `claude` whatever
/// had been started — `wsp resume` on a live opencode printed
/// `claude --resume ses_…`, and [`crate::cmd_attempts::live`] would have asked
/// `Claude::ran` to read an opencode's transcript.
///
/// **Observed rather than declared, which is the choice worth arguing.** The
/// obvious fix is to carry `--kind` down into the claim beside `--model`, and
/// it is worse in two ways: it cannot answer for an agent wsp did not start —
/// a person's own claude claiming at its own shell — and it records what wsp
/// *asked for* rather than what is there. The backend reports the kind on
/// every census row, so the fact is free and it is first-hand. That is also what makes
/// [`crate::cmd_spawn::kind_or_default`]'s fallback safe: an absence lasts one
/// tick on any record whose agent is still alive.
///
/// **Where it lands is both, for the session's reason.** A binding dies with
/// the pane and the claim outlives it, and the one moment anybody asks what was
/// running is the end of the attempt — after `release_pane` has dropped the
/// binding. A kind on the binding alone would be gone exactly when `ran_at`
/// reaches for it.
fn sessions_learned<'a>(
    bindings: &BTreeMap<String, Value>,
    seen: impl Iterator<Item = (&'a str, &'a str, &'a str)>,
) -> Vec<(String, String, String)> {
    seen.filter(|(_, session, _)| !session.trim().is_empty())
        .filter_map(|(seat, session, kind)| {
            // An unbound pane is not this function's business — a session in a
            // seat nobody has claimed anything in is `reconcile`'s to notice, if
            // it is anybody's. A binding with no such key at all reads as empty
            // and is therefore learned rather than skipped: `adopt` writes one
            // of those every time, so that is a live shape and not a defence
            // against an imagined store.
            let b = bindings.get(seat)?;
            let had = b.get("agent_session_id").and_then(|v| v.as_str()).unwrap_or("");
            let was = b.get("agent_kind").and_then(|v| v.as_str()).unwrap_or("");
            // Either field disagreeing is worth a write, and the `||` is what
            // carries the records that already existed: their session was
            // learned long ago and will never change again, so a check on the
            // session alone would leave every one of them with no kind for as
            // long as it lives.
            let moved = had != session || (!kind.trim().is_empty() && was != kind);
            moved.then(|| (seat.to_string(), session.to_string(), kind.to_string()))
        })
        .collect()
}

/// Record against each bound seat what the backend says is sitting in it: the
/// session it is running under, and what kind of agent it is.
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
pub fn learn_sessions<'a>(
    store: &Store,
    seen: impl Iterator<Item = (&'a str, &'a str, &'a str)>,
) -> usize {
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
        for (seat, session, kind) in &learned {
            let Some(mut b) = bindings.get(seat).cloned() else { continue };
            let task = b.get("task_id").and_then(|t| t.as_str()).unwrap_or("").to_string();
            let Some(o) = b.as_object_mut() else { continue };
            o.insert("agent_session_id".to_string(), json!(session));
            // Silence is not a correction here either: a pane whose agent has
            // died reports no kind and the recorded one is still the truth
            // about what was in that seat — which is precisely when it is read,
            // because `ran_at` asks at the end of an attempt.
            if !kind.trim().is_empty() {
                o.insert("agent_kind".to_string(), json!(kind));
            }
            store.set_binding(seat, b);
            // And on the claim, which is the copy that is still there when the
            // attempt ends. Only where there is one: a session learned for a
            // seat holding nothing has no task to be recorded against.
            if let Some(mut c) = store.claims().get(&task).cloned() {
                if let Some(o) = c.as_object_mut() {
                    o.insert("agent_session_id".to_string(), json!(session));
                    if !kind.trim().is_empty() {
                        o.insert("agent_kind".to_string(), json!(kind));
                    }
                    store.set_claim(&task, c);
                }
            }
            store.log_event(
                "session-learned",
                json!({ "id": task, "pane": seat, "session": session, "kind": kind }),
            );
        }
    });
    learned.len()
}

/// Rebuild bindings from claims and whatever is open now.
///
/// This is what makes a binding disposable: seat ids need not be stable across
/// a restart, and nothing outside wsp knows which task a seat was on. So: for
/// every claim belonging to this host, find the workspace it names — by id, or
/// failing that by its label and cwd — and bind its most plausible pane.
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
    /// Seats emptied because the agent in them is gone — its workspace closed,
    /// or its pane went while the workspace stood. Separate from `reaped`
    /// because they are not the same loss: a reaped claim frees work for
    /// somebody else to take, and a swept seat means raised hands stop being
    /// addressed to nobody.
    pub stood_down: usize,
    /// Panel records forgotten for the same reason, and separate again: this
    /// one costs nobody any work, it only stops `panels.json` pointing at panes
    /// that belong to whoever took the workspace id next.
    pub forgotten: usize,
}

/// Returns what it put right.
///
/// `reap` additionally ends every claim whose workspace no backend knows any
/// more — the other half of retiring a set of workspaces, because a claim
/// naming one that has been closed goes on saying work is being done in it.
/// Asked for rather than automatic: a claim outliving its *pane* is an accident
/// of process lifetime and must stand, and a daemon starting before a host has
/// finished restoring a session would otherwise read a half-built world as a
/// mass closure.
/// The machine an id sits on, as a map key: `""` for this seat, the `@mb2`
/// suffix for anywhere else.
///
/// Here so the two sides of the guard below are keyed the same
/// way without either of them spelling out the rule.
///
/// The *id* and not the claim's `host` field, deliberately. `host` is
/// `hostname()` as the process that wrote the claim saw it — and an agent on
/// an executor runs a `wsp` shim that executes on the seat, so its claims
/// record the seat's hostname and `host` cannot tell the machines apart. The
/// workspace id can: it comes back from that machine already qualified. The id is the routing key everywhere else in this design; it is
/// the routing key here too.
fn machine_of(_id: &str) -> &str {
    ""
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
/// conservative reading rather than a defect — an empty answer is what a host
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
///
/// **An empty id names no machine, so it is never examined either
/// (`compound-124`).** `machine_of("")` reads as `""` — this same local
/// bucket a host's own workspaces and panes fall in — so once a host had
/// answered with even one of its own, an empty id used to be waved through as
/// belonging to a machine that had spoken. A compound claim's `workspace_id`
/// is empty this way in the ordinary case: `cmd_agent::claim` falls back to
/// `""` for a seat no host workspace backs, which is every compound seat, so
/// this is not a stale or half-written record but compound's normal shape.
/// The shape `compound-123` fixed — one backend's own local answer read
/// as evidence a second backend's seat had gone — is reachable here through
/// this field instead of a pane id, and the fix is the same rule stated
/// where every caller of this function meets it: an id that names no machine
/// cannot be judged by whether one answered. [`Census::nobody_in`]'s
/// `pane_gone` already refuses to compare an empty pane for the identical
/// reason; this generalises it to `room_gone`, reached through the same
/// function, and to the claim reap in [`reconcile`] that reads
/// `workspace_id` directly.
pub(crate) fn may_reap(answered: &std::collections::BTreeMap<&str, usize>, id: &str) -> bool {
    !id.is_empty() && answered.contains_key(machine_of(id))
}

pub fn reconcile(store: &Store, reap: bool) -> Reconciled {
    let _ = (store, reap);
    Reconciled::default()
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
        eprintln!("wsp: no pane — pass --pane or run it from a seat");
        return 2;
    };
    let (removed, task) = release_pane(store, &pane);
    if let Some(id) = task {
        crate::cycle::poke_task(store, &id, "release");
    }
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
    // after a restart, and releasing half of it would leave the task
    // looking held by a pane that no longer has a claim to stand on.
    let panes = store.panes_for_task(&t.id);
    for pane in &panes {
        if let Some(binding) = store.bindings().get(pane) {
            crate::ending::handed_to_legs(store, &t.id, pane, binding);
        }
        store.clear_binding(pane);
    }

    // The claim is read before `hand_off` clears it: `where` is the only part
    // of this the caller cannot already see, and a released claim leaves
    // nothing behind that says where it had been.
    let where_held = store.claims().get(&t.id).map(|c| claim_where(c));
    if where_held.is_some() {
        end_claim(store, &t.id);
    }
    if !panes.is_empty() || where_held.is_some() {
        // The legs a row staged start now, as `leg done` and `review` start
        // what they owe — not on the daemon's next tick, which after a release
        // was 24s on a quiet machine and 291s on a loaded one (`wsp timeline
        // compounding-048`). A row no run and no legs own is not touched.
        crate::cycle::poke_task(store, &t.id, "release");
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
        // A claim with no pane under it is the ordinary aftermath of a
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

/// `Some((row, holder))` when `pane` is bound to a claimed row whose claim is
/// another seat's.
pub(crate) fn claim_elsewhere(store: &Store, row: &str, pane: &str) -> Option<(String, String)> {
    let claim = store.claims().get(row).cloned()?;
    let panes = store.panes_for_task(row);
    if !panes.iter().any(|p| p == pane) {
        return None;
    }
    let holder = crate::address::claim_holder(&claim, &panes, &store.agents_held())?;
    (holder != pane).then(|| (row.to_string(), holder))
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
    // **A seat that does not hold the row's claim never releases it**
    // (`wsp-603`). A member started twice in one tree has two seats bound to
    // one row; ending the one the claim is not on used to end the claim, with
    // the real seat still running. Its own binding goes, and nothing else.
    if had.as_ref().and_then(|b| b.get("task_id")).and_then(|t| t.as_str()).and_then(|t| claim_elsewhere(store, t, pane)).is_some() {
        let removed = store.clear_binding(pane);
        return (removed, None);
    }
    if let Some(binding) = &had {
        if let Some(task_id) = binding.get("task_id").and_then(|t| t.as_str()) {
            crate::ending::handed_to_legs(store, task_id, pane, binding);
        }
    }
    let removed = store.clear_binding(pane);
    if removed {
        if let Some(task_id) = had.as_ref().and_then(|b| b.get("task_id")).and_then(|t| t.as_str()) {
            released = Some(task_id.to_string());
            end_claim(store, task_id);
        }
    }
    (removed, released)
}

/// The seat a pin set on `ws` should attach to: this process's own, and only
/// when `ws` is the room it is standing in.
///
/// A pin is `wsp mandate` with a different word on it (`compound-106`) —
/// *this workspace is working in project X* rather than *this workspace is
/// for project X* — so setting one follows the same rule `mandate` sets for
/// itself: a `-w` naming some other room says nothing about who, if anyone,
/// is sitting there, so it gets no seat to attach an agent's identity to and
/// cannot set a pin, only read or clear one by its legacy workspace key.
fn pin_seat(_ws: &str) -> Option<String> {
    None
}

pub fn pin(store: &Store, args: &Args) -> i32 {
    let Some(ws) = args.get("workspace") else {
        eprintln!("wsp: no workspace — pass -w, or run it from a seat");
        return 2;
    };
    let Some(seat) = pin_seat(&ws) else {
        eprintln!("wsp: pin can only be set from the room it names — run it there, or omit -w");
        return 2;
    };
    let agent_id = store.agent_for_seat(&seat);

    // `--top` marks a workspace as belonging to no project on purpose: the
    // home for whatever runs the whole space, and for terminals that are not
    // work. Without it, "no project" only ever means "nothing resolved", and
    // the two are not the same thing.
    if args.has("top") {
        store.set_pin(&agent_id, crate::resolve::TOP_LEVEL);
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
        eprintln!("{}", crate::util::no_such("project", &needle));
        return 1;
    };

    store.set_pin(&agent_id, &proj.id);

    if args.json() {
        println!("{}", json!({ "workspace": ws, "project": proj.id }));
    } else {
        println!("workspace {ws} pinned to {}", proj.id);
    }
    0
}

pub fn unpin(store: &Store, args: &Args) -> i32 {
    let Some(ws) = args.get("workspace") else {
        eprintln!("wsp: no workspace — pass -w, or run it from a seat");
        return 2;
    };
    // A clear works from outside the room, unlike a set: it only has to name
    // whichever key holds the pin, and `clear_pin` already checks both.
    let agent = pin_seat(&ws).and_then(|seat| store.agent_in_seat(&seat));
    let removed = store.clear_pin(agent.as_deref(), &ws);
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
/// pane with the right environment, a store with the right pins, and a host
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
    pub workspaces: Vec<Workspace>,
    /// The seat this process is standing in, and the workspace a host's
    /// environment names — the second of which only a multiplexer has.
    pub pane: Option<String>,
    pub workspace: Option<String>,
    /// The agent sitting in `pane`, if one has ever claimed there — tried
    /// ahead of `workspace` everywhere a claim or a pin is matched
    /// (`compound-106`), the same precedence `Here` gives it.
    pub agent: Option<String>,
    /// The process's own directory rather than the pane's: a host reports the
    /// shell's cwd, which is stale the moment anyone `cd`s.
    pub cwd: Option<String>,
}

impl Whereabouts {
    pub(crate) fn live(store: &Store) -> Whereabouts {
        let pane = my_pane();
        Whereabouts {
            index: Index::new(store.projects()),
            pins: store.pins(),
            bindings: store.bindings(),
            claims: store.claims(),
            tasks: store.tasks(),
            workspaces: Vec::new(),
            agent: pane.as_deref().and_then(|p| store.agent_in_seat(p)),
            pane,
            workspace: None,
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
                w.agent.as_deref(),
                w.workspace.as_deref(),
                label.as_deref(),
            ),
        },
        w.agent.as_deref(),
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

/// `wsp agent kinds [--json]` — the agent kinds wsp has behaviour for, read out
/// of [`crate::agent_commands::kinds`] so a chrome offering "New agent ▸" holds
/// no list of its own. Needs no store.
///
/// Any other name `--kind` is given is still passed to the backend, which
/// judges it; the text says so and the JSON carries it as `passthrough`.
pub fn agent(_store: &Store, args: &Args) -> i32 {
    match args.rest.first().map(String::as_str) {
        Some("kinds") => kinds(args),
        // The pi extension this binary carries, printed as is. `wsp install`
        // asks the binary it has just installed for it, rather than writing
        // its own copy, because the process doing the installing is the old one.
        Some("pi-extension") => {
            print!("{}", crate::place_super::PI_EXTENSION);
            0
        }
        other => {
            match other {
                Some(w) => eprintln!("wsp agent: unknown subcommand `{w}`"),
                None => eprintln!("wsp agent: usage: wsp agent kinds [--json] | wsp agent pi-extension"),
            }
            2
        }
    }
}

fn kinds(args: &Args) -> i32 {
    let all = crate::agent_commands::kinds();
    if args.json() {
        let rows: Vec<Value> = all
            .iter()
            .map(|k| {
                json!({
                    "name": k.name, "default": k.default, "resume": k.resume,
                    "clear": k.clear, "models": k.models, "efforts": k.efforts,
                })
            })
            .collect();
        let default = all.iter().find(|k| k.default).map(|k| k.name);
        let refused: Vec<Value> = crate::agent_commands::refusals()
            .iter()
            .map(|(p, why)| json!({ "file": p.display().to_string(), "why": why }))
            .collect();
        println!("{}", json!({ "kinds": rows, "default": default, "passthrough": true, "refused": refused }));
        return 0;
    }
    let w = all.iter().map(|k| k.name.len()).max().unwrap_or(0);
    for k in &all {
        let mut facts = Vec::new();
        if k.default {
            facts.push("default".to_string());
        }
        if k.resume {
            facts.push("resumable".into());
        }
        if !k.models.is_empty() {
            facts.push(format!("models {}", k.models.join("|")));
        }
        if !k.efforts.is_empty() {
            facts.push(format!("efforts {}", k.efforts.join("|")));
        }
        println!("{:w$}  {}", k.name, facts.join(" · "));
    }
    for (p, why) in crate::agent_commands::refusals() {
        println!("refused  {}: {why}", p.display());
    }
    println!("any other kind is passed through by --kind, for the backend to judge");
    0
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

/// Rows of a running list at `doing` with no seat and no log line for five
/// minutes (wsp-427), one line each.
fn silent_lines(store: &Store) -> Vec<String> {
    let mut out = Vec::new();
    for w in store.worklists() {
        if w.status() != crate::model::WorklistStatus::Running {
            continue;
        }
        let ids: Vec<String> = w.groups().iter().flat_map(|g| g.members.iter().cloned()).collect();
        for (id, quiet) in crate::cycle::silent(store, &ids) {
            out.push(format!("{id}: no seat and no log line for {} — {}'s start may not have arrived", util::duration_human(quiet), w.id));
        }
    }
    out
}

pub fn wip(store: &Store, args: &Args) -> i32 {
    // The files unless `WSP_READ=api`; a silent or failed dial reads the files
    // and says why, once (wsp-076).
    let api = match wip_reads_api() {
        false => None,
        true => match api_state(store) {
            Ok(state) => Some(state),
            Err(why) => {
                eprintln!("wsp: wip reads the files: {why}");
                None
            }
        },
    };
    let w = match &api {
        Some(state) => Wip::from_api(store, state),
        None => Wip::from_managers(store),
    };
    let cap_waits = crate::room::waiting_lines(store, &w.agents);
    // wsp-384: the planned lists wsp started on its own, for a day after.
    let mut picked_up = crate::pickup::wip_lines(store);
    picked_up.extend(silent_lines(store));
    match args.json() {
        true => {
            let mut v = wip_json(&w);
            if let (Some(o), false) = (v.as_object_mut(), cap_waits.is_empty()) {
                o.insert("waiting_at_cap".into(), serde_json::json!(cap_waits));
            }
            if let (Some(o), false) = (v.as_object_mut(), picked_up.is_empty()) {
                let started: Vec<serde_json::Value> = crate::pickup::started_lately(store)
                    .into_iter()
                    .map(|(at, id, why)| serde_json::json!({ "at": at, "worklist": id, "why": why }))
                    .collect();
                o.insert("planned_started".into(), serde_json::json!(started));
            }
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default())
        }
        false => {
            for l in wip_lines(&w, &Paint::new(), args.terse()) {
                println!("{l}");
            }
            if !cap_waits.is_empty() {
                println!();
                for l in cap_waits {
                    println!("{l}");
                }
            }
            if !picked_up.is_empty() {
                println!();
                for l in picked_up {
                    println!("{l}");
                }
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
/// read the store and asked a backend mid-print, seeing that state took two live
/// agents in the states you wanted and a task in the right status underneath
/// them. There was nowhere to put one.
pub(crate) struct Wip {
    /// Shared with the store's own reading (`wsp-320`): the daemon builds one
    /// of these on every attention pass, and a copy of every task each time
    /// was most of what one cost.
    pub tasks: std::rc::Rc<Vec<Task>>,
    pub index: Index,
    pub bindings: std::collections::BTreeMap<String, serde_json::Value>,
    pub claims: std::collections::BTreeMap<String, serde_json::Value>,
    pub pins: std::collections::BTreeMap<String, String>,
    /// project id -> seat. Read once here rather than per row: the map is small
    /// and every row asks it the same question.
    pub governors: std::collections::BTreeMap<String, serde_json::Value>,
    /// Every seat holding an agent, through the place-work port rather than
    /// any one backend's shape — a shell with nobody in it is not here, because
    /// `wip` is about who is working. `wsp-100` moved `stamp`'s census onto this port and left this
    /// one on `Pane`; this asks every backend wsp can spawn onto
    /// (`robustness-017`) rather than one alone, which is the whole reason
    /// a seat opened with `--compound` now shows up here too.
    pub agents: Vec<crate::place::Seated>,
    /// agent id -> agent record, read once so each row's claim and pin lookup
    /// joins against it rather than asking the store again (`compound-106`,
    /// the same reason `governors` above is read once for every row).
    pub agents_held: std::collections::BTreeMap<String, serde_json::Value>,
    /// `said.json`, for the sentence each agent last said about itself — see
    /// [`sentence`]. A seat no host draws has no label to read it off,
    /// so this is where a compound seat's status line is seen at all.
    pub said: std::collections::BTreeMap<String, serde_json::Value>,
    /// The machine's daemon, when it has none — [`crate::daemon::loud`]'s one
    /// sentence, read once here because it costs a `ps` and `wip` is asked by a
    /// person looking for work, not by a loop.
    ///
    /// `wsp-145`. This heading is the first line of the answer to "is anything
    /// happening on this machine", and a machine full of agents whose
    /// unattended pass is not running is the machine where the *wake* spools
    /// are all holding — six days of it, read as busy the whole time.
    pub daemon: Option<String>,
    /// A volume holding project roots that is nearly full, and what to remove —
    /// [`crate::disk::loud`]. Absent while there is room.
    pub disk: Option<String>,
    /// The load warning, when the machine has been over 1.5x its cores for a
    /// while: [`crate::throttle::loud`], whatever the throttle's mode.
    pub load: Option<String>,
    /// [`crate::throttle::status_line`]: the build throttle's mode and whether it
    /// holds now, every time and in every mode.
    pub throttle: Option<String>,
    /// What the idle panes' processes hold in resident memory, and the
    /// threshold it warns past (`wsp-338`).
    pub idle_memory: crate::memory::IdleMemory,
    /// `worked.json`: who last held each row, for [`closed_row_of`] (`wsp-582`).
    pub worked: BTreeMap<String, Value>,
    /// The open questions, read once — the store half of [`crate::waiting`]'s
    /// reading, asked for every row.
    pub asks: crate::waiting::Asks,
    /// The idle members' own shells, read once — the seat's half
    /// (`wsp-214`), from [`idle_members_shells`].
    pub shells: crate::waiting::Shells,
    /// `mandates.json`, read once — the standing direction each row publishes
    /// as `direction`.
    pub mandates: std::collections::BTreeMap<String, serde_json::Value>,
    /// seat -> the list and group a barrier check standing there is checking,
    /// and the project its group's work is in (`wsp-230`): it holds no claim
    /// and no binding, so this is the only reading that names it.
    pub checking: std::collections::BTreeMap<String, (String, usize, Option<String>)>,
    /// seat -> what the check standing there records it ran on, for its row's
    /// kind, model and effort. Beside `checking` rather than in its tuple,
    /// because that tuple is what names the seat and this is what it runs.
    pub check_on: std::collections::BTreeMap<String, String>,
    /// Every worklist, read once: whether a governed scope is a list or a
    /// project, and which running list's group a member is in, for each row's
    /// `role` (`wsp-246`).
    pub lists: Vec<crate::model::Worklist>,
    /// seat -> since when it has been in its state, as [`seat_clocks`] noted
    /// it on this reading. Published as `since` (`wsp-250`).
    pub since: std::collections::BTreeMap<String, String>,
    /// `spawned.json`: seat -> the seat that spawned it and the scope that
    /// seat governed (`wsp-249`). Published as `spawned_by`, and what makes a
    /// governor's spawn a `helper`.
    pub spawned: std::collections::BTreeMap<String, serde_json::Value>,
    /// Seats whose prompt has held typed text, never submitted, for more than a
    /// tick (`wsp-386`). Drawn as `typed, not sent` in place of `idle`. On the
    /// files path nothing fills it since wsp-030 deleted `typed-unsent.json`;
    /// under `orders::mode() == store` it is [`DeliveryColumns::unsent`].
    pub unsent: std::collections::BTreeSet<String>,
    /// Tells queued for a seat's next idle, or held for its link (`wsp-659`):
    /// seat -> how many. Drawn as `· N tells queued`; published as `queued`.
    pub queued: std::collections::BTreeMap<String, usize>,
    /// Seats whose supervisor has answered `failed` to a tell, tick after tick
    /// (`wsp-634`): pane → (ticks, the last reason). Drawn as `refusing delivery`,
    /// which is what is true when `typed, not sent` is not — nothing was typed.
    /// Under `store`, [`DeliveryColumns::refusing`]: by seat, the attempts and the
    /// failed order's `why`.
    pub refusing: std::collections::BTreeMap<String, (u64, String)>,
    /// Seats whose first work order was sent again and still started no turn
    /// (`wsp-420`). Drawn as `order not received` in place of `idle`. On the
    /// files path nothing fills it since wsp-030 deleted `first-orders.json`;
    /// under `store` it is [`DeliveryColumns::dropped`].
    pub dropped: std::collections::BTreeSet<String>,
    /// Seats that started a full-suite `cargo test` (`wsp-389`):
    /// [`crate::throttle::full_suite_seats`]. By seat, so a row reopened into a
    /// new seat is not flagged for the last one's run. Drawn as `full suite in a member
    /// seat`; published as `full_suite`.
    pub full_suite: std::collections::BTreeSet<String>,
    /// Seats in a shell command that waits on itself or on a bare `cat`
    /// (`wsp-397`), with the seconds it has held: [`crate::repair::stuck_seats`],
    /// and under `store` the folded `seat.stuck` alone ([`stuck_folded`]).
    /// Drawn as a flag, published as `stuck`.
    pub stuck: std::collections::BTreeMap<String, (crate::stuck::Kind, i64)>,
    /// The far machines gone silent, and the seats on them (`docs/remote-agents.md`
    /// section 3): the heading line, drawn as the dead daemon is, and the word
    /// (`unreachable 4m`) each of their seats wears in place of `gone`.
    pub silent: crate::far::Silence,
}

/// What a reading asks of the host: the idle members' shells, their memory,
/// the daemon and the disks. In process for [`Wip::live`]; from the managers'
/// `seats` replies for [`Wip::from_managers`].
struct Probed {
    shells: crate::waiting::Shells,
    memory: crate::memory::IdleMemory,
    daemon: Option<String>,
    disk: Option<String>,
    /// The load warning and the throttle's line: both read the machine
    /// (`sysctl`, `ioreg`) on the files path, and neither under the API.
    load: Option<String>,
    throttle: Option<String>,
}

/// The domain half of a reading, joined by [`Wip::gathered`]: from the files
/// for [`Wip::live`] and [`Wip::from_managers`] ([`Gathering::files`]), from one
/// `state` reply for [`Wip::from_api`] (wsp-076).
struct Gathering {
    tasks: Vec<Task>,
    bindings: BTreeMap<String, Value>,
    claims: BTreeMap<String, Value>,
    spawned: BTreeMap<String, Value>,
    lists: Vec<crate::model::Worklist>,
    /// Every message, for the open questions ([`crate::waiting::Asks::of_messages`]),
    /// and whether a waiter's pid still runs.
    messages: BTreeMap<String, crate::message::Message>,
    alive: Box<dyn Fn(u32) -> bool>,
    /// The census, each gone seat with its quiet time where one is known.
    agents: Vec<(crate::place::Seated, Option<i64>)>,
    since: BTreeMap<String, String>,
    /// Delivery's seat columns from the orders machine, under
    /// `orders::mode() == store` (wsp-127); `None` on the files path.
    delivery: Option<DeliveryColumns>,
}

impl Gathering {
    /// The files' half, read as `wip` always has.
    fn files(store: &Store, agents: Vec<(crate::place::Seated, Option<i64>)>, since: BTreeMap<String, String>) -> Gathering {
        Gathering {
            delivery: OrderReads::of(store).map(|r| DeliveryColumns::read(&r, None, now_ms())),
            tasks: wip_tasks(store),
            bindings: store.bindings(),
            claims: store.claims(),
            spawned: store.spawned(),
            lists: store.worklists(),
            messages: store.messages(),
            alive: Box::new(|pid| crate::host::Host::is_alive(&crate::host::LOCAL, pid)),
            agents,
            since,
        }
    }
}

/// How long an order may sit `typed` before `wip` draws its seat `typed, not
/// sent`: one of the daemon's 20 s ticks (`daemon::TICK`), the tick the files
/// path's `typed-unsent.json` waited out (wsp-386).
const ORDER_TICK_MS: u64 = 20_000;

/// The clock the orders machine dates its events by: epoch milliseconds.
fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Where `wip` reads the orders machine from, under `orders::mode() == store`
/// (wsp-127): the store handle ([`crate::orders`], the daemon's open store in
/// its own process and the API anywhere else), or a test's store.
enum OrderReads<'a> {
    Handle(&'a Store),
    #[cfg(test)]
    Db(wsp_server::SharedStore),
}

impl OrderReads<'_> {
    /// The reads under `store`; `None` under `files`. A test's store
    /// (`refusals::tests::open`) stands for `store`, as it does for the door.
    fn of(store: &Store) -> Option<OrderReads<'_>> {
        #[cfg(test)]
        if let Some(db) = crate::refusals::TEST_DOOR.with(|d| d.borrow().clone()) {
            return Some(OrderReads::Db(db));
        }
        (crate::orders::mode() == crate::orders::Mode::Store).then_some(OrderReads::Handle(store))
    }

    fn all(&self) -> Result<Vec<wsp_store_contract::Order>, crate::orders::Error> {
        match self {
            OrderReads::Handle(store) => crate::orders::all(store, API_BUDGET),
            #[cfg(test)]
            OrderReads::Db(db) => crate::orders::all_in(db),
        }
    }

    fn typed_since(&self, since: wsp_store_contract::At) -> Result<BTreeSet<wsp_store_contract::OrderId>, crate::orders::Error> {
        match self {
            OrderReads::Handle(store) => crate::orders::typed_since(store, since, API_BUDGET),
            #[cfg(test)]
            OrderReads::Db(db) => crate::orders::typed_since_in(db, since),
        }
    }
}

/// `wip`'s delivery columns as the orders machine says them (wsp-127): what
/// `typed-unsent.json`, `first-orders.json` and `refused-deliveries.json` said
/// on the files path, by seat.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DeliveryColumns {
    /// A seat with an order `typed` for more than a tick ([`ORDER_TICK_MS`]),
    /// or `queued` again after a refusal that counted (`attempts > 0`).
    pub unsent: BTreeSet<String>,
    /// A seat whose last order to settle `failed` (the delivery worker's
    /// attempts ran out at the cap), as `refusals::Door::deaf` reads it: the
    /// attempts, and the last refusal's `why`. A failed first order is
    /// `dropped` instead.
    pub refusing: BTreeMap<String, (u64, String)>,
    /// A seat whose `first` order is `failed`.
    pub dropped: BTreeSet<String>,
}

impl DeliveryColumns {
    /// The columns of `orders`; `typed_lately` names the orders typed within
    /// the last tick.
    pub(crate) fn of<'a>(
        orders: impl IntoIterator<Item = &'a wsp_store_contract::Order>,
        typed_lately: &BTreeSet<wsp_store_contract::OrderId>,
    ) -> DeliveryColumns {
        use wsp_store_contract::OrderState as S;
        let mut d = DeliveryColumns::default();
        let mut settled: BTreeMap<&str, &wsp_store_contract::Order> = BTreeMap::new();
        for o in orders {
            let seat = o.to.0.as_str();
            match o.state {
                S::Typed if !typed_lately.contains(&o.id) => {
                    d.unsent.insert(seat.to_string());
                }
                S::Queued if o.attempts > 0 => {
                    d.unsent.insert(seat.to_string());
                }
                S::Failed if o.first => {
                    d.dropped.insert(seat.to_string());
                }
                _ => {}
            }
            if matches!(o.state, S::Acknowledged | S::Done | S::Failed) {
                let later = settled.get(seat).is_none_or(|was| order_number(&was.id) < order_number(&o.id));
                if later {
                    settled.insert(seat, o);
                }
            }
        }
        for (seat, o) in settled.into_iter().filter(|(_, o)| o.state == S::Failed && !o.first) {
            let why = o.why.clone().unwrap_or_else(|| format!("order {} failed", o.id.0));
            d.refusing.insert(seat.to_string(), (u64::from(o.attempts), why));
        }
        d
    }

    /// The columns as `reads` gives the orders, or as the mirror's `state`
    /// does when `Wip::from_api` reads (wsp-076). Whether a `typed` order has
    /// sat a tick is its `order.typed` event's moment, read through `reads`
    /// only when some order is `typed`. A read that fails draws nothing and
    /// says so on stderr: an order not read is not an order unsent.
    fn read(reads: &OrderReads, state: Option<&wsp_store_contract::State>, now_ms: u64) -> DeliveryColumns {
        let fetched;
        let orders: Vec<&wsp_store_contract::Order> = match state {
            Some(s) => s.orders.values().collect(),
            None => match reads.all() {
                Ok(o) => {
                    fetched = o;
                    fetched.iter().collect()
                }
                Err(e) => {
                    eprintln!("wsp: wip read no orders: {e}");
                    return DeliveryColumns::default();
                }
            },
        };
        let typed: BTreeSet<wsp_store_contract::OrderId> =
            orders.iter().filter(|o| o.state == wsp_store_contract::OrderState::Typed).map(|o| o.id.clone()).collect();
        let lately = match typed.is_empty() {
            true => typed,
            false => reads.typed_since(wsp_store_contract::At(now_ms.saturating_sub(ORDER_TICK_MS))).unwrap_or_else(|e| {
                eprintln!("wsp: wip read no order's typing time: {e}");
                typed
            }),
        };
        DeliveryColumns::of(orders, &lately)
    }
}

/// Where an order falls in its seat's sequence: `cpd-1-o12` is 12.
fn order_number(id: &wsp_store_contract::OrderId) -> u64 {
    id.0.rsplit_once("-o").and_then(|(_, n)| n.parse().ok()).unwrap_or(0)
}

/// `wip`'s `stuck` under the orders machine: each seat's open stall as the
/// manager's `seat.stuck` was folded (`manager/<host>.stuck.json`, wsp-028),
/// not `stuck-commands.json`. Whether the seat was told is its stall order's
/// state (`repair::stuck_ordered`), which `wip` does not publish.
fn stuck_folded<'a>(
    store: &Store,
    seats: impl IntoIterator<Item = &'a String>,
    now: i64,
) -> BTreeMap<String, (crate::stuck::Kind, i64)> {
    seats
        .into_iter()
        .filter_map(|seat| {
            let s = stuck_of_seat(store, seat, now, None).into_iter().find(|s| s.open)?;
            Some((seat.clone(), (s.kind, s.secs)))
        })
        .collect()
}

/// Since when each seat `wip` draws has been in its state: what the fold dated
/// it at ([`crate::fold::state_since`]) when the manager's turn edge or stop
/// reached it. A seat with nobody in it has no state worth a clock.
pub(crate) fn seat_clocks<'a>(store: &Store, seats: impl IntoIterator<Item = &'a crate::place::Seated>) -> std::collections::BTreeMap<String, String> {
    crate::fold::state_since(store, seats.into_iter().filter(|s| s.state != State::Empty).map(|s| s.seat.as_str()))
}

/// Every standing barrier check's seat, named against its list and placed in
/// the project its group's first member lives in.
fn checking(lists: &[crate::model::Worklist], tasks: &[Task]) -> std::collections::BTreeMap<String, (String, usize, Option<String>)> {
    crate::barrier::seats(lists)
        .into_iter()
        .map(|(seat, (list, at))| {
            let project = lists
                .iter()
                .find(|w| w.id == list)
                .and_then(|w| w.groups().into_iter().nth(at - 1))
                .and_then(|g| g.members.iter().find_map(|m| tasks.iter().find(|t| &t.id == m)?.project.clone()));
            (seat, (list, at, project))
        })
        .collect()
}

/// How old the daemon's tick stamp may be before [`daemon_by_stamp`] says it
/// is not ticking: the loop comes round every 20 s and stamps at the top of a
/// pass, so two minutes is a pass that has not started for several ticks, not
/// a slow one.
const DAEMON_QUIET_SECS: i64 = 120;

/// `wip`'s dead-daemon heading without the process table, under
/// `WSP_HOST=manager` (`docs/agent-manager.md` 4.3: no `ps`): the stamp the
/// daemon writes every tick (`Store::DAEMON_TICK`, read by the caller and
/// passed in as `last`) older than [`DAEMON_QUIET_SECS`] on a machine whose
/// manager answered. Not `cycle.log`
/// (wsp-035): the tick writes that only when it has something to say, so an
/// idle fleet's log read as a dead daemon. No stamp at all says nothing — a
/// daemon from before the stamp is not called dead.
fn daemon_by_stamp(last: Option<i64>, heard: bool, now: i64) -> Option<String> {
    if !heard {
        return None;
    }
    let quiet = now - last?;
    (quiet > DAEMON_QUIET_SECS).then(|| {
        format!(
            "the daemon is not ticking — its last tick was {} ago (`wsp install --daemon` loads it)",
            util::duration_human(quiet)
        )
    })
}

impl Wip {
    /// `wip` under `WSP_HOST=manager` (`docs/agent-manager.md` 4.3): one
    /// `seats` to each host in the machine records, in parallel, each heard or
    /// silent within a second. The seats' state, rss and shells and the
    /// host's disks come from those replies; this runs no `ps`, `lsof`,
    /// render or `df`. A seat the manager does not run (one started
    /// in-process) is not here until adoption (wsp-029).
    pub(crate) fn from_managers(store: &Store) -> Wip {
        let reading = crate::host_tcp::census(&store.state, env!("CARGO_PKG_VERSION"));
        let census = &reading.census;
        let seen: Vec<crate::place::Seated> = census.seats().filter(|s| s.state != State::Empty).cloned().collect();
        let since = seat_clocks(store, seen.iter());
        let rss: BTreeMap<String, u64> = reading.rows.iter().map(|(s, r)| (s.clone(), r.rss_bytes)).collect();
        let counts: BTreeMap<String, u32> = reading.rows.iter().map(|(s, r)| (s.clone(), r.shells)).collect();
        let disks: Vec<crate::host_tcp::DiskFree> = reading.facts.values().flat_map(|f| f.disks.iter().cloned()).collect();
        let heard = reading.heard();
        // Silent is never empty (remote-agents' rule): a manager that did not
        // answer is said in the heading, beside any far machine's silence.
        let unheard: Vec<String> = census
            .unheard()
            .map(|(m, why)| {
                let who = match m {
                    "" => "this machine's manager".to_string(),
                    m => format!("{m}'s manager"),
                };
                format!("{who} ({})", match why {
                    crate::place::Refusal::Unreachable(w) => w.clone(),
                    other => other.to_string(),
                })
            })
            .collect();
        let read = Gathering::files(store, seen.into_iter().map(|s| (s, None)).collect(), since);
        let mut w = Wip::gathered(store, read, |agents, bindings| {
            let shells = crate::waiting::Shells::counted(
                agents.iter().filter(|a| a.state == State::Idle && bindings.contains_key(a.seat.as_str())).map(|a| a.seat.as_str()),
                &counts,
            );
            let memory = crate::memory::of_rss(&store.state, agents, bindings, &rss);
            Probed {
                shells,
                memory,
                daemon: daemon_by_stamp(store.daemon_tick(), heard, util::epoch_secs()),
                disk: crate::disk::loud_of(store, &disks),
                load: crate::throttle::loud(store),
                throttle: Some(crate::throttle::status_line(store)),
            }
        });
        if !unheard.is_empty() {
            let line = format!("unreachable: {} — its seats are not read, not gone", unheard.join("; "));
            w.silent.line = Some(match w.silent.line.take() {
                Some(far) => format!("{line}\n{far}"),
                None => line,
            });
        }
        w
    }

    #[cfg(test)]
    pub(crate) fn live(store: &Store) -> Wip {
        let backends = crate::place::local_backends();
        let mut agents = Vec::new();
        let mut heard = false;
        for backend in backends.iter() {
            let Ok(census) = backend.census() else { continue };
            heard = true;
            // Each gone seat is dated by the backend that holds it: the
            // clock lives in that backend's record of the seat, and
            // nothing above the port may read it off a path.
            agents.extend(census.seats().filter(|s| s.state != State::Empty).map(|s| {
                let quiet = (s.state == State::Gone).then(|| backend.quiet_since(&s.seat)).flatten();
                (s.clone(), quiet)
            }));
        }
        let since = seat_clocks(store, agents.iter().map(|(s, _)| s));
        Wip::gathered(store, Gathering::files(store, agents, since), |agents, bindings| Probed {
            shells: idle_members_shells(agents, bindings),
            memory: crate::memory::read(&store.state, agents, bindings, || crate::host::Host::seat_memory(&crate::host::LOCAL)),
            daemon: crate::daemon::dead(&store.state, crate::daemon::running(&store.state).as_deref(), heard),
            disk: crate::disk::loud(store),
            load: crate::throttle::loud(store),
            throttle: Some(crate::throttle::status_line(store)),
        })
    }

    /// `wip` through the state core's API (wsp-076, `docs/state-core.md` 4.5):
    /// rows, claims, lists, asks and seats from one `state` reply, the
    /// projections' verbatim records rebuilt by `crate::mirror`'s `*_of`. What
    /// is no part of the domain yet stays a file read, as on the files path:
    /// projects, governors, mandates, refusals, pins, agents, said, worked,
    /// and the per-seat files the manager's events replaced (wsp-030) are no part of it.
    ///
    /// **It runs no child**: no `ps`, `lsof`, `df`, render, `git` or `wsp`.
    /// The moment readings are no part of the snapshot: the seats' rss and
    /// shells and the host's disks and load come from the managers' `seats`
    /// replies, as under [`Wip::from_managers`]. The throttle's line reads the
    /// person's idle time, the clock and the cores from the local manager's
    /// beat; a build holder's pid and a waiter's are asked of kill(2) by
    /// syscall (`host::LOCAL.pid_runs`, wsp-111); and the dead-daemon heading is the tick
    /// stamp's (the daemon has just answered, so a stamp gone stale is the one
    /// thing left to say).
    pub(crate) fn from_api(store: &Store, state: &wsp_store_contract::State) -> Wip {
        let tasks: Vec<Task> = state
            .rows
            .values()
            .filter_map(crate::mirror::task_of)
            .map(|t| {
                let head = wsp_store::model::TaskHead::of(&t);
                match wip_reads_in_full(&head) {
                    true => t,
                    false => head.bodiless(),
                }
            })
            .collect();
        let claims: BTreeMap<String, Value> = state
            .claims
            .keys()
            .filter_map(|row| Some((row.0.clone(), crate::mirror::claim_of(state.rows.get(row)?)?)))
            .collect();
        let bindings: BTreeMap<String, Value> =
            state.seats.values().filter_map(|s| Some((s.id.0.clone(), crate::mirror::binding_of(s)?))).collect();
        let spawned: BTreeMap<String, Value> =
            state.seats.values().filter_map(|s| Some((s.id.0.clone(), crate::mirror::spawned_of(s)?))).collect();
        let lists: Vec<crate::model::Worklist> = state.lists.values().filter_map(crate::mirror::list_of).collect();
        let messages: BTreeMap<String, Message> =
            state.asks.values().filter_map(crate::mirror::message_of).map(|m| (m.id.clone(), m)).collect();
        let agents: Vec<(crate::place::Seated, Option<i64>)> = state
            .seats
            .values()
            .map(crate::mirror::seated_of)
            .filter(|s| s.state != State::Empty)
            .map(|s| (s, None))
            .collect();
        let since = seat_clocks(store, agents.iter().map(|(s, _)| s));
        let read = Gathering {
            tasks,
            bindings,
            claims,
            spawned,
            lists,
            messages,
            alive: Box::new(|pid| crate::host::LOCAL.pid_runs(pid)),
            agents,
            since,
            delivery: OrderReads::of(store).map(|r| DeliveryColumns::read(&r, Some(state), now_ms())),
        };
        // The moment readings stay out of the contract (wsp-076, the person's
        // decision): the seats' rss and shells and the host's disks and load
        // come live from the managers' `seats` replies, the source
        // `from_managers` reads them from, and no child runs for them.
        let reading = crate::host_tcp::census(&store.state, env!("CARGO_PKG_VERSION"));
        let rss: BTreeMap<String, u64> = reading.rows.iter().map(|(s, r)| (s.clone(), r.rss_bytes)).collect();
        let counts: BTreeMap<String, u32> = reading.rows.iter().map(|(s, r)| (s.clone(), r.shells)).collect();
        let disks: Vec<crate::host_tcp::DiskFree> = reading.facts.values().flat_map(|f| f.disks.iter().cloned()).collect();
        let here = reading.facts.get("");
        let load = here.and_then(|f| load_of_facts(store, f));
        let throttle = throttle_of_facts(store, here);
        Wip::gathered(store, read, |agents, bindings| Probed {
            shells: crate::waiting::Shells::counted(
                agents.iter().filter(|a| a.state == State::Idle && bindings.contains_key(a.seat.as_str())).map(|a| a.seat.as_str()),
                &counts,
            ),
            memory: crate::memory::of_rss(&store.state, agents, bindings, &rss),
            daemon: daemon_by_stamp(store.daemon_tick(), true, util::epoch_secs()),
            disk: crate::disk::loud_of(store, &disks),
            load,
            throttle: Some(throttle),
        })
    }

    /// The store's half of a reading, joined to a census: `read` with each
    /// gone seat's quiet time, and `probe` for what is read of the host.
    fn gathered(
        store: &Store,
        read: Gathering,
        probe: impl FnOnce(&[crate::place::Seated], &BTreeMap<String, Value>) -> Probed,
    ) -> Wip {
        let Gathering { tasks, bindings, claims, spawned, lists, messages, alive, agents, since, delivery } = read;
        // Under the orders machine the four delivery columns are the orders'
        // and the folded stall's (wsp-127): typed-unsent.json,
        // first-orders.json, refused-deliveries.json and stuck-commands.json
        // are not read.
        let (unsent, refusing, dropped, stuck) = match delivery {
            Some(d) => (d.unsent, d.refusing, d.dropped, stuck_folded(store, bindings.keys(), util::epoch_secs())),
            None => {
                let governors = store.governors();
                let refusing = crate::refusals::refusing(store)
                    .into_iter()
                    .filter_map(|(scope, v)| crate::cmd_govern::seat_held(&scope, &governors).map(|s| (s.pane, v)))
                    .collect();
                (Default::default(), refusing, Default::default(), crate::repair::stuck_seats(store))
            }
        };
        let tasks = std::rc::Rc::new(tasks);
        let governors = store.governors();
        let agents_held = store.agents_held();
        let now = util::epoch_secs();
        let verifying = crate::verification::seats(&tasks);
        let checking = checking(&lists, &tasks);
        let said = store.said();
        let spawned_file = store.spawned();
        let agents = agents
            .into_iter()
            .filter(|(s, quiet)| {
                let held = holding(&tasks, &bindings, &claims, &agents_held, s.seat.as_str()).is_some()
                    || governs_seat(&governors, s.seat.as_str())
                    || verifying.contains_key(s.seat.as_str())
                    || checking.contains_key(s.seat.as_str());
                // A census that dates nothing (the API's, the manager's) is
                // dated by when this machine first saw the seat in its state.
                let quiet = quiet.or_else(|| since.get(s.seat.as_str()).map(|t| util::epoch_of(t)).filter(|t| *t > 0));
                !forgotten(s.state, quiet, now, held) && !mute(s, held, said.get(s.seat.as_str()), ended_by_despawn(&spawned_file, s.seat.as_str()))
            })
            .map(|(s, _)| s)
            .collect::<Vec<_>>();
        let Probed { shells, memory: idle_memory, daemon, disk, load, throttle } = probe(&agents, &bindings);
        Wip {
            tasks,
            index: Index::new(store.projects()),
            bindings,
            claims,
            pins: store.pins(),
            governors,
            agents,
            agents_held,
            said,
            daemon,
            disk,
            load,
            throttle,
            idle_memory,
            worked: store.worked(),
            asks: crate::waiting::Asks::of_messages(store, messages, &*alive),
            shells,
            mandates: store.mandates(),
            checking,
            check_on: crate::barrier::ran_on(&lists),
            lists,
            since,
            spawned,
            unsent,
            queued: crate::wake::queued_tells(store),
            refusing,
            dropped,
            full_suite: crate::throttle::full_suite_seats(&store.state),
            stuck,
            silent: crate::far::Silence::read(store),
        }
    }
}

/// [`crate::throttle::status_line`] from the local manager's beat rather than
/// `ioreg`, `pmset` and a `kill` per build holder, for [`Wip::from_api`]: the
/// person's idle time (so `auto` is in force or away as on the files path),
/// the clock and the cores as the beat read them, each holder's pid asked of
/// kill(2), and under `WSP_JOBS=manager` a build's hold read from the
/// manager's job table instead (wsp-052; the files path's `status_line` reads
/// the beat there too). With no beat the idle time is unreadable, as where
/// `ioreg` fails.
///
/// The beat says 100 where `pmset` records no `CPU_Speed_Limit`, which the
/// files path reads as no clock at all (Apple silicon prints none): an
/// unlimited clock draws no cell here, so the two lines agree where it does.
fn throttle_of_facts(store: &Store, f: Option<&crate::host_tcp::Facts>) -> String {
    let given = crate::throttle::Given {
        load1: 0.0,
        cores: f.map(|f| f.cpus as usize).unwrap_or_else(|| std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)),
        speed_limit: f.and_then(|f| f.speed_limit_pct).filter(|&p| p < 100),
    };
    let idle = f.and_then(|f| f.hid_idle_secs);
    crate::throttle::View::of_beat(&store.state, given, idle).status_line()
}

/// [`crate::throttle::loud`] from a manager's host facts rather than `sysctl`
/// and `pmset`, for [`Wip::from_api`]: the load averages and the clock as the
/// manager's last beat read them.
fn load_of_facts(store: &Store, f: &crate::host_tcp::Facts) -> Option<String> {
    let centi = |c: u32| c as f64 / 100.0;
    let one = centi(f.load_centi);
    let five = f.load5_centi.map(centi).unwrap_or(one);
    let fifteen = f.load15_centi.map(centi).unwrap_or(five);
    let given = crate::throttle::Given { load1: one, cores: f.cpus as usize, speed_limit: f.speed_limit_pct };
    let mut view = crate::throttle::View::of_beat(&store.state, given, None);
    view.load = Some((one, five, fifteen));
    view.warning(store.seat_cap())
}

/// How long `wip` waits for the API before it reads the files (`docs/state-core.md`
/// 4.6 (c)).
const API_BUDGET: std::time::Duration = std::time::Duration::from_millis(200);

/// Whether `wip` reads through the API: only under `WSP_READ=api`. Unset or
/// anything else is the files path and never dials, because on a full store the
/// API path costs about five times the files' CPU until it meets state-core's
/// step 1 bar.
fn wip_reads_api() -> bool {
    std::env::var("WSP_READ").is_ok_and(|v| v.trim() == "api")
}

/// One `state` through the API, dialled within [`API_BUDGET`], or why not:
/// nothing answering (no daemon, one with no API, a refused token), a lost or
/// late reply, or a refusal.
fn api_state(store: &Store) -> Result<wsp_store_contract::State, String> {
    let mut client = wsp_client::Client::dial(&store.state, wsp_store_contract::Principal::Wsp("wip".into()), API_BUDGET)
        .map_err(|e| e.to_string())?;
    match client.request("state", json!({})).map_err(|e| e.to_string())?.outcome {
        wsp_api::Outcome::Ok(wsp_api::Answer::State { state, .. }) => Ok(state),
        wsp_api::Outcome::Ok(_) => Err("`state` was answered with another verb's answer".into()),
        wsp_api::Outcome::Err(f) => Err(format!("`state` was refused: {:?} {}", f.code, f.text).trim_end().to_string()),
    }
}

/// The tasks `wip` reads, from the store's index where their bodies are not
/// read (`wsp-320`).
///
/// **In full: everything open, and every row a seat can be read off** — a
/// `## Verification` or `## Legs`, and the Verify and Barrier rows wsp filed
/// before those sections existed, whose `## Overview` and `## Log` the
/// verification and barrier readers still consult. What is left is finished
/// work with none of those, which `wip` reads for its status, project and
/// parent alone: three quarters of the store, and the files it no longer opens.
fn wip_tasks(store: &Store) -> Vec<Task> {
    store.tasks_read_where(wip_reads_in_full)
}

pub(crate) use crate::mirror::wip_reads_in_full;

/// How long a seat must have been gone, with nothing held, before `wip` stops
/// drawing it. A day: a machine read at the start of the morning still shows
/// what an agent was doing when it stopped last night.
pub(crate) const GONE_FOR: i64 = 24 * 60 * 60;

/// Whether a census row is a seat nobody has any use for: gone, dated, past
/// [`GONE_FOR`], and holding nothing (`wsp-169`, wsp-158 item 5).
///
/// **Every refusal here leaves the row drawn, and that is the direction to
/// fail in.** An undated seat (`None` — a compound seat whose directory cannot be read has none
/// either) is not "long ago"; a seat that holds something is the
/// reconciler's (`repair::gone_member`, `repair::seat_vacant`), which finds
/// it *through* this census and would otherwise be told nothing. Only a seat
/// with nothing left to resume is dropped, and only from the reading: its
/// directory is untouched, so `quiet_since` keeps answering and the row
/// returns the moment a claim or a binding does.
pub(crate) fn forgotten(state: State, quiet_since: Option<i64>, now: i64, held: bool) -> bool {
    state == State::Gone && !held && quiet_since.map(|at| now.saturating_sub(at) > GONE_FOR).unwrap_or(false)
}

/// Whether a census row is a gone seat with nothing to say (wsp-094): it
/// holds nothing, and either a despawn ended it (`ended`, its `spawned.json`
/// record's end) or it left no sentence in `said.json` and no agent is on
/// record for it — the row would draw its id and the word `gone` and nothing
/// else. Dropped from the reading at once, not after [`GONE_FOR`]: the day is
/// kept so a morning still shows what an agent was doing when it stopped, and
/// of this seat there is nothing to show. A machine restart leaves a fleet of
/// these in the state core's mirror.
pub(crate) fn mute(seat: &crate::place::Seated, held: bool, said: Option<&Value>, ended: bool) -> bool {
    let sentence = said.and_then(|e| e.get("said")).and_then(Value::as_str).is_some_and(|s| !s.trim().is_empty());
    let blank = !sentence && seat.agent.kind.is_empty() && seat.agent.name.is_empty();
    seat.state == State::Gone && !held && (ended || blank)
}

/// Whether `spawned` (the `spawned.json` file) says a despawn ended `seat`.
fn ended_by_despawn(spawned: &BTreeMap<String, Value>, seat: &str) -> bool {
    spawned.get(seat).and_then(|r| r.get("ended")).is_some()
}

/// The seats among `seats` that `wip` no longer draws, read against the
/// store as `wip` reads it: [`mute`], and [`forgotten`] dated by when the
/// manager said each stopped (`said.json`, folded). What the daemon's sweep
/// forgets (wsp-094).
pub(crate) fn mute_seats(store: &Store, seats: &[crate::place::Seated]) -> Vec<String> {
    if !seats.iter().any(|s| s.state == State::Gone) {
        return Vec::new();
    }
    let now = util::epoch_secs();
    let since: BTreeMap<String, i64> = crate::fold::state_since(store, seats.iter().filter(|s| s.state == State::Gone).map(|s| s.seat.as_str()))
        .into_iter()
        .map(|(k, iso)| (k, util::epoch_of(&iso)))
        .filter(|(_, t)| *t > 0)
        .collect();
    // The cheap half first, as if nothing held them: the tasks are read only
    // when some seat would be dropped but for what it holds.
    let (said, spawned) = (store.said(), store.spawned());
    let dropped = |s: &crate::place::Seated, held: bool| {
        let seat = s.seat.as_str();
        mute(s, held, said.get(seat), ended_by_despawn(&spawned, seat)) || forgotten(s.state, since.get(seat).copied(), now, held)
    };
    let maybe: Vec<&crate::place::Seated> = seats.iter().filter(|s| dropped(s, false)).collect();
    if maybe.is_empty() {
        return Vec::new();
    }
    let tasks = wip_tasks(store);
    let (bindings, claims, agents_held, governors) = (store.bindings(), store.claims(), store.agents_held(), store.governors());
    let lists = store.worklists();
    let (verifying, checking) = (crate::verification::seats(&tasks), checking(&lists, &tasks));
    maybe
        .into_iter()
        .filter(|s| {
            let seat = s.seat.as_str();
            let held = holding(&tasks, &bindings, &claims, &agents_held, seat).is_some()
                || governs_seat(&governors, seat)
                || verifying.contains_key(seat)
                || checking.contains_key(seat);
            dropped(s, held)
        })
        .map(|s| s.seat.as_str().to_string())
        .collect()
}

/// The open task a seat still holds, by its binding or by a claim its agent
/// made — what makes a gone seat somebody's to repair rather than nobody's.
///
/// A claim is keyed by task and names an *agent*, and an agent names a seat
/// through `agents.json`, so the join runs through every agent that ever sat
/// there and not only the latest: a seat a governor reseated holds the claims
/// of the one before it too. A task that is `done`, or gone from the store,
/// holds nothing — a binding to it is a record of work that ended.
pub(crate) fn holding(
    tasks: &[Task],
    bindings: &BTreeMap<String, Value>,
    claims: &BTreeMap<String, Value>,
    agents_held: &BTreeMap<String, Value>,
    seat: &str,
) -> Option<String> {
    let open = |id: &str| tasks.iter().any(|t| t.id == id && t.status().is_open());
    let bound = bindings.get(seat).and_then(|b| b.get("task_id")).and_then(|t| t.as_str());
    if let Some(id) = bound.filter(|id| open(id)) {
        return Some(id.to_string());
    }
    claims
        .iter()
        .filter(|(id, _)| open(id))
        .find(|(_, c)| {
            c.get("agent_id")
                .and_then(|a| a.as_str())
                .and_then(|a| agents_held.get(a))
                .and_then(|a| a.get("seat"))
                .and_then(|s| s.as_str())
                == Some(seat)
        })
        .map(|(id, _)| id.clone())
}

/// Whether `seat` is a project's governor seat, through the same query `wip`
/// draws its `▣` from.
pub(crate) fn governs_seat(governors: &BTreeMap<String, Value>, seat: &str) -> bool {
    let workspace = seat.split(':').next().unwrap_or(seat);
    crate::place::governs(governors, &crate::place::seat_query(workspace, Some(seat))).is_some()
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
    /// The same fact, typed, straight off [`place::Seated::state`] rather than
    /// re-derived from the string above. `state` is wsp's own vocabulary
    /// (`State::as_str`), not one backend's — a caller that fed it back through
    /// a backend's word table would be reading another backend's word through
    /// it, which is exactly the mistake `compound-064` refused. A
    /// caller that wants to branch on the state reads this field and does no
    /// parsing at all.
    ///
    /// Unread outside the tests since `wsp-172`: the watch, its one caller,
    /// branches on [`WipRow::waiting`] now, which is read from this same value
    /// and is the question it was asking.
    #[allow(dead_code)]
    pub(crate) state_typed: crate::place::State,
    /// Whether a turn is actually running in it. Beside `state` rather than
    /// derived from it at the point of drawing, because it is the answer to the
    /// question the heading asks — and because reading it off the word is the
    /// thing four censuses each got wrong in their own way.
    pub(crate) turning: bool,
    pub(crate) needs_you: bool,
    /// The project this pane's workspace is the governor of, if any — which is
    /// no project for every ordinary agent, and that is nearly all of them.
    pub(crate) seat: Option<String>,
    /// What the agent last said it was doing, while that is still about the
    /// work its seat holds. Published as `said` for a reader that draws it —
    /// compound's pane header reads exactly this key.
    pub(crate) said: Option<String>,
    /// Whether the seat is waiting on somebody, and on whom — [`crate::waiting`]'s
    /// one reading, which the reconciler, the barrier and the watch also ask.
    /// Published as `waiting`, and drawn in the STATE column in place of the
    /// bare word, because `idle` is exactly what a waiting seat must not read.
    pub(crate) waiting: Option<crate::waiting::Wait>,
    /// The seat's whole wait in one reading (`wsp-552`): its kind, target,
    /// since, quiet seconds and line — [`crate::waiting::seat_wait`].
    /// Published as `wait`; `wip` prints the line under the row.
    pub(crate) wait: Option<crate::waiting::SeatWait>,
    /// For a governor's row, how many seats under it are waiting: the rows
    /// whose `answers_to` is its scope, whatever they are waiting on.
    pub(crate) members_waiting: usize,
    /// The scope of the governor this seat answers to — [`answers_to`]'s walk,
    /// published as `answers_to` on every row (`wsp-247`).
    pub(crate) answers_to: Option<String>,
    /// With nobody seated on that walk, the scope it starts at, so a reader can
    /// say "no governor seated" rather than guess. Published as
    /// `answers_to_empty`.
    pub(crate) answers_to_empty: Option<String>,
    /// How long the work it holds has been held, as `claim_line` phrases it
    /// (`3h12m`) — off the CLAIM, not the binding, which is remade whenever a
    /// pane is and would reset the clock on work nobody had put down. `None`
    /// when it holds no claim or the claim predates the clock. Published as
    /// `held`.
    pub(crate) held: Option<String>,
    /// Since when the seat has been in its `state`, RFC 3339: the turn's start
    /// while it turns, the stop while it is idle or blocked, the death once it
    /// is gone — the first census wsp read it in that state, so at most one
    /// tick late ([`Store::note_seat_states`]). Unlike `held` every seat has
    /// one: a verifier, a check and a governor hold no claim. `None` only on
    /// a reading that noted nothing. Published as `since` (`wsp-250`).
    pub(crate) since: Option<String>,
    /// The project its mandate points it at — what a verb would send it to
    /// work on, as against `project`, where it stands. The two are separate
    /// facts and both are published; a reader that wants "only when they
    /// differ" compares them. Published as `direction`.
    pub(crate) direction: Option<String>,
    /// What runs in the seat. Published as `kind`, `model` and `effort`.
    pub(crate) runs: Runs,
    /// What the seat is. Published as `role`.
    pub(crate) role: Role,
    /// The seat that spawned it and the scope that seat governed, as `spawn`
    /// recorded it (`wsp-249`). Published as `spawned_by`; `null` for a seat
    /// spawned from no seat.
    pub(crate) spawned_by: Option<Value>,
    /// The leg standing in the seat, when it is one (`docs/legs.md`, *Seen*).
    /// Published as `leg: {name, after, index, of}`; `null` for any other seat.
    pub(crate) leg: Option<crate::legs::Standing>,
    /// The prompt holds text that was typed and never submitted, for more than
    /// a tick (`wsp-386`). Drawn as `typed, not sent`; published as `unsent`.
    pub(crate) unsent: bool,
    /// Tells queued for this seat's next idle (`wsp-659`). Published as `queued`.
    pub(crate) queued: usize,
    /// The seat's first work order was sent again and no turn started on it
    /// (`wsp-420`). Drawn as `order not received`; published as `dropped`.
    pub(crate) dropped: bool,
    /// This seat has started a full-suite `cargo test` (`wsp-389`). Drawn as `full suite in a member seat`;
    /// published as `full_suite`.
    pub(crate) full_suite: bool,
    /// The seat is in a shell command waiting on itself or on a bare `cat`
    /// (`wsp-397`), and for how many seconds it has held. Drawn as a flag;
    /// published as `stuck: {kind, word, secs}`.
    pub(crate) stuck: Option<(crate::stuck::Kind, i64)>,
    /// The scope this pane was the governor of, when it is gone. **A gone pane
    /// is not a governor** (`wsp-386`): `seat` and the governor role are left
    /// off it, so a list's seat that vanished does not read as seated; the
    /// scope is kept here so the row still says whose it was. Published as
    /// `was_governor`.
    pub(crate) was_governor: Option<String>,
    /// The finished row this seat last held and is still up on, when the row was
    /// closed (`wsp done`) with the seat running: `on a closed row` (`wsp-582`).
    /// Published as `closed_row`.
    pub(crate) closed_row: Option<String>,
}

/// The row, closed, that `seat` last held and was never ended from: the seat
/// `worked` says a done row's claim was let go of, when nothing holds the seat now.
/// `wsp despawn <row>` ends it.
pub(crate) fn closed_row_of(
    tasks: &[Task],
    bindings: &BTreeMap<String, Value>,
    worked: &BTreeMap<String, Value>,
    seat: &str,
) -> Option<String> {
    if bindings.contains_key(seat) {
        return None;
    }
    worked
        .iter()
        .filter(|(_, w)| w.get("workspace_id").and_then(Value::as_str) == Some(seat))
        .map(|(id, _)| id)
        .find(|id| tasks.iter().any(|t| &t.id == *id && t.status() == Status::Done))
        .cloned()
}

/// What a seat is, typed, for a reader that places it (`wsp-246`): compound's
/// Agents view hangs a verifier beside its member and a check under its list's
/// governor, and before this it could tell them apart only by parsing `task`,
/// which is display text — [`crate::verification::seat_label`]'s and
/// [`crate::barrier::seat_label`]'s format strings — with `task_id` empty.
///
/// Asked in the order `task` is named in, so the two never disagree about a
/// seat: a governor's record, then the work it holds, then a pass standing
/// there, then a check. A seat a governor spawned (`wsp-249`, its
/// `spawned_by` naming a scope) is a helper where it would otherwise be an
/// agent, or a member of no running list — a run's member stays a member,
/// whoever restarted it, because the run is what places it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Role {
    /// It governs `id`, a project or a worklist — the scope `seat` names.
    Governor { list: bool, id: String },
    /// It is the primary interface (`wsp-279`): the slot that is no project's,
    /// where the person talks to the fleet. compound finds the Chat pane by
    /// this and never by a label; `seat` says `primary` beside it.
    Primary,
    /// It holds `task`, and `placing` is the running list and group it is a
    /// member of.
    Member { task: String, placing: Option<(String, usize)> },
    /// It is verifying `member`, on the member's `pass`th pass.
    Verifier { member: String, pass: usize },
    /// It is checking `list`'s barrier `group`.
    Check { list: String, group: usize },
    /// It is `leg` of the staged row `task` (`docs/legs.md`): it holds no
    /// claim, and the row's `## Legs` is what names it.
    Leg { task: String, leg: String },
    /// It is piece `piece` of the staged row `task` (`docs/pieces.md`): it holds
    /// no claim either, and the row's `## Pieces` names it.
    Piece { task: String, piece: usize },
    /// The governor of `scope` spawned it, and it holds `task` if it holds
    /// work — compound draws it at that row, or under that governor with none
    /// (`docs/ux-design.md` §18 rule 6).
    Helper { scope: String, task: Option<String> },
    /// None of the above.
    #[default]
    Agent,
}

impl Role {
    pub(crate) fn json(&self) -> Value {
        match self {
            Role::Governor { list, id } => {
                json!({ "is": "governor", "of": if *list { "list" } else { "project" }, "id": id })
            }
            Role::Primary => json!({ "is": "primary" }),
            Role::Member { task, placing: None } => json!({ "is": "member", "task": task }),
            Role::Member { task, placing: Some((list, group)) } => {
                json!({ "is": "member", "task": task, "list": list, "group": group })
            }
            Role::Verifier { member, pass } => json!({ "is": "verifier", "member": member, "pass": pass }),
            Role::Check { list, group } => json!({ "is": "check", "list": list, "group": group }),
            Role::Leg { task, leg } => json!({ "is": "leg", "task": task, "leg": leg }),
            Role::Piece { task, piece } => json!({ "is": "piece", "task": task, "piece": piece }),
            Role::Helper { scope, task: None } => json!({ "is": "helper", "scope": scope }),
            Role::Helper { scope, task: Some(t) } => json!({ "is": "helper", "scope": scope, "task": t }),
            Role::Agent => json!({ "is": "agent" }),
        }
    }
}

/// What runs in a seat — the kind (`claude`, `opencode`), the model as it was
/// spawned (`opus`, `sonnet[1m]`, an opencode `provider/model`) and the effort
/// (`high`) — read off the record the seat was started by (`wsp-248`).
///
/// **Each half is `None` when that record does not state it, never guessed.**
/// A spawn that named no tier writes none, and `default` in `spawned at` is
/// the record of a half nobody stated — what the settings file said, which
/// wsp never knew. Filling either from the kind's usual model would be a
/// census that agrees with itself and not with the seat; the model that
/// actually *served* is a different fact, read off the transcript when the
/// claim ends (`cmd_attempts`), and is not this one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Runs {
    pub(crate) kind: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) effort: Option<String>,
}

impl Runs {
    /// A verification pass's or a barrier check's `on`, which is a
    /// [`crate::model::Policy::label`].
    fn of_label(on: &str) -> Runs {
        crate::model::Policy::of_label(on)
            .map(|p| Runs { kind: Some(p.kind), model: p.model, effort: p.effort })
            .unwrap_or_default()
    }

    /// The tier [`spawned_at`] wrote on a claim line, and the kind beside it.
    /// The effort is after the *last* `/`, since an opencode model has its
    /// own (`cmd_attempts::agrees` argues the same split).
    fn of_spawned(kind: Option<String>, tier: &str) -> Runs {
        let stated = |s: &str| Some(s.to_string()).filter(|s| !s.is_empty() && s != "default");
        let (model, effort) = match tier.rsplit_once('/') {
            Some((m, e)) => (stated(m), stated(e)),
            None => (stated(tier), None),
        };
        Runs { kind, model, effort }
    }
}

/// What runs in `seat`, by what the seat is: a governor's from its record in
/// `governors.json`, a verifier's from its pass, a check's from its entry, and
/// a member's from its claim — the kind on the claim (or, for a claim made
/// before it carried one, the binding) and the tier on the claim's log line.
///
/// The governor first because its record is rewritten by the two verbs that
/// start a governor's agent (`cmd_govern::note_started`); a claim made from a
/// governor's seat afterwards did not start anything. Where wsp-117 left a
/// governor's tier unwritten the record says `""`, which is `None` here too.
fn runs(
    w: &Wip,
    seat: &str,
    governs: Option<&str>,
    held: Option<&Task>,
    ran_on: &BTreeMap<String, Option<String>>,
    legged: &BTreeMap<String, crate::legs::Standing>,
    pieced: &BTreeMap<String, crate::pieces::Standing>,
) -> Runs {
    let text = |v: Option<&Value>, k: &str| {
        v.and_then(|v| v.get(k)).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string)
    };
    if let Some(rec) = governs.and_then(|g| w.governors.get(g)) {
        return Runs { kind: text(Some(rec), "kind"), model: text(Some(rec), "model"), effort: text(Some(rec), "effort") };
    }
    if let Some(on) = ran_on.get(seat).cloned().flatten().or_else(|| w.check_on.get(seat).cloned()) {
        return Runs::of_label(&on);
    }
    // A leg holds no claim, so its entry's policy is the record that started it.
    if let Some(s) = legged.get(seat) {
        let p = &s.leg.policy;
        return Runs { kind: Some(p.kind.clone()), model: p.model.clone(), effort: p.effort.clone() };
    }
    if let Some(s) = pieced.get(seat) {
        let p = &s.piece.policy;
        return Runs { kind: Some(p.kind.clone()), model: p.model.clone(), effort: p.effort.clone() };
    }
    let Some(t) = held else { return Runs::default() };
    let kind = text(w.claims.get(&t.id), "agent_kind").or_else(|| text(w.bindings.get(seat), "agent_kind"));
    // The newest claim line, and only when it is this seat's: an older line,
    // or one another pane wrote, is somebody else's start.
    let tier = crate::cmd_attempts::attempts_of(t)
        .pop()
        .filter(|a| a.pane == seat)
        .map(|a| a.spawned)
        .unwrap_or_default();
    Runs::of_spawned(kind, &tier)
}

/// [`crate::waiting::Shells`] for the seats `needs_you` is asked of: idle, and
/// bound to a task. Only those, because the reading is a `ps` and `wip` is
/// polled — a seat in a turn, or one holding nothing, is never stalled work.
pub(crate) fn idle_members_shells(agents: &[crate::place::Seated], bindings: &BTreeMap<String, Value>) -> crate::waiting::Shells {
    crate::waiting::Shells::read(
        agents.iter().filter(|a| a.state == State::Idle && bindings.contains_key(a.seat.as_str())).map(|a| a.seat.as_str()),
        &crate::stuck::waits_on_itself_line,
    )
}

/// The agents, resolved and in reading order: by project, then by pane.
pub(crate) fn wip_rows(w: &Wip) -> Vec<WipRow> {
    let mut rows: Vec<WipRow> = Vec::new();
    // A verifier holds no claim and no binding (`wsp-188`): its seat is named
    // on the member's open pass, and that is where this reads it from.
    let verifying = crate::verification::seated(&w.tasks);
    // A leg holds none either (`docs/legs.md`, *Seen*): its entry on the row's
    // `## Legs` names the seat, running or done and not yet ended.
    let legged = crate::legs::seated(&w.tasks);
    // So does a piece (`docs/pieces.md`, *Seen and told*): one line each.
    let pieced = crate::pieces::seated(&w.tasks);
    let ran_on = crate::verification::ran_on_by_seat(&w.tasks);
    let running = crate::worklist::Running::over(&w.lists);
    let homes = crate::worklist::homes(w.lists.iter().filter(|l| w.governors.contains_key(&l.id)), &w.tasks, &w.index);
    for a in &w.agents {
        let seat = a.seat.as_str();
        let bound = w
            .bindings
            .get(seat)
            .and_then(|b| b.get("task_id"))
            .and_then(|t| t.as_str())
            .and_then(|id| w.tasks.iter().find(|t| t.id == id));

        // A seat spelled `<workspace>:<pane>` carries the durable id `pins`
        // and `governors` are keyed on — the port has no word of its own for a
        // workspace (`robustness-017` d2), so this reads the one already in
        // the seat. A backend with no workspace of its own mints a seat with
        // no `:`, and
        // the whole string stands in for one that matches nothing, which pins
        // and governors already treat as the seat holding no project.
        let workspace_id = seat.split(':').next().unwrap_or(seat);
        let agent = Store::agent_in_seat_from_map(&w.agents_held, seat);
        let r = resolve::resolve(
            &w.index,
            &w.pins,
            resolve::Held {
                binding: bound
                    .and_then(|t| t.project.clone())
                    .or_else(|| {
                        verifying.get(seat).and_then(|(m, _)| w.tasks.iter().find(|t| &t.id == m)).and_then(|t| t.project.clone())
                    })
                    .or_else(|| w.checking.get(seat).and_then(|(_, _, p)| p.clone())),
                claim: resolve::claimed_project(&w.claims, &w.tasks, agent.as_deref(), Some(workspace_id), None),
            },
            agent.as_deref(),
            Some(workspace_id),
            None,
            Some(&a.cwd),
        );

        // Not `agent_status == "idle"`, which was three words short: see
        // `cmd_govern::needs_a_person`. Running and not turning is the whole
        // predicate, and it is asked of the port rather than of a
        // backend's spelling so the next word one adds does not read as work.
        let stopped = a.state.stopped();
        let doing = bound.map(|t| t.status() == Status::Doing).unwrap_or(false);
        let governs = crate::place::governs(&w.governors, &crate::place::seat_query(workspace_id, Some(seat)));
        let (seat_of_project, was_governor) = match a.state == State::Gone {
            true => (None, governs),
            false => (governs, None),
        };
        let waiting = crate::waiting::reading(Some(a.state), &w.asks, &w.shells, seat, bound.map(|t| t.id.as_str()).unwrap_or(""));
        // A member waiting on its own shell is stopped on live work and needs
        // nobody (`wsp-214`): the shell wakes it. Every other wait still
        // needs somebody — a prompt a person, a question whoever it went to.
        //
        // **Except a seat in a tool call that waits on a person** — blocked in
        // `wsp ask --wait` (`wsp-300`) or held on a dialog by a hook
        // (`wsp-301`), §26.8 D9: it is `working` to the screen and `stopped` is
        // false, and it is stopped on a person all the same.
        let in_ask_wait = waiting.as_ref().is_some_and(|w| matches!(w.why, crate::waiting::Why::Question(_)));
        // **Not from a stale read** (wsp-639): a far seat whose machine's beat
        // is late is not known to have stopped, only not heard lately, and a
        // seat read `idle` off the last answer is not owed to anybody.
        let fresh = !w.silent.late.contains(seat);
        let needs_you = in_ask_wait
            || fresh && cmd_govern::needs_a_person(stopped, doing, seat_of_project.is_some())
                && waiting.as_ref().is_none_or(|w| w.owed())
                // A leg that has written its baton is finished: its seat stands
                // until the tick ends it, and nobody is owed anything from it.
                && legged.get(seat).is_none_or(|s| s.leg.state != crate::legs::State::Done)
                && pieced.get(seat).is_none_or(|s| s.piece.state != crate::pieces::State::Done);
        // The task whose claim started this seat: its binding's, or — once the
        // binding has gone with a reseat — the open one its agent claimed.
        let held = bound.or_else(|| {
            holding(&w.tasks, &w.bindings, &w.claims, &w.agents_held, seat).and_then(|id| w.tasks.iter().find(|t| t.id == id))
        });

        let leg = legged.get(seat);
        let role = match (&seat_of_project, held, verifying.get(seat), w.checking.get(seat)) {
            // A scope is a list when a worklist has its slug, whether or
            // not it has started — the order `cmd_govern::scope_of` reads
            // the one key space in.
            (Some(id), ..) if crate::governors::is_primary(id) => Role::Primary,
            (Some(id), ..) => Role::Governor { list: w.lists.iter().any(|l| &l.id == id), id: id.clone() },
            (None, Some(t), ..) => Role::Member {
                task: t.id.clone(),
                placing: running.of(&t.id).map(|p| (p.list.clone(), p.group)),
            },
            (None, None, Some((member, pass)), _) => Role::Verifier { member: member.clone(), pass: *pass },
            (None, None, None, Some((list, at, _))) => Role::Check { list: list.clone(), group: *at },
            _ => Role::Agent,
        };
        // A leg is bound to its row, which reads as the row's member above; the
        // record that names the seat is the row's `## Legs`.
        let role = match (role, leg) {
            (Role::Member { .. } | Role::Agent, Some(s)) => Role::Leg { task: s.task.clone(), leg: s.leg.name.clone() },
            (role, _) => role,
        };
        let role = match (role, pieced.get(seat)) {
            (Role::Member { .. } | Role::Agent, Some(s)) => Role::Piece { task: s.task.clone(), piece: s.piece.n },
            (role, _) => role,
        };
        let spawned_by = w.spawned.get(seat).filter(|v| v.is_object()).cloned();
        let role = match (role, spawned_by.as_ref().and_then(|b| b.get("scope")).and_then(Value::as_str)) {
            (Role::Agent, Some(scope)) => Role::Helper { scope: scope.to_string(), task: None },
            (Role::Member { task, placing: None }, Some(scope)) => Role::Helper { scope: scope.to_string(), task: Some(task) },
            (role, _) => role,
        };
        let (answers_to, answers_to_empty) = answers_to(w, &role, seat, r.project.as_deref(), &running, &homes);
        let refusing = w.refusing.get(seat).filter(|_| !a.state.turn_in_flight()).cloned();
        let unsent = !a.state.turn_in_flight() && w.unsent.contains(seat) && refusing.is_none();
        let dropped = !a.state.turn_in_flight() && w.dropped.contains(seat);
        let stuck = w.stuck.get(seat).copied();
        let mut wait = crate::waiting::seat_wait(&crate::waiting::Facts {
            state: a.state,
            waiting: waiting.as_ref(),
            in_state_since: w.since.get(seat).map(|s| util::epoch_of(s)).filter(|&e| e > 0),
            stuck: stuck.map(|(k, secs)| (k.key(), k.word(), secs)),
            said: held.map(crate::repair::run_said).unwrap_or_default(),
            unreachable: w.silent.seats.get(seat).cloned(),
            unsent,
            refusing,
            dropped,
            rests_on: held.and_then(|t| rests_on(w, t)),
            now: util::epoch_secs(),
        });
        // A seat stopped at Claude Code's folder-trust dialog says so and names
        // the fix, rather than leaving a member to ask a person (`wsp-466`). Only
        // a seat on a prompt reads its screen.
        if let Some(sw) = wait.as_mut().filter(|_| waiting.as_ref().is_some_and(|x| x.why == crate::waiting::Why::Prompt)) {
            if let Some(fix) = read_pane_text(seat).as_deref().and_then(crate::local_trust::dialog_fix) {
                sw.line = format!("{} — {fix}", sw.line);
            }
        }

        rows.push(WipRow {
            project: r.project.unwrap_or_else(|| "—".into()),
            task: legged
                .get(seat)
                .map(|s| crate::legs::seat_label(&s.task, &s.leg.name))
                .or_else(|| pieced.get(seat).map(|s| crate::pieces::seat_label(&s.task, &s.piece)))
                .or_else(|| bound.map(|t| t.title.clone()))
                .or_else(|| verifying.get(seat).map(|(m, _)| crate::verification::seat_label(m)))
                .or_else(|| w.checking.get(seat).map(|(list, at, _)| crate::barrier::seat_label(list, *at)))
                .unwrap_or_else(|| if a.label.is_empty() { "(unbound)".into() } else { format!("({})", a.label) }),
            task_id: bound.map(|t| t.id.clone()).unwrap_or_default(),
            pane: seat.to_string(),
            workspace: workspace_id.to_string(),
            state: a.state.as_str().to_string(),
            state_typed: a.state,
            turning: a.state.turn_in_flight(),
            needs_you,
            seat: seat_of_project.clone(),
            said: sentence(&w.said, seat, bound.map(|t| t.id.as_str())),
            waiting,
            wait,
            members_waiting: 0,
            // A leg's is when it opened: the row's claim, if any, is not its.
            held: (match (legged.get(seat), pieced.get(seat)) {
                (Some(s), _) => s.leg.started.as_deref(),
                (None, Some(s)) => s.piece.started.as_deref(),
                (None, None) => bound
                    .and_then(|t| w.claims.get(&t.id))
                    .and_then(|c| c.get("claimed_at"))
                    .and_then(|c| c.as_str()),
            })
                .filter(|c| !c.is_empty())
                .map(|c| util::since(c))
                .filter(|&secs| secs > 0)
                .map(util::duration_human),
            since: w.since.get(seat).cloned(),
            // Only a project the index knows, as the panel reads it: a
            // mandate on one that has since been removed points nowhere.
            direction: crate::cmd_mandate::from_map(&w.mandates, agent.as_deref(), Some(workspace_id))
                .filter(|p| w.index.get(p).is_some()),
            runs: runs(w, seat, seat_of_project.as_deref(), held, &ran_on, &legged, &pieced),
            answers_to,
            answers_to_empty,
            role,
            spawned_by,
            leg: leg.cloned(),
            unsent,
            queued: w.queued.get(seat).copied().unwrap_or(0),
            dropped,
            full_suite: w.full_suite.contains(seat),
            stuck,
            was_governor,
            closed_row: closed_row_of(&w.tasks, &w.bindings, &w.worked, seat),
        });
    }
    // A governor's count, once every row has its own reading — off the same
    // `answers_to` the row publishes, so the count and the tree agree.
    // Only the waits somebody owes: a member waiting on its own shell is not
    // one the governor has to answer (`wsp-214`).
    let waiting_under: Vec<String> = rows
        .iter()
        .filter(|r| r.waiting.as_ref().is_some_and(|w| w.owed()))
        .filter_map(|r| r.answers_to.clone())
        .collect();
    for r in rows.iter_mut() {
        if let Some(scope) = &r.seat {
            r.members_waiting = waiting_under.iter().filter(|s| *s == scope).count();
        }
    }
    rows.sort_by(|a, b| a.project.cmp(&b.project).then(a.pane.cmp(&b.pane)));
    rows
}

/// The first row or list `t` rests on (`after:`) that has not finished, and
/// its standing — `("pieces-trial", "planned: alone")` — for the `other_work`
/// wait (`wsp-552`). A row at review has landed its part; a list is finished
/// when it is done or cancelled. A name that is neither is not read as a wait.
fn rests_on(w: &Wip, t: &Task) -> Option<(String, String)> {
    t.after.iter().find_map(|o| {
        if let Some(row) = w.tasks.iter().find(|r| &r.id == o) {
            let s = crate::worklist::Settlement::of(row);
            return (!s.settled()).then(|| (o.clone(), s.word().to_string()));
        }
        let l = w.lists.iter().find(|l| &l.id == o)?;
        if l.is_cancelled() || l.status() == crate::model::WorklistStatus::Done {
            return None;
        }
        let word = if l.is_planned() { "planned" } else { l.status().as_str() };
        let standing = match l.alone == Some(true) {
            true => format!("{word}: alone"),
            false => word.to_string(),
        };
        Some((o.clone(), standing))
    })
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

/// Whom a seat answers to: the scope of the nearest seated governor on the
/// walk its role starts, or — when nobody on that walk is seated — `None`
/// beside the scope the walk starts at (`wsp-247`). wsp walks it once here, so
/// the chrome hangs each seat under the governor this names and derives
/// nothing itself (compound `docs/ux-design.md` §18 H-2).
///
/// The walks are the ones the questions already take, so a row is drawn under
/// the seat its `wsp ask` would reach:
/// - a **member**, [`crate::governors::seat_for`] over the running list it is in and
///   its project's ancestors — what [`cmd_govern::answering_seat`] asks;
/// - a **verifier**, its member's, since it hangs beside the member it reads;
/// - a **leg**, its row's, since it works the row;
/// - a **barrier check**, its list's, falling through to the project its
///   group's work is in;
/// - a **list's governor**, the governor of the project the list stands in
///   ([`crate::governors::seat_above`] from its home, `wsp-263`);
/// - a **project's governor**, nobody: the census is a tree of lists under
///   projects, and a project custodian is a root of it, so both halves are
///   `None`;
/// - a **helper**, its row's walk if it holds one and its project's if not —
///   where its `wsp ask` goes, which is not always who spawned it: a list's
///   governor's helper on no row answers to its project's governor, and its
///   `spawned_by` says the list's, two facts the chrome reads apart;
/// - any **other seat**, its project's walk.
fn answers_to(
    w: &Wip,
    role: &Role,
    seat: &str,
    project: Option<&str>,
    running: &crate::worklist::Running,
    homes: &BTreeMap<String, String>,
) -> (Option<String>, Option<String>) {
    let task_walk = |id: &str| match w.tasks.iter().find(|t| t.id == id) {
        Some(t) => (running.list_for(t), t.project.as_deref(), Some(t)),
        None => (None, None, None),
    };
    let (list, project, task) = match role {
        // The primary interface answers to the person, which is `null` with
        // nobody named as the walk's start: it is on no walk at all.
        Role::Governor { list: false, .. } | Role::Primary => return (None, None),
        Role::Governor { list: true, id } => {
            let home = homes.get(id).map(String::as_str);
            return match crate::governors::seat_above(&w.governors, &w.index, id, home) {
                Some(s) => (Some(s.scope), None),
                None => (None, home.map(str::to_string)),
            };
        }
        Role::Member { task, .. }
        | Role::Verifier { member: task, .. }
        | Role::Leg { task, .. }
        | Role::Piece { task, .. }
        | Role::Helper { task: Some(task), .. } => {
            task_walk(task)
        }
        Role::Check { list, .. } => (Some(list.as_str()), w.checking.get(seat).and_then(|(_, _, p)| p.as_deref()), None),
        Role::Agent | Role::Helper { task: None, .. } => (None, project, None),
    };
    // A row's walk is the task's (`wsp-216`): the seat that started it comes
    // before its project's, as its `wsp ask` and its review go.
    let found = match task {
        Some(t) => cmd_govern::seat_for_task(&w.governors, &w.index, list, t, project),
        None => crate::governors::seat_for(&w.governors, &w.index, list, project),
    };
    match found {
        // A post wsp is filling is where the row's ask waits, and it is empty:
        // the walk's start with nobody in it (`wsp-216`).
        Some(s) if s.vacant => (None, Some(s.scope)),
        Some(s) => (Some(s.scope), None),
        None => (None, list.or(project).map(str::to_string)),
    }
}

/// `wip --json`'s `attention`: the items that are no mirrored entity (seats),
/// sorted by [`crate::classify`] on this read into `needs_you` and `follow_up`
/// (`wsp-096`). Groups and asks carry the same answer as their own `attention`
/// field in the mirror. Each entry is an object, `kind` and its id beside the
/// classifier's `attention`, `text`, and `waiting_on` or `due`, and `action`
/// where the classifier gives one (`wsp-097`: no seat yet; a seat held on a
/// dialog is acted on through that question). No seat is follow-up yet: one waiting on
/// a named other party is `wsp-095`'s third sub-task.
///
/// And the finished rows (`wsp-105`), as the one item the mirror's rows fold
/// into: `kind` `finished`, `rows` their ids, and the item
/// [`crate::mirror::finished_item`] makes ("N finished rows to close" with the
/// group action, or one row alone with its Mark done).
fn wip_attention(w: &Wip, rows: &[WipRow]) -> serde_json::Value {
    let (mut needs_you, mut follow_up) = (Vec::new(), Vec::new());
    let finished = crate::mirror::finished_rows(&w.tasks, &w.lists);
    if let Some(a) = crate::mirror::finished_item(&finished) {
        let mut v = a.json();
        v["kind"] = "finished".into();
        v["rows"] = finished.keys().cloned().collect::<Vec<_>>().into();
        needs_you.push(v);
    }
    for r in rows {
        let why = r.waiting.as_ref().filter(|w| w.owed()).map(|w| w.sentence());
        let seat = crate::classify::Seat { needs_you: r.needs_you, task: &r.task_id, waiting: why.as_deref() };
        let Some(a) = crate::classify::of(crate::classify::Item::Seat(seat)) else { continue };
        let mut v = a.json();
        v["kind"] = "seat".into();
        v["pane"] = r.pane.as_str().into();
        v["task"] = r.task_id.as_str().into();
        match a.kind {
            crate::classify::Kind::NeedsYou => needs_you.push(v),
            crate::classify::Kind::FollowUp => follow_up.push(v),
        }
    }
    json!({ "needs_you": needs_you, "follow_up": follow_up })
}

fn wip_json(w: &Wip) -> serde_json::Value {
    let rows = wip_rows(w);
    let (blocked, in_review, inbox) = wip_queues(w);
    let agents: Vec<wsp_view::wip::Agent> = rows
        .iter()
        .map(|r| wsp_view::wip::Agent {
            project: r.project.clone(),
            task: r.task.clone(),
            task_id: r.task_id.clone(),
            pane: r.pane.clone(),
            workspace: r.workspace.clone(),
            state: r.state.clone(),
            turning: r.turning,
            needs_you: r.needs_you,
            seat: r.seat.clone(),
            said: r.said.clone(),
            held: r.held.clone(),
            since: r.since.clone(),
            direction: r.direction.clone(),
            waiting: r.waiting.as_ref().map(|w| w.json()),
            wait: r.wait.as_ref().map(|w| w.json()),
            members_waiting: r.members_waiting,
            kind: r.runs.kind.clone(),
            model: r.runs.model.clone(),
            effort: r.runs.effort.clone(),
            role: r.role.json(),
            answers_to: r.answers_to.clone(),
            answers_to_empty: r.answers_to_empty.clone(),
            spawned_by: r.spawned_by.clone(),
            unsent: r.unsent,
            queued: r.queued,
            dropped: r.dropped,
            full_suite: r.full_suite,
            stuck: r.stuck.map(|(k, secs)| json!({ "kind": k.key(), "word": k.word(), "secs": secs })),
            was_governor: r.was_governor.clone(),
            closed_row: r.closed_row.clone(),
            leg: r.leg.as_ref().map(|s| json!({
                "name": s.leg.name, "after": s.after, "index": s.index, "of": s.of,
            })),
            owed: r.waiting.as_ref().is_some_and(|w| w.owed()),
        })
        .collect();
    let head = wsp_view::wip::Head {
        attention: wip_attention(w, &rows),
        daemon: w.daemon.clone(),
        unreachable: w.silent.line.clone(),
        disk: w.disk.clone(),
        load: w.load.clone(),
        throttle: w.throttle.clone(),
        idle_memory: w.idle_memory.json(),
        blocked: blocked.iter().map(|t| t.json()).collect(),
        review: in_review.iter().map(|t| t.json()).collect(),
        inbox,
    };
    wsp_view::wip::wip_json(&agents, &head)
}

/// What the `REVIEW` line says to do about a row at `review` (`wsp-323`).
///
/// This is printed to every seat, and a governor or custodian reading `wsp done
/// <id>` here passed it on to a member as "finish with `wsp done`" — a verb
/// agents are denied by the decision of 2026-08-19, so the member stopped and
/// the row sat at `doing`. It says whose verb each is, and what a member does.
const REVIEW_VERBS: &str = "the person's: wsp done <id> · wsp reopen <id> — a member finishes with wsp review";

/// What the STATE column says of a seat whose prompt holds text nobody sent.
const UNSENT_WORD: &str = "typed, not sent";

/// `unreachable 4m` for a seat on a silent machine; the bare word where the
/// machine's last-heard time is not to hand.
fn silent_word(w: &Wip, seat: &str) -> String {
    w.silent.seats.get(seat).cloned().unwrap_or_else(|| "unreachable".to_string())
}
const DROPPED_WORD: &str = "order not received";

/// The note on a row whose seat ran the whole suite (`wsp-389`).
const FULL_SUITE_WORD: &str = "full suite in a member seat";

fn wip_lines(w: &Wip, p: &Paint, terse: bool) -> Vec<String> {
    let rows = wip_rows(w);
    let (blocked, in_review, inbox) = wip_queues(w);
    let needs = rows.iter().filter(|r| r.needs_you).count();
    let mut out = Vec::new();

    if rows.is_empty() {
        out.push(p.dim("no agents running"));
        for d in w.daemon.iter().chain(&w.silent.line).chain(&w.disk).chain(&w.load).chain(&w.idle_memory.warning()) {
            out.push(p.red(d));
        }
        out.extend(w.throttle.iter().map(|t| p.dim(t)));
        out.push(p.dim(&w.idle_memory.line()));
    } else {
        // Not `all busy`, which is what this said and was the same sentence
        // `doctor`'s `up, 12 agents` was saying on the night seven of
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
        let waiting = rows.iter().filter(|r| r.waiting.as_ref().is_some_and(|w| w.owed())).count();
        if waiting > 0 {
            head.push_str(&format!("  ·  {}", p.yellow(&format!("{waiting} waiting"))));
        }
        out.push(head);
        // The machine's daemon, and what its absence costs. `wsp-145`: a heading
        // saying six agents, five running, is a true sentence about work and
        // says nothing about the pass that decides whether a seat is ever woken
        // at all — which is the half that was missing for six days on a machine
        // that looked busy the entire time, and nobody asked why every wake
        // spool was still holding.
        //
        // Its own line rather than another `·` clause on the heading: it is a
        // fault and the heading is a census, and a heading that goes yellow
        // every hour trains its reader past the yellow.
        for d in w.daemon.iter().chain(&w.silent.line).chain(&w.disk).chain(&w.load).chain(&w.idle_memory.warning()) {
            out.push(p.red(d));
        }
        out.extend(w.throttle.iter().map(|t| p.dim(t)));
        out.push(p.dim(&w.idle_memory.line()));
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
        // A waiting seat says for how long, off `since` (`wsp-250`): the
        // stop it is waiting from, which `held` — the claim's age — is not.
        let waited = |r: &WipRow, w: &crate::waiting::Wait| match r.since.as_deref().map(util::since) {
            Some(secs) if secs > 0 => w.word().replacen("waiting", &format!("waiting {}", util::duration_human(secs)), 1),
            _ => w.word(),
        };
        // Wide enough for the longest word drawn, so a `waiting · wsp-process`
        // does not push the flag after it out of line with everybody else's.
        let sw = rows
            .iter()
            .map(|r| r.waiting.as_ref().map(|w| waited(r, w).chars().count()).unwrap_or(0))
            .max()
            .unwrap_or(0)
            .max(rows.iter().filter(|r| r.state == "unreachable").map(|r| silent_word(w, &r.pane).chars().count()).max().unwrap_or(0))
            .max(if rows.iter().any(|r| r.unsent) { UNSENT_WORD.len() } else { 8 })
            .max(if rows.iter().any(|r| r.dropped) { DROPPED_WORD.len() } else { 8 });
        for r in &rows {
            let state = match (&r.waiting, r.state.as_str()) {
                // Before the word, and in place of it: a waiting seat reads
                // `idle` on its screen, and `idle` is what got one ended.
                (Some(w), _) => p.yellow(&util::pad(&waited(r, w), sw)),
                (None, _) if r.unsent => p.yellow(&util::pad(UNSENT_WORD, sw)),
                (None, _) if r.dropped => p.yellow(&util::pad(DROPPED_WORD, sw)),
                // A far seat on a silent machine, drawn as a dead daemon is: red,
                // with how long since it was heard.
                (None, "unreachable") => p.red(&util::pad(&silent_word(w, &r.pane), sw)),
                (None, "working") => p.green(&util::pad("working", sw)),
                (None, "idle") => p.dim(&util::pad("idle", sw)),
                (None, other) => p.dim(&util::pad(other, sw)),
            };
            // A seat says so instead of saying it needs you. The two can never
            // both be true — [`cmd_govern::needs_a_person`] is what makes them
            // exclusive — and the column is the same width either way.
            let flag = match (&r.seat, r.needs_you) {
                (Some(proj), _) if r.members_waiting > 0 => format!(
                    "{} {}",
                    p.cyan(&format!("{} {}", glyph_seat(), cmd_govern::governor_of(proj))),
                    p.yellow(&format!("· {} waiting", r.members_waiting))
                ),
                (Some(proj), _) => p.cyan(&format!("{} {}", glyph_seat(), cmd_govern::governor_of(proj))),
                (None, true) => p.yellow("← needs you"),
                // The row's own line (`docs/legs.md`, *Seen*): a leg holds no
                // claim, so the count of its row's legs is said here.
                (None, false) if r.leg.is_some() => r.leg.as_ref().map(|s| p.dim(&crate::legs::tally(s))).unwrap_or_default(),
                // And a piece's: the count of its row's pieces, and the batch.
                (None, false) if matches!(r.role, Role::Piece { .. }) => crate::pieces::seated(&w.tasks)
                    .get(&r.pane)
                    .map(|s| p.dim(&crate::pieces::tally(s)))
                    .unwrap_or_default(),
                // A gone seat that is still drawn is drawn because it holds
                // something, and says what: the reconciler's to repair, not a
                // row nobody swept (`wsp-169`).
                (None, false) => match r.state_typed == State::Gone {
                    true => match (&r.was_governor, holding(&w.tasks, &w.bindings, &w.claims, &w.agents_held, &r.pane)) {
                        (Some(scope), _) => p.dim(&format!("was {}", cmd_govern::governor_of(scope))),
                        (None, held) => held.map(|t| p.dim(&format!("holds {t}"))).unwrap_or_default(),
                    },
                    false => String::new(),
                },
            };
            // A seat still up on a row that was closed under it: stands out, and
            // names the verb that ends it (`wsp-582`).
            let flag = match &r.closed_row {
                Some(row) => format!("{flag} {}", p.yellow(&format!("· on a closed row {row} — wsp despawn {row}"))).trim_start().to_string(),
                None => flag,
            };
            // What the agent said it is doing, in place of what the work is
            // called, when it has said something about this work — the trade a
            // pane label has always made, and on a seat no host draws
            // this is the only place a person reading `wip` sees it.
            let doing = r.said.as_deref().unwrap_or(&r.task);
            let flag = match r.full_suite {
                true => format!("{flag} {}", p.yellow(&format!("· {FULL_SUITE_WORD}"))).trim_start().to_string(),
                false => flag,
            };
            // What is waiting for this seat's next idle (`wsp-659`): a tell sent
            // to it mid-turn, queued rather than refused.
            let flag = match r.queued {
                0 => flag,
                n => format!("{flag} {}", p.yellow(&format!("· {n} {} queued", if n == 1 { "tell" } else { "tells" })))
                    .trim_start()
                    .to_string(),
            };
            let flag = match r.stuck {
                Some((kind, secs)) => {
                    format!("{flag} {}", p.red(&format!("· {} {}", kind.word(), util::duration_human(secs)))).trim_start().to_string()
                }
                None => flag,
            };
            out.push(format!(
                "{}  {}  {}  {} {}",
                util::pad(&r.project, pw),
                util::pad(&util::truncate(doing, tw), tw),
                p.dim(&util::pad(&r.pane, 7)),
                state,
                flag
            ));
            // The seat's wait in one line, under it (`wsp-552`): which delay
            // it is and since when, without opening the pane. A turn in
            // flight already says `working`, so it gets none; nor does a seat
            // whose wait is its stuck call, which the flag above already says.
            let said_above = |sw: &crate::waiting::SeatWait| match sw.kind {
                crate::waiting::Kind::Working => true,
                crate::waiting::Kind::CannotBeTold => r.stuck.is_some(),
                _ => false,
            };
            if let Some(sw) = r.wait.as_ref().filter(|sw| !said_above(sw) && !terse) {
                out.push(format!("{}  {}", " ".repeat(pw), p.dim(&format!("↳ {}", sw.line))));
            }
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
                if let Some(since) = crate::cycle::blocked_since(t) {
                    let quiet = util::epoch_secs() - since;
                    let flagged = quiet >= crate::cycle::BLOCKED_ESCALATES_AFTER;
                    out.push(format!(
                        "      {}",
                        p.dim(&format!("blocked {}{}", util::duration_human(quiet), if flagged { " — flagged" } else { "" }))
                    ));
                }
            }
        }
    }

    // Work an agent has finished with. `review` is the agent's terminal verb —
    // it stops there and says so, and only a person says `done` — so this is
    // the list of things waiting on you rather than on anybody working.
    if !in_review.is_empty() {
        out.push(String::new());
        out.push(format!("{}  {}   {}", p.yellow(&util::pad("REVIEW", 8)), in_review.len(), p.dim(REVIEW_VERBS)));
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

/// What a non-compound pane shows now, as text. `None`: no such backend reads
/// a screen since `wsp-120`, and a compound seat is read through
/// `Compound::read_screen` by the callers that know it is one.
pub(crate) fn read_pane_text(_pane: &str) -> Option<String> {
    None
}

/// `wsp peek [<target>]` — read what is actually on a pane.
///
/// Every interface bug on 2026-08-15 cost a round trip through a person's eyes:
/// a key that did nothing because the binary was stale, a cursor scrolled off
/// the pane, a closed column still showing a shell prompt. In each case the
/// pane was displaying the answer and the agent that wrote the code could not
/// see it, so the loop was guess, build, install, ask, wait.
///
/// The backend can read the characters on a seat. What was missing is a way to
/// name the pane you mean.
/// Nobody reaches for it under pressure if reaching for it starts with listing
/// every pane on the machine and picking through the JSON.
///
/// So the target resolves the way everything else in wsp resolves: by what you
/// call the thing, not by the id the backend filed it under.
/// What a needle named, once it has been read against the panes reported.
///
/// Reading the pane is the backend's and cannot be anything else. *Naming* it
/// is ours, and it is the half with rules in it — which label, preferring this
/// workspace, falling back to any; a pane id only if a pane wears it; a task
/// only if the store knows it. Those rules were inside `peek`, between a
/// socket check and a screen read, so none of them could be asked about
/// without a live host and two panels open in different workspaces.
pub(crate) enum Peeked {
    /// A pane to read, and what to call it in the header.
    At(String, String),
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

/// Read a needle against the panes reported.
///
/// `here` is the workspace to prefer — the caller's, or whatever `--workspace`
/// said. A label is preferred within it and accepted outside it, because there
/// is a panel in every workspace and the one you mean is almost always yours,
/// but peeking at another workspace's is a real thing to want.
///
/// The task lookup arrives as a closure rather than a value because it is only
/// reached by the last arm. `find_task("")` matches every open task by suffix,
/// so resolving it up front to hand in would be a fuzzy search over the whole
/// store on every `wsp peek` — the commonest call, which names no task at all.
pub(crate) fn peek_target(
    panes: &[Pane],
    here: Option<&str>,
    me: Option<&str>,
    needle: &str,
    seated: impl Fn(&str) -> bool,
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
        // The common case, and the reason there is a default at all: "what
        // does the sidebar look like right now".
        "" | "panel" => named(
            mine(crate::panel::PANEL_LABEL),
            "the panel",
            "no panel in this workspace — run `wsp panel` in a pane",
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
        // A pane id, given as a host writes them.
        n if n.contains(':') && panes.iter().any(|p| p.pane_id == n) => {
            Peeked::At(n.to_string(), format!("pane {n}"))
        }
        // A seat compound minted (`cpd-N`), which no host's pane list will
        // ever contain and which has no colon to be taken for
        // one by. Read by asking who holds it, not by its spelling (`wsp-156`).
        n if seated(n) => Peeked::At(n.to_string(), format!("seat {n}")),
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
    // Not gated on a host being up: `peek_target` tolerates an empty pane
    // list, and `Peeked::At` can name a `cpd-` seat this process resolves
    // through the store.
    let here = args.get("workspace");
    let panes: Vec<Pane> = Vec::new();
    let needle = args.rest.first().cloned().unwrap_or_default();

    let (pane, what) = match peek_target(
        &panes,
        here.as_deref(),
        None,
        &needle,
        compound_holds,
        |n| {
            store.find_task(n).map(|t| Holder {
                panes: store.panes_for_task(&t.id),
                id: t.id,
                title: t.title,
            })
        },
    ) {
        Peeked::At(pane, what) => (pane, what),
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

    // Which backend holds this pane, asked rather than assumed
    // (`compound-091`'s fold). `wsp peek` used to read every pane through one
    // host unconditionally, so a `cpd-` seat read back as unreadable — the
    // seat's id already said which backend it was on.
    //
    // A targeted `socket_of` rather than a full `census()` scan: the latter
    // reads every compound seat's record to answer a question about one, and
    // `read_screen` below reads this seat's record again regardless — no
    // sense paying for the whole directory to answer "is it this one".
    let compound = crate::place_compound::Compound::new();
    let compound_seat = crate::place::Seat::new(&pane);
    let on_compound = compound.socket_of(&compound_seat).is_some();

    let (text, truncated) = if on_compound {
        // No scrollback here: `compound-render screen` (`compound-sup` before
        // the pty/renderer split, `compound-136`) is one frame, so `--source` and
        // `--lines` — a reach into what has scrolled past — have nothing to
        // ask for on this backend.
        match compound.read_screen(&compound_seat) {
            Ok(t) => (t, false),
            Err(e) => {
                eprintln!("wsp: compound would not read {pane}: {e}");
                return 1;
            }
        }
    } else {
        eprintln!("wsp: {pane} is not a compound seat, and nothing else here reads a screen");
        return 1;
    };
    let text = text.as_str();

    if args.json() {
        println!("{}", serde_json::to_string_pretty(&json!({
            "pane": pane, "what": what, "source": source, "text": text,
            "truncated": truncated,
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
        let out = wsp_base::calls::Command::new("git")
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

/// What a path left uncommitted in the store says about the writer that left
/// it, where the path can tell: a task file is a log line (a seat's release,
/// a landing, an ending) and a worklist file is a check's state (an attempt
/// marked `ended`). Anything else is named by its path alone.
fn uncommitted_writer(path: &str) -> &'static str {
    match path.split('/').next().unwrap_or("") {
        "tasks" => "a log line on a task: a seat's release, an ending or a landing",
        "worklists" => "a worklist: an attempt's end or a check's state",
        _ => "a store write",
    }
}

/// `doctor`'s warning for files in the store that are written and not
/// committed (`wsp-433`): wsp commits what it writes, so each one is a writer
/// that skipped it. Says how many, the first three, and who writes such files.
fn uncommitted_warning(loose: &[String]) -> Option<String> {
    if loose.is_empty() {
        return None;
    }
    let named: Vec<&str> = loose.iter().take(3).map(|s| s.as_str()).collect();
    let rest = match loose.len() > named.len() {
        true => format!(" and {} more", loose.len() - named.len()),
        false => String::new(),
    };
    let mut writers: Vec<&str> = loose.iter().map(|p| uncommitted_writer(p)).collect();
    writers.sort();
    writers.dedup();
    Some(format!(
        "{} uncommitted in the store: {}{rest} — wsp commits what it writes, so a writer skipped it ({}); commit them by hand",
        loose.len(),
        named.join(", "),
        writers.join("; ")
    ))
}

/// What is sitting in the store that no command wrote.
///
/// Every `wsp` command commits the files it writes, so whatever is left is a
/// hand edit — `agents.md` is the usual one, a hook script the other. That
/// used to be invisible because the next command along swept it into its own
/// commit; now it waits, correctly, for the person who made it, and the only
/// thing missing is anybody being told.
fn store_uncommitted(root: &std::path::Path) -> Vec<String> {
    let Ok(out) = wsp_base::calls::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["status", "--porcelain"])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
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

/// Every seat on this machine, from every backend that will answer.
///
/// **The one fleet reading, and it exists because there were three
/// (`wsp-119`).** `wip`, the panel and the attention pass each built a census,
/// and the one that judged a *binding* — [`bound_state`], and with it `doctor`
/// and the `agent-gone` signal — built it out of one backend's pane listings
/// and nothing else, so on the machine `compound-112`'s flip makes the
/// ordinary one, that reading had no answer for every seat in the fleet and
/// said so by reporting nothing.
///
/// Three readers of one question is already two too many; three *definitions*
/// of it is how the exception for a seat nobody is watching got quietly lost
/// from one of them. So the fold lives here, once.
///
/// A backend that said nothing is a [`crate::place::Census::silent`] entry and
/// not an absence: `unheard()` is how `wsp watch` tells a governor which source
/// it is blind through, and dropping the entry would have it report blindness
/// nowhere on a machine that is blind everywhere.
pub(crate) fn fleet_census() -> crate::place::Census {
    match crate::place_compound::Compound::new().census() {
        Ok(compound) => compound,
        Err(e) => crate::place::Census::silent("compound", e),
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
/// all seven and said `up, 12 agents`. Nothing had been written to a task
/// in hours. The reading that separates them was on the socket the whole time
/// (`place::State::turn_in_flight`), and this asks it.
///
/// **[`Bound::Emptied`] is the third, and it is this function being asked one
/// backend's question and handed a compound seat.** `wsp-119`. This took two
/// `Pane` listings, so a seat that backend did not list — every seat
/// `compound-112`'s flip opens, which after it is every seat on the ordinary
/// machine — missed both arms and fell to [`may_reap`], which a compound seat
/// never passes: it names no machine, so the answer is [`Bound::Unheard`]
/// and the check concludes it knows nothing about a seat it could have read in
/// full. The failure was not a threshold and no threshold would have fixed it.
/// **A dead compound seat raised nothing**: its pid had exited, its claim and
/// its worktree were still held, and the one reading that could have seen that
/// was another backend's listing with nothing in it.
///
/// So the rows come from the port now, from the same folded [`crate::place::Census`]
/// `wip` builds, and this function is a reading of *a seat* rather than of one
/// backend's pane. That is the whole of the repair, and it is two defects rather
/// than one: a dead compound seat raised nothing, and the readers that judged
/// a stopped seat were reading two different sources — `wip` and the panel off
/// the census, this and `doctor` off another backend — so nothing stopped a signal and
/// the census beside it from disagreeing about the same seat in the same
/// second.
pub(crate) enum Bound {
    /// A turn is in flight. This, and only this, is work happening.
    Turning,
    /// An agent is in the seat and no turn is running. Finished, stopped, or
    /// waiting on a person — see [`State::turn_in_flight`] for why this does not
    /// try to say which, and [`quiet_note`] for what makes it worth reporting.
    Quiet(State),
    /// The seat is still there and its agent is not: a pane listed with
    /// nothing in it, or a compound seat whose pid has exited. `despawn`, then
    /// decide.
    Emptied,
    /// No source lists it, and the machine that could have said something did.
    Gone,
    /// Nothing that could have listed this seat said anything, so nothing is
    /// known. Never reaped on, never reported.
    Unheard,
}

pub(crate) fn bound_state(
    seat: &str,
    rows: &[crate::place::Seated],
    answered: &std::collections::BTreeMap<&str, usize>,
) -> Bound {
    // The census's own reading, unqualified: a reading of *whether work is
    // happening* must not be qualified by readiness, and the census's own
    // qualification is the one that has to stand, so that this and `wip`
    // cannot answer differently about one seat.
    match rows.iter().find(|r| r.seat.as_str() == seat) {
        Some(r) => match r.state {
            // A seat on a silent machine is held, not missing: nothing is known
            // about it until the link is back, and nothing is reaped on it.
            State::Unreachable => Bound::Unheard,
            s if s.turn_in_flight() => Bound::Turning,
            s if s.is_running() => Bound::Quiet(s),
            // A seat that is still there with nothing running in it. A pane with
            // no agent in it and a compound seat whose pid has exited are the
            // same fact reached two ways, and `State::Gone`'s own doc is the
            // second of them — a listing cannot answer it, which is why this arm reads the agent as well as the state: a row whose
            // state nobody can vouch for but which names an agent in it is an
            // agent we cannot see, not an empty seat, and the two want opposite
            // handling.
            _ if r.agent.kind.is_empty() || matches!(r.state, crate::place::State::Gone) => {
                Bound::Emptied
            }
            // An agent in a seat that is not turning and not visibly running:
            // `Quiet`, and `quiet_note` has nothing to say about a state this
            // far from a fact.
            _ => Bound::Quiet(r.state),
        },
        None => match may_reap(answered, seat) {
            true => Bound::Gone,
            false => Bound::Unheard,
        },
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

/// What `doctor` says about the seats, and about the bindings that outlived them.
///
/// `tasks` is here for one reason: an agent's last turn is a fact about a seat
/// and its last *output* is a fact about the store, and the check this function
/// gained needs both in the same place. See [`quiet_note`].
///
/// **`seats` rather than one backend's listing (`wsp-119`).** The binding loop
/// used to run only where that one backend was up, which left the ordinary
/// machine — the one `compound-112`'s flip leaves with every seat on it a
/// compound seat — with a `doctor` that reported no problem about any of them,
/// on the day six of seven agents were killed by a limit. The census is asked
/// instead, and it answers for every backend wsp can spawn onto.
fn seat_health(
    seats: &[crate::place::Seated],
    bindings: &std::collections::BTreeMap<String, serde_json::Value>,
    tasks: &[Task],
    problems: &mut Vec<String>,
    notes: &mut Vec<String>,
) {
    // No machine answers for a seat the census does not list, so nothing may
    // be judged gone: a binding no row names reads [`Bound::Unheard`].
    let answered = answered_by_machine(std::iter::empty());
    // The sweep over every binding, judged on the census.
    let (mut emptied, mut gone) = (Vec::new(), Vec::new());
    let (mut quiet, mut turning) = (Vec::new(), 0usize);
    for (pane, b) in bindings {
        // Named by the work rather than by the seat, because the work is what
        // the reader has to decide about and what the verb underneath takes.
        let task = b.get("task_id").and_then(|x| x.as_str()).unwrap_or("");
        let named = match task.is_empty() {
            true => pane.clone(),
            false => format!("{task} ({pane})"),
        };
        match bound_state(pane, seats, &answered) {
            Bound::Emptied => emptied.push(named),
            Bound::Gone => gone.push(named),
            Bound::Turning => turning += 1,
            Bound::Quiet(state) => {
                // The later of the two, because either one moving is the work
                // advancing: `started_at` covers an agent put on a task that
                // already had a long history, and `updated` covers everything
                // after that.
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
            "{} seat(s) alive with the agent gone — {} — claim and worktree \
             still held. `wsp despawn <id>` \
             gives one up; `wsp spawn <id>` puts a fresh agent on it",
            emptied.len(),
            few(&emptied)
        ));
    }
    if !gone.is_empty() {
        notes.push(format!(
            "{} binding(s) on seats no backend lists any more — `wsp release <id>` ends one",
            gone.len()
        ));
    }
    // A problem rather than a note, which is the whole point of the task this
    // came from: it was a note-shaped failure — everything reporting healthy —
    // that cost a night. The conversation in each of these is intact, so
    // `wsp tell` is the first thing to try and `wsp despawn` the last.
    problems.extend(quiet.iter().cloned());
    let _ = turning;
}

/// What `doctor` says about a reference that names nothing in the live store.
///
/// `Err` is a problem and `Ok` is a note, so a caller cannot file one under the
/// wrong heading by accident — which is the whole of the distinction being
/// drawn here.
///
/// One function for two kinds because it is one judgement made twice: **a
/// record that was retired is not a record that is missing.** Reporting
/// `unknown parent wsp-020` about a file sitting in `archive/tasks/2026-08/`
/// is a false statement, and it is a false statement made by the one tool
/// somebody runs *to tell a broken store from a healthy one* — so it sends
/// them hunting for a corruption that is not there, and it does it under a ✗.
/// `worklist-043`: the same blindness `Store::resolve_task` had, one surface
/// along, and the reason enumerating the readers came before fixing the one
/// that was found.
///
/// The problem sentence is unchanged from before the archive was consulted. An
/// id nothing ever answered to is still exactly as broken as it always was, and
/// what this adds is only the ability to stop saying it about the other case.
fn dangling(subject: &str, kind: &str, name: &str, filed: bool) -> Result<String, String> {
    match filed {
        true => Ok(format!("task {subject} references {kind} `{name}`, which is archived rather than missing")),
        false => Err(format!("task {subject} references unknown {kind} `{name}`")),
    }
}

pub fn doctor(store: &Store, args: &Args) -> i32 {
    let mut problems: Vec<String> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    if !store.exists() {
        problems.push(format!("no store at {} — run `wsp init`", util::contract(&store.root)));
    } else {
        notes.push(format!("store {}", util::contract(&store.root)));
    }
    if !store.root.join(".git").exists() {
        notes.push("store is not a git repo (history disabled)".into());
    } else {
        if let Some(w) = uncommitted_warning(&store_uncommitted(&store.root)) {
            warnings.push(w);
        }
        // Only a store git cannot commit in has anything to repair: a store
        // whose commits work (global identity, signing, environment) is left
        // as it is, and `--fix` and `--fix-identity` both leave it so.
        if let Some(fault) = store.git_commit_fault() {
            if args.has("fix") || args.has("fix-identity") {
                match store.git_ensure_identity() {
                    Ok(filled) if filled.is_empty() => problems.push(format!("store git: {fault}; nothing the repo can set repairs it")),
                    Ok(filled) => notes.push(format!("store git: {fault}; set repo-local {} (nothing else changed)", filled.join(", "))),
                    Err(e) => problems.push(format!("store git: {fault}; could not repair it: {e}")),
                }
            } else {
                problems.push(format!(
                    "store git cannot commit: {fault} — every store write fails to commit; `wsp doctor --fix-identity` sets the missing repo-local user.name/user.email and, only if signing is what fails, commit.gpgsign=false (repo-local, never the global config)"
                ));
            }
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

    // A piece id reads `<row>-<n>`, so a project named `<x>-<digits>` would be
    // read as a row. Refused at `project add` and `code`; named here for one
    // that was made before.
    problems.extend(crate::store::piece_ambiguous_projects(&index.projects));

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
    // And the branch side of the same question, read once for the same reason.
    // `stale` walks directories, so everything a removed tree left behind in
    // `refs/heads` has been invisible to every reader wsp has.
    let whose = crate::cmd_checkout::naming(store);
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
                let seated = !store.panes_for_task(crate::leg_tree::row_of(&s.task)).is_empty();
                notes.push(format!(
                    "{}: the tree for {} is finished with — {} — {}",
                    util::contract(&root),
                    s.task,
                    s.note,
                    s.why.fix(&s.task, seated)
                ));
            }
            // The refs a removed tree left behind. Two findings and they are
            // not equally interesting: a branch whose work is on the trunk is
            // a name and nothing else, so however many there are they get one
            // dim line — `doctor` is read often and seven lines of nothing is
            // how a report trains its reader past the line that matters. A
            // branch still holding commits is *work* nothing else mentions,
            // and it gets a line each.
            let strays = crate::cmd_checkout::strays(&root, &whose);
            let (holding, litter): (Vec<_>, Vec<_>) = strays.iter().partition(|s| s.commits > 0);
            if !litter.is_empty() {
                notes.push(format!(
                    "{}: {} whose trees have gone and whose work is on the trunk — `wsp checkout --sweep`",
                    util::contract(&root),
                    crate::cmd_checkout::n_branches(litter.len())
                ));
            }
            for s in holding {
                // A problem rather than a note, and only for the renumbered
                // one: work under a task's own id is a `wsp checkout` away and
                // nothing is at risk, but a former id is not reachable through
                // wsp at all — which is the hazard `cmd_checkout::remove` names
                // in its own docs and says it cannot repair from there.
                let line = format!("{}: {}", util::contract(&root), crate::cmd_checkout::stranded_line(s));
                match s.whose {
                    crate::cmd_checkout::Whose::Former(_) => problems.push(line),
                    _ => notes.push(line),
                }
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

    // A repository that has stopped packing: a crashed detached gc leaves its
    // lock behind and every later auto-gc dies on it (`wsp-380`). The store
    // is looked at with the project roots.
    let mut gc_roots = vec![store.root.clone()];
    gc_roots.extend(seen.iter().cloned());
    crate::gc::health(&gc_roots, args.has("fix"), &mut problems, &mut notes);

    // And the binary those trees were built into, which is the one file none
    // of the checks above can see: the stamp travels in the bytes, so it is
    // asked of the installed wsp rather than worked out from a tree.
    crate::cmd_install::health(&seen, &mut notes);
    crate::cmd_role::health(&mut warnings);

    // Both "unknown" checks below ask the archive before they say the word —
    // see [`dangling`] for why that is a correctness fix and not a politeness.
    let archived_ids = store.archived_ids();
    let archived_projects: Vec<String> =
        store.archived_projects().into_iter().map(|(p, _)| p.id).collect();
    for t in &tasks {
        if let Some(proj) = &t.project {
            if index.get(proj).is_none() {
                match dangling(&t.id, "project", proj, archived_projects.contains(proj)) {
                    Ok(note) => notes.push(note),
                    Err(problem) => problems.push(problem),
                }
            }
        }
        if t.title.trim().is_empty() {
            problems.push(format!("task {} has an empty title", t.id));
        }
        if let Some(parent) = &t.parent {
            if !tasks.iter().any(|x| &x.id == parent) {
                match dangling(&t.id, "parent", parent, archived_ids.contains(parent)) {
                    Ok(note) => notes.push(note),
                    Err(problem) => problems.push(problem),
                }
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

    // The same question about the standing hands: a record that names a subject
    // which is gone, drawn on every panel and named nowhere else. `archived`
    // rides in from above so the check does not re-walk the archive to say
    // retired rather than unknown.
    crate::message::health(store, &tasks, &archived, &mut problems);

    section_damage(store, args, &tasks, &index.projects, &mut problems, &mut notes);

    // The census every backend answers for, and the same one `wip` builds — so
    // a binding this reports on and a row a person is looking at cannot be two
    // readings of one seat. `wsp-119`; see [`seat_health`].
    let seats: Vec<crate::place::Seated> = fleet_census().seats().cloned().collect();
    seat_health(&seats, &bindings, &tasks, &mut problems, &mut notes);
    // Whether the machine has the one daemon it should have, said in the same
    // words as `wip`, a brief's seat line and `wsp watch --status` say it —
    // through `a_backend_answered`, the one gate all four ask. A gate that is
    // asked two ways is a gate that is really two gates.
    crate::daemon::health(
        store.daemon_holder().map(|(pid, _)| pid),
        crate::daemon::running(&store.state).as_deref(),
        crate::daemon::a_backend_answered(),
        &mut problems,
        &mut notes,
    );
    crate::orders::health(store, &mut notes);
    crate::mirror::api_health(
        &store.state,
        crate::daemon::running(&store.state).as_deref(),
        &mut problems,
        &mut notes,
    );
    // And whether far machines have gone silent: their seats are held, not
    // gone, and say so in the daemon's own manner (`wsp-145`).
    notes.extend(crate::far::Silence::read(store).line);
    // And whether the disk the trees are on is running out (`wsp-327`), naming
    // the largest build output that could go.
    crate::disk::health(store, &mut problems, &mut notes);
    // And what the build throttle is doing, and whether the load warning is up.
    crate::throttle::health(store, &mut notes);
    // And whether anything that said it was reporting still is. The only check
    // here whose subject is a *reporter* rather than a piece of work, and the
    // only place in wsp that can tell a fleet with nothing to say from a
    // watcher that stopped saying it.
    crate::cmd_watch::health(store, &mut problems);
    // And whether the one thing that reports with nobody looking has anywhere
    // to report *to*. The pass is the fix for "nothing notices unattended" and
    // an empty `hooks/` would make it a fix that notices and tells no one.
    crate::attention::health(store, &mut notes);
    // And a level the pass is holding for a seat that has gone (`wsp-649`).
    warnings.extend(crate::attention::orphans(store).into_iter().map(|(_, line)| line));
    // And whether Claude Code itself is still telling wsp anything: a
    // partly-installed hook snippet looks configured — SessionStart fires, a
    // brief arrives — while every other turn boundary is silently invisible
    // (`compound-107`).
    crate::place_super::hook_snippet_health(&mut problems, &mut notes);
    // And pi's half of the same thing: the extension that is its hooks.
    crate::place_super::pi_extension_health(&mut problems, &mut notes);
    // And whether the compound wire agrees with itself: wsp's own hand-copied
    // WIRE_VERSION against what the installed compound-sup actually speaks,
    // and the host binary beside it against a build that only touched one of
    // the two — the two shapes `compound-026` was filed for.
    //
    // On a machine with a manager (`wsp install`, wsp-024) the local half is the
    // manager's own answer: it says hello with the token, its version is wsp's,
    // and compound-sup's wire is inside the range it names. Without a local
    // record there is no manager to ask, and the older check of compound-sup
    // stands in.
    if !crate::manager::health(
        &store.state,
        crate::manager::installed_sup().as_deref(),
        env!("CARGO_PKG_VERSION"),
        &mut problems,
        &mut notes,
    ) {
        crate::place_compound::wire_health(&mut problems, &mut notes);
    }
    // And the same wire, asked of every machine the seat drives: one line each
    // (`docs/remote-agents.md` section 2), so a far compound-sup that is behind
    // is said before a spawn is refused on it.
    crate::cmd_machine::health(store, &mut problems, &mut notes);

    if args.json() {
        println!("{}", json!({ "problems": problems, "warnings": warnings, "notes": notes }));
        return if problems.is_empty() { 0 } else { 1 };
    }

    let p = Paint::new();
    for n in &notes {
        println!("{} {}", p.dim("·"), n);
    }
    for w in &warnings {
        println!("{} {}", p.yellow("!"), w);
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
                crate::log::note(store, &mut t, "sections folded back under the prose they were written in (`wsp doctor --fix`)")
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

/// `wsp reconcile` — rebuild bindings from claims, and say what it did.
pub fn reconcile_verb(store: &Store, args: &crate::Args) -> i32 {
    let r = reconcile(store, args.has("reap"));
    println!("reconciled {} binding(s) from claims", r.bound);
    println!("named {} pane(s) after the task they hold", r.named);
    if args.has("reap") {
        println!("ended {} claim(s) whose workspace is gone", r.reaped);
        println!("emptied {} seat(s) whose agent is gone", r.stood_down);
        println!("forgot {} panel record(s) whose workspace is gone", r.forgotten);
    }
    0
}

/// The verbs this module answers to, each with the help it carries. `main` and
/// the help text are assembled from these; nothing in `main.rs` names them.
pub const VERBS: &[crate::verbs::Verb] = &[
    crate::verbs::Verb {
        names: &["claim"],
        run: claim,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Agents,
                order: 260,
                text: r##"  wsp claim <id>                    bind this pane to a task, leaving the last"##,
            },
        ],
    },
    crate::verbs::Verb {
        names: &["release"],
        run: release,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Agents,
                order: 430,
                text: r##"  wsp release                       unbind this pane, leaving whatever is in it"##,
            },
            crate::verbs::Help {
                section: crate::verbs::Section::Agents,
                order: 440,
                text: r##"  wsp release <id>                  …or end that task's claim, wherever it is
                                    held — including a claim no pane is under"##,
            },
        ],
    },
    crate::verbs::Verb {
        names: &["pin"],
        run: pin,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Agents,
                order: 450,
                text: r##"  wsp pin <proj> [-w ws]            pin a workspace to a project"##,
            },
            crate::verbs::Help {
                section: crate::verbs::Section::Agents,
                order: 460,
                text: r##"  wsp pin --top [-w ws]             pin it outside the tree entirely"##,
            },
        ],
    },
    crate::verbs::Verb {
        names: &["unpin"],
        run: unpin,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Agents,
                order: 470,
                text: r##"  wsp unpin [-w ws]                 take the pin off again"##,
            },
        ],
    },
    crate::verbs::Verb {
        names: &["where"],
        run: where_am_i,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Agents,
                order: 480,
                text: r##"  wsp where                         what project am I in, and why"##,
            },
        ],
    },
    crate::verbs::Verb {
        names: &["agent"],
        run: agent,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Agents,
                order: 490,
                text: r##"  wsp agent kinds [--json]          the agent kinds wsp has behaviour for — name,
                                    default, resume, models, efforts — for a menu
                                    that should not keep its own list. Any other
                                    kind is passed by --kind, for the backend"##,
            },
            crate::verbs::Help {
                section: crate::verbs::Section::Agents,
                order: 500,
                text: r##"  wsp agent pi-extension            the pi extension this binary carries, which
                                    `wsp install` writes into pi's extensions"##,
            },
        ],
    },
    crate::verbs::Verb {
        names: &["wip", "status"],
        run: wip,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Agents,
                order: 510,
                text: r##"  wsp wip                           everything in flight, with agents"##,
            },
        ],
    },
    crate::verbs::Verb {
        names: &["overlap"],
        run: overlap,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Agents,
                order: 610,
                text: r##"  wsp overlap                       who else is standing in this tree"##,
            },
        ],
    },
    crate::verbs::Verb {
        names: &["peek"],
        run: peek,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Agents,
                order: 690,
                text: r##"  wsp peek [panel|view|board|<task>] [--source recent] [--lines N]
                                    what is on that pane, or the frame the
                                    sidebar surface last drew; --source recent
                                    reaches back through what has scrolled past,
                                    for when the question is what happened
                                    rather than what is showing"##,
            },
        ],
    },
    crate::verbs::Verb {
        names: &["doctor"],
        run: doctor,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Plumbing,
                order: 150,
                text: r##"  wsp doctor [--fix | --fix-identity]
                                    integrity check. --fix-identity only repairs a
                                    store git that cannot commit: sets the
                                    missing repo-local user.name/user.email and,
                                    only if signing is what fails,
                                    commit.gpgsign=false (repo-local, never the
                                    global config). --fix does that too, and
                                    folds stray sections back in task bodies and
                                    clears stale gc state in the repos"##,
            },
        ],
    },
    crate::verbs::Verb {
        names: &["say"],
        run: say,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Plumbing,
                order: 160,
                text: r##"  wsp say "…" | - | --from FILE     say where you have got to, on your pane;
                                    `--clear` takes the label off again. Prose
                                    through a stream, the spelling every other
                                    prose verb takes — a bare `-` with nothing
                                    piped in is refused rather than worn"##,
            },
        ],
    },
    crate::verbs::Verb {
        names: &["tell"],
        run: tell,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Agents,
                order: 700,
                text: r##"  wsp tell <id> "…" | - | --from F  say something to the agent holding that
                                    task, without ending it — `-` reads the
                                    message from stdin, --from from a file. The
                                    repair for an agent whose turn stopped: the
                                    conversation is intact, and a respawn throws
                                    it away.
                                    The same sentence twice inside two minutes
                                    is read as a retry and refused; `--again`
                                    means it. A seat that cannot say whether it
                                    is at a prompt is refused rather than typed
                                    at, because the text would land in whatever
                                    dialog is holding the keyboard; look with
                                    `wsp peek`, then `--anyway` takes the
                                    decision yourself
                                    <id> may be an address: primary,
                                    governor:<list|project>, seat:<row>,
                                    verifier:<row>, check:<list>, list:<name>;
                                    `person` puts it on the person's list.
                                    The seat reads who spoke, and ends with a
                                    one-line footer naming the reply, `wsp tell
                                    <sender> --re <id> -`; `--no-reply` leaves
                                    it off, for direction that is owed nothing
                                    back. `--re <id>` marks this as the reply to
                                    that message and is refused for an unknown
                                    one
                                    `list:<name>` tells every live seat on the
                                    list once, each with its own id; `--log`
                                    shows it as one fan-out with its count
                                    `--log <address|id>` reads the tells to and
                                    from that post off the event log, replies
                                    under what they answer, a held tell shown
                                    held and then delivered; `--full` adds the
                                    text"##,
            },
        ],
    },
    crate::verbs::Verb {
        names: &["flag"],
        run: flag,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Plumbing,
                order: 170,
                text: r##"  wsp flag <id> ["why"]             raise a hand on a task — at the seat that
                                    governs it, or on every panel if there is none"##,
            },
            crate::verbs::Help {
                section: crate::verbs::Section::Plumbing,
                order: 180,
                text: r##"  wsp flag <id> --title T --body -  …with a card: a heading and a paragraph"##,
            },
            crate::verbs::Help {
                section: crate::verbs::Section::Plumbing,
                order: 190,
                text: r##"  wsp flag <id> --from FILE         …with that paragraph out of a file, the
                                    spelling every other prose verb takes"##,
            },
            crate::verbs::Help {
                section: crate::verbs::Section::Plumbing,
                order: 200,
                text: r##"  wsp flag <id> --ask claim         …and a question a keypress answers"##,
            },
            crate::verbs::Help {
                section: crate::verbs::Section::Plumbing,
                order: 220,
                text: r##"  wsp flag [--clear <id>] [--seen <id>] [--seat] [--person]
                                    what is raised, and whose it is; --seat
                                    narrows it to this seat's own, --person to
                                    the person's (no notice wsp wrote for a
                                    seat); --clear lowers one, and --seen puts
                                    the card away and leaves the hand up"##,
            },
        ],
    },
    crate::verbs::Verb {
        names: &["reconcile"],
        run: reconcile_verb,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Plumbing,
                order: 360,
                text: r##"  wsp reconcile [--reap]            rebuild bindings from claims, and rename;
                                    --reap ends claims whose workspace is gone"##,
            },
        ],
    },
];

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

    /// wsp-035: under `WSP_HOST=manager`, `wip` reads the daemon by the stamp
    /// it writes every tick, not by `cycle.log` — which an idle tick never
    /// writes, so a fleet with nothing to say read as a dead daemon.
    #[test]
    fn an_idle_daemon_that_ticks_is_not_called_dead_and_a_stale_tick_stamp_is() {
        let (_env, store) = scratch("daemon-stamp");
        let t0 = util::epoch_secs();
        // The one line the log gets, at the start; nothing after.
        std::fs::write(store.state.join("cycle.log"), format!("{} daemon up\n", util::now_iso())).unwrap();

        // No stamp yet — a daemon from before it — says nothing.
        assert_eq!(daemon_by_stamp(store.daemon_tick(), true, t0 + 3600), None);

        // Ten minutes of ticks with nothing to log: never not ticking.
        for now in (t0..=t0 + 600).step_by(20) {
            store.stamp_daemon_tick(now);
            assert_eq!(daemon_by_stamp(store.daemon_tick(), true, now + 19), None, "ticking at +{}s", now - t0);
        }
        assert_eq!(
            std::fs::read_to_string(store.state.join("cycle.log")).unwrap().lines().count(),
            1,
            "the log stayed quiet the whole time"
        );

        // A stamp older than the bound is a daemon that has stopped ticking.
        let last = t0 + 600;
        assert_eq!(daemon_by_stamp(store.daemon_tick(), true, last + DAEMON_QUIET_SECS), None, "at the bound");
        let said = daemon_by_stamp(store.daemon_tick(), true, last + DAEMON_QUIET_SECS + 1).expect("past the bound");
        assert!(said.contains("not ticking"), "{said}");
        assert!(!said.contains("cycle.log"), "{said}");

        // A machine whose manager did not answer is not judged at all.
        assert_eq!(daemon_by_stamp(store.daemon_tick(), false, last + 3600), None);
    }

    /// `wsp-523`: a fan-out to a list nobody knows is refused before anything is
    /// typed or logged.
    #[test]
    fn a_tell_to_an_unknown_list_is_refused_and_leaves_no_fan_out() {
        let (_env, store) = scratch("tell-list");
        let args = Args::synth("tell", &["list:nope", "hello"], &[]);
        assert_eq!(tell(&store, &args), 1);
        assert!(store.events_of("agent-told-list").is_empty());
        assert!(store.events_of("agent-told").is_empty());
    }

    /// `wsp-521`: the footer is on by default, off with `--no-reply`, and one
    /// line either way — the seat's argv refuses a control character.
    #[test]
    fn a_tell_ends_with_a_one_line_reply_footer_unless_told_not_to() {
        let said = dressed("governor:pieces", "m-1a2b", "run the check", true);
        assert_eq!(
            said,
            "governor:pieces says: run the check [from governor:pieces · reply: wsp tell governor:pieces --re m-1a2b -]"
        );
        assert!(!said.contains('\n') && !said.chars().any(char::is_control), "{said:?}");
        let bare = dressed("governor:pieces", "m-1a2b", "stop", false);
        assert_eq!(bare, "governor:pieces says: stop");
        assert!(!bare.contains("reply:"));
        // A sentence that ends in a newline does not leave the footer on its own line.
        assert!(!dressed("primary", "m-1", "go on\n", true).contains('\n'));
    }

    /// The sender is right from a governor's seat, from the primary, from a
    /// seat on a row, and from a shell.
    #[test]
    fn the_sender_is_an_address_from_a_seat_the_primary_and_a_shell() {
        let gov = |pane: &str| json!({ "workspace": "w1", "pane": pane });
        let governors: BTreeMap<String, Value> = [
            ("pieces".to_string(), gov("w1:p2")),
            (crate::governors::PRIMARY.to_string(), gov("w1:p1")),
        ]
        .into();
        let bindings: BTreeMap<String, Value> = [("cpd-9".to_string(), json!({ "task_id": "wsp-521" }))].into();
        let of = |pane: Option<&str>| crate::cmd_message::sender_of(&governors, &bindings, pane);
        assert_eq!(of(Some("w1:p2")), "governor:pieces");
        assert_eq!(of(Some("w1:p1")), "primary");
        assert_eq!(of(Some("cpd-9")), "seat:wsp-521");
        assert_eq!(of(Some("cpd-77")), "cpd-77", "a pane that holds nothing is named as it is");
        assert_eq!(of(None), "person");
        // And the footer's reply is an address `tell` itself reads.
        assert!(crate::address::parse(&of(Some("w1:p2"))).is_some());
        assert!(crate::address::parse(&of(Some("cpd-9"))).is_some());
    }

    /// `--re` is recorded on the `agent-told` event, and refused for an id nothing
    /// sent or raised — before anything is typed.
    #[test]
    fn re_is_recorded_and_an_unknown_id_is_refused() {
        let (_env, store) = scratch("tell-re");
        let args = crate::Args::parse(vec!["wsp".into(), "tell".into(), "--json".into()]);
        let first = Sent::new("wsp-491", "wsp-491", "cpd-1", "wsp-491", "first", &args).by("primary", None, true);
        assert_eq!(delivered(&store, Ok(Delivery::Started), &first), 0);
        let reply = Sent::new("primary", "primary", "cpd-2", "primary", "second", &args)
            .by("seat:wsp-491", Some(first.id.as_str()), false);
        assert_eq!(delivered(&store, Ok(Delivery::Started), &reply), 0);

        let told = store.events_of("agent-told");
        let last = told.last().unwrap();
        assert_eq!(last["re"], json!(first.id));
        assert_eq!(last["from"], json!("seat:wsp-491"));
        assert_eq!(last["reply"], json!(false));
        assert_eq!(told[0]["from"], json!("primary"));
        assert_eq!(told[0]["reply"], json!(true));

        assert_eq!(known_message(&store, &first.id), Ok(first.id.clone()));
        assert!(known_message(&store, "m-nothing").is_err());
        let args = Args::synth("tell", &["seat:wsp-491", "hello"], &[("re", "m-nothing")]);
        assert_eq!(tell(&store, &args), 1, "an unknown id is refused before the seat is looked for");
    }

    /// A reply to `person` is an item on their list and is typed into no pane.
    #[test]
    fn a_reply_to_the_person_becomes_an_item() {
        let (_env, store) = scratch("tell-person");
        let args = Args::synth("tell", &["person", "done, and it holds"], &[]);
        let me = Party::seat("pieces");
        let code = tell_the_person_as(&store, &args, "done, and it holds", None, me.clone(), Some("w1:p2".into()), "governor:pieces");
        assert_eq!(code, 0);
        let items = message::for_the_person(&store);
        assert_eq!(items.len(), 1, "{items:?}");
        assert!(items[0].text.contains("done, and it holds"));
        assert!(items[0].for_person);
        assert_eq!(store.events_of("agent-told").last().unwrap()["target"], json!("person"));
        // From a shell there is nobody to tell the person *from*.
        assert_eq!(tell_the_person_as(&store, &args, "hello", None, Party::Human, None, "person"), 1);
        assert_eq!(message::for_the_person(&store).len(), 1);
    }

    /// A direct `wsp tell` that the seat's supervisor acknowledged leaves the
    /// same cycle.log line the wake path writes (wsp-490); one that only
    /// started a turn leaves none.
    #[test]
    fn an_acknowledged_direct_tell_is_told_in_cycle_log() {
        let (_env, store) = scratch("acked-tell");
        // Built the way `tell` builds it: a task needle resolved through
        // `target`, whose display string is `id — title`.
        let task = crate::model::Task::new("wsp-490 probe: throwaway compound seat, reporting back", "wsp-491");
        store.save_task(&task).unwrap();
        let args = crate::Args::parse(vec!["wsp".into(), "tell".into(), "--json".into()]);
        let sent = |text: &str| {
            let t = store.find_task("wsp-491").unwrap();
            let what = format!("{} — {}", t.id, util::truncate(&t.title, 44));
            assert!(what.len() > 10, "the display string carries the title: {what}");
            Sent::new(&addressee_id(&store, "wsp-491", &what), &what, "cpd-1", "wsp-491", text, &args)
        };
        crate::store::SAID.with(|s| s.borrow_mut().clear());
        assert_eq!(delivered(&store, Ok(Delivery::Started), &sent("a")), 0);
        assert!(crate::store::SAID.with(|s| s.borrow().is_empty()), "a started turn says nothing here");
        assert_eq!(delivered(&store, Ok(Delivery::Acked), &sent("b")), 0);
        let said = crate::store::SAID.with(|s| s.borrow().clone());
        assert_eq!(said, vec!["told wsp-491: delivered, acknowledged by the seat".to_string()]);
        let file = std::fs::read_to_string(store.state_file("cycle.log")).unwrap();
        assert!(file.contains("told wsp-491: delivered, acknowledged by the seat"), "{file}");
    }

    /// A direct tell the seat's supervisor gave up on is in cycle.log as the
    /// wake path words it, and one nobody took is not a supervisor failure.
    #[test]
    fn a_failed_direct_tell_is_told_in_cycle_log() {
        let (_env, store) = scratch("failed-tell");
        let args = crate::Args::parse(vec!["wsp".into(), "tell".into(), "--json".into()]);
        let sent = Sent::new("wsp-491", "wsp-491 — probe", "cpd-1", "wsp-491", "hi", &args);
        crate::store::SAID.with(|s| s.borrow_mut().clear());
        assert_eq!(delivered(&store, Err(Refusal::Failed("cpd-1: no turn after 4 tries".into())), &sent), 1);
        let said = crate::store::SAID.with(|s| s.borrow().clone());
        assert_eq!(said, vec!["told wsp-491: delivery failed — cpd-1: no turn after 4 tries".to_string()]);
        let file = std::fs::read_to_string(store.state_file("cycle.log")).unwrap();
        assert!(file.contains("told wsp-491: delivery failed — cpd-1: no turn after 4 tries"), "{file}");
        crate::store::SAID.with(|s| s.borrow_mut().clear());
        assert_eq!(delivered(&store, Err(Refusal::NotTaken), &sent), 1);
        assert!(crate::store::SAID.with(|s| s.borrow().is_empty()));
    }

    /// The row's own line, `wsp add "-p" -p demo`: the first `-p` takes the
    /// second as its value, and the refusal has to say that rather than blame
    /// a project. Driven through the parser and through `add` itself, so a
    /// site that stopped calling `util::no_such` fails here, not in review.
    #[test]
    fn a_title_a_flag_ate_is_reported_as_a_flag_and_files_nothing() {
        let (_env, store) = scratch("dash-title");
        let mut demo = crate::model::Project::new("demo");
        demo.name = "demo".into();
        store.save_project(&demo).unwrap();
        let index = Index::new(store.projects());

        let argv: Vec<String> = ["add", "-p", "-p", "demo"].iter().map(|s| s.to_string()).collect();
        let args = crate::Args::parse(argv);
        let said = named_project(&args, &index).expect("-p was given").unwrap_err();
        assert!(said.contains("`--`"), "{said}");
        assert!(said.contains("looks like a flag"), "{said}");
        assert!(!said.contains("no such"), "must not blame the project: {said}");

        let rest = Args::synth("add", &["demo"], &[("project", "-p")]);
        assert_eq!(crate::cmd_task::add(&store, &rest), 1, "refused, not filed");
        assert!(store.tasks().is_empty(), "nothing was filed: {:?}", store.tasks());

        // A real typo is told what it always was.
        let typo = crate::Args::parse(["add", "x", "-p", "dmeo"].iter().map(|s| s.to_string()).collect());
        assert_eq!(named_project(&typo, &index).unwrap().unwrap_err(), "wsp: no such project `dmeo`");
    }

    /// A pin is a mandate with a different word on it (`compound-106`), and
    /// setting one is refused the same way: with no seat environment there
    /// is no seat this process is standing in, so there is nothing to attach
    /// a fresh agent id to.
    #[test]
    fn pin_refuses_to_set_from_outside_the_room() {
        let (_env, store) = scratch("pin-outside-room");
        store.save_project(&crate::model::Project::new("wsp")).unwrap();
        let args = Args::synth("pin", &["wsp"], &[("workspace", "w1")]);
        assert_eq!(pin(&store, &args), 2);
        assert!(store.pins().is_empty(), "a refused set writes nothing");
    }

    /// `wsp unpin` clears a pin a pre-migration writer left keyed on the
    /// workspace, with nobody ever having claimed the room — the dual clear
    /// `store::clear_pin` does needs no agent to find it.
    #[test]
    fn unpin_clears_a_legacy_workspace_keyed_pin() {
        let (_env, store) = scratch("unpin-legacy");
        store.set_pin("w1", "wsp");
        let args = Args::synth("unpin", &[], &[("workspace", "w1")]);
        assert_eq!(unpin(&store, &args), 0);
        assert!(store.pins().is_empty(), "the legacy key is gone, not left behind");
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

    /// **A raised hand goes into the seat's spool, and that is the second of the
    /// two verbs `wsp-146` moved.**
    ///
    /// A hand used to reach a governor only as a `flag` **level**, which
    /// `Line::disposition` spools rather than wakes — deliberately, because
    /// `hooks/on-attention-raised` already reaches a person for free and a hand
    /// is not worth 208k on its own. That argument is about a person with a panel
    /// in front of them, and an agent raising a hand to its governor is
    /// addressing a *seat*, which was never on the hook. So `wsp flag` writes
    /// into the spool itself, where `Class::Message` dispositions it to a wake,
    /// and the level still goes everywhere else it always did.
    ///
    /// Asserted on the record rather than on a receipt: what matters is that the
    /// words are *owed* by a post, which is a fact about the store, and that the
    /// record says the seat was handed it — which is what stops the pass
    /// announcing the same hand to the same seat a minute later.
    #[test]
    fn a_hand_raised_under_a_seat_is_owed_by_that_seat_rather_than_only_drawn() {
        let (_env, store) = scratch("flag-to-seat");
        a_task(&store, "wsp-001");
        // The governor record written directly rather than through `take`:
        // the record is what is under test, not the verb that writes it.
        store.set_governor(
            "wsp",
            json!({ "workspace": "w1", "pane": "w1:p1", "host": util::hostname(), "since": util::iso_at(1_000) }),
        );

        assert_eq!(raise_one(&store, &["wsp-001", "the store is corrupt"], &[]), 0);

        let watches = store.watches();
        let rec = watches.get(&crate::wake::key_for("wsp")).expect("the seat has a spool");
        let spool = crate::cmd_watch::Spool::of_json(rec.get("spool").unwrap_or(&Value::Null));
        assert_eq!(spool.depth(), 1, "the hand is owed by the seat, not only drawn on a panel");
        // The rendered words, read off the record rather than through
        // `Spooled`'s own fields: what is owed is a line a governor will read in
        // a composer, and this is that line.
        let said = rec["spool"][0]["text"].as_str().unwrap_or_default().to_string();
        assert!(
            said.contains("the store is corrupt"),
            "and it carries the words, because the level only ever carried the title: {said}"
        );
        assert!(
            crate::message::raised(&store)[0].handed_to("wsp"),
            "and the record knows the seat was handed it, so the pass does not announce it twice"
        );
    }

    /// **`wsp-272`: a notice the run writes is wsp's, for the seat it went to,
    /// and off the person's panel.** The 2026-10-10 shape was `from: human,
    /// for: null` on six barrier passes already handed to a seat, so compound's
    /// hand and the panel showed the person what the governor had been told.
    #[test]
    fn a_run_notice_says_it_is_wsps_and_for_the_seat_and_is_not_the_persons() {
        let (_env, store) = scratch("run-notice");
        a_task(&store, "wsp-001");
        store.set_governor(
            "wsp",
            json!({ "workspace": "w1", "pane": "w1:p1", "host": util::hostname(), "since": util::iso_at(1_000) }),
        );

        let m = notice(&store, "wsp-001", "Barrier 4 of wsp-for-ux passed. The verdict: holds.").expect("raised");
        let back = store.message(&m.id).expect("stored");
        assert!(back.from.is_wsp(), "written by the run, not a keyboard: {:?}", back.from);
        assert_eq!(back.source(), message::Source::Wsp);
        assert_eq!(back.for_seat.as_deref(), Some("wsp"), "for the seat that answers for the member");
        assert_eq!(back.to_json()["for"], json!({ "seat": "wsp" }), "and the JSON says so");
        assert!(back.handed_to("wsp"), "the words went into that seat's spool");
        assert!(!back.is_the_persons(), "so the person's open set leaves it out");
        let said = store.watches()[&crate::wake::key_for("wsp")]["spool"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(said.contains("Barrier 4 of wsp-for-ux passed") && said.contains(&m.id), "{said}");
    }

    /// With no seat anywhere above the member, nobody but the person can read
    /// it — so it is theirs, as an unaddressed hand always was, and it still
    /// says who wrote it.
    #[test]
    fn a_run_notice_with_no_seat_to_take_it_is_the_persons_and_still_wsps() {
        let (_env, store) = scratch("run-notice-no-seat");
        a_task(&store, "wsp-001");
        let m = notice(&store, "wsp-001", "Barrier 1 of solo passed.").expect("raised");
        assert!(m.from.is_wsp());
        assert_eq!(m.for_seat, None);
        assert!(m.is_the_persons(), "the only reader there is");
    }

    /// A compound-hosted seat has `WSP_SEAT_ID` and no other host's pane
    /// variable, and `wsp flag` read only that variable — so a hand a compound seat raised
    /// was recorded as the person's (three of the eight up on 2026-10-10).
    #[test]
    fn a_hand_raised_from_a_compound_seat_is_not_recorded_as_the_person() {
        let (_env, store) = scratch("flag-compound-seat");
        a_task(&store, "wsp-001");
        // A compound seat's record names the seat as its room as well as its pane.
        store.set_governor("tooling", json!({ "workspace": "cpd-41", "pane": "cpd-41", "host": util::hostname() }));
        std::env::set_var(crate::base::seat::SEAT_ENV, "cpd-41");
        let governs = raise_one(&store, &["wsp-001", "possibly moot"], &[]);
        std::env::set_var(crate::base::seat::SEAT_ENV, "cpd-42");
        let agent = raise_one(&store, &["wsp-001", "found something"], &[]);
        std::env::remove_var(crate::base::seat::SEAT_ENV);
        assert_eq!((governs, agent), (0, 0));

        let up = crate::message::raised(&store);
        assert_eq!(up[0].from, Party::seat("tooling"), "the seat's post, as `wsp ask` already records it");
        assert_eq!(up[1].from, Party::pane("cpd-42", ""), "an agent in a seat it does not govern is its pane");
        assert!(up.iter().all(|m| m.is_the_persons()), "a hand is still for the person: only the run's notices leave");
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

    /// The heading is quoted onto somebody's panel too.
    ///
    /// The row text has had this check since it was written, and `--title` —
    /// which is what *replaces* the row text when it is given — did not. Same
    /// reason neither has a `-` form: a heading is one line, and `--body -` is
    /// where a paragraph goes. That is an argument about length and not about
    /// what a shell does to a sentence on the way in, which is what this is.
    /// `worklist-047`, found by enumerating the typed intakes.
    #[test]
    fn a_title_that_is_captured_terminal_output_is_refused() {
        let (_env, store) = scratch("flag-title-ctl");
        a_task(&store, "wsp-001");

        assert_eq!(
            raise_one(&store, &["wsp-001", "the tree is shared"], &[("title", "alone 87/745  \ralone 88/745")]),
            2,
        );
        assert!(
            crate::message::raised(&store).is_empty(),
            "a hand went up wearing a captured progress bar as its heading",
        );
        assert_eq!(raise_one(&store, &["wsp-001", "the tree is shared"], &[("title", "the store is corrupt")]), 0);
    }

    /// `wsp-276`: a hand's caption is its `--caption`, else its `--title`, else
    /// its sentence's first clause — and a bare `wsp flag <id>`, which says
    /// *look at this row*, is the row's own.
    #[test]
    fn a_hand_is_captioned_by_its_caption_its_title_its_sentence_or_its_row() {
        let (_env, store) = scratch("flag-caption");
        a_task(&store, "wsp-001");
        let raise = |args: &[&str], flags: &[(&str, &str)]| -> serde_json::Value {
            let before: Vec<String> = crate::message::all(&store).into_iter().map(|m| m.id).collect();
            assert_eq!(raise_one(&store, args, flags), 0);
            let m = crate::message::all(&store).into_iter().find(|m| !before.contains(&m.id)).expect("a hand went up");
            store.message(&m.id).unwrap().to_json()["caption"].clone()
        };
        let said = ["wsp-001", "Ed: the tree is shared. Two agents wrote it"];
        assert_eq!(raise(&said, &[("caption", "two agents in one tree")]), json!({ "text": "two agents in one tree", "from": "authored" }));
        assert_eq!(raise(&said, &[("title", "the store is corrupt")]), json!({ "text": "the store is corrupt", "from": "title" }));
        assert_eq!(raise(&said, &[]), json!({ "text": "The tree is shared", "from": "derived" }));
        assert_eq!(raise(&["wsp-001"], &[])["from"], "task");
        assert_eq!(raise_one(&store, &said, &[("caption", &"x".repeat(61))]), 2);
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

    /// The same spelling on the other three telling verbs.
    ///
    /// `tell`, `ask` and `answer` share [`from_source`] with `govern --tell`,
    /// and until `worklist-036` all four took `-` and none took a file — while
    /// the house rule printed in every brief is to pass prose through a file,
    /// because a shell evaluates backticks inside double quotes. Driven at the
    /// function, because that is where the four of them are one thing; the
    /// verbs above it differ only in what they do with the paragraph.
    #[test]
    fn a_message_can_come_out_of_a_file_on_every_verb_that_sends_one() {
        let env = crate::util::isolated("prose-from-file");
        let path = env.path("brief.txt");
        std::fs::write(&path, "the `--refs` pass is the one to run first\n").unwrap();
        let args = Args::synth("tell", &["worklist-002"], &[("from", path.to_str().unwrap())]);

        assert_eq!(
            from_source(&args, "").unwrap(),
            "the `--refs` pass is the one to run first",
            "the backticks are why a file is the way to send this at all",
        );

        // A sentence as well as a file is a caller who does not know which one
        // is being sent, and choosing either loses the other silently.
        let args = Args::synth("tell", &["worklist-002"], &[("from", path.to_str().unwrap())]);
        assert_eq!(from_source(&args, "and one typed too"), Err(2));

        // A file with nothing in it is the empty hand again by another road.
        let empty = env.path("empty.txt");
        std::fs::write(&empty, "\n").unwrap();
        let args = Args::synth("tell", &["worklist-002"], &[("from", empty.to_str().unwrap())]);
        assert_eq!(from_source(&args, ""), Err(2));

        // And the typed form is untouched.
        let args = Args::synth("tell", &["worklist-002"], &[]);
        assert_eq!(from_source(&args, "come and look at this").unwrap(), "come and look at this");
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
            hand_aside(&std::collections::BTreeMap::new(), &up[0], None, false).contains("asks to take it"),
            "a question read as a notice: {}",
            hand_aside(&std::collections::BTreeMap::new(), &up[0], None, false),
        );

        // And a hand that asks for nothing does not pretend to: the phrase is
        // only worth its width when it is true.
        raise_one(&store, &["wsp-001", "just look at this"], &[]);
        let up = crate::message::raised(&store);
        assert!(!hand_aside(&std::collections::BTreeMap::new(), &up[1], None, false).contains("asks to"), "a notice claimed to be a question");
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

    /// Archive a row **as an older build did**, leaving what was raised about it
    /// standing. This build closes those with the row (`wsp-308`), so an orphan
    /// is no longer made by `wsp rm`; the records an older binary already left
    /// are what `doctor` still has to name, and this puts one back as it was.
    fn archive_leaving_hands_up(store: &Store, id: &str) {
        let standing: Vec<_> = crate::message::raised(store);
        store.archive_task(&store.task(id).unwrap()).unwrap();
        for m in standing {
            let mut m = store.message(&m.id).unwrap();
            if !m.is_open() {
                m.via.pop();
                m.state_raw = "open".into();
                store.save_message(&m).unwrap();
            }
        }
    }

    /// `doctor` says what the panel draws, which for a while nothing else did.
    ///
    /// ui-006's second half. Clearing an orphaned hand was fixed by resolving
    /// through the record rather than the task table — the test above — and
    /// the fault's quieter half stayed: `wsp rm` leaves standing hands alone,
    /// so an orphan is *ordinary* state, while `doctor` walked bindings,
    /// claims and trees and called the store healthy. Asserted on both words
    /// the line must keep apart, because "unknown" about a retired task is a
    /// lie from the one tool read as truth (`dangling`'s argument), and on the
    /// repair clause naming this hand's own id — never ambiguous, where the
    /// task id can name several.
    #[test]
    fn doctor_names_a_hand_standing_on_a_retired_task() {
        let (_env, store) = scratch("doctor-flag-retired");
        a_task(&store, "wsp-001");
        assert_eq!(raise_one(&store, &["wsp-001", "the store will not parse"], &[]), 0);

        // Up about live work: nothing to say, which is the common case and
        // the reason this check may cost doctor nothing.
        let mut problems = Vec::new();
        crate::message::health(&store, &store.tasks(), &store.archived_ids(), &mut problems);
        assert!(problems.is_empty(), "{problems:?}");

        archive_leaving_hands_up(&store, "wsp-001");
        assert!(store.task("wsp-001").is_none(), "the subject is gone");

        crate::message::health(&store, &store.tasks(), &store.archived_ids(), &mut problems);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("retired"), "{problems:?}");
        assert!(!problems[0].contains("unknown"), "retired is not missing: {problems:?}");
        let id = crate::message::raised(&store)[0].id.clone();
        assert!(
            problems[0].contains(&format!("wsp flag --clear {id}")),
            "the line names the verb and the unambiguous id: {problems:?}",
        );
    }

    /// And a subject nothing ever answered to gets its honest word too.
    ///
    /// The raise path refuses such an id (`task_or_why`), but the legacy
    /// migration cannot — it mints a hand out of whatever key an older binary
    /// wrote into `flags.json`, and `inbox-001` was the one Ed's panel actually
    /// carried. So the fixture goes through the migration, which is the one
    /// thing that still produces this case.
    #[test]
    fn doctor_says_unknown_about_a_hand_on_an_id_that_never_resolved() {
        let (_env, store) = scratch("doctor-flag-unknown");
        store.set_legacy_flag("inbox-404", json!({ "said": "clearing me failed" }));
        let adopted = crate::message::adopt_legacy_flags(&store);
        assert_eq!(adopted.len(), 1, "the migration mints the hand: {adopted:?}");

        let mut problems = Vec::new();
        crate::message::health(&store, &store.tasks(), &store.archived_ids(), &mut problems);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("unknown task `inbox-404`"), "{problems:?}");
    }

    /// A question whose subject is gone is pointed at the verbs that close it,
    /// not at the one that refuses.
    ///
    /// [`Shape::may`] refuses to let a question be cleared — `worklist-004`,
    /// where clearing looked like answering — so a health line advising
    /// `--clear` would send its reader straight at a refusal. The same split
    /// `dispose` prints when its act is bounced.
    #[test]
    fn doctor_sends_an_orphaned_question_to_the_verbs_that_close_it() {
        let (_env, store) = scratch("doctor-flag-question");
        a_task(&store, "wsp-001");
        assert_eq!(
            raise_one(&store, &["wsp-001", "take this?"], &[("ask", "claim"), ("pane", "w4:p2")]),
            0
        );
        archive_leaving_hands_up(&store, "wsp-001");

        let mut problems = Vec::new();
        crate::message::health(&store, &store.tasks(), &store.archived_ids(), &mut problems);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("wsp answer"), "{problems:?}");
        assert!(problems[0].contains("--abandon"), "{problems:?}");
        assert!(!problems[0].contains("--clear"), "the clear door is shut here: {problems:?}");
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

    /// **The defect this row was filed for, in the one direction that was
    /// silent** (`wsp-119`): a compound seat whose agent has exited, with its claim and its worktree still held.
    ///
    /// A compound seat never appeared in the old backend's two listings, so the
    /// old signature missed both arms and fell through to the reap guard —
    /// which a compound id cannot pass, because it names no machine. The answer
    /// was `Unheard`, and `Unheard` is the arm that says *nothing is known*, so
    /// the check reported ignorance about a seat it could have read in full, and
    /// said nothing about a dead one. Both callers inherited it: `doctor` on
    /// every machine, and the `agent-gone` signal on every machine without
    /// that backend, which after `compound-112` is every machine.
    ///
    /// Driven through the census rather than through a pane fixture, because
    /// the whole of the fault was that the answer came from somewhere that
    /// could not see the seat.
    #[test]
    fn a_compound_seat_whose_agent_exited_is_emptied_rather_than_unheard() {
        let answered = answered_by_machine(["w1:p1"]);
        let rows = vec![crate::place::Seated {
            seat: crate::place::Seat::new("cpd-70"),
            label: String::new(),
            cwd: String::new(),
            agent: crate::place::Agent { kind: "claude".into(), ..Default::default() },
            // What `place_compound::state_of` answers when the pid the seat
            // recorded is not in the process table.
            state: State::Gone,
            session: String::new(),
        }];
        assert!(
            matches!(bound_state("cpd-70", &rows, &answered), Bound::Emptied),
            "the seat is there and its agent is not, which is an emptied seat"
        );
        // The other answer, and it is `may_reap`'s rather than this function's:
        // a seat no source listed is `Gone` once its machine has answered.
        assert!(matches!(bound_state("cpd-71", &[], &answered), Bound::Gone));
        // And on a machine where nothing answered, nothing may be judged at
        // all — the arm that has to mean *nobody knows anything about this*.
        assert!(matches!(bound_state("cpd-71", &[], &BTreeMap::new()), Bound::Unheard));
    }

    /// A seat with an agent in it that no source can vouch for is `Quiet`, and
    /// not `Emptied`.
    ///
    /// The arm that would send a person to `wsp despawn` a seat that may still
    /// have a live agent in it is the one that has to be careful, and the way to
    /// be careful is to read the agent as well as the state: a listing cannot
    /// distinguish an exited agent's pane from a shell, so a row whose state is
    /// unreadable but which names an agent is an agent we cannot *see* — and
    /// `State::stopped`'s own doc is the rule, applied one step earlier.
    #[test]
    fn a_seat_with_an_unreadable_agent_in_it_is_not_called_emptied() {
        let answered = answered_by_machine(["w1:p1"]);
        let rows = vec![crate::place::Seated {
            seat: crate::place::Seat::new("w1:p1"),
            label: String::new(),
            cwd: String::new(),
            agent: crate::place::Agent { kind: "claude".into(), ..Default::default() },
            state: State::Unknown,
            session: String::new(),
        }];
        assert!(
            matches!(bound_state("w1:p1", &rows, &answered), Bound::Quiet(State::Unknown)),
            "we cannot see it, so we do not tell a person to despawn it"
        );
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

        let (pane, what) = target(&store, &t.id, &|_| false).expect("the task's pane");
        assert_eq!(pane, "w1:p1");
        assert!(what.starts_with(&t.id), "{what}");

        assert_eq!(target(&store, "w1:p1", &|_| false).map(|(p, _)| p), Some("w1:p1".into()), "a pane id, as given");
        assert_eq!(target(&store, "w1", &|_| false), None, "a workspace may hold two agents and names neither");
        assert_eq!(target(&store, "nothing-like-this", &|_| false), None);

        // A seat a backend minted without a colon is a pane too, and is read
        // by asking who holds it — not by its spelling (`wsp-156`).
        let compound = |n: &str| n == "cpd-347";
        assert_eq!(
            target(&store, "cpd-347", &compound).map(|(p, w)| (p, w)),
            Some(("cpd-347".into(), "pane cpd-347".into())),
            "a compound seat has no colon and is still the thing to type at"
        );
        assert_eq!(target(&store, "cpd-348", &compound), None, "a seat nobody holds is not one because of its shape");
    }

    /// **The retry is the harm, and this is what recognises one.**
    ///
    /// worklist-010's cost was not the false failure report; it was what the
    /// report caused. `wsp govern wsp --tell` said `the wsp seat was not told`
    /// about a message that had arrived, so the seat retried, and one paragraph
    /// was delivered to the `wsp` seat two or three times in a day — a governor
    /// sending the same direction three times to an agent that acted on the
    /// first. A backend `tell` that no longer lies stops the retries this
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

        // Whole seconds: a tick between the log and the read makes it 1.
        let n = first.already_sent(&store).expect("the retry after a lie");
        assert!((0..=1).contains(&n), "the retry after a lie, {n} s on");
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

    /// A far seat on a silent machine reads `unreachable <age>` in `wip` — the
    /// state column, the heading's own line and the JSON — in place of `gone`,
    /// and a seat on a machine that is heard does not (`docs/remote-agents.md`
    /// section 3).
    #[test]
    fn wip_draws_a_seat_on_a_silent_machine_as_unreachable_with_how_long() {
        let mut w = wip_world();
        w.agents[2].state = State::Unreachable;
        let pane = w.agents[2].seat.as_str().to_string();
        let plain = wip_lines(&w, &Paint::new(), false).join("\n");
        assert!(!plain.contains("unreachable:"), "no heading line without a silent machine:\n{plain}");

        w.silent.line = Some("unreachable: mb2 (unreachable 4m) — their seats are held, not gone".into());
        w.silent.seats.insert(pane.clone(), "unreachable 4m".into());
        let text = wip_lines(&w, &Paint::new(), false).join("\n");
        assert!(text.contains("unreachable: mb2 (unreachable 4m)"), "the heading says it as a dead daemon is said:\n{text}");
        let row = text.lines().find(|l| l.contains(&pane)).expect("the seat is drawn");
        assert!(row.contains("unreachable 4m"), "the row wears the word and the age:\n{row}");
        assert!(!row.contains("gone"), "{row}");

        let j = wip_json(&w);
        assert_eq!(j["unreachable"], w.silent.line.clone().map(Into::into).unwrap_or(json!(null)));
        let r = j["agents"].as_array().unwrap().iter().find(|r| r["pane"] == pane.as_str()).unwrap();
        assert_eq!(r["state"], "unreachable");
    }

    /// A seat that reads unreachable is never `Gone` to the check that reaps on
    /// a bound seat: nothing is known about it, so nothing is done.
    #[test]
    fn a_bound_seat_that_reads_unreachable_is_unheard_and_not_gone() {
        let rows = vec![crate::place::Seated {
            seat: crate::place::Seat::new("cpd-7"),
            label: String::new(),
            cwd: String::new(),
            agent: crate::place::Agent { kind: "claude".into(), name: "t-1".into(), args: vec![], key: None },
            state: State::Unreachable,
            session: String::new(),
        }];
        let answered = answered_by_machine(["w0:p1"]);
        assert!(matches!(bound_state("cpd-7", &rows, &answered), Bound::Unheard));
        assert!(matches!(bound_state("cpd-9", &[], &answered_by_machine(["cpd-9"])), Bound::Gone), "a seat absent from an answering machine is still gone");
    }

    /// The generalisation, not a second rule beside the old one: `reap` used to
    /// refuse outright when the backend answered with nothing at all, for exactly
    /// this reason, on the one machine there was. That case still holds.
    #[test]
    fn a_backend_answering_with_nothing_still_reaps_nothing() {
        let answered = answered_by_machine(std::iter::empty());
        assert!(!may_reap(&answered, "w0"));
        assert!(!may_reap(&answered, ""), "a claim too old to carry a workspace id either");
    }

    /// The two names an unscoped pane may not take. The panel filters its own
    /// panes out of the tree by label, so an agent that said "wsp" while
    /// holding nothing would be taken for furniture and dropped from it.
    #[test]
    fn a_pane_never_takes_the_panels_own_name() {
        assert_eq!(plain_full("wsp"), None);
        assert_eq!(plain_full("  wsp  "), None);
        assert_eq!(plain_full("wsp:view"), None);
        assert_eq!(plain_full(""), None);

        // Anything else is itself, whole — and cut to `LABEL_MAX` only on the
        // way to the wire.
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
        // name that disagrees with the pane's is a worse answer than a short one.
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
    /// the project is not what the label leads with: a sidebar draws 26
    /// columns and cuts the right-hand end, so a project-first label loses the
    /// state — which is the half a person scanning for a free agent is reading.
    #[test]
    fn a_looking_pane_says_the_state_before_the_project() {
        assert_eq!(looking_label(Some("render"), true), "looking for work in render");
        assert_eq!(looking_label(Some("render"), false), "nothing actionable in render");

        // Unscoped — `wsp next` outside any project. Still the two states.
        assert_eq!(looking_label(None, true), "looking for work");
        assert_eq!(looking_label(None, false), "nothing actionable");

        // What survives a 26-column cut is the state, for the longest slug we have.
        let long = looking_label(Some("strata-prototype"), true);
        assert!(long.chars().take(26).collect::<String>().starts_with("looking for work"));

        // And nothing here can ever be a name the panel hunts for.
        for l in [looking_label(Some("wsp"), true), looking_label(Some("wsp"), false)] {
            assert_ne!(l, crate::panel::PANEL_LABEL);
            assert_ne!(l, crate::panel::VIEW_LABEL);
        }
    }

    /// wsp-115's acceptance, on the seat it was filed from: a compound seat
    /// says something. It used to refuse and exit 1 before looking at the
    /// sentence; the exit code is
    /// asserted as well as the record, because a version that wrote the record
    /// and still refused would pass on the store alone.
    ///
    /// The sentence is short on purpose. `set_said` keeps nothing when the
    /// label is already whole, so a record that lived only in that half would
    /// hold nothing for exactly the sentences agents usually say.
    #[test]
    fn a_compound_seat_says_something_and_it_is_kept() {
        let iso = util::isolated("say-compound");
        let store = Store::at(iso.home(), iso.state());
        store.ensure_dirs().unwrap();
        let mut t = Task::new("Say where you have got to", "wsp-900");
        t.project = Some("wsp".into());
        t.status_raw = "doing".into();
        store.save_task(&t).unwrap();
        store.set_binding("cpd-9", json!({ "task_id": "wsp-900", "pane_id": "cpd-9" }));
        std::env::set_var(crate::base::seat::SEAT_ENV, "cpd-9");

        let code = say(&store, &Args::synth("say", &["running the suite"], &[]));
        std::env::remove_var(crate::base::seat::SEAT_ENV);
        assert_eq!(code, 0, "a compound seat is not a reason to refuse a status line");

        let said = store.said();
        assert_eq!(said["cpd-9"]["said"], "running the suite", "the sentence, as typed: {said:?}");
        assert_eq!(said["cpd-9"]["task"], "wsp-900", "and the work it was about");
        assert_eq!(sentence(&said, "cpd-9", Some("wsp-900")).as_deref(), Some("running the suite"));

        // And a person reading `wip` sees it, which on this seat is the only
        // place anybody can.
        let w = Wip {
            tasks: vec![t].into(),
            index: Index::new(vec![crate::model::Project::new("wsp")]),
            bindings: store.bindings(),
            claims: std::collections::BTreeMap::new(),
            pins: std::collections::BTreeMap::new(),
            governors: std::collections::BTreeMap::new(),
            agents: vec![seated_agent("cpd-9", "working", "cpd-9")],
            agents_held: std::collections::BTreeMap::new(),
            said,
            daemon: None,
            disk: None,
            load: None,
            throttle: None,
            idle_memory: Default::default(),
            worked: Default::default(),
            asks: Default::default(),
            shells: Default::default(),
            mandates: Default::default(),
            checking: Default::default(),
            check_on: Default::default(),
            lists: Vec::new(),
            since: Default::default(),
            spawned: Default::default(),
            unsent: Default::default(),
            queued: Default::default(),
            refusing: Default::default(),
            dropped: Default::default(),
            full_suite: Default::default(),
            stuck: Default::default(),
            silent: Default::default(),
        };
        assert_eq!(wip_json(&w)["agents"][0]["said"], "running the suite", "the key compound's header reads");
        let text = wip_lines(&w, &Paint::new(), false).join("\n");
        assert!(text.contains("running the suite"), "{text}");
        drop(iso);
    }

    /// A sentence is about the work the seat held when it was said. Once the
    /// seat holds other work, or none, it is not what the agent is doing — and
    /// nobody has to have remembered to clear it for that to be true.
    #[test]
    fn a_sentence_said_about_one_task_is_not_read_as_the_next_ones() {
        let mut said = BTreeMap::new();
        said.insert("cpd-9".to_string(), json!({ "said": "bisecting the flake", "task": "wsp-900" }));
        assert_eq!(sentence(&said, "cpd-9", Some("wsp-900")).as_deref(), Some("bisecting the flake"));
        assert_eq!(sentence(&said, "cpd-9", Some("wsp-901")), None, "claimed something else since");
        assert_eq!(sentence(&said, "cpd-9", None), None, "released since");
        assert_eq!(sentence(&said, "cpd-8", Some("wsp-900")), None, "another seat's sentence");

        // A seat that held nothing when it spoke is read while it still holds
        // nothing, which is what `say_looking` is for.
        said.insert("cpd-7".to_string(), json!({ "said": "looking for work" }));
        assert_eq!(sentence(&said, "cpd-7", None).as_deref(), Some("looking for work"));
        assert_eq!(sentence(&said, "cpd-7", Some("wsp-900")), None);

        // The label half alone, which is all an entry from before wsp-115 has,
        // is not a sentence.
        said.insert("w1:p1".to_string(), json!({ "label": "wsp/900 · a…", "full": "wsp/900 · a b" }));
        assert_eq!(sentence(&said, "w1:p1", None), None);
    }

    /// The two halves of an entry are two writers' facts, and neither takes
    /// the other's with it. `reconcile` re-asserting a pane's name is not the
    /// agent saying something else, and the agent saying something is not the
    /// pane's long name going away.
    #[test]
    fn renaming_a_pane_keeps_what_its_agent_said_and_clear_takes_both() {
        let iso = util::isolated("said-halves");
        let store = Store::at(iso.home(), iso.state());
        store.ensure_dirs().unwrap();
        store.set_sentence("w1:p1", "running the suite", Some("wsp-900"));
        store.set_said("w1:p1", "wsp/900 · running…", "wsp/900 · running the suite in full");
        store.set_said("w1:p1", "wsp/900 · short", "wsp/900 · short");
        let said = store.said();
        assert_eq!(said["w1:p1"]["said"], "running the suite", "{said:?}");
        assert!(said["w1:p1"].get("label").is_none(), "a whole label keeps no long form: {said:?}");

        // With nothing said and a label that is whole, there is no entry at all.
        store.set_said("w2:p1", "short", "short");
        assert!(!store.said().contains_key("w2:p1"));

        assert!(store.clear_said("w1:p1"));
        assert!(store.said().is_empty());
        drop(iso);
    }

    /// The minute an agent spends looking for work is visible on a compound
    /// seat too. It used to return on a host check before reading which
    /// seat it was in, so the one agent a person would most want to see
    /// was looking read as the one that had done nothing.
    #[test]
    fn a_compound_seat_looking_for_work_says_so() {
        let iso = util::isolated("looking-compound");
        let store = Store::at(iso.home(), iso.state());
        store.ensure_dirs().unwrap();
        std::env::set_var(crate::base::seat::SEAT_ENV, "cpd-9");
        say_looking(&store, Some("wsp"), true);
        std::env::remove_var(crate::base::seat::SEAT_ENV);
        assert_eq!(sentence(&store.said(), "cpd-9", None).as_deref(), Some("looking for work in wsp"));
        drop(iso);
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

    /// A `Pane` as `agent.list` and `pane.list` render one, **with** the
    /// `interactive_ready` a host sends for an agent it launched. That field is
    /// not decoration: a census refuses to read a status word without it, so a
    /// fixture that omits it is an agent the host has never described, and the census reads it as `Unknown` — a seat wsp cannot see.
    /// `wsp-119`: `bound_state` used to read the status off such a row anyway,
    /// which is how `doctor` and `wip` came to hold opposite facts about one
    /// agent.
    /// `wsp-582`: a seat still up on a row that was closed under it is marked in
    /// `wip` as such; a seat that something holds, or whose row is open, is not.
    #[test]
    fn wip_marks_a_seat_on_a_closed_row() {
        let mut w = wip_world();
        let mut done = wip_task("closed-1", "finished", Some("wsp"), "done");
        done.status_raw = "done".into();
        let open = wip_task("open-1", "still going", Some("wsp"), "doing");
        let mut all: Vec<Task> = w.tasks.iter().cloned().collect();
        all.extend([done, open]);
        w.tasks = all.into();
        w.worked.insert("closed-1".into(), json!({ "workspace_id": "w8:p1" }));
        w.worked.insert("open-1".into(), json!({ "workspace_id": "w3:p1" }));
        assert_eq!(closed_row_of(&w.tasks, &w.bindings, &w.worked, "w8:p1").as_deref(), Some("closed-1"));
        assert_eq!(closed_row_of(&w.tasks, &w.bindings, &w.worked, "w3:p1"), None, "an open row's seat is not on a closed one");
        w.bindings.insert("w8:p1".into(), json!({ "task_id": "open-1" }));
        assert_eq!(closed_row_of(&w.tasks, &w.bindings, &w.worked, "w8:p1"), None, "a seat something holds now is not");
        w.bindings.remove("w8:p1");
        w.agents.push(seated_agent("w8:p1", "idle", "wsp"));
        let rows = wip_rows(&w);
        assert_eq!(rows.iter().find(|r| r.pane == "w8:p1").unwrap().closed_row.as_deref(), Some("closed-1"));
        assert!(wip_json(&w)["agents"].to_string().contains("closed_row") || wip_json(&w).to_string().contains("closed_row"));
    }

    /// A runner's own status word, as a census reads it.
    fn of_word(status: &str) -> State {
        match status.trim() {
            "working" => State::Working,
            "idle" | "done" => State::Idle,
            "blocked" => State::Blocked,
            _ => State::Unknown,
        }
    }

    fn wip_agent(pane: &str, ws: &str, state: &str, title: &str) -> Pane {
        Pane {
            pane_id: pane.to_string(),
            workspace_id: ws.to_string(),
            agent: "claude".into(),
            agent_status: state.to_string(),
            title: title.to_string(),
            ..Default::default()
        }
    }

    /// A census row, as `Wip::live` now builds one. `state` is the runner's own
    /// word for it — `of_word` is the translation a census applies, so a test
    /// can still write `"done"` and mean the same thing `agent_status` used
    /// to. `label` is what the seat was opened with, the fallback a row shows
    /// when it holds no task.
    fn seated_agent(pane: &str, state: &str, label: &str) -> crate::place::Seated {
        crate::place::Seated {
            seat: crate::place::Seat::new(pane),
            label: label.to_string(),
            agent: crate::place::Agent { kind: "claude".into(), ..Default::default() },
            state: of_word(state),
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
    /// The state this view exists to show is a join of a runner fact (an agent
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
            ].into(),
            index: Index::new(vec![crate::model::Project::new("wsp")]),
            bindings,
            claims: std::collections::BTreeMap::new(),
            pins: std::collections::BTreeMap::new(),
            governors: std::collections::BTreeMap::new(),
            agents: vec![
                seated_agent("w1:p1", "working", "wsp"),
                // Stopped, on a task that is still doing: the ← this view is for.
                seated_agent("w2:p1", "idle", "wsp"),
                // An agent holding nothing, which is its own kind of row.
                seated_agent("w3:p1", "working", "reading the README"),
            ],
            agents_held: std::collections::BTreeMap::new(),
            said: std::collections::BTreeMap::new(),
            daemon: None,
            disk: None,
            load: None,
            throttle: None,
            idle_memory: Default::default(),
            worked: Default::default(),
            asks: Default::default(),
            shells: Default::default(),
            mandates: Default::default(),
            checking: Default::default(),
            check_on: Default::default(),
            lists: Vec::new(),
            since: Default::default(),
            spawned: Default::default(),
            unsent: Default::default(),
            queued: Default::default(),
            refusing: Default::default(),
            dropped: Default::default(),
            full_suite: Default::default(),
            stuck: Default::default(),
            silent: Default::default(),
        }
    }

    /// `wsp-188`. A verifier holds no claim and no binding, so `wip` would draw
    /// its seat as one holding nothing. Its seat is on the member's open pass,
    /// and that is where the row is named from: against the member.
    #[test]
    fn wip_names_a_verifier_seat_against_the_member_it_is_verifying() {
        let mut w = wip_world();
        let m = std::rc::Rc::make_mut(&mut w.tasks).iter_mut().find(|t| t.id == "t-004").unwrap();
        let mut p = crate::verification::Pass::opened(Some("abc1234".into()), None);
        p.pane = Some("w3:p1".into());
        crate::verification::write(m, &[p]);
        let rows = wip_rows(&w);
        let row = rows.iter().find(|r| r.pane == "w3:p1").unwrap();
        assert_eq!(row.task, "t-004 · verifying");
        assert_eq!(row.project, "wsp", "placed where its member is, not wherever the seat happens to stand");
        assert!(row.task_id.is_empty(), "it holds nothing: the member's own agent is who holds t-004");
        let text = wip_lines(&w, &Paint::new(), false).join("\n");
        assert!(text.contains("t-004 · verifying"), "{text}");
    }

    /// `wsp-230`. A barrier check holds no claim and no binding either, so it
    /// is named against the list and group it is checking, in the project its
    /// group's work is in, rather than drawn as a seat holding nothing.
    #[test]
    fn wip_names_a_check_seat_against_its_list_and_group() {
        let mut w = wip_world();
        w.checking.insert("w3:p1".into(), ("gpui-migration".into(), 4, Some("wsp".into())));
        let rows = wip_rows(&w);
        let row = rows.iter().find(|r| r.pane == "w3:p1").unwrap();
        assert_eq!(row.task, "gpui-migration · barrier 4");
        assert!(row.task_id.is_empty(), "it holds no row");
        assert_eq!(row.project, "wsp");
    }

    /// `wsp-263`, amending `wsp-247`: a list's governor answers to the
    /// governor of the project the list stands in — the nearest seat on its
    /// home's walk — and a project's governor to nobody. With nobody seated on
    /// the walk it is `null`, and `answers_to_empty` names the home so the
    /// chrome can say "no governor seated" rather than guess.
    #[test]
    fn a_gone_pane_is_not_drawn_as_a_governor_and_an_unsent_prompt_is_not_idle() {
        let mut w = wip_world();
        w.governors.insert("tonight".into(), json!({ "workspace": "w6", "pane": "w6:p1" }));
        let mut gone = seated_agent("w6:p1", "idle", "w6:p1");
        gone.state = State::Gone;
        w.agents.push(gone);
        w.agents.push(seated_agent("w7:p1", "idle", "w7:p1"));
        w.unsent.insert("w7:p1".into());
        let j = wip_json(&w);
        let row = |pane: &str| j["agents"].as_array().unwrap().iter().find(|r| r["pane"] == pane).unwrap().clone();
        assert_eq!(row("w6:p1")["seat"], Value::Null, "a gone pane is no governor");
        assert_eq!(row("w6:p1")["was_governor"], "tonight");
        assert_eq!(row("w7:p1")["unsent"], true);
        let text = wip_lines(&w, &Paint::new(), false).join("\n");
        assert!(text.contains("typed, not sent"), "{text}");
        assert!(text.contains("was governor · tonight") || text.contains("was "), "{text}");
    }

    /// `wsp-420`: a seat whose first order was sent again and started no turn
    /// is drawn as that, and not as `idle`, which is what it reads as.
    #[test]
    fn a_seat_whose_first_order_never_started_a_turn_is_not_drawn_as_idle() {
        let mut w = wip_world();
        w.agents.push(seated_agent("w7:p1", "idle", "w7:p1"));
        w.dropped.insert("w7:p1".into());
        let j = wip_json(&w);
        let row = j["agents"].as_array().unwrap().iter().find(|r| r["pane"] == "w7:p1").unwrap().clone();
        assert_eq!(row["dropped"], true);
        let text = wip_lines(&w, &Paint::new(), false).join("\n");
        assert!(text.contains("order not received"), "{text}");
    }

    #[test]
    fn a_seat_that_ran_the_full_suite_says_so_and_the_others_do_not() {
        let mut w = wip_world();
        w.full_suite.insert("w1:p1".into());
        let j = wip_json(&w);
        let row = |pane: &str| j["agents"].as_array().unwrap().iter().find(|r| r["pane"] == pane).unwrap().clone();
        assert_eq!(row("w1:p1")["full_suite"], true);
        assert_eq!(row("w2:p1")["full_suite"], false);
        let text = wip_lines(&w, &Paint::new(), false).join("\n");
        assert_eq!(text.matches("full suite in a member seat").count(), 1, "{text}");
    }

    #[test]
    fn a_seat_in_a_command_waiting_on_itself_shows_the_flag_with_the_seconds_it_held() {
        let mut w = wip_world();
        w.stuck.insert("w1:p1".into(), (crate::stuck::Kind::SelfWait, 372));
        let j = wip_json(&w);
        let row = |pane: &str| j["agents"].as_array().unwrap().iter().find(|r| r["pane"] == pane).unwrap().clone();
        assert_eq!(row("w1:p1")["stuck"]["secs"], 372);
        assert_eq!(row("w1:p1")["stuck"]["kind"], "self");
        assert!(row("w2:p1")["stuck"].is_null());
        let text = wip_lines(&w, &Paint::new(), false).join("\n");
        assert_eq!(text.matches("waiting on itself 6m").count(), 1, "{text}");
    }

    // ---- delivery under the orders machine (wsp-127) ----------------------

    const STALL: &str = "until ! pgrep -f claude; do sleep 5; done";

    /// An order sent to `seat` and typed under its lease, never answered:
    /// the delivery worker's ask that nobody took.
    fn typed_order(db: &wsp_server::SharedStore, seat: &str) -> wsp_store_contract::OrderId {
        use wsp_store_contract::{Command, Fence, IdempotencyKey, LeaseName, Leases, Principal, SeatId};
        let to = SeatId(seat.into());
        let cmd = Command::OrderSend { to: to.clone(), text: "go".into(), first: false };
        let id = crate::orders::send_in(db, cmd, &Principal::Person("ed".into())).unwrap();
        let by = Principal::Wsp("wsp".into());
        let lease = LeaseName::deliver(&to);
        let token = Leases::take(&**db, &lease, &by, std::time::Duration::from_secs(60)).unwrap().expect("free");
        let deliver = Command::OrderDeliver { order: id.clone(), fence: Fence { lease: lease.clone(), token } };
        db.apply(deliver, IdempotencyKey(format!("test:deliver:{}", id.0)), by).unwrap();
        Leases::release(&**db, &lease, token).unwrap();
        id
    }

    /// An order to `seat` refused until the machine's attempts ran out.
    fn failed_order(db: &wsp_server::SharedStore, seat: &str, first: bool, why: &str) {
        let by = wsp_store_contract::Principal::Wsp("daemon".into());
        let id = crate::orders::send_keyed_in(db, &wsp_store_contract::SeatId(seat.into()), "go", &by, first, &format!("test:{seat}:{first}"))
            .unwrap();
        crate::refusals::tests::fail(db, &id, why);
    }

    /// The manager's `seat.stuck` for `seat`, folded: a call `waited` seconds
    /// old when it was heard at `heard`.
    fn folded_stall(store: &Store, seat: &str, waited: i64, heard: i64) {
        let local = crate::manager::Local {
            host: "test-host".into(),
            addr: std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            port: 9,
            exec_dir: store.state.join("exec"),
        };
        crate::manager::write_record(&store.state, &local, "token").unwrap();
        let path = crate::fold::stuck_path(&store.state, "test-host");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let was: Value = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}));
        let mut all = was.as_object().cloned().unwrap_or_default();
        all.insert(seat.into(), json!({ "command": STALL, "waited_secs": waited, "heard": heard }));
        std::fs::write(&path, Value::Object(all).to_string()).unwrap();
    }

    /// Under `store`, an order typed two ticks ago draws `typed, not sent`, a
    /// failed first order `order not received` and a manager stall `stuck`; and
    /// `wip --json` is byte-identical to the files path's for the same facts
    /// (the files path's `unsent` and `dropped` as `typed-unsent.json` and
    /// `first-orders.json` filled them, its `stuck` from `stuck-commands.json`).
    #[test]
    fn under_the_orders_machine_wip_draws_the_files_paths_words_and_json_from_the_orders() {
        let env = util::isolated("wip-orders-json");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        // `store` holds only under the manager, whose folded stall `stuck` reads
        // (`isolated` holds the env lock and puts `WSP_HOST` back).
        std::env::set_var(wsp_wire::host_tcp::ENV, "manager");
        let now = util::epoch_secs();

        // The files path's facts: a stall recorded in stuck-commands.json.
        std::fs::write(
            store.state.join("stuck-commands.json"),
            json!({ "w3:p1": { "kind": "self", "command": STALL, "at": now - 400, "told": true } }).to_string(),
        )
        .unwrap();
        let mut files = wip_world();
        files.agents[0].state = State::Idle;
        files.unsent.insert("w2:p1".into());
        files.dropped.insert("w1:p1".into());
        files.stuck = crate::repair::held_all(&store).into_iter().map(|(s, h)| (s, (h.kind, now - h.at))).collect();
        assert_eq!(files.stuck.len(), 1, "the files path reads its stall");

        // The same facts in the orders machine and the fold.
        let opened = crate::refusals::tests::open(&store, &["w1:p1", "w2:p1", "w3:p1"]);
        typed_order(&opened.0, "w2:p1");
        failed_order(&opened.0, "w1:p1", true, "no prompt came up");
        folded_stall(&store, "w3:p1", 400, now);
        let reads = OrderReads::of(&store).expect("the test's store stands for `store`");
        let two_ticks = now_ms() + 2 * ORDER_TICK_MS;
        let cols = DeliveryColumns::read(&reads, None, two_ticks);
        assert_eq!(cols.unsent, BTreeSet::from(["w2:p1".to_string()]), "typed two ticks ago");
        assert_eq!(cols.dropped, BTreeSet::from(["w1:p1".to_string()]), "a failed first order");
        assert!(cols.refusing.is_empty(), "a failed first order is not refusing: {:?}", cols.refusing);
        assert!(DeliveryColumns::read(&reads, None, now_ms()).unsent.is_empty(), "typed this tick is not unsent yet");

        let mut stored = wip_world();
        stored.agents[0].state = State::Idle;
        stored.unsent = cols.unsent;
        stored.dropped = cols.dropped;
        stored.refusing = cols.refusing;
        stored.stuck = stuck_folded(&store, stored.bindings.keys().chain(["w3:p1".to_string()].iter()), now);

        let (a, b) = (wip_json(&files), wip_json(&stored));
        let row = |j: &Value, pane: &str| j["agents"].as_array().unwrap().iter().find(|r| r["pane"] == pane).unwrap().clone();
        assert_eq!(row(&b, "w2:p1")["unsent"], true);
        assert_eq!(row(&b, "w1:p1")["dropped"], true);
        assert_eq!(row(&b, "w3:p1")["stuck"]["kind"], "self");
        assert_eq!(serde_json::to_string(&a).unwrap(), serde_json::to_string(&b).unwrap(), "byte-identical for the same facts");
        let text = wip_lines(&stored, &Paint::new(), false).join("\n");
        assert!(text.contains(UNSENT_WORD) && text.contains(DROPPED_WORD), "{text}");
        assert!(text.contains("waiting on itself"), "{text}");
        assert_eq!(text, wip_lines(&files, &Paint::new(), false).join("\n"));
    }

    /// Under `store`, the gather reads none of typed-unsent.json,
    /// first-orders.json, refused-deliveries.json or stuck-commands.json: each
    /// names a seat the orders do not, which the files path draws and `store`
    /// does not. The store handle's reading and the mirror's `state` through
    /// `Wip::from_api` give the same columns.
    #[test]
    fn under_the_orders_machine_wip_reads_none_of_the_delivery_files() {
        let env = util::isolated("wip-orders-files");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        // `store` holds only under the manager, whose folded stall `stuck` reads
        // (`isolated` holds the env lock and puts `WSP_HOST` back).
        std::env::set_var(wsp_wire::host_tcp::ENV, "manager");
        let now = util::epoch_secs();
        for seat in ["w1:p1", "w2:p1", "w3:p1", "w9:p9"] {
            store.set_binding(seat, json!({ "pane_id": seat }));
        }
        store.set_governor("wsp", json!({ "workspace": "w9:p9", "pane": "w9:p9", "host": util::hostname() }));
        let file = |name: &str, v: Value| std::fs::write(store.state.join(name), v.to_string()).unwrap();
        file("typed-unsent.json", json!({ "w9:p9": { "at": now - 100 } }));
        file("first-orders.json", json!({ "w9:p9": { "at": now - 100, "sent": 2 } }));
        file("refused-deliveries.json", json!({ "wsp": { "n": 4, "since": now - 100, "why": "from the file" } }));
        file("stuck-commands.json", json!({ "w9:p9": { "kind": "self", "command": STALL, "at": now - 100, "told": true } }));

        // The files path is the rollback under the manager (wsp-128): `store`
        // is the default there, so the files read names it.
        std::env::set_var(crate::orders::ENV, "files");
        let by_files = Wip::live(&store);
        std::env::remove_var(crate::orders::ENV);
        assert!(by_files.refusing.contains_key("w9:p9"), "the files path reads refused-deliveries.json: {:?}", by_files.refusing);
        assert!(by_files.stuck.contains_key("w9:p9"), "the files path reads stuck-commands.json: {:?}", by_files.stuck);

        let opened = crate::refusals::tests::open(&store, &["w1:p1", "w2:p1", "w3:p1"]);
        failed_order(&opened.0, "w1:p1", true, "no prompt came up");
        failed_order(&opened.0, "w2:p1", false, "the seat refused it");
        folded_stall(&store, "w3:p1", 400, now);
        let w = Wip::live(&store);
        assert_eq!(w.dropped, BTreeSet::from(["w1:p1".to_string()]));
        assert_eq!(w.refusing, BTreeMap::from([("w2:p1".to_string(), (3, "the seat refused it".to_string()))]));
        assert!(w.unsent.is_empty(), "{:?}", w.unsent);
        assert_eq!(w.stuck.keys().collect::<Vec<_>>(), ["w3:p1"], "the folded stall alone");
        assert_eq!(w.stuck["w3:p1"].0, crate::stuck::Kind::SelfWait);

        // The mirror's `state`, its seats carrying the same bindings.
        let mut state = opened.0.snapshot().unwrap().0;
        for s in state.seats.values_mut() {
            s.fields.insert(crate::mirror::BINDING.into(), json!({ "pane_id": s.id.0 }).to_string());
        }
        let reads = OrderReads::of(&store).unwrap();
        let later = now_ms() + 2 * ORDER_TICK_MS;
        assert_eq!(DeliveryColumns::read(&reads, Some(&state), later), DeliveryColumns::read(&reads, None, later));
        let api = Wip::from_api(&store, &state);
        assert_eq!((&api.unsent, &api.dropped, &api.refusing), (&w.unsent, &w.dropped, &w.refusing));
        assert_eq!(api.stuck.keys().collect::<Vec<_>>(), ["w3:p1"]);
        drop(opened);
    }

    /// The columns' rules, on orders alone: `typed` past a tick or `queued`
    /// again after a counted refusal is unsent; the last settled order failed
    /// is refusing, unless an acknowledged one came after it.
    #[test]
    fn delivery_columns_follow_the_orders_states() {
        use wsp_store_contract::{Order, OrderId, OrderState as S, Principal, SeatId};
        let o = |id: &str, state: S, attempts: u32, first: bool, why: Option<&str>| Order {
            id: OrderId(id.into()),
            from: Principal::Person("ed".into()),
            to: SeatId(id.rsplit_once("-o").unwrap().0.into()),
            text: "go".into(),
            state,
            attempts,
            first,
            why: why.map(str::to_string),
        };
        let orders = [
            o("a-o1", S::Typed, 0, false, None),
            o("b-o1", S::Typed, 0, false, None),
            o("c-o1", S::Queued, 1, false, Some("busy")),
            o("d-o1", S::Queued, 0, false, None),
            o("e-o1", S::Failed, 3, false, Some("deaf")),
            o("f-o1", S::Failed, 3, false, Some("deaf")),
            o("f-o2", S::Acknowledged, 0, false, None),
        ];
        let lately = BTreeSet::from([OrderId("b-o1".into())]);
        let d = DeliveryColumns::of(orders.iter(), &lately);
        assert_eq!(d.unsent, BTreeSet::from(["a".to_string(), "c".to_string()]), "typed past a tick, or queued after a counted refusal");
        assert_eq!(d.refusing, BTreeMap::from([("e".to_string(), (3, "deaf".to_string()))]));
        assert!(d.dropped.is_empty());
    }

    #[test]
    fn wip_json_says_a_list_governor_answers_to_its_projects_governor() {
        let mut w = wip_world();
        std::rc::Rc::make_mut(&mut w.tasks).push(wip_task("data-001", "first of the run", Some("data"), "doing"));
        let mut data = crate::model::Project::new("data");
        data.parent = Some("wsp".into());
        w.index = Index::new(vec![crate::model::Project::new("wsp"), data]);
        let mut list = crate::model::Worklist::new("tonight", "the list");
        list.set_groups(&crate::model::parse_groups("- 1  data-001\n"));
        list.set_status(crate::model::WorklistStatus::Running);
        w.lists = vec![list];
        w.governors.insert("wsp".into(), json!({ "workspace": "w5", "pane": "w5:p1" }));
        w.governors.insert("tonight".into(), json!({ "workspace": "w6", "pane": "w6:p1" }));
        for seat in ["w5:p1", "w6:p1"] {
            w.agents.push(seated_agent(seat, "working", seat));
        }
        let row = |j: &Value, pane: &str| j["agents"].as_array().unwrap().iter().find(|r| r["pane"] == pane).unwrap().clone();

        let j = wip_json(&w);
        assert_eq!(row(&j, "w6:p1")["answers_to"], "wsp", "its home is data, unseated, and wsp is above it");
        assert_eq!(row(&j, "w6:p1")["answers_to_empty"], Value::Null);
        assert_eq!(row(&j, "w5:p1")["answers_to"], Value::Null, "a project's governor answers to nobody in the census");
        assert_eq!(row(&j, "w1:p1")["answers_to"], "wsp", "and a member's row says so too since wsp-247");

        w.governors.remove("wsp");
        let j = wip_json(&w);
        assert_eq!(row(&j, "w6:p1")["answers_to"], Value::Null, "nobody seated on the walk");
        assert_eq!(row(&j, "w6:p1")["answers_to_empty"], "data", "and the home it would answer to");
    }

    /// `wsp-247`. Every row names the governor it answers to, on the walk its
    /// `wsp ask` would take: a run's member its list's seat, a member outside
    /// the run in a nested project the nearest governed ancestor, a verifier
    /// its member's, a check its list's, a list's governor its home project's,
    /// and a project's governor nobody. With nobody seated on the walk it is
    /// `null` beside the scope the walk starts at — and a governor's
    /// `members_waiting` counts the waiting rows that name it, verifier and all.
    #[test]
    fn wip_json_names_the_governor_every_seat_answers_to() {
        let mut w = wip_world();
        let mut data = crate::model::Project::new("data");
        data.parent = Some("wsp".into());
        w.index = Index::new(vec![crate::model::Project::new("wsp"), data]);
        std::rc::Rc::make_mut(&mut w.tasks).push(wip_task("data-001", "in tonight's run", Some("data"), "review"));
        std::rc::Rc::make_mut(&mut w.tasks).push(wip_task("data-002", "outside the run", Some("data"), "doing"));
        let mut p = crate::verification::Pass::opened(Some("abc1234".into()), None);
        p.pane = Some("w9:p1".into());
        crate::verification::write(std::rc::Rc::make_mut(&mut w.tasks).iter_mut().find(|t| t.id == "data-001").unwrap(), &[p]);
        let mut list = crate::model::Worklist::new("tonight", "the list");
        list.set_groups(&crate::model::parse_groups("- 1  data-001\n"));
        list.set_status(crate::model::WorklistStatus::Running);
        w.lists = vec![list];
        w.checking.insert("w10:p1".into(), ("tonight".into(), 1, Some("data".into())));
        w.bindings.insert("w7:p1".into(), json!({ "task_id": "data-001" }));
        w.bindings.insert("w8:p1".into(), json!({ "task_id": "data-002" }));
        w.governors.insert("wsp".into(), json!({ "workspace": "w5", "pane": "w5:p1" }));
        w.governors.insert("tonight".into(), json!({ "workspace": "w6", "pane": "w6:p1" }));
        for seat in ["w5:p1", "w6:p1", "w7:p1", "w8:p1", "w9:p1", "w10:p1"] {
            w.agents.push(seated_agent(seat, if seat == "w8:p1" { "idle" } else { "working" }, seat));
        }
        // Two seats under the list on a prompt, one under wsp behind a question.
        for seat in ["w7:p1", "w9:p1"] {
            w.agents.iter_mut().find(|a| a.seat.as_str() == seat).unwrap().state = State::Blocked;
        }
        w.asks = crate::waiting::Asks::one("w8:p1", "data-002", "q-1", crate::waiting::On::Seat("wsp".into()));
        let row = |j: &Value, pane: &str| j["agents"].as_array().unwrap().iter().find(|r| r["pane"] == pane).unwrap().clone();
        let answers = |j: &Value, pane: &str| (row(j, pane)["answers_to"].clone(), row(j, pane)["answers_to_empty"].clone());
        let to = |s: &str| (json!(s), Value::Null);
        let empty = |s: &str| (Value::Null, json!(s));

        let j = wip_json(&w);
        assert_eq!(row(&j, "w9:p1")["role"]["is"], "verifier", "the fixture is what it says: {j}");
        assert_eq!(row(&j, "w10:p1")["role"]["is"], "check", "{j}");
        assert_eq!(answers(&j, "w7:p1"), to("tonight"), "a run's member: its list's seat");
        assert_eq!(answers(&j, "w8:p1"), to("wsp"), "outside the run, data is unseated and wsp is above it");
        assert_eq!(answers(&j, "w1:p1"), to("wsp"), "a member in a governed project: that project's seat");
        assert_eq!(answers(&j, "w9:p1"), to("tonight"), "a verifier: its member's");
        assert_eq!(answers(&j, "w10:p1"), to("tonight"), "a check: its list's");
        assert_eq!(answers(&j, "w6:p1"), to("wsp"), "a list's governor: its home project's");
        assert_eq!(answers(&j, "w5:p1"), (Value::Null, Value::Null), "a project's governor: nobody");
        assert_eq!(row(&j, "w6:p1")["members_waiting"], 2, "the member and its verifier, off the same field");
        assert_eq!(row(&j, "w5:p1")["members_waiting"], 1, "the member behind its question");

        // Nobody seated on the project's walk: null, and where it would start.
        w.governors.remove("wsp");
        let j = wip_json(&w);
        assert_eq!(answers(&j, "w8:p1"), empty("data"), "the walk starts at its own project");
        assert_eq!(answers(&j, "w1:p1"), empty("wsp"));
        assert_eq!(answers(&j, "w6:p1"), empty("data"), "the list's home");
        assert_eq!(answers(&j, "w7:p1"), to("tonight"), "the run's seat still stands");

        // And nobody on the list's either: the walk starts at the list.
        w.governors.remove("tonight");
        let j = wip_json(&w);
        assert_eq!(answers(&j, "w7:p1"), empty("tonight"));
        assert_eq!(answers(&j, "w9:p1"), empty("tonight"));
        assert_eq!(answers(&j, "w10:p1"), empty("tonight"));
    }

    /// `docs/legs.md`, *Seen* (wsp-294). A leg holds no claim, so `wip` reads
    /// it off the row's `## Legs`: both running siblings are rows, each
    /// typed as a leg with `leg: {name, after, index, of}`, the label is
    /// `<task> · <leg>` with no `n/m`, and a leg that has written its baton
    /// needs nobody while the tick has yet to end its seat.
    #[test]
    fn wip_shows_both_running_siblings_of_a_staged_row_and_a_done_leg_needs_nobody() {
        use crate::legs::{Leg, State};
        let mut w = wip_world();
        let m = std::rc::Rc::make_mut(&mut w.tasks).iter_mut().find(|t| t.id == "t-001").unwrap();
        let mut plan = Leg::pending("plan", "decide");
        plan.state = State::Done;
        plan.pane = Some("cpd-40".into());
        let mut store_leg = Leg { after: Some(vec!["plan".into()]), ..Leg::pending("build-store", "store") };
        store_leg.state = State::Running;
        store_leg.pane = Some("cpd-41".into());
        let mut spawn_leg = Leg { after: Some(vec!["plan".into()]), ..Leg::pending("build-spawn", "spawn") };
        spawn_leg.state = State::Running;
        spawn_leg.pane = Some("cpd-42".into());
        let finish = Leg { after: Some(vec!["build-store".into(), "build-spawn".into()]), ..Leg::pending("finish", "land") };
        crate::legs::write(m, &[plan, store_leg, spawn_leg, finish]);
        // The three bound to the row, as `leg_seat::attach` binds them, and none
        // of them holding its claim.
        for seat in ["cpd-40", "cpd-41", "cpd-42"] {
            w.bindings.insert(seat.into(), json!({ "task_id": "t-001" }));
            w.agents.push(seated_agent(seat, if seat == "cpd-40" { "idle" } else { "working" }, seat));
        }

        let j = wip_json(&w);
        let row = |pane: &str| j["agents"].as_array().unwrap().iter().find(|r| r["pane"] == pane).unwrap().clone();
        assert_eq!(row("cpd-41")["role"], json!({ "is": "leg", "task": "t-001", "leg": "build-store" }));
        assert_eq!(row("cpd-42")["role"], json!({ "is": "leg", "task": "t-001", "leg": "build-spawn" }));
        assert_eq!(row("cpd-41")["leg"], json!({ "name": "build-store", "after": ["plan"], "index": 2, "of": 4 }));
        assert_eq!(
            row("cpd-42")["leg"],
            json!({ "name": "build-spawn", "after": ["plan"], "index": 3, "of": 4 }),
            "the sibling is a row of its own"
        );
        assert_eq!(row("cpd-40")["leg"], json!({ "name": "plan", "after": [], "index": 1, "of": 4 }));
        assert_eq!(row("cpd-41")["task"], "t-001 · build-store", "no n/m");
        assert_eq!(row("cpd-41")["task_id"], "t-001", "still the row's");
        assert_eq!(row("cpd-41")["model"], "sonnet", "the entry's policy, with no claim to read it from");
        assert_eq!(row("cpd-41")["effort"], "medium");
        // Idle on a `doing` row is `needs_you` for a member; for a leg that has
        // written its baton it is the tick's to end.
        assert_eq!(row("cpd-40")["needs_you"], false, "a done leg's seat waits on the tick");
        assert_eq!(row("w2:p1")["needs_you"], true, "and a member still does");
        assert_eq!(row("w1:p1")["leg"], json!(null), "a seat that is no leg says so");

        let text = wip_lines(&w, &Paint::new(), false).join("\n");
        assert!(text.contains("t-001 · build-store"), "{text}");
        assert!(text.contains("t-001 · build-spawn"), "{text}");
        assert!(text.contains("legs 2 running, 1 done of 4"), "{text}");
    }

    /// `docs/pieces.md`, *Seen and told* (wsp-508). A piece holds no claim, so
    /// `wip` reads it off the row's `## Pieces`: two running pieces of one row
    /// are two rows, each `role: {is: piece, task, piece}`, labelled
    /// `<row>-<n> <title>`, the row's count and batch said on the line, and a
    /// done piece whose seat the tick has yet to end needs nobody.
    #[test]
    fn wip_shows_two_running_pieces_of_one_row_each_on_its_own_line() {
        use crate::pieces::{Piece, Record, State};
        let mut w = wip_world();
        let m = std::rc::Rc::make_mut(&mut w.tasks).iter_mut().find(|t| t.id == "t-001").unwrap();
        let piece = |n: usize, title: &str, batch: usize, state: State, pane: Option<&str>| Piece {
            state,
            pane: pane.map(str::to_string),
            ..Piece::pending(n, title, batch)
        };
        let rec = Record {
            cap: 3,
            closed: Vec::new(),
            pieces: vec![
                piece(1, "types", 1, State::Done, Some("cpd-40")),
                piece(2, "board component", 2, State::Running, Some("cpd-41")),
                piece(3, "rail component", 2, State::Running, Some("cpd-42")),
                piece(4, "docs", 3, State::Pending, None),
            ],
        };
        crate::pieces::write(m, &rec);
        for seat in ["cpd-40", "cpd-41", "cpd-42"] {
            w.bindings.insert(seat.into(), json!({ "task_id": "t-001" }));
            w.agents.push(seated_agent(seat, if seat == "cpd-40" { "idle" } else { "working" }, seat));
        }

        let j = wip_json(&w);
        let row = |pane: &str| j["agents"].as_array().unwrap().iter().find(|r| r["pane"] == pane).unwrap().clone();
        assert_eq!(row("cpd-41")["role"], json!({ "is": "piece", "task": "t-001", "piece": 2 }));
        assert_eq!(row("cpd-42")["role"], json!({ "is": "piece", "task": "t-001", "piece": 3 }), "the sibling is a row of its own");
        assert_eq!(row("cpd-41")["task"], "t-001-2 board component");
        assert_eq!(row("cpd-41")["task_id"], "t-001", "still the row's");
        assert_eq!(row("cpd-41")["model"], "sonnet", "the entry's policy, with no claim to read it from");
        assert_eq!(row("cpd-40")["needs_you"], false, "a done piece's seat waits on the tick");
        assert_eq!(row("w2:p1")["needs_you"], true, "and a member still does");

        let text = wip_lines(&w, &Paint::new(), false).join("\n");
        assert!(text.contains("t-001-2 board component"), "{text}");
        assert!(text.contains("t-001-3 rail component"), "{text}");
        assert!(text.contains("pieces 2 running, 1 done of 4 · batch 2 of 3"), "{text}");
    }

    /// `wsp tell <piece-id>` reaches the entry's seat, and the addressee is the
    /// piece's id; a piece with no seat, or a row with no such piece, reaches none.
    #[test]
    fn tell_resolves_a_piece_id_to_its_entrys_seat() {
        let iso = util::isolated("tell-piece");
        let store = Store::at(iso.home(), iso.state());
        store.ensure_dirs().unwrap();
        let mut t = Task::new("a row", "m-1");
        t.set_status(Status::Doing);
        let rec = crate::pieces::Record {
            pieces: vec![
                crate::pieces::Piece {
                    state: crate::pieces::State::Running,
                    pane: Some("cpd-7".into()),
                    ..crate::pieces::Piece::pending(1, "board", 1)
                },
                crate::pieces::Piece::pending(2, "rail", 1),
            ],
            ..Default::default()
        };
        crate::pieces::write(&mut t, &rec);
        store.save_task(&t).unwrap();
        let got = target(&store, "m-1-1", &|_| false);
        assert_eq!(got, Some(("cpd-7".to_string(), "m-1-1 — board".to_string())));
        assert_eq!(addressee_id(&store, "m-1-1", "m-1-1 — board"), "m-1-1");
        assert_eq!(target(&store, "m-1-2", &|_| false), None, "a piece with no seat");
        assert_eq!(target(&store, "m-1-9", &|_| false), None, "a row with no such piece");
        drop(iso);
    }

    /// `wsp say` from a piece's pane speaks as `<row>-<n>`.
    #[test]
    fn say_from_a_pieces_pane_speaks_as_the_piece() {
        let iso = util::isolated("say-piece");
        let store = Store::at(iso.home(), iso.state());
        store.ensure_dirs().unwrap();
        let mut t = Task::new("a row", "wsp-901");
        t.set_status(Status::Doing);
        store.save_task(&t).unwrap();
        store.set_binding("cpd-8", json!({ "task_id": "wsp-901", "pane_id": "cpd-8", "piece": 2 }));
        store.set_binding("cpd-9", json!({ "task_id": "wsp-901", "pane_id": "cpd-9" }));
        let said = |pane: &str| {
            let code = say(&store, &Args::synth("say", &["running the whole suite over every crate once more"], &[("pane", pane)]));
            assert_eq!(code, 0);
            store.said()[pane]["full"].as_str().unwrap_or_default().to_string()
        };
        assert_eq!(said("cpd-8"), "wsp-901-2 · running the whole suite over every crate once more");
        assert_eq!(said("cpd-9"), "wsp-901 · running the whole suite over every crate once more", "a member says it as the row");
        drop(iso);
    }

    /// `wsp-246`. Every row says what its seat is, typed, so a reader placing
    /// it never parses `task` — one seat of each kind, each named off the
    /// record that makes it that: a project's and a list's governor off
    /// `governors.json` and the worklists, a member off its binding and the
    /// running list it is in, a verifier off the member's pass (its second,
    /// the first one ended), a check off its list's group entry.
    #[test]
    fn wip_json_types_each_seats_role() {
        let mut w = wip_world();
        let m = std::rc::Rc::make_mut(&mut w.tasks).iter_mut().find(|t| t.id == "t-004").unwrap();
        let mut first = crate::verification::Pass::opened(Some("abc1234".into()), None);
        first.pane = Some("cpd-484".into());
        first.state = crate::verification::State::Blocks;
        first.ending = crate::verification::Ending::Ended;
        let mut again = crate::verification::Pass::opened(Some("def5678".into()), None);
        again.pane = Some("cpd-485".into());
        crate::verification::write(m, &[first, again]);
        // The list's first member lives in another project than the list's
        // other work: the list is what its governor governs, wherever the row
        // is drawn.
        std::rc::Rc::make_mut(&mut w.tasks).push(wip_task("data-001", "first of the run", Some("data"), "review"));
        w.index = Index::new(vec![crate::model::Project::new("wsp"), crate::model::Project::new("data")]);
        let mut list = crate::model::Worklist::new("gpui-migration", "the list");
        list.set_groups(&crate::model::parse_groups(
            "- 1  data-001\n  checks: 2026-10-10T09:20:00Z running read:55c4839abcde pane:cpd-486\n- 2  t-001\n",
        ));
        list.set_status(crate::model::WorklistStatus::Running);
        w.checking = checking(std::slice::from_ref(&list), &w.tasks);
        w.lists = vec![list];
        w.governors.insert("wsp".into(), json!({ "workspace": "w5", "pane": "w5:p1" }));
        w.governors.insert("gpui-migration".into(), json!({ "workspace": "w6", "pane": "w6:p1" }));
        for seat in ["cpd-485", "cpd-486", "w5:p1", "w6:p1"] {
            w.agents.push(seated_agent(seat, "working", seat));
        }

        let j = wip_json(&w);
        let role = |pane: &str| {
            j["agents"].as_array().unwrap().iter().find(|r| r["pane"] == pane).unwrap_or_else(|| panic!("no row {pane}"))
                ["role"]
                .clone()
        };
        assert_eq!(role("w5:p1"), json!({ "is": "governor", "of": "project", "id": "wsp" }));
        assert_eq!(role("w6:p1"), json!({ "is": "governor", "of": "list", "id": "gpui-migration" }));
        assert_eq!(
            role("w1:p1"),
            json!({ "is": "member", "task": "t-001", "list": "gpui-migration", "group": 2 }),
            "a member of a running list says which group"
        );
        assert_eq!(role("w2:p1"), json!({ "is": "member", "task": "t-002" }), "and one on no list says none");
        assert_eq!(role("cpd-485"), json!({ "is": "verifier", "member": "t-004", "pass": 2 }), "verified again");
        assert_eq!(role("cpd-486"), json!({ "is": "check", "list": "gpui-migration", "group": 1 }));
        assert_eq!(role("w3:p1"), json!({ "is": "agent" }));
        // `task` keeps its text for the TUI.
        let task = |pane: &str| j["agents"].as_array().unwrap().iter().find(|r| r["pane"] == pane).unwrap()["task"].clone();
        assert_eq!(task("cpd-485"), "t-004 · verifying");
        assert_eq!(task("cpd-486"), "gpui-migration · barrier 1");
    }

    /// `wsp-248`. Each seat's kind, model and effort, read off the record that
    /// started it — and each fixture written in that record's own spelling, so
    /// a change to how one is written fails here rather than reading as `null`
    /// in compound's Agents view: a member's claim (`agent_kind`, and `spawned
    /// at` on its log line), a verifier's pass (`on claude opus/high`), a
    /// check's entry (`on:` with its spaces as `+`), a governor's record in
    /// `governors.json`. A seat with none of them, a governor whose tier
    /// wsp-117 left unwritten, and a member whose newest claim line is another
    /// pane's give `null` rather than a guess.
    #[test]
    fn wip_json_says_what_runs_in_each_seat_from_the_record_that_started_it() {
        let mut w = wip_world();
        let log = |t: &mut Task, line: &str| t.body = format!("## Log\n- {line}\n");
        for t in std::rc::Rc::make_mut(&mut w.tasks).iter_mut() {
            match t.id.as_str() {
                "t-001" => log(t, "2026-10-10T09:00:00Z claimed by pane w1:p1 · spawned at sonnet[1m]/high"),
                "t-002" => log(t, "2026-10-10T09:00:00Z claimed by pane w2:p1 · spawned at opencode/x-preview-f-free/default"),
                "t-003" => log(t, "2026-10-10T09:00:00Z claimed by pane w9:p1 · spawned at opus/high"),
                "t-004" => {
                    t.body = "## Verification\n- 2026-10-10T09:10:00Z running · read abc1234 · pane cpd-485 · on claude opus/high\n".into()
                }
                _ => {}
            }
        }
        w.claims.insert("t-001".into(), json!({ "agent_kind": "claude", "claimed_at": "2026-10-10T09:00:00Z" }));
        // A claim from before the claim carried a kind: the binding has it.
        w.claims.insert("t-002".into(), json!({ "claimed_at": "2026-10-10T09:00:00Z" }));
        w.bindings.get_mut("w2:p1").unwrap()["agent_kind"] = json!("opencode");
        // t-003 is bound here, but its newest claim line is another pane's.
        w.bindings.insert("w7:p1".into(), json!({ "task_id": "t-003", "agent_kind": "claude" }));
        let groups = crate::model::parse_groups(
            "- 1  m-1\n  checks: 2026-10-10T09:20:00Z running read:55c4839abcde pane:cpd-486\n          on:pi+openrouter/some-model/high\n",
        );
        let mut list = crate::model::Worklist::new("gpui-migration", "the list");
        list.set_groups(&groups);
        w.check_on = crate::barrier::ran_on(&[list]);
        w.governors.insert(
            "wsp".into(),
            json!({ "workspace": "w5", "pane": "w5:p1", "kind": "claude", "model": "opus", "effort": "xhigh" }),
        );
        w.governors.insert("run".into(), json!({ "workspace": "w6", "pane": "w6:p1", "kind": "claude", "model": "", "effort": "" }));
        for seat in ["cpd-485", "cpd-486", "w5:p1", "w6:p1", "w7:p1"] {
            w.agents.push(seated_agent(seat, "working", seat));
        }

        let j = wip_json(&w);
        let runs = |pane: &str| {
            let r = j["agents"].as_array().unwrap().iter().find(|r| r["pane"] == pane).unwrap_or_else(|| panic!("no row {pane}"));
            (r["kind"].clone(), r["model"].clone(), r["effort"].clone())
        };
        let n = serde_json::Value::Null;
        assert_eq!(runs("w1:p1"), (json!("claude"), json!("sonnet[1m]"), json!("high")), "a member, off its claim");
        assert_eq!(
            runs("w2:p1"),
            (json!("opencode"), json!("opencode/x-preview-f-free"), n.clone()),
            "the effort is after the last `/`, and `default` is nobody having said"
        );
        assert_eq!(runs("cpd-485"), (json!("claude"), json!("opus"), json!("high")), "a verifier, off its pass");
        assert_eq!(
            runs("cpd-486"),
            (json!("pi"), json!("openrouter/some-model"), json!("high")),
            "a check, off its entry, the model's own `/` kept"
        );
        assert_eq!(runs("w5:p1"), (json!("claude"), json!("opus"), json!("xhigh")), "a governor, off its record");
        assert_eq!(runs("w6:p1"), (json!("claude"), n.clone(), n.clone()), "wsp-117's empty tier is not a tier");
        assert_eq!(runs("w7:p1"), (json!("claude"), n.clone(), n.clone()), "another pane's claim line is not this seat's tier");
        assert_eq!(runs("w3:p1"), (n.clone(), n.clone(), n.clone()), "no record, nothing guessed");
        let bare = j["agents"].as_array().unwrap().iter().find(|r| r["pane"] == "w3:p1").unwrap();
        for k in ["kind", "model", "effort"] {
            assert!(bare.get(k).is_some(), "the key `{k}` is always present, as `null`");
        }
    }

    /// `wsp-145`. The heading counts agents and turns, which is a true
    /// sentence about *work* and says nothing about the pass that decides
    /// whether a seat is ever woken at all. Six days of a dead daemon on a
    /// machine that read `3 agents · 2 running a turn` the entire time is the
    /// cost, and the whole of the cost was that nothing here said so.
    ///
    /// Asserted on both shapes it can be in — with agents and with none — since
    /// the empty machine is the one where `wip` is the first thing anybody runs.
    #[test]
    fn a_machine_whose_daemon_is_gone_says_so_at_the_top_of_the_answer() {
        let mut w = wip_world();
        assert!(
            !wip_lines(&w, &Paint::new(), false).join("\n").contains("no wsp daemon"),
            "said it when the machine has a daemon"
        );

        w.daemon = crate::daemon::loud(Some(&[]), true);
        let text = wip_lines(&w, &Paint::new(), false).join("\n");
        assert!(text.contains("no wsp daemon"), "{text}");
        // Above the rows and above the queues: this is the state of the machine,
        // not a line about one agent, and it is what a person came here to read.
        assert!(
            text.lines().position(|l| l.contains("no wsp daemon"))
                < text.lines().position(|l| l.contains("PROJECT")),
            "it was drawn underneath the table: {text}"
        );
        // And on the empty machine too, where `wip` is the first thing run.
        let mut bare = wip_world();
        bare.agents.clear();
        assert!(
            wip_lines(&bare, &Paint::new(), false).join("\n").contains("no agents running"),
            "the fixture does not reach the empty branch"
        );
        bare.daemon = crate::daemon::loud(Some(&[]), true);
        assert!(
            wip_lines(&bare, &Paint::new(), false).join("\n").contains("no wsp daemon"),
            "an empty machine is not a machine that does not need a daemon"
        );
        // Published for a reader that does not draw: `doctor` is the only other
        // place this is said, and a script cannot ask whether *it* would have
        // seen a daemon.
        assert!(wip_json(&w)["daemon"].as_str().unwrap().contains("no wsp daemon"));
    }

    /// `wsp-331`. The throttle's mode and whether it holds now are on the
    /// answer every time, with agents or without, whatever the mode and with no
    /// load warning up: a line that appeared only under load would leave the
    /// person unable to tell `off` from `auto` while the machine was fine.
    #[test]
    fn the_throttle_line_is_on_wip_in_every_mode_with_and_without_agents() {
        for line in ["throttle: off — builds run", "throttle: on — in force", "throttle: auto — in force, the person is here"] {
            let mut w = wip_world();
            w.throttle = Some(line.to_string());
            let text = wip_lines(&w, &Paint::new(), false).join("\n");
            assert!(text.contains(line), "{text}");
            assert!(
                text.lines().position(|l| l.contains(line)) < text.lines().position(|l| l.contains("PROJECT")),
                "drawn under the table: {text}"
            );
            assert_eq!(wip_json(&w)["throttle"].as_str(), Some(line));
            let mut bare = wip_world();
            bare.agents.clear();
            bare.throttle = Some(line.to_string());
            assert!(wip_lines(&bare, &Paint::new(), false).join("\n").contains(line));
        }
    }

    /// `wsp-338`. The idle panes' total is on the answer with agents or
    /// without, and the warning is there past the threshold and absent under it.
    #[test]
    fn idle_pane_memory_shows_its_total_and_warns_only_past_the_threshold() {
        use crate::memory::{IdleMemory, Pane};
        let held = |mb: &[u64], warn_mb: u64| IdleMemory {
            panes: mb.iter().enumerate().map(|(i, m)| Pane { seat: format!("w{i}:p1"), name: format!("wsp-{i}"), kb: m * 1024 }).collect(),
            unread: 0,
            warn_kb: warn_mb * 1024,
        };
        for terse in [false, true] {
            let mut w = wip_world();
            w.idle_memory = held(&[300, 200], 1000);
            let text = wip_lines(&w, &Paint::new(), terse).join("\n");
            assert!(text.contains("idle panes: 2 holding 500 MB"), "{text}");
            assert!(!text.contains("warning"), "under the threshold: {text}");
            assert_eq!(wip_json(&w)["idle_memory"]["over"], false);

            w.idle_memory = held(&[900, 800], 1000);
            let text = wip_lines(&w, &Paint::new(), terse).join("\n");
            assert!(text.contains("idle panes: 2 holding 1.7 GB"), "{text}");
            assert!(text.contains("idle panes hold 1.7 GB, over the 1000 MB warning — largest: wsp-0 (900 MB), wsp-1 (800 MB)"), "{text}");
            assert_eq!(wip_json(&w)["idle_memory"]["total_kb"], 1700 * 1024);
            assert_eq!(wip_json(&w)["idle_memory"]["over"], true);
        }
        let mut bare = wip_world();
        bare.agents.clear();
        assert!(wip_lines(&bare, &Paint::new(), false).join("\n").contains("idle panes: none"));
    }

    /// wsp-639: a far seat on a machine whose beat is late is not known to have
    /// stopped, so it does not draw `← needs you` off the last answer.
    #[test]
    fn wip_does_not_call_a_far_seat_that_needs_you_from_a_late_beat() {
        let mut w = wip_world();
        assert!(wip_rows(&w).iter().any(|r| r.pane == "w2:p1" && r.needs_you));
        w.silent.late.insert("w2:p1".into());
        assert!(wip_rows(&w).iter().all(|r| !r.needs_you), "a stale read asks nobody");
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
        busy.agents[1].state = of_word("working");
        assert!(wip_rows(&busy).iter().all(|r| !r.needs_you));
        let text = wip_lines(&busy, &Paint::new(), false).join("\n");
        assert!(!text.contains("need you"), "{text}");
        assert!(
            !text.contains("all busy"),
            "the sentence this printed over five stopped agents on 2026-08-19:\n{text}"
        );
        assert!(text.contains("3 running a turn"), "{text}");
    }

    /// The other two words a runner has for a pane with no turn running, and this
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
            w.agents[1].state = of_word(word);
            let rows = wip_rows(&w);
            let flagged: Vec<&str> =
                rows.iter().filter(|r| r.needs_you).map(|r| r.pane.as_str()).collect();
            assert_eq!(flagged, ["w2:p1"], "`{word}` is a pane running no turn");
        }

        // And a word no runner has ever sent is still not evidence of work. An
        // absence stays an absence — the rule the whole port is built on — so
        // it is not called stopped either.
        let mut w = wip_world();
        w.agents[1].state = of_word("something-new");
        assert!(wip_rows(&w).iter().all(|r| !r.needs_you), "not knowing is not knowing it stopped");
    }

    /// `state` on the row is wsp's own vocabulary (`State::as_str`) —
    /// `working`/`idle`/`blocked`/`gone`/`starting`/`empty`/`unknown`, not a
    /// runner's `idle`/`working`/`blocked`/`done`. A caller that fed it back
    /// through a runner's word table — as `cmd_watch` did before this row —
    /// would be reading it as a raw word and losing `starting` and `gone`,
    /// which `of_word` has no arm for. `state_typed` is the port's own
    /// answer, carried alongside rather than re-derived, so it survives the
    /// round trip.
    #[test]
    fn the_typed_state_survives_words_of_word_cannot_read_back() {
        for word in ["working", "idle", "blocked", "done", "something-new"] {
            let mut w = wip_world();
            w.agents[1].state = of_word(word);
            let row = wip_rows(&w).into_iter().find(|r| r.pane == "w2:p1").unwrap();
            assert_eq!(row.state_typed, w.agents[1].state, "{word}");
            assert_eq!(row.state, w.agents[1].state.as_str(), "{word}");
        }

        // `starting` and `gone` are real answers a backend's census can give
        // (`place_compound`, `place_super`), and neither is in `of_word`'s
        // table — that function only understands a runner's raw vocabulary. Re-parsing the row's string
        // through it loses both; reading `state_typed` does not.
        for state in [crate::place::State::Starting, crate::place::State::Gone] {
            let mut w = wip_world();
            w.agents[1].state = state;
            let row = wip_rows(&w).into_iter().find(|r| r.pane == "w2:p1").unwrap();
            assert_eq!(row.state_typed, state);
            assert_ne!(
                of_word(&row.state),
                state,
                "{state:?} round-tripped through of_word, which is the bug"
            );
        }
    }

    /// A pane holding nothing still appears — `wip` is who is running, not who
    /// has claimed — and says what it can about itself rather than nothing.
    #[test]
    fn an_agent_holding_nothing_is_still_an_agent() {
        let rows = wip_rows(&wip_world());
        let bare = rows.iter().find(|r| r.pane == "w3:p1").expect("a pane with no binding is still a row");
        assert_eq!(bare.task, "(reading the README)", "the seat's own label is the best on offer");
        assert!(bare.task_id.is_empty());
        assert!(!bare.needs_you, "holding nothing cannot be blocked on you");

        // With not even a label, it says so rather than leaving the cell blank.
        let mut w = wip_world();
        w.agents[2].label = String::new();
        let rows = wip_rows(&w);
        assert_eq!(rows.iter().find(|r| r.pane == "w3:p1").unwrap().task, "(unbound)");
    }

    /// A mixed census must resolve every row rather than only the
    /// `<workspace>:<pane>`-shaped ones — this is the join each row goes
    /// through, proven against a seat with
    /// no `:` in it at all, which is what a compound session mints.
    #[test]
    fn a_compound_seat_resolves_beside_a_workspace_pane_in_the_same_census() {
        let mut w = wip_world();
        w.bindings.insert("cpd-0".to_string(), json!({ "task_id": "t-002" }));
        w.agents.push(seated_agent("cpd-0", "working", "compound-064"));

        let rows = wip_rows(&w);
        let cpd = rows.iter().find(|r| r.pane == "cpd-0").expect("the compound seat is still a row");
        assert_eq!(cpd.project, "wsp", "a task binding resolves the project with no workspace at all");
        assert_eq!(cpd.workspace, "cpd-0", "no `:` to split, so the seat stands for its own workspace");
        assert!(cpd.turning);

        // And it does not disturb the pane rows already resolved beside it.
        assert!(rows.iter().find(|r| r.pane == "w2:p1").unwrap().needs_you);
    }

    /// `wsp-603`: ending the seat that does not hold a row's claim ends that
    /// seat's binding and leaves the claim, and the holder's binding, standing.
    #[test]
    fn releasing_a_seat_that_is_not_the_claims_leaves_the_claim_with_its_holder() {
        let _env = util::isolated("release-the-twin");
        let store = Store::open();
        store.save_task(&wip_task("t-001", "doubled", Some("wsp"), "doing")).unwrap();
        store.set_agent("a-1", json!({ "seat": "cpd-1", "started": "2026-10-01T00:00:00Z" }));
        store.set_agent("a-2", json!({ "seat": "cpd-2", "started": "2026-10-01T00:00:01Z" }));
        store.set_binding("cpd-1", json!({ "task_id": "t-001", "pane_id": "cpd-1" }));
        store.set_binding("cpd-2", json!({ "task_id": "t-001", "pane_id": "cpd-2" }));
        store.set_claim("t-001", json!({ "agent_id": "a-2", "claimed_at": "2026-10-01T00:00:02Z" }));

        let (removed, released) = release_pane(&store, "cpd-1");
        assert!(removed && released.is_none(), "its own binding goes and no claim is released");
        assert!(store.claims().contains_key("t-001"), "the claim stays");
        assert!(store.bindings().contains_key("cpd-2"), "with its holder");

        let panes = store.panes_for_task("t-001");
        let held = crate::address::claim_holder(&store.claims()["t-001"], &panes, &store.agents_held());
        assert_eq!(held.as_deref(), Some("cpd-2"));

        let (_, released) = release_pane(&store, "cpd-2");
        assert_eq!(released.as_deref(), Some("t-001"), "the holder's release still ends the claim");
        assert!(!store.claims().contains_key("t-001"));
    }

    /// `wsp-603`: `wsp tell <row>` goes to the seat that holds the claim, and
    /// says so, when the row has two seats and the unclaimed one is bound first.
    #[test]
    fn a_tell_to_a_row_with_two_seats_reaches_the_one_holding_the_claim() {
        let _env = util::isolated("tell-the-holder");
        let store = Store::open();
        store.save_task(&wip_task("t-001", "doubled", Some("wsp"), "doing")).unwrap();
        store.set_agent("a-1", json!({ "seat": "cpd-1", "started": "2026-10-01T00:00:00Z" }));
        store.set_agent("a-2", json!({ "seat": "cpd-2", "started": "2026-10-01T00:00:01Z" }));
        store.set_binding("cpd-1", json!({ "task_id": "t-001", "pane_id": "cpd-1" }));
        store.set_binding("cpd-2", json!({ "task_id": "t-001", "pane_id": "cpd-2" }));
        store.set_claim("t-001", json!({ "agent_id": "a-2", "claimed_at": "2026-10-01T00:00:02Z" }));

        let (pane, what) = target(&store, "t-001", &|_| false).expect("the row has a seat");
        assert_eq!(pane, "cpd-2", "the claim's seat, not the first one bound");
        assert!(what.contains("cpd-2 holds the claim"), "and it is named: {what}");

        // With no claim there is no holder to name, and the row still reaches a seat.
        store.clear_claim("t-001");
        let (_, what) = target(&store, "t-001", &|_| false).expect("a seat still");
        assert!(!what.contains("holds the claim"), "{what}");
    }

    /// `WSP_HOST=manager` with a manager that does not answer: a tell to a seat
    /// this machine holds is held (and said so), not refused, and a flush with
    /// the manager still silent keeps it.
    #[test]
    fn a_tell_to_a_silent_manager_is_held_and_the_flush_keeps_it() {
        let env = util::isolated("tell-silent-manager");
        let store = Store::open();
        let exec = store.state.join("exec");
        std::fs::create_dir_all(&exec).unwrap();
        std::fs::write(exec.join(crate::manager::TOKEN_FILE), "some-token\n").unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let mut local = crate::manager::Local::new(&exec, port);
        local.host = "test-host".into();
        crate::manager::install_record(&store.state, &local, std::time::Duration::from_secs(1)).unwrap();
        std::fs::create_dir_all(crate::place_compound::Compound::new().root.join("cpd-held")).unwrap();
        let _ = &env;

        std::env::set_var("WSP_HOST", "manager");
        let args = Args::synth("tell", &["cpd-held", "carry on"], &[]);
        let code = tell_seat(
            &store, &args, &Paint::new(), "me", None, "carry on", "cpd-held", true, None, "cpd-held".into(), "pane cpd-held".into(),
        );
        let (flushed, held) = (crate::wake::flush_held_tells_now(&store), crate::wake::held_tells(&store, "cpd-held"));
        std::env::remove_var("WSP_HOST");
        assert_eq!(code, 0, "held is not a failure");
        assert_eq!(held.len(), 1, "one tell waits: {held:?}");
        assert!(held[0].1.contains("carry on"));
        assert_eq!(flushed, (0, 0), "a silent manager leaves it held");
    }

    /// A compound seat directory whose pid is dead, last written `ago` seconds
    /// before now: the shape `wip` calls `gone`, dated by the only clock a dead
    /// agent leaves.
    fn gone_seat(env: &util::Isolated, name: &str, ago: i64) {
        let _ = env;
        let compound = crate::place_compound::Compound::new();
        let dir = compound.root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        // A pid no process holds: `i32::MAX` is past any pid macOS or Linux hands out.
        let rec = json!({ "pid": i32::MAX, "agent": { "kind": "claude", "name": "t-1", "args": [] } });
        std::fs::write(dir.join("seat.json"), rec.to_string()).unwrap();
        let then = std::time::SystemTime::now() - std::time::Duration::from_secs(ago as u64);
        for f in [dir.join("seat.json"), dir.clone()] {
            std::fs::File::open(&f).unwrap().set_modified(then).unwrap();
        }
    }

    /// `wsp-169`, wsp-158 item 5. Twenty `gone` rows from runs that ended days
    /// ago were drawn on every `wip`, and nothing removed them. Three seats, one
    /// directory each: long gone with nothing, gone a moment ago, and long gone
    /// but still holding a claim — and only the first stops being drawn.
    #[test]
    fn a_long_gone_seat_holding_nothing_is_not_drawn_and_the_rest_are() {
        let env = util::isolated("wip-sweeps-the-long-gone");
        let store = Store::open();
        gone_seat(&env, "cpd-1", 3 * GONE_FOR);
        gone_seat(&env, "cpd-2", 60);
        gone_seat(&env, "cpd-3", 3 * GONE_FOR);
        store.save_task(&wip_task("t-001", "work a gone agent left open", Some("wsp"), "doing")).unwrap();
        store.set_agent("a-held", json!({ "seat": "cpd-3", "started": "2026-10-01T00:00:00Z" }));
        store.set_claim("t-001", json!({ "agent_id": "a-held", "claimed_at": "2026-10-01T00:00:01Z" }));

        let w = Wip::live(&store);
        let panes: Vec<&str> = w.agents.iter().map(|s| s.seat.as_str()).collect();
        assert!(!panes.contains(&"cpd-1"), "long gone and holding nothing is a record of nothing: {panes:?}");
        assert!(panes.contains(&"cpd-2"), "gone a moment ago is still news: {panes:?}");
        assert!(panes.contains(&"cpd-3"), "holding a claim is the reconciler's, not swept: {panes:?}");

        // And it says why it is still there, rather than reading as a row nobody got to.
        let text = wip_lines(&w, &Paint::new(), false).join("\n");
        let held = text.lines().find(|l| l.contains("cpd-3")).expect("the held seat is drawn");
        assert!(held.contains("holds t-001"), "{held}");
        assert!(!text.lines().find(|l| l.contains("cpd-2")).unwrap().contains("holds"), "{text}");
    }

    /// The seat is dropped from the reading and not from the disk, so the same
    /// seat is drawn again the moment something holds it.
    #[test]
    fn a_swept_seat_returns_when_a_binding_names_an_open_task_again() {
        let env = util::isolated("wip-sweep-is-a-reading");
        let store = Store::open();
        gone_seat(&env, "cpd-1", 3 * GONE_FOR);
        assert!(Wip::live(&store).agents.is_empty());

        store.save_task(&wip_task("t-001", "work", Some("wsp"), "doing")).unwrap();
        store.set_binding("cpd-1", json!({ "task_id": "t-001" }));
        assert_eq!(Wip::live(&store).agents.len(), 1, "an open task bound to it holds it");

        store.save_task(&wip_task("t-001", "work", Some("wsp"), "done")).unwrap();
        assert!(Wip::live(&store).agents.is_empty(), "a binding to finished work holds nothing");
    }

    /// wsp-094: after a machine restart the state core holds every seat that
    /// ran, ended by the fold with no census word. Through the API, `wip`
    /// reads such a seat as gone (not unknown) and does not draw it once it
    /// has nothing to say; a gone seat that said something, or whose agent is
    /// on record, is still news, and one holding something (here a governor slot) is kept.
    #[test]
    fn wip_through_the_api_drops_a_seat_a_restart_ended_with_nothing_to_say() {
        use wsp_store_contract as c;
        let _env = util::isolated("wip-api-restart");
        let store = Store::open();
        let seat = |id: &str, fields: &[(&str, &str)]| c::Seat {
            id: c::SeatId(id.into()),
            machine: c::MachineName("here".into()),
            kind: "claude".into(),
            model: None,
            state: c::SeatState::Gone,
            before_lost: None,
            row: None,
            attempt: 0,
            pid: Some(19479),
            fields: fields.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        };
        let mut state = c::State::default();
        for s in [
            seat("cpd-1000", &[("epoch", "1")]),
            seat("cpd-1001", &[("epoch", "1"), ("census", "gone")]),
            seat("cpd-1002", &[("census", "gone")]),
            seat("cpd-1003", &[("census", "gone"), ("agent", r#"{"kind":"claude","name":"t-1","args":[]}"#)]),
            seat("cpd-1004", &[]),
        ] {
            state.seats.insert(s.id.clone(), s);
        }
        store.set_sentence("cpd-1002", "stopped on the barrier", None);
        store.set_governor("wsp", json!({ "workspace": "cpd-1004", "pane": "cpd-1004", "host": util::hostname() }));

        let w = Wip::from_api(&store, &state);
        let drawn: BTreeMap<&str, State> = w.agents.iter().map(|s| (s.seat.as_str(), s.state)).collect();
        assert!(!drawn.contains_key("cpd-1000"), "no census word, nothing to say: {drawn:?}");
        assert!(!drawn.contains_key("cpd-1001"), "gone, nothing to say: {drawn:?}");
        assert_eq!(drawn.get("cpd-1002"), Some(&State::Gone), "it said something: {drawn:?}");
        assert_eq!(drawn.get("cpd-1003"), Some(&State::Gone), "its agent is on record: {drawn:?}");
        assert_eq!(drawn.get("cpd-1004"), Some(&State::Gone), "it holds the governor's slot: {drawn:?}");
        assert!(drawn.values().all(|s| *s != State::Unknown), "{drawn:?}");
    }

    #[test]
    fn only_a_dated_gone_seat_past_the_threshold_with_nothing_held_is_forgotten() {
        let now = 1_000_000;
        let old = Some(now - GONE_FOR - 1);
        assert!(forgotten(State::Gone, old, now, false));
        assert!(!forgotten(State::Gone, Some(now - GONE_FOR + 1), now, false), "not yet");
        assert!(!forgotten(State::Gone, old, now, true), "held");
        assert!(!forgotten(State::Gone, None, now, false), "an undated seat is not a long-gone one");
        assert!(!forgotten(State::Idle, old, now, false), "only a gone seat is ever swept");
    }

    /// `compound-077`: `wsp tell` used to ask one backend alone whether a seat
    /// existed at all, so a `compound`-hosted agent read as "nothing holds
    /// this" no matter what it was doing. `locate_seat` is the fix, proven
    /// directly against a real compound seat —
    /// the case that used to fail before this row even reached the
    /// "no agent"/"blocked" checks below it.
    #[test]
    fn locate_seat_finds_a_compound_hosted_agent() {
        let _env = util::isolated("locate-seat-compound");
        let compound = crate::place_compound::Compound::new();
        let seat = compound.open(&crate::place::Order::default()).expect("a compound seat");
        let dir = compound.dir_of(&seat).unwrap();
        // Before the record is overwritten below, which is what would strand
        // the real supervisor (`compound-127`).
        let _ends = crate::place_compound::EndsOnDrop(
            serde_json::from_str::<Value>(&std::fs::read_to_string(dir.join("seat.json")).unwrap())
                .unwrap()["pid"]
                .as_u64()
                .expect("a live pid") as u32,
        );
        let mut rec: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("seat.json")).unwrap()).unwrap();
        rec["pid"] = json!(std::process::id());
        rec["agent"] = json!({ "kind": "claude", "name": "t-1", "args": [] });
        std::fs::write(dir.join("seat.json"), rec.to_string()).unwrap();

        let backends = crate::place::local_backends();
        let (_, row) = locate_seat(&backends, seat.as_str())
            .unwrap_or_else(|| panic!("{} answers for itself", seat.as_str()));
        assert_eq!(row.seat, seat);
        assert_eq!(row.agent.kind, "claude", "the agent this seat holds, not just that it exists");

        assert!(
            locate_seat(&backends, "cpd-does-not-exist").is_none(),
            "a seat nothing opened answers for nothing"
        );
    }

    /// `wsp-103`. Two facts compound's agents view draws and could not read:
    /// how long the claim has been held, and where the mandate points — which
    /// is a different question from `project`, where the pane stands.
    #[test]
    fn wip_json_publishes_how_long_a_claim_has_been_held_and_where_the_mandate_points() {
        let mut w = wip_world();
        w.index = Index::new(vec![crate::model::Project::new("wsp"), crate::model::Project::new("data")]);
        w.claims.insert(
            "t-001".into(),
            json!({ "claimed_at": util::iso_at(util::epoch_secs() - (3 * 3600 + 12 * 60 + 5)) }),
        );
        // A mandate on `data` while the pane stands in `wsp`: both true at once.
        w.mandates.insert("w1".into(), json!({ "project": "data" }));
        // One on a project that no longer exists points nowhere.
        w.mandates.insert("w2".into(), json!({ "project": "gone" }));

        let v = wip_json(&w);
        let row = |pane: &str| v["agents"].as_array().unwrap().iter().find(|a| a["pane"] == pane).unwrap().clone();
        assert_eq!(row("w1:p1")["held"], "3h12m", "the claim's clock, phrased as `claim_line` phrases it");
        assert_eq!(row("w1:p1")["project"], "wsp", "where it stands keeps its meaning");
        assert_eq!(row("w1:p1")["direction"], "data", "and where it is pointed is its own key");
        assert_eq!(row("w2:p1")["held"], serde_json::Value::Null, "no claim, no duration");
        assert_eq!(row("w2:p1")["direction"], serde_json::Value::Null, "an unknown project is no direction");
        assert_eq!(row("w3:p1")["direction"], serde_json::Value::Null, "no mandate, no direction");
        assert!(row("w3:p1").get("held").is_some(), "the keys are always present");
    }

    /// `wsp-250`. A seat stopped on a prompt says how long it has been waiting,
    /// from the `since` the fold dated.
    #[test]
    fn a_seat_on_a_prompt_says_how_long_it_has_been_waiting() {
        let mut w = wip_world();
        let twelve = util::epoch_secs() - 12 * 60 - 5;
        w.agents[0].state = State::Blocked;
        w.since.insert(w.agents[0].seat.as_str().to_string(), util::iso_at(twelve));
        let text = wip_lines(&w, &Paint::new(), false).join("\n");
        let line = text.lines().find(|l| l.contains("w1:p1")).unwrap_or_default();
        assert!(line.contains("waiting 12m · person"), "{text}");
    }

    /// `wsp-172`. Both kinds of waiting are drawn where `idle` used to be: a
    /// prompt as `waiting · person`, and an open `wsp ask` as `waiting · <the
    /// scope whose governor it went to>`. The governor's own line counts the
    /// members waiting under it, and `--json` carries the reading for
    /// compound's chrome.
    #[test]
    fn wip_shows_a_seat_on_a_prompt_and_a_seat_behind_its_question_as_waiting_and_on_whom() {
        let mut w = wip_world();
        w.agents[0].state = State::Blocked;
        w.asks = crate::waiting::Asks::one("w2:p1", "t-002", "q-1", crate::waiting::On::Seat("wsp".into()));
        w.governors.insert("wsp".into(), json!({ "workspace": "w3", "host": util::hostname() }));

        let text = wip_lines(&w, &Paint::new(), false).join("\n");
        let line = |pane: &str| text.lines().find(|l| l.contains(pane)).unwrap_or_default().to_string();
        assert!(line("w1:p1").contains("waiting · person"), "a prompt waits on a person: {text}");
        assert!(line("w2:p1").contains("waiting · wsp"), "a question waits on the seat it went to: {text}");
        assert!(!line("w2:p1").contains("idle"), "and neither reads idle: {text}");
        assert!(line("w3:p1").contains("2 waiting"), "the governor counts its waiting members: {text}");
        assert!(text.lines().next().unwrap().contains("2 waiting"), "and the heading says so: {text}");

        let v = wip_json(&w);
        let row = |pane: &str| v["agents"].as_array().unwrap().iter().find(|a| a["pane"] == pane).unwrap().clone();
        assert_eq!(row("w1:p1")["waiting"]["why"], "prompt");
        assert_eq!(row("w1:p1")["waiting"]["on"], "person");
        assert_eq!(row("w2:p1")["waiting"]["why"], "answer");
        assert_eq!(row("w2:p1")["waiting"]["on"], "wsp");
        assert_eq!(row("w2:p1")["waiting"]["question"], "q-1");
        assert_eq!(row("w3:p1")["waiting"], serde_json::Value::Null, "a working seat is not waiting");
        assert_eq!(row("w3:p1")["members_waiting"], 2);
        assert_eq!(v["waiting"], 2);
    }

    /// `wsp-300`, §26.8 D9. A member inside `wsp ask --person --wait` is in a
    /// tool call, so its screen says `working`; its row says it needs the
    /// person and which question, and the governor counts it waiting.
    #[test]
    fn wip_counts_a_seat_blocked_in_ask_wait_as_needing_the_person_while_it_reads_working() {
        let mut w = wip_world();
        w.asks = crate::waiting::Asks::waiter("w1:p1", "t-001", "m-7");
        let v = wip_json(&w);
        let row = |pane: &str| v["agents"].as_array().unwrap().iter().find(|a| a["pane"] == pane).unwrap().clone();
        assert_eq!(row("w1:p1")["state"], "working", "the screen's word is untouched: {v}");
        assert_eq!(row("w1:p1")["needs_you"], true);
        assert_eq!(row("w1:p1")["waiting"]["why"], "question");
        assert_eq!(row("w1:p1")["waiting"]["on"], "person");
        assert_eq!(row("w1:p1")["waiting"]["id"], "m-7");
        assert_eq!(row("w3:p1")["needs_you"], false, "and a neighbour is not");
    }

    /// `wsp-301`, compound's §26.8 D9. A seat a hook holds a dialog for reads
    /// `working` — a tool call is in flight — and is stopped on a person all
    /// the same: `needs_you`, and `waiting` names the question.
    #[test]
    fn a_seat_held_on_a_dialog_needs_the_person_though_it_reads_working() {
        let mut w = wip_world();
        let row = |w: &Wip, pane: &str| {
            wip_json(w)["agents"].as_array().unwrap().iter().find(|a| a["pane"] == pane).unwrap().clone()
        };
        assert_eq!(row(&w, "w1:p1")["needs_you"], false, "working, and nobody is owed anything");
        w.asks = crate::waiting::Asks::held_one("w1:p1", "t-001", "m-hold");
        let held = row(&w, "w1:p1");
        assert_eq!(held["state"], "working");
        assert_eq!(held["needs_you"], true);
        assert_eq!(held["waiting"]["why"], "question");
        assert_eq!(held["waiting"]["on"], "person");
        assert_eq!(held["waiting"]["id"], "m-hold");
        assert_eq!(wip_json(&w)["needs_you"], 2, "it is counted beside the stopped member");
        assert_eq!(row(&w, "w3:p1")["needs_you"], false, "and another seat is not");
    }

    /// `wsp-105`, the governor's condition: `wip --json` prints the finished
    /// rows as the one item the mirror's rows fold into, from the same
    /// builder, so the census and the CLI cannot disagree.
    #[test]
    fn wip_json_lists_the_finished_rows_as_one_item_the_mirror_also_carries() {
        use crate::verification::{record, State as V};
        let s = crate::testkit::store("wip-finished");
        for id in ["r-1", "r-2"] {
            let mut t = Task::new(&format!("title of {id}"), id);
            t.status_raw = "review".into();
            s.save_task(&t).unwrap();
            record(&s, id, V::Holds, "it holds", None, None).unwrap();
        }
        let mut w = wip_world();
        let mut tasks: Vec<Task> = w.tasks.iter().cloned().collect();
        tasks.extend(s.tasks());
        w.tasks = std::rc::Rc::new(tasks);
        let needs_you = wip_json(&w)["attention"]["needs_you"].as_array().unwrap().clone();
        let item = needs_you.iter().find(|e| e["kind"] == "finished").expect("the finished rows are one item");
        assert_eq!(item["rows"], json!(["r-1", "r-2"]));
        assert_eq!(item["text"], "2 finished rows to close");
        let mirrored = crate::mirror::rows(&w.tasks, &BTreeMap::new(), &w.lists, &BTreeMap::new());
        let first = mirrored
            .iter()
            .find_map(|e| match e {
                wsp_store_contract::Entity::Row(r) if r.id.0 == "r-1" => Some(r.fields.clone()),
                _ => None,
            })
            .unwrap();
        let carried: serde_json::Value = serde_json::from_str(&first["group_action"]).unwrap();
        assert_eq!(item["action"], carried, "one builder, one answer");
        assert_eq!(needs_you.iter().filter(|e| e["kind"] == "finished").count(), 1);
    }

    /// `wsp-096`. `wip --json`'s `attention` lists every seat that needs the
    /// person, by the one classifier, with what it waits on; a working seat is
    /// in neither list, and a seat that comes to need the person moves into
    /// `needs_you` on the next read.
    #[test]
    fn wip_json_lists_the_seats_that_need_the_person_under_attention() {
        let mut w = wip_world();
        let listed = |w: &Wip, list: &str| -> Vec<serde_json::Value> {
            wip_json(w)["attention"][list].as_array().unwrap().clone()
        };
        let panes = |v: &[serde_json::Value]| v.iter().map(|e| e["pane"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        let flagged: Vec<String> = wip_rows(&w).iter().filter(|r| r.needs_you).map(|r| r.pane.clone()).collect();
        assert_eq!(panes(&listed(&w, "needs_you")), flagged, "the same seats `needs_you` marks");
        assert!(!panes(&listed(&w, "needs_you")).contains(&"w1:p1".to_string()), "working");
        assert_eq!(listed(&w, "follow_up"), Vec::<serde_json::Value>::new());
        w.asks = crate::waiting::Asks::held_one("w1:p1", "t-001", "m-hold");
        let now = listed(&w, "needs_you");
        let held = now.iter().find(|e| e["pane"] == "w1:p1").expect("held on a dialog, it needs you");
        assert_eq!(held["kind"], "seat");
        assert_eq!(held["attention"], "needs-you");
        assert_eq!(held["task"], "t-001");
        assert!(held["text"].as_str().unwrap().contains("m-hold"), "{held}");
        assert!(held.get("action").is_none(), "wsp-097: a seat carries none, its dialog's question does");
        for e in &now {
            assert!(!e["text"].as_str().unwrap().is_empty(), "{e}");
        }
    }

    /// `wsp-214`. A member idle at `doing` on its own background shell is drawn
    /// `waiting · itself`, and is not `needs_you`, not one of its governor's
    /// waiting members and not in the heading's count: nobody owes it anything.
    #[test]
    fn wip_shows_a_member_on_its_own_shell_as_waiting_on_itself_and_nobody_is_needed() {
        let mut w = wip_world();
        w.agents[1].state = State::Idle;
        w.shells = crate::waiting::Shells::one("w2:p1", 1);
        w.governors.insert("wsp".into(), json!({ "workspace": "w3", "host": util::hostname() }));
        let v = wip_json(&w);
        let row = |pane: &str| v["agents"].as_array().unwrap().iter().find(|a| a["pane"] == pane).unwrap().clone();
        assert_eq!(row("w2:p1")["state"], "idle", "the fixture is an idle member at doing: {v}");
        assert_eq!(row("w2:p1")["waiting"]["on"], "itself");
        assert_eq!(row("w2:p1")["waiting"]["why"], "shell");
        assert_eq!(row("w2:p1")["needs_you"], false, "the shell wakes it");
        assert_eq!(row("w3:p1")["members_waiting"], 0, "its governor owes it nothing");
        assert_eq!(v["waiting"], 0);

        let text = wip_lines(&w, &Paint::new(), false).join("\n");
        let line = text.lines().find(|l| l.contains("w2:p1")).unwrap_or_default();
        assert!(line.contains("waiting · itself") && !line.contains("needs you"), "{text}");
        assert!(!text.lines().next().unwrap().contains("waiting"), "the heading counts nothing: {text}");
    }

    /// The row that was wrong for a whole night. `w2:p1` is idle on a task that
    /// is still `doing`, which for a worker means a person has become the
    /// blocker — and for a seat means it is between the agents it is
    /// sequencing, which is where a seat spends most of its time. The same
    /// pane, the same census reading, and the opposite answer.
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
    /// names a workspace, and workspace ids are per machine. Read the other
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

    /// No census costs the view its agents and nothing else. The queues
    /// below are the store's, and they are what you read `wip` for when
    /// nothing is running.
    #[test]
    fn no_census_is_wip_without_the_agents() {
        let mut w = wip_world();
        w.agents.clear();
        let text = wip_lines(&w, &Paint::new(), false).join("\n");

        assert!(text.contains("no agents running"), "{text}");
        assert!(text.contains("BLOCKED"), "blocked work is the store's:\n{text}");
        assert!(text.contains("waiting on a decision"), "{text}");
        assert!(text.contains("REVIEW"), "{text}");
        assert!(text.contains("finished, waiting on you"), "{text}");
        assert!(text.contains("INBOX"), "unfiled work is still unfiled:\n{text}");
    }

    /// `wsp-323`: `wsp done` is the person's. The REVIEW line is printed to
    /// every seat, a governor passed its verb to a member, and the member —
    /// denied `done` — stopped with the row at `doing`. The line names the
    /// owner of each verb and the one a member does use.
    #[test]
    fn the_review_line_says_done_is_the_persons_and_a_member_reviews() {
        let mut w = wip_world();
        w.agents.clear();
        let text = wip_lines(&w, &Paint::new(), false).join("\n");
        let line = text.lines().find(|l| l.contains("REVIEW")).expect("a REVIEW line");
        assert!(line.contains("the person's"), "{line}");
        assert!(line.contains("wsp done <id>"), "{line}");
        assert!(line.contains("a member finishes with wsp review"), "{line}");
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
            workspaces: vec![Workspace {
                id: "w1".into(),
                label: "strata/001 · a task filed somewhere else".into(),
                ..Default::default()
            }],
            pane: Some("w1:p1".into()),
            workspace: Some("w1".into()),
            agent: None,
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

    /// A silent host costs the chain one link — the workspace label — and the
    /// rest of it answers as it did. A pane with nothing else to go on falls
    /// through to the directory rather than to nothing.
    #[test]
    fn a_silent_host_costs_the_chain_its_label_and_no_more() {
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

    /// A retired record is not a missing one, and `doctor` is the one tool run
    /// specifically to tell a broken store from a healthy one — so saying
    /// `unknown parent wsp-020` about a file sitting in the archive is not a
    /// small inaccuracy. It is a ✗ against a store with nothing wrong with it,
    /// and it sends the reader hunting for a corruption that is not there.
    ///
    /// The other half is asserted in the same test on purpose: an id nothing
    /// ever answered to is exactly as broken as it was before, in the same
    /// words. Softening *that* is how a check stops being worth reading.
    #[test]
    fn a_reference_to_an_archived_record_is_a_note_and_a_reference_to_nothing_is_a_fault() {
        let filed = dangling("worklist-027", "parent", "wsp-020", true).expect("a note, not a fault");
        assert!(filed.contains("archived"), "{filed}");
        assert!(!filed.contains("unknown"), "which is the word that was wrong: {filed}");

        let gone = dangling("worklist-027", "parent", "wsp-999", false).expect_err("a fault");
        assert_eq!(gone, "task worklist-027 references unknown parent `wsp-999`");
    }

    /// The one note that mentions a thing, or the whole list to read when there
    /// is none — the useful half of a failure here is what `doctor` said instead.
    fn note_about<'a>(notes: &'a [String], word: &str) -> &'a str {
        match notes.iter().find(|n| n.contains(word)) {
            Some(n) => n,
            None => panic!("nothing about `{word}` in {notes:?}"),
        }
    }

    /// **A claim reads who holds the task off the census of the backend it was
    /// asked about, and on compound that is a refusal** (`wsp-142` moved the
    /// read; nothing drove it).
    ///
    /// The holder list was one host's `pane.list`, on which no compound seat
    /// appears, so under compound the guard against two agents on one task
    /// never refused — the second took the work over silently and the first
    /// went on editing the same tree. Driven through the real compound census
    /// rather than a hand-built row, for the reason the test below gives: the
    /// fault was in which seats got read, and a row built by hand skips that.
    #[test]
    fn a_claim_on_compound_is_refused_while_another_seat_runs_an_agent_on_the_task() {
        let env = util::isolated("claim-held-compound");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let t = Task::new("held by a running agent", "cpd-71");
        store.save_task(&t).unwrap();

        let compound = crate::place_compound::Compound::new();
        // Both seats are opened for real and then rewritten, the way
        // `place_compound`'s own tests stand a seat up without a pty: this
        // process's pid is the one thing certain to be alive, and the kill on
        // drop is for the supervisor the open started, read before the record
        // stops naming it.
        let holder = compound.open(&crate::place::Order::default()).expect("a compound seat");
        let claimant = compound.open(&crate::place::Order::default()).expect("a second seat");
        let _ends: Vec<_> = [&holder, &claimant]
            .iter()
            .map(|s| {
                let rec = std::fs::read_to_string(compound.dir_of(s).unwrap().join("seat.json")).unwrap();
                crate::place_compound::EndsOnDrop(
                    serde_json::from_str::<Value>(&rec).unwrap()["pid"].as_u64().expect("a live pid") as u32,
                )
            })
            .collect();
        let dir = compound.dir_of(&holder).unwrap();
        std::fs::write(
            dir.join("seat.json"),
            json!({ "pid": std::process::id(), "agent": { "kind": "claude", "name": "a", "args": [] } })
                .to_string(),
        )
        .unwrap();
        // Fresh, because a hook's word expires and an expired one reads as
        // unknown, which is not running.
        std::fs::write(
            dir.join("said.json"),
            json!({ "state": "idle", "hook": "SessionStart", "at": util::now_iso() }).to_string(),
        )
        .unwrap();
        store.set_binding(holder.as_str(), json!({ "task_id": "cpd-71" }));
        let seen: Vec<_> = compound.census().unwrap().seats().map(|r| (r.seat.clone(), r.state)).collect();
        assert!(
            seen.iter().any(|(s, st)| *s == holder && st.is_running()),
            "the holder has to read as running or this proves nothing: {seen:?}"
        );

        let args = Args::synth("claim", &["cpd-71"], &[("pane", claimant.as_str())]);
        assert_ne!(claim(&store, &args), 0, "a task a running agent holds is not taken");
        assert!(
            store.bindings().get(claimant.as_str()).is_none(),
            "a refused claim writes nothing: the claimant was never bound"
        );
    }

    /// **`doctor` on the machine `compound-112`'s flip leaves** (`wsp-119`):
    /// one compound seat, its agent killed.
    ///
    /// The whole binding sweep used to run only where another backend was up,
    /// so on this machine it did not run at all — and `doctor`, the one tool a person
    /// runs to tell a broken machine from a healthy one, printed *no problems*
    /// over a claim and a worktree held by a seat with nothing in it. That is
    /// `robustness-083`'s failure wearing a different hat: everything reporting
    /// healthy, for a fleet that was not.
    ///
    /// Driven through the real census rather than a hand-built row, because the
    /// fault was never in the reading — it was in which seats got read.
    #[test]
    fn doctor_reports_a_compound_seat_with_a_dead_agent() {
        let env = util::isolated("doctor-compound-dead");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();

        let compound = crate::place_compound::Compound::new();
        let seat = compound.open(&crate::place::Order::default()).expect("a compound seat");
        let dir = compound.dir_of(&seat).unwrap();
        let _ends = crate::place_compound::EndsOnDrop(
            serde_json::from_str::<Value>(&std::fs::read_to_string(dir.join("seat.json")).unwrap())
                .unwrap()["pid"]
                .as_u64()
                .expect("a live pid") as u32,
        );
        let mut child = wsp_base::calls::Command::new("/bin/sh").args(["-c", "exit 0"]).spawn().unwrap();
        child.wait().unwrap();
        let mut rec: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("seat.json")).unwrap()).unwrap();
        rec["pid"] = json!(child.id());
        rec["agent"] = json!({ "kind": "claude", "name": "t-1", "args": [] });
        std::fs::write(dir.join("seat.json"), rec.to_string()).unwrap();
        let t = Task::new("held by a seat with nobody in it", "cpd-70");
        store.save_task(&t).unwrap();
        store.set_binding(seat.as_str(), json!({ "task_id": "cpd-70" }));
        let bindings = store.bindings();
        assert_eq!(bindings.len(), 1, "the seat is bound: {bindings:?}");

        let seats = fleet_census().seats().cloned().collect::<Vec<_>>();
        let (mut problems, mut notes) = (Vec::new(), Vec::new());
        seat_health(&seats, &bindings, &store.tasks(), &mut problems, &mut notes);

        assert!(problems.is_empty(), "a dead agent is not a fault in the store: {problems:?}");
        let empty = note_about(&notes, "agent gone");
        assert!(empty.contains("cpd-70"), "named by the work: {empty}");
        assert!(empty.contains("wsp despawn"), "{empty}");
    }

    fn labelled(pane: &str, ws: &str, label: &str) -> Pane {
        Pane {
            pane_id: pane.to_string(),
            workspace_id: ws.to_string(),
            label: label.to_string(),
            ..Default::default()
        }
    }

    fn no_task(_: &str) -> Option<Holder> {
        None
    }

    fn none_seated(_: &str) -> bool {
        false
    }

    fn at(p: Peeked) -> (String, String) {
        match p {
            Peeked::At(pane, what) => (pane, what),
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
        assert_eq!(at(peek_target(&panes, Some("w2"), None, "", none_seated, no_task)).0, "w2:p9");
        assert_eq!(at(peek_target(&panes, Some("w1"), None, "panel", none_seated, no_task)).0, "w1:p9");

        // A workspace with no panel of its own falls through to one that has.
        assert_eq!(at(peek_target(&panes, Some("w7"), None, "panel", none_seated, no_task)).0, "w1:p9");
        // …and so does a caller with no workspace at all, which is what a
        // shell outside any seat is.
        assert_eq!(at(peek_target(&panes, None, None, "panel", none_seated, no_task)).0, "w1:p9");

        // The view is a different surface with a different label, and asking
        // for it from a workspace that has none finds the one that exists.
        assert_eq!(at(peek_target(&panes, Some("w1"), None, "view", none_seated, no_task)).0, "w2:p8");
    }

    /// Missing is named as what is missing, not as what was asked for: "no view
    /// pane open" is a fact you can act on, "nothing to peek at for view" is
    /// not — and the two surfaces are opened in different ways, so the hint has
    /// to differ too.
    #[test]
    fn a_target_that_is_not_open_says_how_to_open_it() {
        let none: Vec<Pane> = Vec::new();
        let hint = |needle: &str| match peek_target(&none, Some("w1"), None, needle, none_seated, no_task) {
            Peeked::Nothing(h) => h,
            _ => panic!("expected nothing for {needle}"),
        };
        assert!(hint("").contains("wsp panel"));
        assert!(hint("panel").contains("wsp panel"));
        assert!(hint("view").contains("↵` on a row in the panel"));
        assert!(hint("board").contains("wsp wip"));

        // A needle nothing recognises is a different answer again: the target
        // does not exist, rather than existing and being empty.
        assert!(matches!(peek_target(&none, None, None, "banana", none_seated, no_task), Peeked::Unknown));
    }

    /// A pane id is taken literally, but only if a pane wears it — otherwise it
    /// falls through to the task search, where `w9:p1` is a needle like any
    /// other rather than a confident answer that is wrong.
    #[test]
    fn a_pane_id_is_only_a_pane_id_if_a_pane_has_it() {
        let panes = vec![labelled("w1:p3", "w1", "")];
        assert_eq!(at(peek_target(&panes, None, None, "w1:p3", none_seated, no_task)).1, "pane w1:p3");
        assert!(matches!(peek_target(&panes, None, None, "w9:p1", none_seated, no_task), Peeked::Unknown));

        // `me` is this pane, whatever was reported — and a caller nothing has
        // told about itself has no `me` to look at.
        assert_eq!(at(peek_target(&panes, None, Some("w1:p3"), "me", none_seated, no_task)).0, "w1:p3");
        assert!(matches!(peek_target(&panes, None, None, "me", none_seated, no_task), Peeked::Nothing(_)));
    }

    /// `wsp-156`: a compound seat is `cpd-N` — no colon, and never in a host's
    /// pane list — so it was refused as "no pane, task or project matching" while
    /// live and claimed. What makes it a seat is that its backend holds it.
    #[test]
    fn a_seat_is_a_seat_because_its_backend_holds_it_not_because_of_how_it_is_spelled() {
        let panes = vec![labelled("w1:p3", "w1", "")];
        let held = |n: &str| n == "cpd-347";
        assert_eq!(at(peek_target(&panes, None, None, "cpd-347", held, no_task)).0, "cpd-347");
        assert!(
            matches!(peek_target(&panes, None, None, "cpd-348", held, no_task), Peeked::Unknown),
            "a seat nothing holds falls through to the task search like any other needle"
        );
        // The keywords stay keywords: `me` and `panel` are not a seat a
        // backend could be asked about.
        assert!(matches!(peek_target(&panes, None, None, "me", |_| true, no_task), Peeked::Nothing(_)));
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
        let (pane, what) = at(peek_target(&panes, None, None, "001", none_seated, held));
        assert_eq!(pane, "w4:p1");
        assert!(what.starts_with("t-001 — the seam under the panel"), "{what}");

        let unheld = |_: &str| {
            Some(Holder { id: "t-002".into(), title: "nobody is on it".into(), panes: Vec::new() })
        };
        assert!(matches!(peek_target(&panes, None, None, "002", none_seated, unheld), Peeked::Nothing(_)));
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
            agent: None,
            tasks: vec![wip_task("t-001", "somebody else's tree", Some("strata"), "doing")],
            workspaces: vec![Workspace {
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

    /// The bottom of the chain is the only link a host supplies, so it is the
    /// only one that goes away when the host stops answering. Everything above it
    /// is the store's and answers the same either way.
    #[test]
    fn the_workspace_label_is_the_last_resort_and_the_only_host_link() {
        let mut h = here();
        h.cwd = None;
        assert_eq!(standing_in(&h).as_deref(), Some("music"), "matched by its name");

        // The host silent: the label link is gone and nothing else is.
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
    /// per machine and mean nothing on another host, which is why a claim carries
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

    /// A pane nothing has told us about — a shell outside any seat, or a hook
    /// with no environment — falls through to the directory, which is the one
    /// link that needs neither a pane nor a workspace.
    #[test]
    fn a_pane_nothing_named_still_has_a_directory() {
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
                    // A record that has a session has a kind: they are learned
                    // from the same row on the same tick. The one that has
                    // neither is the seat between the claim and the agent.
                    if !s.is_empty() {
                        b["agent_kind"] = json!("claude");
                    }
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

        // Between claim and start: there is a pane and nothing in it.
        let nothing = sessions_learned(&store, [("w1:p1", "", "")].into_iter());
        assert!(nothing.is_empty(), "no agent, no session, nothing to write");

        // The agent comes up and the backend says which session it is.
        let learned = sessions_learned(&store, [("w1:p1", "309b2e2c", "claude")].into_iter());
        assert_eq!(
            learned,
            vec![("w1:p1".to_string(), "309b2e2c".to_string(), "claude".to_string())]
        );

        // An `adopt`ed binding has no such key and is in exactly the same
        // position, so absence has to read as empty rather than as "skip".
        let adopted = bound(&[("w1:p1", "robustness-060", None)]);
        assert_eq!(
            sessions_learned(&adopted, [("w1:p1", "309b2e2c", "claude")].into_iter()).len(),
            1
        );
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
            sessions_learned(&store, [("w1:p1", "", "claude")].into_iter()).is_empty(),
            "a dead agent's silence must not erase the id a resume is keyed on"
        );
        assert!(
            sessions_learned(&store, [("w1:p1", "   ", "claude")].into_iter()).is_empty(),
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

        let learned = sessions_learned(&store, [("w1:p1", "f1487db7", "claude")].into_iter());
        assert_eq!(
            learned,
            vec![("w1:p1".to_string(), "f1487db7".to_string(), "claude".to_string())]
        );

        // And the ordinary tick, where nothing has changed, writes nothing:
        // the census runs this every tick against every pane on the machine.
        let same = sessions_learned(&store, [("w1:p1", "309b2e2c", "claude")].into_iter());
        assert!(same.is_empty(), "a store write per tick per pane is not a projection");
    }

    /// Only seats wsp has bound. A backend reports a session for every agent on the
    /// machine — a colleague's pane, the governor, a `claude` somebody started
    /// by hand — and none of those is a binding this may invent.
    #[test]
    fn a_session_in_a_seat_wsp_has_not_bound_teaches_it_nothing() {
        let store = bound(&[("w1:p1", "robustness-060", Some(""))]);
        let seen = [("w1:p1", "309b2e2c", "claude"), ("w9:p1", "77fa398b", "claude")];

        let learned = sessions_learned(&store, seen.into_iter());
        assert_eq!(
            learned,
            vec![("w1:p1".to_string(), "309b2e2c".to_string(), "claude".to_string())]
        );
    }

    /// The kind obeys the same two rules as the session, and it has to be
    /// asserted separately because it is judged separately: a pane whose agent
    /// has died reports no kind either, and the moment it is worth the most is
    /// the moment it stops being reported.
    #[test]
    fn a_seat_whose_agent_died_keeps_the_kind_it_was_last_seen_with() {
        let store = bound(&[("w1:p1", "core-031", Some("309b2e2c"))]);
        let mut b = store.get("w1:p1").unwrap().clone();
        b["agent_kind"] = json!("opencode");
        let store: BTreeMap<String, Value> =
            [("w1:p1".to_string(), b)].into_iter().collect();

        assert!(
            sessions_learned(&store, [("w1:p1", "309b2e2c", "")].into_iter()).is_empty(),
            "silence about the kind is not a correction any more than silence about the session"
        );
        assert_eq!(
            sessions_learned(&store, [("w1:p1", "309b2e2c", "claude")].into_iter()).len(),
            1,
            "and a different kind is a correction, which is what a restarted pane looks like"
        );
    }

    /// The `||` in the test, and the case it exists for: every record in the
    /// store on the day `core-031` landed already had its session and will never
    /// change it again. Judging the row on the session alone would leave each of
    /// them with no kind for as long as it lived, and reading its default
    /// forever.
    #[test]
    fn a_binding_that_already_knew_its_session_still_learns_its_kind() {
        let store = bound(&[("w1:p1", "core-031", Some("309b2e2c"))]);
        let mut b = store.get("w1:p1").unwrap().clone();
        b.as_object_mut().unwrap().remove("agent_kind");
        let store: BTreeMap<String, Value> =
            [("w1:p1".to_string(), b)].into_iter().collect();

        assert_eq!(
            sessions_learned(&store, [("w1:p1", "309b2e2c", "opencode")].into_iter()),
            vec![("w1:p1".to_string(), "309b2e2c".to_string(), "opencode".to_string())],
            "an unchanged session with a kind nobody recorded is still news"
        );
    }

    /// A store on disk with one pane holding one task.
    ///
    /// What is under test is which
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

    /// wsp-290: the clauses a release line and a leg's end line carry, with the
    /// peak between the turns and the spend, and left out where none was read.
    #[test]
    fn the_ran_clauses_carry_the_peak_between_turns_and_spend() {
        use crate::agent_kind::{Ran, Spend};
        let mut ran = Ran {
            models: vec!["sonnet-5-5".into()],
            efforts: vec!["high".into()],
            turns: 63,
            peak: 180_000,
            spend: Spend { cache: 4_100_000, input: 100_000, output: 5_000, micros: None },
        };
        assert_eq!(ran_text(&ran), " · ran sonnet-5-5/high · 63 turns · peak 180k · 4.1M cache 100k in 5.0k out");
        ran.peak = 0;
        assert_eq!(ran_text(&ran), " · ran sonnet-5-5/high · 63 turns · 4.1M cache 100k in 5.0k out");
    }

    // ---- `wait` (`wsp-552`) ----------------------------------------------

    /// A row whose `## Log` holds `lines`, each dated `ago` seconds back.
    fn logged(id: &str, status: &str, lines: &[(&str, i64)]) -> Task {
        let mut t = wip_task(id, id, Some("wsp"), status);
        let now = util::epoch_secs();
        let log: String = lines.iter().map(|(l, ago)| format!("- {} {l}\n", util::iso_at(now - ago))).collect();
        t.body = format!("## Log\n{log}");
        t
    }

    /// One seat in each kind of wait, bound to a row of its own, with `since`
    /// a minute back for each.
    fn waits_world() -> Wip {
        let mut w = wip_world();
        let now = util::epoch_secs();
        let start = (crate::cycle::STARTED_BY, 600);
        let rows = [
            ("w4:p1", logged("t-010", "doing", &[start]), State::Idle),
            ("w5:p1", logged("t-011", "doing", &[start]), State::Blocked),
            ("w6:p1", logged("t-012", "doing", &[start]), State::Idle),
            ("w7:p1", logged("t-013", "doing", &[start]), State::Idle),
            ("w8:p1", logged("t-014", "doing", &[start, (&format!("{} idle with this row still at doing", crate::repair::NOTICED), 30)]), State::Idle),
            ("w9:p1", logged("t-015", "doing", &[start, (&format!("{} the verifier blocked: x", crate::cycle::SENT_BACK), 20)]), State::Gone),
            ("w10:p1", logged("t-016", "doing", &[start]), State::Idle),
            ("w11:p1", logged("t-017", "doing", &[start]), State::Working),
        ];
        for (seat, mut t, state) in rows {
            if t.id == "t-013" {
                t.after = vec!["pieces-trial".into()];
            }
            w.bindings.insert(seat.into(), json!({ "task_id": t.id }));
            std::rc::Rc::make_mut(&mut w.tasks).push(t);
            let mut a = seated_agent(seat, "idle", seat);
            a.state = state;
            w.agents.push(a);
        }
        for a in &w.agents {
            w.since.insert(a.seat.as_str().to_string(), util::iso_at(now - 60));
        }
        let mut list = crate::model::Worklist::new("pieces-trial", "the trial");
        list.set_planned();
        list.alone = Some(true);
        w.lists = vec![list];
        w.shells = crate::waiting::Shells::own(
            "w4:p1",
            crate::waiting::Own { n: 1, work: "cargo nextest".into(), age: 240, quiet: 0 },
        );
        w.asks = crate::waiting::Asks::one("w6:p1", "t-012", "q-7", crate::waiting::On::Seat("wsp".into()));
        w.unsent.insert("w10:p1".into());
        w.stuck.insert("w11:p1".into(), (crate::stuck::Kind::SelfWait, 300));
        w
    }

    fn wait_of(j: &Value, pane: &str) -> Value {
        j["agents"].as_array().unwrap().iter().find(|r| r["pane"] == pane).unwrap()["wait"].clone()
    }

    #[test]
    fn wip_json_carries_a_working_seats_wait_with_its_age() {
        let j = wip_json(&waits_world());
        let wait = wait_of(&j, "w1:p1");
        assert_eq!(wait["kind"], "working", "{wait}");
        assert_eq!(wait["line"], "working 1m", "{wait}");
        assert!(wait["since"].is_string() && wait["quiet_secs"].is_null(), "{wait}");
    }

    #[test]
    fn wip_json_carries_a_seat_waiting_on_its_own_shell_with_the_work_its_age_and_quiet() {
        let wait = wait_of(&wip_json(&waits_world()), "w4:p1");
        assert_eq!(wait["kind"], "itself", "{wait}");
        assert_eq!(wait["target"], "cargo nextest", "{wait}");
        assert_eq!(wait["quiet_secs"], 0, "{wait}");
        assert_eq!(wait["line"], "waiting on itself: cargo nextest (4m, busy)", "{wait}");
    }

    #[test]
    fn wip_json_carries_a_seat_on_a_prompt_as_waiting_on_a_person() {
        let wait = wait_of(&wip_json(&waits_world()), "w5:p1");
        assert_eq!(wait["kind"], "person", "{wait}");
        assert_eq!(wait["line"], "waiting on a person 1m: a prompt on its screen", "{wait}");
    }

    #[test]
    fn wip_json_carries_a_seat_behind_its_ask_as_waiting_on_the_governor_it_went_to() {
        let wait = wait_of(&wip_json(&waits_world()), "w6:p1");
        assert_eq!(wait["kind"], "governor", "{wait}");
        assert_eq!(wait["target"], "wsp", "{wait}");
        assert_eq!(wait["line"], "waiting on the wsp governor 1m: its question q-7", "{wait}");
    }

    #[test]
    fn wip_json_carries_a_seat_whose_row_rests_on_an_unfinished_list_as_on_other_work() {
        let wait = wait_of(&wip_json(&waits_world()), "w7:p1");
        assert_eq!(wait["kind"], "other_work", "{wait}");
        assert_eq!(wait["target"], "pieces-trial", "{wait}");
        assert_eq!(wait["line"], "waiting on pieces-trial (planned: alone, 1m)", "{wait}");
    }

    #[test]
    fn wip_json_carries_a_stopped_seat_nothing_was_said_to_as_idle() {
        let wait = wait_of(&wip_json(&waits_world()), "w2:p1");
        assert_eq!(wait["kind"], "idle", "{wait}");
        assert_eq!(wait["line"], "idle since turn end 1m", "{wait}");
    }

    #[test]
    fn wip_json_carries_a_seat_the_run_told_to_finish_as_told_since_the_telling() {
        let wait = wait_of(&wip_json(&waits_world()), "w8:p1");
        assert_eq!(wait["kind"], "told", "{wait}");
        assert_eq!(wait["line"], "idle since turn end 1m: told to finish", "{wait}");
        let since = util::epoch_of(wait["since"].as_str().unwrap());
        assert!((util::epoch_secs() - since - 30).abs() <= 2, "since is the telling: {wait}");
    }

    #[test]
    fn wip_json_carries_an_ended_seat_whose_row_was_sent_back_as_reseating() {
        let wait = wait_of(&wip_json(&waits_world()), "w9:p1");
        assert_eq!(wait["kind"], "reseating", "{wait}");
        assert!(wait["line"].as_str().unwrap().starts_with("sent back 2"), "{wait}");
        assert!(wait["line"].as_str().unwrap().ends_with("s ago: reseating"), "{wait}");
    }

    #[test]
    fn wip_json_carries_a_seat_nothing_typed_reaches_as_cannot_be_told() {
        let j = wip_json(&waits_world());
        let unsent = wait_of(&j, "w10:p1");
        assert_eq!(unsent["kind"], "cannot_be_told", "{unsent}");
        assert_eq!(unsent["target"], "unsent", "{unsent}");
        assert_eq!(unsent["line"], "cannot be told: typed, not sent", "{unsent}");
    }

    /// wsp-634: a seat whose supervisor refuses delivery typed nothing, and is
    /// drawn as that and not as `typed, not sent` — even where both are true.
    #[test]
    fn wip_json_says_refusing_delivery_and_not_typed_not_sent() {
        let mut w = waits_world();
        w.refusing.insert("w10:p1".into(), (6, "cpd-1: refused".into()));
        let j = wip_json(&w);
        let refusing = wait_of(&j, "w10:p1");
        assert_eq!(refusing["kind"], "cannot_be_told", "{refusing}");
        assert_eq!(refusing["target"], "refusing", "{refusing}");
        assert_eq!(refusing["line"], "cannot be told: refusing delivery, 6 ticks running: cpd-1: refused", "{refusing}");
        // A shell call `stuck.rs` named never ends, so a tell queues behind it.
        let stuck = wait_of(&j, "w11:p1");
        assert_eq!(stuck["kind"], "cannot_be_told", "{stuck}");
        assert_eq!(stuck["target"], "self", "{stuck}");
        assert_eq!(stuck["line"], "cannot be told: waiting on itself 5m", "{stuck}");
    }

    #[test]
    fn a_send_back_that_did_not_reach_the_pane_is_cannot_be_told_and_a_fresh_start_clears_every_mark() {
        let t = logged(
            "t-1",
            "doing",
            &[
                (crate::cycle::STARTED_BY, 600),
                (&format!("{} x", crate::cycle::SENT_BACK), 60),
                ("wsp: the sent-back reason did not reach its pane — typed when the seat reads idle", 50),
            ],
        );
        let said = crate::repair::run_said(&t);
        assert!(said.undelivered.is_some() && said.sent_back.is_some(), "{said:?}");
        let f = crate::waiting::Facts {
            state: State::Idle,
            waiting: None,
            in_state_since: None,
            stuck: None,
            said,
            unreachable: None,
            unsent: false,
            refusing: None,
            dropped: false,
            rests_on: None,
            now: util::epoch_secs(),
        };
        let sw = crate::waiting::seat_wait(&f).unwrap();
        assert_eq!(sw.kind, crate::waiting::Kind::CannotBeTold);
        assert_eq!(sw.target.as_deref(), Some("send-back"));
        let mut t = t;
        t.log(&format!("{} again", crate::cycle::STARTED_BY));
        assert_eq!(crate::repair::run_said(&t), crate::waiting::Said::default(), "a new agent has been told nothing");
    }

    #[test]
    fn wip_text_prints_each_held_or_stopped_seats_wait_line_under_it() {
        let text = wip_lines(&waits_world(), &Paint::new(), false).join("\n");
        for line in [
            "↳ waiting on itself: cargo nextest (4m, busy)",
            "↳ idle since turn end 1m: told to finish",
            "↳ waiting on pieces-trial (planned: alone, 1m)",
            "↳ cannot be told: typed, not sent",
        ] {
            assert!(text.contains(line), "{line} in:\n{text}");
        }
        assert!(!text.contains("↳ working"), "a turn in flight already says working:\n{text}");
    }
}
