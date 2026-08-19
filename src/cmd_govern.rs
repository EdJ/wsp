//! `wsp govern` — the seat that coordinates a project's agents.
//!
//! On 2026-08-17 one workspace ran twelve agents across the `robustness`
//! backlog for a night, and it worked. It worked by convention: the seat was an
//! ordinary claim on an ordinary task that happened to be the artefact it was
//! writing, so nothing in wsp knew the difference between the agent sequencing
//! the work and the agents doing it. Two symptoms, both observed rather than
//! imagined, are what this file exists to remove:
//!
//! - `wsp wip` drew the seat as an agent that **needs you**. That reading is
//!   right for a worker — idle process on a `doing` task means a person is the
//!   blocker — and exactly wrong for a seat, which is idle between the agents
//!   it is waiting on. It was the loudest row on the panel all night and it
//!   never meant anything.
//! - `wsp flag` says *raised on every panel*, because there is nowhere better
//!   to send it. A raised hand about a `robustness` task went to the one screen
//!   a person was looking at, and the agent coordinating `robustness` could not
//!   see it at all without being told.
//!
//! # What a governor is, of the three things it could have been
//!
//! **A property of a workspace, recorded against a project.** Not a claim: the
//! seat's claim changed twice during that night — it borrowed a task to have
//! somewhere to stand — while the seat itself did not move. Not a role on a
//! claim either, for the same reason plus one more: a seat is entitled to hold
//! no task at all, and a record that only exists while it does would blink out
//! every time it put work down.
//!
//! A workspace, not a pane, because a pane is the most perishable identifier
//! herdr has — the same argument that keys claims on workspaces. An agent that
//! is cleared and restarted in place is the same seat with a shorter memory,
//! and the record should survive that even though the thread does not.
//!
//! That is where the record is **kept**, and it is not where "is this pane the
//! seat" is **answered**: a workspace holds more than one agent, so the coarse
//! read hands the custodial identity to whoever else walks in. The record keeps
//! both halves and [`governs`] carries the argument — worklist-035.
//!
//! # …and what it turned out to be, once a person had to talk to one
//!
//! The record above routes a raised hand and it is not a **position**. Ed,
//! 2026-08-17, on holding two governorships from one workspace: *"I don't see
//! you as governor, I still see you as robustness/078 — and you've not been
//! moved to sit below the wsp line as I would expect."* The decision on
//! robustness-048 settles the shape:
//!
//! **A governor is a slot on a project, and the agent in it is a custodian
//! rather than a claimant.** It is a third kind of node beside projects and
//! tasks, and that is affordable only because it is a *slot* — no status of its
//! own, no prose, no lifecycle, nothing to finish. What it adds to the model is
//! one edge that did not exist: an agent can be assigned to a **project**.
//! Every other assignment in wsp is agent-to-task.
//!
//! Three things follow, and each one is a thing a person can see:
//!
//! - **It has a place.** [`Slot`] is what the panel draws under the project it
//!   belongs to — not under whatever task its occupant borrowed to have
//!   somewhere to stand, because position is what a tree means.
//! - **It is addressable.** `wsp govern <project> --tell` speaks to whoever is
//!   in the slot now, and the panel's `T` is the same sentence from the row.
//!   Addressing the *position*, never the pane: the pane the agent started in
//!   is display only, and [`occupant`] asks the runner who is in that workspace
//!   at the moment of speaking.
//! - **It outlives its occupant.** A slot with nobody in it still draws, still
//!   answers, and can be filled again. `--clear` vacates and leaves the slot
//!   standing; `--remove` is the separate decision that this project has no
//!   governor at all.
//! - **And one agent holds one of them.** See [`governs`]: taking a second slot
//!   hands the first back, because a record that says an agent is in two
//!   positions is one nothing can draw and nobody can vacate.
//!
//! **An agent assigned to a slot gets different instructions.** Not "you have
//! been claimed onto robustness-048, begin work" but "you are the custodian of
//! this project": it sequences, directs, reviews and holds the record for
//! everything beneath it, rather than finishing one piece of work and standing
//! down. That sentence is [`crate::cmd_spawn::Handover::Custodian`], and the
//! brief that arrives with it is the project's rather than a task's — see
//! [`crate::cmd_brief`].
//!
//! # Per hierarchy, and the chain that makes absence cheap
//!
//! `wsp` has a seat; `robustness` may have its own; a sub-project may have one.
//! That is the rule tags, decisions and the handbook already follow — inherited
//! down the chain, specialised at each level — and it falls out of keying the
//! record on the project: [`seat_for`] asks the task's project, then its parent,
//! then its parent, and takes the first answer. One per level is a real
//! arrangement and not the expected one, which is why the panel draws a
//! *vacancy* only where there is no slot above it.
//!
//! **The chain always terminates, because the person is the governor of last
//! resort.** A flag with no seat anywhere above it is raised on every panel,
//! which is exactly what happens today. So the normal state — no governor
//! anywhere — is one missing file, one `BTreeMap::new()`, and every behaviour
//! in this tree unchanged. That is the whole answer to "what happens when there
//! is no governor", and it is why nothing here has a default to configure.
//!
//! # The key is a **scope**, and a running worklist is asked first
//!
//! `governors.json` is keyed on a project id *or* a worklist slug, and
//! [`seat_for`] tries the worklist before the project chain:
//!
//!     a task in a *running* worklist  ->  that worklist's seat
//!     otherwise, its project          ->  that project's seat, then its parents
//!     otherwise                       ->  every panel
//!
//! **The routing had to move with the work, because moving it is what the work
//! was being moved for.** The `batch` was made a project *because* a governor
//! seat is per-project and flag routing follows it, which cost 26 tasks a move
//! into a project and a move back out. A worklist references its members
//! instead, so a hand raised on `render-071` at 3am reaches whoever is running
//! the batch tonight rather than whoever governs `render` in general — and
//! nothing has to be moved for that to be true.
//!
//! The step in front is **not** a second escalation policy. It is one more
//! level on the same walk, and it does not stop at a list nobody is sitting in:
//! a worklist with no seat falls through to the project chain, which is where a
//! list composed out of one backlog was being answered for anyway.
//!
//! One key space, both ways — `Store::scope_taken` is where that is enforced,
//! and it is what buys `wsp govern <slug>` with no new flag. Only the *routing*
//! asks whether a list is running: a seat is taken on a list before it starts,
//! because that is how somebody comes to be there to start it.
//!
//! # A coordination point, not an approval gate
//!
//! Nothing in that night's work needed permission from the seat. It needed
//! sequencing and review, and both are things the seat *does* rather than
//! things other agents wait on. So no verb in wsp consults a governor before
//! acting: a governor changes who is **expected to look** at a raised hand and
//! how a seat is **drawn**, and changes nothing about what any agent may do.
//! A gate here would put a round-trip in front of every agent for the benefit
//! of none, and there is a test at the bottom of this file that says so.
//!
//! The one exception is a guard rather than a gate, and it runs in the other
//! direction: `wsp despawn` refuses to end a governing pane without `--force`,
//! because the seat is the one agent that cannot be restarted without losing
//! the thread. It costs the seat, not the agents under it.
//!
//! # Testing anything in this file needs `wsp sandbox`
//!
//! `WSP_HOME` and `WSP_STATE` isolate the store and **not herdr**, because
//! herdr is one server per machine: [`rename_seat`] below renames a real
//! workspace and a real pane whatever store it was pointed at. Measured the
//! hard way on 2026-08-17 — `wsp govern robustness -w w1` against a temporary
//! store renamed the live governor's own window. `wsp sandbox` (robustness-013)
//! is the tool: its own herdr session, its own socket, its own store.
//!
//! # It costs nothing until something is addressed to it
//!
//! `wsp brief` is read on every request of every session, so a line added here
//! is paid tens of thousands of times over a night. The seat line is drawn only
//! when a seat exists above the pane reading it, and the flag receipt names the
//! seat only when it found one. With no governor set, every output in this tree
//! is byte-for-byte what it was.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::herdr;
use crate::resolve::Index;
use crate::store::Store;
use crate::util::{self, Paint};
use crate::Args;

/// A seat, resolved: which scope it governs and where it is sitting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seat {
    /// The scope the record is filed under — a project id or a worklist slug —
    /// and it is the scope *governed*, not necessarily the one that was asked
    /// about. [`seat_for`] walks, so a flag on a `data` task can resolve to the
    /// `wsp` seat and a flag on a member of tonight's list to the list's, and
    /// the reader wants to be told which.
    pub scope: String,
    pub workspace: String,
    /// The pane the agent was in when it took the seat, and the *exact* half of
    /// the record where [`Seat::workspace`] is the durable one.
    ///
    /// No longer display-only, which is worklist-035 — [`governs`] carries the
    /// argument. It can still go stale, and what answers that is
    /// [`crate::cmd_agent::reconcile`] vacating a slot whose pane herdr no
    /// longer lists, plus the re-take `wsp resume` and a custodian's own
    /// `wsp govern` both perform. Empty where the record was written for a room
    /// the process was not standing in (`wsp govern -w`).
    pub pane: String,
    pub since: String,
    /// The session the custodian is running under, learned from the backend
    /// after the fact by [`learn_seats`] — never written when the seat is
    /// taken, because at that instant there is a shell in the room and no
    /// agent. Empty until something has seen one.
    ///
    /// This field is `render-061`, and the reason it is on the *seat* rather
    /// than reachable through a binding is the finding that task was filed on:
    /// a binding is written per claim, and a custodian is the one kind of agent
    /// that deliberately holds no task. Checked on the machine on 2026-08-18
    /// with two governors up for a day — `bindings.json` was `{}`, and the two
    /// panes that had survived longest were the two that could not be resumed.
    pub session: String,
    /// Where that session was running, which is what `claude --resume` has to
    /// be standing in for the transcript to mean anything. Learned with the
    /// session and from the same reading.
    pub cwd: String,
}

impl Seat {
    /// Is *this* pane the one sitting in the slot?
    ///
    /// Written once because three places were asking it and two of them were
    /// asking it of the room. [`governs`] is this same predicate over a whole
    /// map and is where the argument lives; `pane: None` is the caller that has
    /// no pane and means the room.
    pub fn sat_in(&self, workspace: &str, pane: Option<&str>) -> bool {
        self.workspace == workspace && (self.pane.is_empty() || pane.is_none_or(|p| p == self.pane))
    }
}

/// A slot on a project or a worklist: the position, and whoever is in it now.
///
/// [`Seat`] is the occupancy and this is the post. The difference is the whole
/// of "it outlives its occupant": a slot whose agent has gone is a slot with
/// `occupant: None`, which still has a project, a place in the tree and a row
/// you can stand on — where before, the record was deleted and the position
/// went with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    /// The scope the post is on — see [`Seat::scope`].
    pub scope: String,
    /// Filled, on this machine. `None` covers both an empty slot and one held
    /// from another host, which are the same thing from here: nobody you can
    /// reach. `host` below says which.
    pub occupant: Option<Seat>,
    /// The machine the slot was last filled from, or empty for a vacant one.
    /// Carried so a slot held on the laptop does not draw here as empty, which
    /// would invite two agents into one position.
    pub host: String,
    /// When the slot was last filled, or vacated. One field, because what a
    /// reader wants is "and how long has *that* been true".
    pub since: String,
}

impl Slot {
    /// Somebody is in it, here.
    pub fn filled(&self) -> bool {
        self.occupant.is_some()
    }

    /// Held from another machine — not ours to draw as empty and not ours to
    /// speak to.
    pub fn elsewhere(&self) -> bool {
        self.occupant.is_none() && !self.host.is_empty() && self.host != util::hostname()
    }
}

/// Every slot that exists, in project order — vacant ones included.
///
/// The display read, where [`seat_for`] is the routing one. Routing wants the
/// nearest seat that can actually answer; a tree wants every position there is,
/// because a post nobody is standing in is exactly the thing a person has to be
/// able to see in order to fill it.
pub fn slots(governors: &BTreeMap<String, Value>) -> Vec<Slot> {
    governors
        .iter()
        .map(|(scope, rec)| Slot {
            occupant: seat_of(scope, rec),
            host: rec.get("host").and_then(Value::as_str).unwrap_or_default().to_string(),
            since: rec
                .get("since")
                .or_else(|| rec.get("vacated"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            scope: scope.clone(),
        })
        .collect()
}

/// One record, if it belongs to this machine.
///
/// A workspace id is herdr's and means nothing on another host — the same
/// reason a claim and a mandate each carry one. A seat on another machine is
/// not a seat you can reach, so it reads as no seat rather than as a wrong one.
fn seat_of(scope: &str, rec: &Value) -> Option<Seat> {
    let host = rec.get("host").and_then(Value::as_str).unwrap_or("");
    if !host.is_empty() && host != util::hostname() {
        return None;
    }
    // A record naming no workspace names no seat. `govern` never writes one,
    // so this is a hand-edited or half-written file — and a seat you cannot
    // reach must read as absent rather than as somewhere.
    let workspace = rec.get("workspace").and_then(Value::as_str).unwrap_or_default();
    if workspace.is_empty() {
        return None;
    }
    Some(Seat {
        scope: scope.to_string(),
        workspace: workspace.to_string(),
        pane: rec.get("pane").and_then(Value::as_str).unwrap_or_default().to_string(),
        since: rec.get("since").and_then(Value::as_str).unwrap_or_default().to_string(),
        session: str_at(rec, "session"),
        cwd: str_at(rec, "cwd"),
    })
}

fn str_at(rec: &Value, key: &str) -> String {
    rec.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

/// The seat responsible for a piece of work: the running worklist it is in, or
/// the nearest seat above its project.
///
/// The escalation the task asked about, and it is a walk rather than a policy.
/// `robustness` has a seat and `wsp` has a seat: a hand raised in `robustness`
/// reaches the first, and the same hand raised while that seat is away reaches
/// the second. Nothing decides to escalate — the walk simply does not stop at a
/// level that has nobody in it, which is also why standing down needs no
/// hand-over. **The worklist step obeys that same rule and is not an exception
/// to it**: a list with no seat, or one whose agent has stood down, falls
/// through to the project chain rather than routing to nobody.
///
/// `list` is the front of the walk — the *running* worklist this task is a
/// member of, and `None` for the ordinary state where nothing is running. It
/// is a parameter rather than something read here because the rule is a map
/// lookup repeated over keys in priority order, and holds no store: one read of
/// `worklists/` ([`crate::worklist::Running`]) serves every question a command
/// asks, where a store read inside this would repeat it per raised hand.
pub fn seat_for(
    governors: &BTreeMap<String, Value>,
    index: &Index,
    list: Option<&str>,
    project: Option<&str>,
) -> Option<Seat> {
    if governors.is_empty() {
        return None;
    }
    let above = project
        .into_iter()
        .flat_map(|p| std::iter::once(p.to_string()).chain(index.ancestors(p)));
    list.map(str::to_string)
        .into_iter()
        .chain(above)
        .find_map(|s| governors.get(&s).and_then(|rec| seat_of(&s, rec)))
}

/// The scope this workspace is the custodian of — a project or a worklist — if
/// it is the custodian of one.
///
/// **One agent, one governorship.** Ed, 2026-08-17, reversing what
/// wsp-063 built for. That task argued a night coordinating `robustness`
/// while answering for `wsp` above it was one agent and not two, which
/// described the night accurately and is still the wrong model: it makes two
/// questions unanswerable in principle rather than merely hard. Which row draws
/// the agent, when it is in two positions at once? And what does a vacancy look
/// like when the same occupant fills both? One slot to an agent settles both by
/// construction, and the answer to *"who answers for `wsp` while the
/// `robustness` custodian is busy"* is the chain [`seat_for`] already walks —
/// upwards, to a different agent.
///
/// So this is an `Option` rather than a list, and the invariant lives in the
/// type: [`take`] stands the workspace down from whatever it held before, the
/// way a claim hands off the task it is leaving. A store that already holds two
/// — this one did, the night the rule changed — answers with the first in id
/// order and heals the next time anybody takes a seat.
///
/// No workspace is no seat, never a seat. A pane herdr answered for without a
/// workspace id, or a caller outside herdr entirely, would otherwise match a
/// record whose own field failed to parse — and the answer to "am I the seat"
/// would come back yes for a process that is not in a workspace at all.
///
/// # `pane` — and why the exact read is the pane's and not the room's
///
/// The record is keyed on the workspace and that is still right: it is what
/// survives an agent being cleared and restarted, and it is what the panel
/// draws under. But **a workspace can hold more than one agent**, and asking
/// this question of the *room* answers yes for every one of them. worklist-035,
/// driven on `796c2d2`: a seat's pane was ended, two later spawns landed in
/// that same workspace, and both were told they were the custodian — which
/// exempted both from [`needs_a_person`] for their whole lives, one of them a
/// member of a running worklist on an unattended night. One line of `wip` held
/// the fact and the silence together.
///
/// So a caller with a pane in hand passes it, and a record that names a pane is
/// only that pane's. `None` is for the caller that genuinely has no pane —
/// naming the *room* after its seat ([`rename_seat`], the workspace token in
/// `sync`) is a question about the workspace, and answering it per pane would
/// be answering a different question.
///
/// **The case for the other answer, stated rather than assumed.** A custodian
/// that splits its own workspace to run something gets told, in the second
/// pane, that it is nobody's seat. That is the cost, it is real, and it is the
/// cheaper of the two: wsp cannot tell that pane from a worker's, and the two
/// failures are not the same size. A seat told *this workspace is nobody's
/// seat* gets a sentence naming the repair and types `wsp govern` again. A
/// worker told it **is** the seat is told nothing at all — it is exempted from
/// the one predicate an unattended run depends on, and stays exempt until
/// somebody happens to read a panel. Silence is the failure this is being
/// repaired for, so the ambiguity is resolved towards the noisy answer.
///
/// A record with no pane on it — hand-written, or written by `wsp govern -w`
/// naming a room this process is not standing in — falls back to the room,
/// because there is nothing better to compare and a seat with no address is
/// still a seat.
pub fn governs(
    governors: &BTreeMap<String, Value>,
    workspace: &str,
    pane: Option<&str>,
) -> Option<String> {
    if workspace.is_empty() {
        return None;
    }
    governors
        .iter()
        .find(|(p, rec)| seat_of(p, rec).is_some_and(|s| s.sat_in(workspace, pane)))
        .map(|(p, _)| p.clone())
}

/// Is this stopped agent a person's problem?
///
/// The rule `wip` and the panel have both always applied — a process running no
/// turn on a `doing` task means whoever is working it has stopped and a person
/// is the blocker — with the one exception the seat creates. A governor is idle
/// *between* the agents it is sequencing, which is most of the time, and
/// reading that as a stall marked the busiest agent on the machine as stuck.
///
/// Here rather than at the three call sites so the exception is stated once. It
/// was already the same expression written out three times; it is now the same
/// expression with a reason attached, written out none. Whether the pane is a
/// seat comes in as a bool because the two callers know it differently — `wip`
/// has the map in hand, the panel has already joined it onto the row — and
/// neither should have to hold the other's shape to ask the question.
///
/// The first argument is named `stopped` and not `idle`, and robustness-083
/// renamed it because the two are not the same and the difference was three
/// silent failures wide. `agent_status == "idle"` is one of the *three* words
/// herdr has for a pane running no turn — `done` and `blocked` are the others,
/// and on this machine on 2026-08-19 four of twelve agents were answering
/// `done`. Every one of them was stopped on live work and reported as busy.
/// [`crate::place::State::turn_in_flight`] is the reading that answers this
/// without enumerating herdr's spelling.
pub fn needs_a_person(stopped: bool, doing: bool, seat: bool) -> bool {
    stopped && doing && !seat
}

/// Put a workspace in a project's slot, and say whom it displaced.
///
/// Taken by [`govern`] from a pane that is already sitting there, and by
/// [`crate::cmd_spawn`] on behalf of a workspace it has just opened — which is
/// the whole reason this is a function rather than four lines inside the
/// command. A custodian spawned into a new workspace is the *normal* way a slot
/// gets filled now, and that caller has no environment of its own to read.
/// What herdr calls a workspace, and the pane in it, that holds a slot.
///
/// The sidebar is the other tree a person reads, and until this it described
/// the seat by the task it had borrowed: measured 2026-08-17 on the live seat,
/// `robustness/078 · build a design artefact fo…`, with nothing anywhere
/// saying seat, `robustness` or `wsp`. A position that is invisible in the one
/// place you look all day is a position in name only.
///
/// The pane wears it too, which is the other half of the same lesson. A pane is
/// named after the work in it — and a custodian holds no work, so releasing its
/// borrowed claim left the panel drawing the position as `unassigned`. What a
/// slot's occupant is doing *is* the position, so that is its name.
///
/// The mark leads so the name is recognisable as ours without matching text —
/// see [`is_governor_label`], which is what stops a claim writing a task's title
/// over it.
/// The words. Ed's, 2026-08-17, on a row that read `▣ unassigned` after the
/// custodian released the task it had borrowed: *a custodian holding no task is
/// not unassigned and not empty — it is the governor of its project, which is
/// the most assigned thing on the panel.*
///
/// One string for every surface — the panel row, the pane and workspace names
/// herdr shows, `wsp wip`'s column — and each says the **project**, because
/// that is the one fact a slot always knows and the one an agent's own name can
/// never supply. `seat` is the vocabulary of the record and `slot` of the model;
/// this is what a person reads.
///
/// No glyph. `▣` is wsp's mark for the position and every surface of wsp's draws
/// it in its own first column, so a label carrying one too came out as `▣ ▣
/// governor · wsp` in the census at the foot of the panel. herdr's sidebar has
/// no mark column and gets the words, which is what it had for a task as well.
pub fn governor_of(project: &str) -> String {
    format!("governor · {project}")
}

/// Whether a name is one a slot put there.
///
/// Matched on the phrase, the way [`crate::cmd_agent`] matches a task's name on
/// its scope: the one part of the string nobody types by hand. What it protects
/// is the label of a workspace and a pane that hold a position — a claim
/// renames both after the work, which is right for a worker and wrong for a
/// governor, whose work *is* the position.
pub fn is_governor_label(label: &str) -> bool {
    label.trim_start().starts_with("governor · ")
}

/// Put the seat's own name on the workspace and on the agent pane in it, or
/// take it back off.
///
/// Both directions in one function because they are one rule read from either
/// end: a workspace holding a slot is named after it, and one that has just
/// given it up is handed back to herdr, which names an unnamed workspace after
/// whatever is standing in it. Only ever writes over a name of ours — a label
/// somebody typed is theirs, and a pane wearing a task's name is a pane doing
/// that task.
fn rename_seat(store: &Store, workspace: &str) {
    if workspace.is_empty() || !herdr::available() {
        return;
    }
    // The room's question, not a pane's: this names the workspace after its
    // seat, so it wants to know whether the room holds one at all.
    let held = governs(&store.governors(), workspace, None);
    let panes = herdr::panes().unwrap_or_default();
    // Every agent pane in the room. A workspace usually has one; a second agent
    // in there is somebody else's work and keeps its own name, which is why
    // only ours are renamed back.
    let ours: Vec<&herdr::Pane> = panes
        .iter()
        .filter(|p| p.workspace_id == workspace && !p.agent.is_empty())
        .collect();
    match held {
        Some(project) => {
            let label = governor_of(&project);
            let _ = herdr::rename_workspace(workspace, &label);
            for p in ours {
                // A pane still holding a task keeps the task's name: it says
                // what is happening in there now, and what is happening is that
                // task. The seat's name is for the pane that has nothing else
                // to be called.
                if p.label.is_empty() || is_governor_label(&p.label) || p.label == crate::cmd_agent::UNASSIGNED_LABEL {
                    let _ = herdr::rename_pane(&p.pane_id, &label);
                }
            }
        }
        None => {
            if herdr::workspaces().unwrap_or_default().iter().any(|w| w.id == workspace && is_governor_label(&w.label)) {
                let _ = herdr::rename_workspace(workspace, "");
            }
            for p in ours.iter().filter(|p| is_governor_label(&p.label)) {
                let _ = herdr::rename_pane(&p.pane_id, crate::cmd_agent::UNASSIGNED_LABEL);
            }
        }
    }
}

pub fn take(store: &Store, project: &str, workspace: &str, pane: &str) -> Option<Seat> {
    // Taking a seat somebody else is in is allowed and is said out loud. The
    // alternative is a refusal on a record whose whole content is "an agent is
    // sitting here", which goes stale every time a session ends without
    // standing down — and a seat you cannot take back after a crash is worse
    // than one that changes hands with a line of output.
    let displaced = store
        .governors()
        .get(project)
        .and_then(|rec| seat_of(project, rec))
        .filter(|s| s.workspace != workspace);

    // One agent, one governorship. Taking a second slot stands this workspace
    // down from the one it held, the way claiming a second task hands off the
    // first rather than quietly leaving it claimed — and for the same reason:
    // a record that says an agent is in two places is a record nothing can draw
    // and nobody can vacate. The slot it leaves stays on its project, empty.
    //
    // The room and not the pane, which is the asymmetry worklist-035 leaves
    // behind and it is deliberate: **a read must be exact and a write must be
    // conservative.** A wrong yes on a read is silent — a worker exempted from
    // `needs_a_person` for its whole life. A pane-exact write here would be the
    // opposite failure: a seat re-taken from a new pane in the same room would
    // leave the old record standing, and two records naming one workspace is a
    // shape `rename_seat` cannot name and `occupant` cannot resolve.
    if let Some(had) = governs(&store.governors(), workspace, None).filter(|p| p != project) {
        vacate(store, &had);
    }

    // The thread is kept when the room is. An agent re-running `wsp govern`
    // in the seat it already holds — which `wsp resume` does, and which a
    // custodian does by hand after a `/clear` — must not erase the session it
    // is running under, because the window between wiping it and `sync`
    // learning it again is a window in which a herdr restart loses the seat
    // for good. A *different* workspace is a different occupant and starts
    // with nothing recorded, which is the same rule read from the other end.
    let kept = store
        .governors()
        .get(project)
        .filter(|rec| str_at(rec, "workspace") == workspace)
        .map(|rec| (str_at(rec, "session"), str_at(rec, "cwd")))
        .unwrap_or_default();
    store.set_governor(
        project,
        json!({
            "workspace": workspace,
            "pane": pane,
            "host": util::hostname(),
            "since": util::now_iso(),
            "session": kept.0,
            "cwd": kept.1,
        }),
    );
    store.log_event("governor-set", json!({ "project": project, "workspace": workspace }));
    rename_seat(store, workspace);
    displaced
}

/// Empty a slot without taking it off the project.
///
/// The half of "it outlives its occupant" that a person can see. Every earlier
/// version of standing down deleted the record, so a night that ended took the
/// position down with the agent and the project woke up with no governor and no
/// sign there had ever been one. What is dropped is the occupancy — workspace,
/// pane, host — and what is kept is the post, which is the thing another agent
/// can be put into tomorrow.
///
/// Also what `reconcile --reap` does to a slot whose workspace herdr has
/// closed: the agent is gone, the position is not.
///
/// **What it drops is the occupancy and what it keeps is the way back.** The
/// record it leaves carries `last`: the workspace, host, session and cwd of
/// whoever was just in it, which is exactly what `wsp resume` needs and exactly
/// what nothing else must read. Under its own key rather than left in place,
/// because every reader of a governor record asks *who is in this seat now* —
/// [`seat_of`], [`governs`], [`Slot::elsewhere`] — and a vacated record that
/// still answered `workspace` or `host` at the top level would tell all three
/// that somebody is sitting here. The nesting is the whole of the guarantee:
/// one key nobody had reason to look in before, so a slot cannot be brought
/// back to life by a field left behind.
pub fn vacate(store: &Store, project: &str) -> bool {
    let governors = store.governors();
    let Some(rec) = governors.get(project) else { return false };
    // Already empty. Said as false so a caller can report what it actually
    // changed rather than what it looked at.
    if seat_of(project, rec).is_none() && rec.get("workspace").is_none() {
        return false;
    }
    let was = rec.get("workspace").and_then(Value::as_str).unwrap_or_default().to_string();
    store.set_governor(project, json!({ "vacated": util::now_iso(), "last": stood_down(rec) }));
    store.log_event("governor-vacated", json!({ "project": project }));
    // The room keeps the name of whatever it still answers for, and gets its
    // own back when that is nothing.
    rename_seat(store, &was);
    true
}

/// What `doctor` says about the seats: which of them nobody is sitting in.
///
/// **This is the check that was missing, and it is the reason worklist-035 was
/// found by accident rather than reported.** On `796c2d2`, with the `acc`
/// seat's pane nine minutes dead: `wsp govern` listed it as live, `wsp flag`
/// went on answering *raised to the acc governor · w1*, `reconcile --reap`
/// printed `emptied 0`, and `wsp doctor` said `✓ no problems`. Every surface
/// wsp has agreed, and every one of them was wrong. The manual recipe the seat
/// wrote down in the meantime was `wsp govern | grep -v empty` and then
/// `wsp peek` on each pane it named — which is this function, typed out.
///
/// A **problem** rather than a note, on `cmd_verify`'s rule: an empty seat is
/// the state [`crate::cmd_govern`]'s own comment calls *worse than no seat* —
/// hands go on being routed to nobody, and until [`governs`] was made exact the
/// next agent into that room inherited the position as well. Nothing else in
/// wsp reports it, so a note here would be a fact nobody reads about a failure
/// nobody sees.
///
/// Silence is not evidence, the same as everywhere else: a herdr that is down,
/// unreachable, or answered a pane listing with nothing gets no opinion. That
/// is why this takes the probe rather than calling herdr itself — `doctor` has
/// already paid for the census, and the two must not disagree about what was
/// heard.
pub fn health(probe: &crate::cmd_agent::Probe, store: &Store, problems: &mut Vec<String>) {
    let crate::cmd_agent::Probe::Up { panes, .. } = probe else { return };
    if panes.is_empty() {
        return;
    }
    for slot in slots(&store.governors()) {
        let Some(seat) = &slot.occupant else { continue };
        // Nothing to check against. `wsp govern -w` names a room this process
        // was not standing in and writes no pane, and a record with no address
        // is not evidence of an empty one.
        if seat.pane.is_empty() {
            continue;
        }
        if panes.iter().any(|p| p.pane_id == seat.pane) {
            continue;
        }
        // The pane and the room are said separately because the repairs
        // differ, and the repair is the half of this line worth reading. A room
        // that is still open can be sat in again as it stands; one that has
        // gone with its pane needs somewhere to sit first.
        let (state, fill) = match panes.iter().any(|p| p.workspace_id == seat.workspace) {
            true => (
                format!("its pane {} is gone and {} is still open", seat.pane, seat.workspace),
                format!("`wsp govern {} -w {}` puts somebody back in it", slot.scope, seat.workspace),
            ),
            false => (
                format!("its pane {} and its workspace {} are both gone", seat.pane, seat.workspace),
                // Not `wsp spawn --govern`, which takes a project: a scope here
                // is a project *or* a worklist slug, and half the hints would
                // have named a verb that cannot take it.
                format!("`wsp govern {}` from a workspace that has one seats it there", slot.scope),
            ),
        };
        problems.push(format!(
            "the seat for `{}` is empty — {state}, and every hand raised under it \
             is being addressed to nobody. {fill}; `wsp govern {} --clear` leaves \
             the post standing and says out loud that nobody is in it",
            slot.scope, slot.scope
        ));
    }
}

/// The occupancy a vacated record keeps, out of the record it is replacing.
///
/// Everything a resume is keyed on and nothing a live reader looks at. A
/// record that was already vacated hands its own `last` back rather than
/// wrapping it again — standing an empty slot down twice must not bury the
/// thread one level deeper each time, and `reconcile --reap` runs on every
/// daemon start.
fn stood_down(rec: &Value) -> Value {
    if let Some(last) = rec.get("last") {
        return last.clone();
    }
    json!({
        "workspace": str_at(rec, "workspace"),
        "pane": str_at(rec, "pane"),
        "host": str_at(rec, "host"),
        "session": str_at(rec, "session"),
        "cwd": str_at(rec, "cwd"),
        "since": str_at(rec, "since"),
    })
}

/// The last agent to hold this slot, filled or not — the reader `wsp resume`
/// asks and the only one entitled to look in `last`.
///
/// A live occupant is preferred over a remembered one, and both are answered
/// as a [`Seat`] because they are the same fact at different ages: this is who
/// was sitting here, where, and under which session. `elsewhere` is not
/// filtered out here the way [`seat_of`] filters it — a resume names the host
/// it is going to, and refusing to *read* a seat on another machine would make
/// `wsp resume` unable to say "that seat is on mb2" at all.
pub fn last_seat(governors: &BTreeMap<String, Value>, scope: &str) -> Option<Seat> {
    let rec = governors.get(scope)?;
    let of = |r: &Value| {
        let workspace = str_at(r, "workspace");
        (!workspace.is_empty()).then(|| Seat {
            scope: scope.to_string(),
            workspace,
            pane: str_at(r, "pane"),
            since: str_at(r, "since"),
            session: str_at(r, "session"),
            cwd: str_at(r, "cwd"),
        })
    };
    of(rec).or_else(|| rec.get("last").and_then(of))
}

/// The host a slot was last held from — a live occupancy's, or a vacated
/// record's memory of one. Empty where nothing has ever sat here.
pub fn host_of(governors: &BTreeMap<String, Value>, project: &str) -> String {
    let Some(rec) = governors.get(project) else { return String::new() };
    match str_at(rec, "host") {
        h if !h.is_empty() => h,
        _ => rec.get("last").map(|l| str_at(l, "host")).unwrap_or_default(),
    }
}

/// Record against each seat the session the backend says is sitting in it.
///
/// The seat-shaped half of [`crate::cmd_agent::learn_sessions`], and separate
/// from it for one reason that is not tidiness: a binding is keyed on a **pane**
/// and a seat on a **workspace**, so the two cannot share a loop over the same
/// key. Everything else about them is the same judgement, and the argument for
/// both rules lives on that function: silence is not a correction, a different
/// session is. The cwd travels with the session because a transcript resumed in
/// the wrong tree is worse than one not resumed at all — `claude --resume`
/// takes the id and inherits the directory from wherever it is run.
///
/// Called from `sync`, which reads every pane on every tick and therefore pays
/// no round-trip for this, and from `spawn`, which asks once the moment its
/// custodian is up. Returns how many records changed, which is zero on every
/// tick after the one where an agent started.
///
/// **The pane is in the tuple because this is a writer.** Keyed on the room it
/// was the same fault as everywhere else, in the one place where it corrupts
/// rather than merely mis-draws: a second agent in a governed workspace has a
/// different session id, so the record's `session` was rewritten to the
/// *worker's* on the next tick — and `session` is what `wsp resume` uses to
/// bring the custodian back. A seat sharing a room would have been resumed as
/// its neighbour. worklist-035; [`governs`] carries the argument.
pub fn learn_seats<'a>(
    store: &Store,
    seen: impl Iterator<Item = (&'a str, &'a str, &'a str, &'a str)>,
) -> usize {
    let governors = store.governors();
    // Workspace -> project, computed once: `governs` is a scan, and a machine
    // running twenty panes would otherwise scan the whole file per pane.
    let learned: Vec<(String, String, String)> = seen
        .filter(|(_, _, session, _)| !session.trim().is_empty())
        .filter_map(|(workspace, pane, session, cwd)| {
            let project = governs(&governors, workspace, Some(pane))?;
            let seat = seat_of(&project, governors.get(&project)?)?;
            // The session is the only field a change is judged on. A cwd that
            // has moved under a session wsp already knows is herdr answering
            // about a pane whose shell has `cd`-ed, and the tree the agent was
            // *started* in is the one to bring it back in.
            (seat.session != session).then(|| (project, session.to_string(), cwd.to_string()))
        })
        .collect();
    if learned.is_empty() {
        return 0;
    }
    store.locked(|| {
        let governors = store.governors();
        for (project, session, cwd) in &learned {
            // Re-read inside the lock, for `learn_sessions`' reason: a
            // `wsp govern` landing between the two readings has moved the slot
            // to another workspace, and writing back from the stale copy would
            // put this session on somebody else's seat.
            let Some(mut rec) = governors.get(project).cloned() else { continue };
            let Some(o) = rec.as_object_mut() else { continue };
            o.insert("session".to_string(), json!(session));
            if !cwd.is_empty() {
                o.insert("cwd".to_string(), json!(cwd));
            }
            store.set_governor(project, rec);
            store.log_event(
                "session-learned",
                json!({ "project": project, "session": session, "cwd": cwd }),
            );
        }
    });
    learned.len()
}

/// The pane the slot's agent is in *now*, which is not the pane it started in.
///
/// The record names a workspace because that is the durable half, and the pane
/// on it can go stale — an agent cleared and restarted comes back on another
/// pane in the same room. So anything that speaks to a slot asks the runner who
/// is in that room at the moment of speaking, and a slot answered by nobody is
/// a vacancy rather than a stale address.
///
/// **The room stands in for the pane only where the room is unambiguous**, and
/// that qualification is worklist-035's. This is the *speaking* path — `wsp
/// govern <scope> --tell`, and the panel's `T` — so a wrong answer here hands a
/// custodial work order to whichever agent the iterator reached first. With one
/// agent in the room the fallback is sound: that agent is the restarted seat,
/// there is nobody else it could be. With two it is a guess, and a guess that
/// delivers direction meant for the custodian to a worker under it. Two agents
/// and a stale pane is exactly the state that fault was filed on, so the
/// fallback stops there and the caller gets *the seat is empty* — which is
/// true, and which names the repair.
pub fn occupant(seat: &Seat) -> Option<herdr::Pane> {
    let panes = herdr::panes().ok()?;
    // The recorded pane first when it is still there and still has an agent.
    if let Some(p) = panes.iter().find(|p| p.pane_id == seat.pane && !p.agent.is_empty()) {
        return Some(p.clone());
    }
    let mut in_the_room = panes.iter().filter(|p| p.workspace_id == seat.workspace && !p.agent.is_empty());
    match (in_the_room.next(), in_the_room.next()) {
        (Some(only), None) => Some(only.clone()),
        _ => None,
    }
}

/// What a name means as a **scope**: a worklist slug, or a project.
///
/// One key space, enforced at the moment a name is handed out
/// (`Store::scope_taken`), so at most one of the two can answer and the order
/// settles nothing — except between an exact name and a fuzzy one. That is why
/// the exact worklist is asked before [`Index::find`], whose last resort is a
/// unique id prefix, and the fuzzy worklist after it: a list called `batch`
/// must not lose its own name to a project whose id merely begins with it.
///
/// **The status is not asked.** A seat is taken on a list before it runs —
/// that is how somebody comes to be sitting there to start it — and it is only
/// the routing in [`seat_for`] that cares whether the run has begun.
fn scope_of(store: &Store, index: &Index, needle: &str) -> Option<String> {
    let n = needle.trim().to_ascii_lowercase();
    if n.is_empty() {
        return None;
    }
    if store.worklist(&n).is_some() {
        return Some(n);
    }
    if let Some(proj) = index.find(&n) {
        return Some(proj.id.clone());
    }
    let mut near = store.worklists().into_iter().filter(|w| w.id.starts_with(&n));
    match (near.next(), near.next()) {
        (Some(w), None) => Some(w.id),
        _ => None,
    }
}

/// `wsp govern [<scope>] [--clear|--remove|--tell "…"]`
pub fn govern(store: &Store, args: &Args) -> i32 {
    let p = Paint::new();
    let index = Index::new(store.projects());
    let env = herdr::Env::read();
    let workspace = args.get("workspace").or(env.workspace_id.clone());
    // The pane is the environment's only when the workspace is too. `-w` names
    // a room this process is not standing in, so its own pane says nothing
    // about who is sitting there — it must not be stamped onto that record
    // (the despawn guard would point at a pane in another workspace) and it
    // must not be used to ask who the seat is either.
    let pane = match args.get("workspace") {
        Some(_) => None,
        None => env.pane_id.clone().filter(|p| !p.is_empty()),
    };
    let governors = store.governors();

    if args.has("clear") || args.has("remove") {
        return stand_down(store, &index, args, workspace.as_deref());
    }

    // Nothing named: report. Inside a workspace that is the answer to "what am
    // I the seat for, and who is the seat above me"; outside one there is no
    // such question, so it is the roster. Neither changes anything — a command
    // that only reports is one an agent can run without having decided.
    let Some(needle) = args.rest.first().cloned() else {
        return report(store, &index, args, workspace.as_deref(), pane.as_deref());
    };

    let Some(scope) = scope_of(store, &index, &needle) else {
        eprintln!("wsp: no such project or worklist `{needle}`");
        return 1;
    };

    // Speaking to the slot never takes it. A person naming a project and a
    // sentence is talking to whoever is in the position, and the commonest
    // mistake this shape could make — a typo in the sentence flag silently
    // making the shell that typed it the governor — is worth one branch to
    // rule out.
    if args.has("tell") {
        // `--tell` normally carries the sentence. It does not when the sentence
        // begins with a dash — the flag parser reads that as the next flag —
        // so the words after the project are taken as the sentence too, and a
        // person who types the obvious thing is not answered with a parse.
        let typed = told(args);
        // The verb this task was raised on. A governor brief is the longest
        // prose anything in wsp sends, and it is prose *about the code* — file
        // names, verb names, identifiers — so it is written with backticks,
        // and inside the double quotes a shell needs for a paragraph every one
        // of them runs a command. What arrived on 2026-08-18 was fluent with
        // the load-bearing nouns missing, and nothing at the receiving end
        // looked truncated. `-` is what `edit --overview`, `note`, `flag
        // --body` and `tell` already spell; until this it was delivered to the
        // governor as the literal word `-`, which is the same defect `wsp note`
        // was fixed for, one file along.
        // And `--from FILE` beside it, because a governor brief is the longest
        // prose anything in wsp sends and the brief that asks for one says to
        // pass it through a file. One function for all four telling verbs —
        // see [`crate::cmd_agent::from_source`].
        let text = match crate::cmd_agent::from_source(args, &typed) {
            Ok(t) => t,
            Err(code) => return code,
        };
        return tell(store, &governors, &scope, &text, args);
    }

    let Some(ws) = workspace else {
        eprintln!("wsp: no workspace — pass -w, or run inside herdr");
        return 2;
    };

    // What this workspace is giving up by taking this, read before the write
    // rather than after it. One agent holds one governorship, so `take` hands
    // the old one back — and a hand-over that happens silently is one you find
    // out about from a panel a day later.
    //
    // Asked of the room and not of this pane, for [`take`]'s reason: what is
    // being handed back is a *write*, and a write leaves the coarse reading
    // alone deliberately.
    let handed_back = governs(&governors, &ws, None).filter(|p| p != &scope);
    let displaced = take(store, &scope, &ws, pane.as_deref().unwrap_or_default());

    if args.json() {
        println!(
            "{}",
            json!({
                // The key keeps the word `project` while the value has become a
                // scope, on the bar this change is held to: with no worklist
                // running every output in this tree is byte-for-byte what it
                // was, and a renamed key is a broken reader for the benefit of
                // a name.
                "project": scope,
                "workspace": ws,
                "displaced": displaced.map(|s| s.workspace),
                "stood_down_from": handed_back,
            })
        );
        return 0;
    }
    println!("{} {}", p.cyan("▣"), p.bold(&scope));
    if let Some(was) = displaced {
        println!("  {}", p.dim(&format!("taken from {}", was.workspace)));
    }
    if let Some(was) = &handed_back {
        println!("  {}", p.dim(&format!("stood down from {was}, and its seat is open")));
    }
    println!("  {}", p.dim("raised hands here reach this workspace · wsp govern --clear to stand down"));
    0
}

/// The sentence as it was typed, out of the three shapes the flag parser can
/// hand it over in.
///
/// `--tell` normally carries the sentence as its value. It does not when the
/// sentence begins with a dash, because the parser reads that as the next flag
/// — so the words after the project are taken as the sentence too, and a person
/// who types the obvious thing is not answered with a parse. That fallback is
/// also what makes `--tell -` work without a case of its own: a lone dash is
/// not a value either, so it lands in `rest` and comes back out here.
fn told(args: &Args) -> String {
    match args.get("tell") {
        Some(t) if t != "true" => t,
        _ => args.rest[1..].join(" "),
    }
}

/// `wsp govern <scope> --tell "…"` — a sentence for whoever is in the slot.
///
/// The surface the position was missing. A person has been directing the
/// governor all night through a terminal that happens to contain it, and wsp
/// knew nothing about any of it; this is that conversation addressed to the
/// **post** instead of to a pane somebody had to find first.
///
/// Through [`crate::agent_commands`], because how a sentence reaches an agent
/// is a fact about the agent's kind and not about herdr. And with no `/clear`
/// in front of it, unlike every hand-over in `panel::verbs`: a work order is
/// given to an agent that has just finished something else, where this is a
/// word to one that is in the middle of a night's sequencing. Emptying the
/// governor's context to speak to it would destroy the one thing the position
/// exists to hold.
fn tell(store: &Store, governors: &BTreeMap<String, Value>, scope: &str, text: &str, args: &Args) -> i32 {
    use crate::place::Seat as Pane;

    let text = text.trim();
    if text.is_empty() {
        eprintln!("wsp: nothing to say");
        return 2;
    }
    let Some(seat) = governors.get(scope).and_then(|rec| seat_of(scope, rec)) else {
        eprintln!("wsp: no seat on `{scope}` — wsp govern {scope} fills it");
        return 1;
    };
    let Some(pane) = occupant(&seat) else {
        eprintln!("wsp: the {scope} seat is empty — nobody is in {} to tell", seat.workspace);
        return 1;
    };

    let place = crate::place_herdr::Herdr::new();
    let how = crate::agent_commands::of(&pane.agent);
    let sent = crate::cmd_agent::Sent::new(
        scope,
        &format!("the {scope} seat"),
        &pane.pane_id,
        &pane.pane_id,
        text,
        args,
    );
    if let Some(ago) = sent.already_sent(store) {
        if !args.has("again") {
            return crate::cmd_agent::twice(&sent, ago, &Paint::new());
        }
    }
    crate::cmd_agent::delivered(store, how.tell(&place, &Pane::new(&pane.pane_id), text), &sent)
}

/// `wsp govern --clear [<project>]` — this workspace stops being the seat.
///
/// Bare, it stands down from everything this workspace holds, because a session
/// ending does not end one seat at a time. Named, it gives up one and keeps the
/// rest, which is how a seat that governs `wsp` and `robustness` hands the
/// second one on.
///
/// `--clear` vacates and `--remove` takes the slot off the project, and the
/// difference is which of the two facts changed. An agent standing down is a
/// fact about the agent — the position is still the project's, still drawn,
/// still fillable by the next one. Deciding a project needs no governor at all
/// is a fact about the project, and it is rare enough to be worth typing.
fn stand_down(store: &Store, index: &Index, args: &Args, workspace: Option<&str>) -> i32 {
    let p = Paint::new();
    let governors = store.governors();
    let remove = args.has("remove");
    let held: Option<String> = match args.rest.first() {
        Some(needle) => match scope_of(store, index, needle) {
            Some(scope) => Some(scope),
            None => {
                eprintln!("wsp: no such project or worklist `{needle}`");
                return 1;
            }
        },
        None => match workspace {
            // Coarse for [`take`]'s reason — this is the write side. A bare
            // `--clear` stands the *room* down, because a session ending is a
            // room emptying and a custodian back on a new pane must still be
            // able to give up the seat it holds.
            Some(ws) => governs(&governors, ws, None),
            None => {
                eprintln!("wsp: no workspace — pass -w, or name the scope");
                return 2;
            }
        },
    };

    let cleared = held.filter(|proj| match remove {
        true => {
            let gone = store.clear_governor(proj);
            if gone {
                store.log_event("governor-cleared", json!({ "project": proj }));
                rename_seat(store, workspace.unwrap_or_default());
            }
            gone
        }
        false => vacate(store, proj),
    });

    if args.json() {
        println!("{}", json!({ "cleared": cleared, "removed": remove }));
    } else {
        match (&cleared, remove) {
            (None, _) => println!("{}", p.dim("this workspace is nobody's seat")),
            (Some(proj), true) => println!("{} {}", p.dim("no seat any more on —"), proj),
            (Some(proj), false) => println!("{} {}", p.dim("stood down, seat left open —"), proj),
        }
    }
    0
}

/// What is seated: this workspace's own, the seat above it, or the whole roster.
fn report(store: &Store, index: &Index, args: &Args, workspace: Option<&str>, pane: Option<&str>) -> i32 {
    let p = Paint::new();
    let governors = store.governors();
    // "What am I the seat for" is a read, so it is the pane's — a worker
    // sharing a room with a custodian is not the custodian. See [`governs`].
    let mine: Option<String> = workspace.and_then(|ws| governs(&governors, ws, pane));

    let slots = slots(&governors);

    if args.json() {
        let seats: Vec<Value> = slots
            .iter()
            .map(|s| {
                json!({
                    // `project` for the reason the receipt above keeps it: the
                    // key is what a reader was written against and the value is
                    // now a scope.
                    "project": s.scope,
                    "workspace": s.occupant.as_ref().map(|o| o.workspace.clone()),
                    "pane": s.occupant.as_ref().map(|o| o.pane.clone()),
                    "filled": s.filled(),
                    "host": s.host,
                    "since": s.since,
                })
            })
            .collect();
        println!("{}", json!({ "workspace": workspace, "governs": mine, "seats": seats }));
        return 0;
    }

    if slots.is_empty() {
        println!("{}", p.dim("no seats — wsp govern <scope> takes one"));
        return 0;
    }
    // Vacant slots draw too, and that is the point of the list: a position
    // nobody is standing in is the row you are reading this to find.
    for s in &slots {
        let here = mine.as_deref() == Some(s.scope.as_str());
        let mark = if here { p.cyan("▣") } else { p.dim("·") };
        let who = match (&s.occupant, s.elsewhere()) {
            (Some(o) , _) if o.pane.is_empty() => o.workspace.clone(),
            (Some(o), _) => format!("{} · {}", o.workspace, o.pane),
            (None, true) => format!("on {}", s.host),
            (None, false) => "empty · wsp spawn -p <project> --govern fills it".to_string(),
        };
        println!("{} {}  {}", mark, p.bold(&s.scope), p.dim(&who));
    }
    if mine.is_none() {
        // Where a hand raised *here* would go. The question an agent asks is
        // never "who are the seats" — it is "who is mine" — and the roster
        // above answers the first without answering the second.
        let project = crate::cmd_agent::current_project(store, args, index).ok().flatten();
        // The task in hand as well as the project, because the first step of
        // the walk is keyed on the task: an agent standing on a member of
        // tonight's list is answered for by the list, and a roster that told it
        // otherwise would be wrong about the one thing it was asked.
        let held = crate::cmd_agent::task_in_hand(
            &store.bindings(),
            &store.claims(),
            crate::cmd_agent::my_pane().as_deref(),
            workspace,
        );
        let lists = crate::worklist::Running::read(store);
        let list = held.as_deref().and_then(|t| lists.list_of(t));
        match seat_for(&governors, index, list, project.as_deref()) {
            Some(s) => println!("{}", p.dim(&format!("work here reaches the {} seat", s.scope))),
            None => println!("{}", p.dim("no seat above this pane — raised hands reach a person")),
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Project;

    fn tree() -> Index {
        let mut wsp = Project::new("wsp");
        wsp.parent = Some("tooling".into());
        let mut rob = Project::new("robustness");
        rob.parent = Some("wsp".into());
        let mut data = Project::new("data");
        data.parent = Some("robustness".into());
        Index::new(vec![Project::new("tooling"), wsp, rob, data])
    }

    /// The message the shell rewrote, and the spelling that stops it.
    ///
    /// Every shape has to arrive as the same string, because the one that does
    /// not is the one that gets sent: `--tell -` used to reach the governor as
    /// the literal word `-`, delivered, receipted and empty of everything the
    /// sender wrote.
    #[test]
    fn the_dash_reaches_govern_as_the_stream_in_every_spelling() {
        let parse = |line: &[&str]| Args::parse(line.iter().map(|s| (*s).to_string()).collect());

        for line in [
            vec!["govern", "render", "--tell", "-"],
            vec!["govern", "render", "--tell=-"],
        ] {
            assert_eq!(told(&parse(&line)).trim(), "-", "{line:?}");
        }

        // …and a sentence is still a sentence, in both the shapes that carry
        // one: as the flag's value, and as the words after the project when it
        // begins with a dash the parser would have eaten.
        let a = parse(&["govern", "render", "--tell", "come and look at this"]);
        assert_eq!(told(&a), "come and look at this");
        let b = parse(&["govern", "render", "--tell", "--overview is the one to read"]);
        assert_eq!(told(&b), "--overview is the one to read");
    }

    fn seated(pairs: &[(&str, &str)]) -> BTreeMap<String, Value> {
        pairs
            .iter()
            .map(|(proj, ws)| {
                (proj.to_string(), json!({ "workspace": ws, "host": util::hostname() }))
            })
            .collect()
    }

    #[test]
    fn a_hand_raised_in_a_project_with_a_seat_reaches_that_seat() {
        let g = seated(&[("robustness", "w1"), ("wsp", "w9")]);
        let s = seat_for(&g, &tree(), None, Some("robustness")).unwrap();
        assert_eq!((s.scope.as_str(), s.workspace.as_str()), ("robustness", "w1"));
    }

    /// The escalation question the overview asks, and the answer is that there
    /// is no escalation step — the walk simply does not stop at an empty level.
    #[test]
    fn a_hand_raised_where_the_local_seat_is_empty_reaches_the_one_above() {
        let g = seated(&[("wsp", "w9")]);
        let s = seat_for(&g, &tree(), None, Some("data")).unwrap();
        assert_eq!(s.scope, "wsp", "past robustness, which has nobody in it");
    }

    /// The normal state, and the one that has to stay cheap: no seat anywhere
    /// is not an error, a default or a fallback seat. It is `None`, which every
    /// caller already draws as today's behaviour.
    #[test]
    fn no_seat_anywhere_above_a_project_is_simply_no_seat() {
        assert_eq!(seat_for(&BTreeMap::new(), &tree(), None, Some("data")), None);
        assert_eq!(seat_for(&seated(&[("robustness", "w1")]), &tree(), None, Some("tooling")), None);
    }

    /// A seat is reachable from below and from nowhere else. `robustness` is
    /// inside `wsp`, so the `wsp` seat answers for it; `tooling` is outside
    /// `robustness`, and a seat that answered downward would have every hand in
    /// the tree arriving at every seat in it.
    #[test]
    fn a_seat_answers_for_what_is_under_it_and_not_for_its_siblings() {
        let g = seated(&[("robustness", "w1")]);
        let index = tree();
        assert!(seat_for(&g, &index, None, Some("data")).is_some());
        assert_eq!(seat_for(&g, &index, None, Some("wsp")), None);
    }

    /// A workspace id is herdr's, and herdr's ids are per machine. A seat taken
    /// on the laptop is not a seat the desktop can reach, and reading it as one
    /// would address every raised hand at a workspace that does not exist here.
    #[test]
    fn a_seat_on_another_machine_is_not_a_seat_here() {
        let g: BTreeMap<String, Value> =
            [("robustness".to_string(), json!({ "workspace": "w1", "host": "somewhere-else" }))]
                .into_iter()
                .collect();
        assert_eq!(seat_for(&g, &tree(), None, Some("robustness")), None);
    }

    /// The step this task added, and the whole of what it is for: a hand
    /// raised on a member of tonight's run reaches **whoever is running it**,
    /// not whoever governs the project the task happens to live in.
    ///
    /// The `batch` was made a project to get this answer and it cost 26 tasks a
    /// move in and a move back out. `render-071` stays in `render` now, and the
    /// routing is what moves.
    #[test]
    fn a_hand_on_a_member_of_a_running_list_reaches_the_lists_seat() {
        let g = seated(&[("batch", "w7"), ("robustness", "w1"), ("wsp", "w9")]);
        let s = seat_for(&g, &tree(), Some("batch"), Some("robustness")).unwrap();
        assert_eq!((s.scope.as_str(), s.workspace.as_str()), ("batch", "w7"));

        // And the same task with nothing running is answered by its project,
        // which is the sentence above read backwards: the list is the only
        // thing that changed, so it is the only thing that may change the
        // answer.
        let s = seat_for(&g, &tree(), None, Some("robustness")).unwrap();
        assert_eq!(s.scope, "robustness");
    }

    /// The worklist step is one more level on the same walk and not a policy of
    /// its own — so it does not stop at a list nobody is sitting in.
    ///
    /// Both shapes of "nobody": a list with no record at all, which is a run
    /// nobody has taken the seat of, and a list whose slot has been vacated,
    /// which is the governor that stood down at 4am. Either one falls through
    /// to the project chain, because the alternative is a raised hand delivered
    /// to an empty room while a seat that would have answered sits one level up.
    #[test]
    fn a_list_with_nobody_in_its_seat_falls_through_to_the_project_chain() {
        let g = seated(&[("robustness", "w1")]);
        let s = seat_for(&g, &tree(), Some("batch"), Some("data")).unwrap();
        assert_eq!(s.scope, "robustness", "past a list with no seat, and past data");

        let (_env, store) = store("fallthrough");
        take(&store, "batch", "w7", "w7:p1");
        take(&store, "robustness", "w1", "w1:p1");
        assert!(vacate(&store, "batch"), "the governor stood down mid-run");
        let s = seat_for(&store.governors(), &tree(), Some("batch"), Some("data")).unwrap();
        assert_eq!(s.scope, "robustness", "an empty list seat routes nothing, as an empty project one does");
    }

    /// The bar the whole change is held to, as an assertion about the routing:
    /// **with nothing running, the walk is the ancestor walk it always was.**
    /// `list` is `None` in every session that has never made a worklist, and
    /// `None` costs one `Option` that is not iterated.
    #[test]
    fn with_nothing_running_the_walk_is_the_walk_it_was() {
        let g = seated(&[("robustness", "w1"), ("wsp", "w9")]);
        let index = tree();
        for project in ["data", "robustness", "wsp", "tooling"] {
            assert_eq!(
                seat_for(&g, &index, None, Some(project)).map(|s| s.scope),
                std::iter::once(project.to_string())
                    .chain(index.ancestors(project))
                    .find(|p| g.contains_key(p)),
                "{project}"
            );
        }
    }

    /// One key space, so a name is a project **or** a worklist and `wsp govern`
    /// takes either with no flag to say which.
    ///
    /// The order matters in exactly one place, and it is the reason the exact
    /// worklist is asked first: `Index::find`'s last resort is a unique id
    /// prefix, so a list called `batch` would otherwise lose its own name to a
    /// project called `batchelor`. Exact before fuzzy, on both sides.
    #[test]
    fn a_scope_is_a_worklist_slug_or_a_project_and_the_exact_name_wins() {
        let (_env, store) = store("scope");
        let mut wsp = crate::model::Project::new("wsp");
        wsp.name = "wsp".into();
        store.save_project(&wsp).unwrap();
        store.save_project(&crate::model::Project::new("batchelor")).unwrap();
        store.save_worklist(&crate::model::Worklist::new("batch", "Overnight batch")).unwrap();
        let index = Index::new(store.projects());

        assert_eq!(scope_of(&store, &index, "batch").as_deref(), Some("batch"), "its own name");
        assert_eq!(scope_of(&store, &index, "batchelor").as_deref(), Some("batchelor"));
        assert_eq!(scope_of(&store, &index, "wsp").as_deref(), Some("wsp"), "a project is still a scope");
        assert_eq!(scope_of(&store, &index, "bat").as_deref(), Some("batchelor"), "a unique project prefix");
        assert_eq!(scope_of(&store, &index, "nothing"), None);
        assert_eq!(scope_of(&store, &index, "  "), None);
    }

    /// A seat is taken on a list the same way it is taken on a project, because
    /// it is the same record under the same key — which is what "the key is a
    /// scope" buys and what makes `wsp govern <slug>` need no new flag.
    ///
    /// Including the rule that outlasts the change: one agent holds one of
    /// them, so a workspace that takes the list's seat hands back the project's.
    #[test]
    fn a_seat_on_a_list_is_a_seat_like_any_other() {
        let (_env, store) = store("list-seat");
        take(&store, "robustness", "w1", "w1:p1");
        take(&store, "batch", "w1", "w1:p1");

        assert_eq!(governs(&store.governors(), "w1", Some("w1:p1")).as_deref(), Some("batch"));
        let slots = slots(&store.governors());
        let robustness = slots.iter().find(|s| s.scope == "robustness").expect("the post stayed");
        assert!(!robustness.filled(), "and it was handed back, empty");
        assert_eq!(
            seat_for(&store.governors(), &tree(), Some("batch"), Some("robustness")).map(|s| s.workspace),
            Some("w1".to_string())
        );
    }

    /// A workspace holds more than one agent, and only one of them is the seat.
    ///
    /// **worklist-035, and every consequence of it is on this one line.** With
    /// `governs` keyed on the room, two spawns that landed in the custodian's
    /// workspace were each told they were the custodian: exempted from
    /// [`needs_a_person`] for their whole lives, handed a custodial work order
    /// in `wsp brief`, given the seat's inbox, and renamed after the seat when
    /// they finished their tasks. One of them was a member of a running
    /// worklist, on a night nobody was reading.
    #[test]
    fn a_second_agent_in_the_seats_workspace_is_a_worker_and_not_a_co_custodian() {
        let (_env, store) = store("two-in-a-room");
        take(&store, "acc", "w1", "w1:p2");
        let g = store.governors();

        assert_eq!(governs(&g, "w1", Some("w1:p2")).as_deref(), Some("acc"), "the seat itself");
        assert_eq!(governs(&g, "w1", Some("w1:p1")), None, "and its neighbour, which is nobody's seat");

        // The consequence, said as the predicate an unattended run depends on:
        // a worker stopped on a `doing` task is a person's problem, and sharing
        // a room with a custodian does not make it stop being one.
        assert!(
            needs_a_person(true, true, governs(&g, "w1", Some("w1:p1")).is_some()),
            "the worker beside the seat is still the loudest row on the panel",
        );
        assert!(
            !needs_a_person(true, true, governs(&g, "w1", Some("w1:p2")).is_some()),
            "and the seat is still idle between the agents it is waiting on",
        );
    }

    /// The one caller that is genuinely asking about the *room* keeps the
    /// coarse answer, and a record with no pane on it has nothing else to give.
    ///
    /// `wsp govern -w <ws>` writes no pane — the pane it is standing in is in
    /// another workspace entirely — and `sync` names a *workspace* after its
    /// seat, which is true of the workspace however many panes are in it.
    #[test]
    fn a_room_is_asked_about_as_a_room_and_a_record_with_no_pane_answers_for_one() {
        let (_env, store) = store("room");
        take(&store, "wsp", "w1", "w1:p6");
        assert_eq!(
            governs(&store.governors(), "w1", None).as_deref(),
            Some("wsp"),
            "the workspace holds a seat, which is what the workspace token says",
        );

        let hand_written = seated(&[("wsp", "w1")]);
        assert_eq!(
            governs(&hand_written, "w1", Some("w1:p1")).as_deref(),
            Some("wsp"),
            "no pane recorded is nothing to compare, and a seat with no address is still a seat",
        );
    }

    /// The `wip` row that was wrong all night. Idle on a `doing` task is a
    /// person being the blocker for a worker and is the resting state for a
    /// seat, which spends most of its time waiting on the agents under it.
    #[test]
    fn an_idle_seat_is_not_a_person_being_the_blocker() {
        let g = seated(&[("robustness", "w1")]);
        let seat = |ws: &str| governs(&g, ws, Some(&format!("{ws}:p1"))).is_some();
        assert!(needs_a_person(true, true, seat("w2")), "an ordinary agent, stopped");
        assert!(!needs_a_person(true, true, seat("w1")), "the seat, between agents");
        assert!(!needs_a_person(false, true, seat("w2")), "working is never a stall");
        assert!(!needs_a_person(true, false, seat("w2")), "and neither is finished work");
    }

    /// Without a governor the rule is the one both call sites already had, to
    /// the letter. This is the cheap-when-absent promise as an assertion rather
    /// than a paragraph.
    #[test]
    fn with_no_seats_the_rule_is_exactly_what_it_was() {
        let none = BTreeMap::new();
        for (idle, doing) in [(true, true), (true, false), (false, true), (false, false)] {
            assert_eq!(needs_a_person(idle, doing, governs(&none, "w1", Some("w1:p1")).is_some()), idle && doing);
        }
    }

    /// **One agent, one governorship** — the reversal of what wsp-063
    /// built, decided by Ed on 2026-08-17 while looking at this seat holding
    /// two.
    ///
    /// 013's argument was that a night coordinating `robustness` while
    /// answering for `wsp` above it is one agent and not two. True of the
    /// night, and still the wrong model: it leaves two questions unanswerable
    /// in principle rather than merely hard — which row draws an agent that is
    /// in two positions, and what a vacancy looks like when the same occupant
    /// fills both. The chain is what answers "who covers `wsp` while the
    /// `robustness` governor is busy", and its answer is a different agent.
    ///
    /// Taking a second slot therefore hands the first back, the way claiming a
    /// second task hands off the first. The slot it leaves is not deleted — it
    /// is the project's, and it stays there empty.
    #[test]
    fn one_agent_holds_one_governorship_and_taking_another_hands_it_back() {
        let (_env, store) = store("one");
        take(&store, "robustness", "w1", "w1:p1");
        assert_eq!(governs(&store.governors(), "w1", Some("w1:p1")).as_deref(), Some("robustness"));

        take(&store, "wsp", "w1", "w1:p1");
        assert_eq!(governs(&store.governors(), "w1", Some("w1:p1")).as_deref(), Some("wsp"), "it moved");
        let slots = slots(&store.governors());
        let robustness = slots.iter().find(|s| s.scope == "robustness").expect("the post stayed");
        assert!(!robustness.filled(), "and it is empty rather than gone");

        // Another workspace's seat is untouched by either.
        take(&store, "data", "w2", "w2:p1");
        assert_eq!(governs(&store.governors(), "w2", Some("w2:p1")).as_deref(), Some("data"));
        assert_eq!(governs(&store.governors(), "w1", Some("w1:p1")).as_deref(), Some("wsp"));
        assert_eq!(governs(&store.governors(), "w3", Some("w3:p1")), None);
    }

    /// The check that was missing, and the whole reason worklist-035 was found
    /// by accident.
    ///
    /// On `796c2d2`, with the `acc` seat's pane nine minutes dead, every
    /// surface wsp has agreed and every one of them was wrong: `wsp govern`
    /// listed the seat as live, `wsp flag` went on answering *raised to the acc
    /// governor*, `reconcile --reap` printed `emptied 0`, and `wsp doctor` said
    /// `✓ no problems`. A **problem** and not a note, because an empty seat is
    /// the state this file's own comment calls worse than no seat, and nothing
    /// else in wsp reports it at all.
    #[test]
    fn doctor_says_which_seat_nobody_is_sitting_in() {
        let (_env, store) = store("health");
        take(&store, "acc", "w1", "w1:p2");

        let pane = |id: &str, ws: &str| herdr::Pane {
            pane_id: id.to_string(),
            workspace_id: ws.to_string(),
            ..Default::default()
        };
        let up = |panes: Vec<herdr::Pane>| crate::cmd_agent::Probe::Up { agents: Vec::new(), panes };
        let say = |probe: &crate::cmd_agent::Probe| {
            let mut problems = Vec::new();
            health(probe, &store, &mut problems);
            problems
        };

        // The seat is sitting there. Nothing to say.
        assert!(say(&up(vec![pane("w1:p2", "w1")])).is_empty());

        // The pane is gone and the room is still open — the case that was
        // invisible. The line names the repair that fits: somebody can be put
        // back in a workspace that is still there.
        let ps = say(&up(vec![pane("w1:p1", "w1")]));
        assert_eq!(ps.len(), 1, "{ps:?}");
        assert!(ps[0].contains("the seat for `acc` is empty"), "{ps:?}");
        assert!(ps[0].contains("w1:p2"), "and which pane it was waiting on: {ps:?}");
        assert!(ps[0].contains("wsp govern acc -w w1"), "{ps:?}");

        // Room and pane both gone: the same fault, and a repair that has to
        // find somewhere to sit first.
        let ps = say(&up(vec![pane("w9:p1", "w9")]));
        assert!(ps[0].contains("both gone"), "{ps:?}");
        assert!(!ps[0].contains("-w w1"), "no pointing at a workspace that is not there: {ps:?}");

        // Silence is not evidence, here as in `reconcile`: a herdr that is
        // down, unreachable, or answered the pane listing with nothing knows
        // nothing about any seat.
        assert!(say(&crate::cmd_agent::Probe::Down).is_empty());
        assert!(say(&crate::cmd_agent::Probe::Unreachable("refused".into())).is_empty());
        assert!(say(&up(Vec::new())).is_empty(), "an empty answer is not an empty machine");

        // And a slot that has been stood down properly is not a fault. It is
        // the *repair*, and reporting it would make the fix look like the bug.
        vacate(&store, "acc");
        assert!(say(&up(vec![pane("w1:p1", "w1")])).is_empty());
    }

    /// A store of its own — **and the process pointed at it**, which is the half
    /// that passing a store in does not buy. `take` renames the seat's
    /// workspace, and that goes through `herdr::panes`, which fans out over
    /// whatever machines the *ambient* store names. See
    /// [`crate::util::isolated`].
    fn store(tag: &str) -> (util::Isolated, Store) {
        let env = util::isolated(&format!("govern-{tag}"));
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        (env, store)
    }

    /// The property this task turns on, as an assertion: **the slot outlives
    /// its occupant.**
    ///
    /// Standing down used to delete the record, so a night ending took the
    /// position off the project with the agent, and the morning could not tell
    /// "nobody is in the seat" from "this project never had one". Vacating
    /// keeps the post and empties it: no seat to route a raised hand to — that
    /// half must go on being exact — and a row that still exists to be filled.
    #[test]
    fn standing_down_empties_the_seat_and_leaves_it_standing() {
        let (_env, store) = store("vacate");
        take(&store, "wsp", "w1", "w1:p1");
        assert!(seat_for(&store.governors(), &tree(), None, Some("wsp")).is_some());

        assert!(vacate(&store, "wsp"), "there was somebody in it");
        let g = store.governors();
        assert!(g.contains_key("wsp"), "the position went with the agent");
        assert_eq!(seat_for(&g, &tree(), None, Some("wsp")), None, "an empty seat routes nothing");

        let slots = slots(&g);
        assert_eq!(slots.len(), 1);
        assert!(!slots[0].filled(), "drawn, and drawn as empty");
        assert!(!slots[0].elsewhere(), "empty here is not held elsewhere");

        // Filled again by the next agent, which is the whole point of keeping
        // it, and vacating twice changes nothing the second time.
        assert!(!vacate(&store, "wsp"), "already empty");
        take(&store, "wsp", "w2", "w2:p1");
        assert_eq!(
            seat_for(&store.governors(), &tree(), None, Some("wsp")).map(|s| s.workspace),
            Some("w2".to_string())
        );
    }

    /// Removing is the other decision, and it is about the project rather than
    /// about the agent: this project has no governor at all. Kept separate
    /// because one of the two happens every time a session ends and the other
    /// should be typed on purpose.
    #[test]
    fn removing_a_seat_takes_the_position_off_the_project() {
        let (_env, store) = store("remove");
        take(&store, "wsp", "w1", "w1:p1");
        assert!(store.clear_governor("wsp"));
        assert!(slots(&store.governors()).is_empty(), "no position, not an empty one");
    }

    /// A slot held from another machine is not an empty one. Reading it as
    /// empty would invite a second agent into a position that is already taken,
    /// which is the one thing a one-per-project record exists to stop.
    #[test]
    fn a_slot_filled_from_another_machine_reads_as_held_rather_than_empty() {
        let g: BTreeMap<String, Value> = [(
            "wsp".to_string(),
            json!({ "workspace": "w1", "host": "somewhere-else" }),
        )]
        .into_iter()
        .collect();
        let s = &slots(&g)[0];
        assert!(!s.filled(), "not reachable from here");
        assert!(s.elsewhere(), "and not empty either");
    }

    /// The name a seat writes on its workspace, and the test that stops a claim
    /// writing over it. Recognised on the mark rather than on the words,
    /// because the words are project ids and a person may type one.
    #[test]
    fn a_workspace_wearing_the_seats_name_is_recognisably_ours() {
        let label = governor_of("robustness");
        assert!(label.contains("robustness"), "a name that does not say which project: {label}");
        assert!(label.contains("governor"), "and says what the agent in it is: {label}");
        assert!(is_governor_label(&label));
        assert!(!is_governor_label("robustness/078 · build a design artefact"));
        assert!(!is_governor_label(""));
    }
}
