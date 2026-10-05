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

use std::collections::{BTreeMap, BTreeSet};

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
    /// The pane the agent was in when it took the seat, and the *exact* half of
    /// the record where the governor record's `workspace` key is the durable
    /// one — kept on the record, off this struct
    /// (`compound-096`; see [`room_of`]).
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
    /// Where that session was running, which is what a resume has to be
    /// standing in for the transcript to mean anything. Learned with the
    /// session and from the same reading.
    pub cwd: String,
    /// What kind of agent it is — `claude`, `opencode` — because what a resume
    /// *is* differs by kind and only the kind knows how to say it. Learned with
    /// the session, from the same reading and under the same rules; see
    /// [`learn_seats`]. Empty until something has seen one, which
    /// [`crate::cmd_spawn::kind_or_default`] reads as the default.
    pub kind: String,
}

/// The room a scope's seat is filed under, read off the governor record
/// itself rather than off a parsed [`Seat`] — the field that carried it,
/// `Seat::workspace`, is gone (`compound-096`); the record's own `workspace`
/// key is not and is what every caller here reads instead, the way
/// [`governs`] already did. Mirrors [`host_of`]: a live record answers first,
/// a vacated one's `last` second, and a scope with neither answers empty.
pub fn room_of(governors: &BTreeMap<String, Value>, scope: &str) -> String {
    let Some(rec) = governors.get(scope) else { return String::new() };
    match str_at(rec, "workspace") {
        w if !w.is_empty() => w,
        _ => rec.get("last").map(|l| str_at(l, "workspace")).unwrap_or_default(),
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
/// The seat on a scope, from the governors record.
///
/// The wake path's entry to the same resolution `--tell` takes: a scope names a
/// post, a post names a workspace and a pane, and only then is there anything
/// to say a sentence to.
pub fn seat_of_scope(scope: &str, governors: &BTreeMap<String, Value>) -> Option<Seat> {
    governors.get(scope).and_then(|rec| seat_of(scope, rec))
}

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
        pane: rec.get("pane").and_then(Value::as_str).unwrap_or_default().to_string(),
        since: rec.get("since").and_then(Value::as_str).unwrap_or_default().to_string(),
        session: str_at(rec, "session"),
        cwd: str_at(rec, "cwd"),
        kind: str_at(rec, "kind"),
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

/// The seat that answers for one task, asked once.
///
/// **The walk [`seat_for`] makes, with the store reads that feed it, and it
/// exists because two receipts were each spelling it out.** `wsp flag` says
/// *raised to the … governor* and `wsp ask` says *asked of the … governor*, and
/// `wsp-146` needed the seat itself out of the second one — so the two walks
/// were about to become three. Three definitions of *who answers for this* is
/// how the seat exception gets lost, and `cmd_govern::needs_a_person` has already
/// lost it once.
///
/// The list first, because a member of tonight's run is answered for by whoever
/// is running it rather than by whoever governs the project it happens to live
/// in — which is the same sentence [`seat_for`]'s own docs give, and is why
/// neither receipt writes it again.
pub fn answering_seat(store: &Store, task: &crate::model::Task) -> Option<Seat> {
    let index = Index::new(store.projects());
    let lists = crate::worklist::Running::read(store);
    let governors = store.governors();
    seat_for(&governors, &index, lists.list_of(&task.id), task.project.as_deref())
}

/// The same walk, started **one step past** a scope that cannot answer for
/// itself.
///
/// [`seat_for`] stops at the first seat it finds, which is the right answer to
/// *who answers for this work* and the wrong one to *who hears that this seat
/// has stopped*: the seat it reaches is the one the news is about. So this is
/// the chain with that scope's own step removed, terminating — like every other
/// address in this tree — at [`crate::cmd_watch::EVERYONE`] when nothing above
/// is filled. `worklist-041`.
///
/// **A worklist has nothing above it, and that is the walk's shape rather than
/// a gap.** [`seat_for`]'s chain is `list, project, ancestors(project)`, so the
/// list step is the *front*: what lies past it is the project chain of one
/// member, and a list that cuts across projects — which is what a list is for —
/// has as many of those as it has members. There is no single scope above a
/// run, so a stalled worklist seat escalates to everybody, which is the honest
/// answer and is also the loudest one available. A project scope walks
/// [`Index::ancestors`], and `robustness` stalling reaches `wsp` exactly as a
/// hand raised in `robustness` would while that seat is away.
///
/// `ancestors` answers empty for anything it has never heard of, so the
/// worklist case needs no test of its own: one expression covers both.
pub fn seat_above(governors: &BTreeMap<String, Value>, index: &Index, scope: &str) -> Option<Seat> {
    if governors.is_empty() {
        return None;
    }
    index
        .ancestors(scope)
        .into_iter()
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
pub fn governs(governors: &BTreeMap<String, Value>, seat: &crate::place::Seat) -> Option<String> {
    let who = seat.as_str();
    if who.is_empty() {
        return None;
    }
    // Off the record's own fields, read raw, rather than through [`seat_of`]
    // and [`Seat::sat_in`] — deliberately, so this reads as the row that
    // leaves `Seat::workspace` unused: nothing here names the struct field,
    // so deleting it costs this function nothing to keep answering right.
    //
    // A pane id already carries its workspace (`"w1:p1"`), which is what
    // makes one string enough where two used to be needed: an exact pane
    // match no longer has to be confirmed against a separate workspace,
    // because two different rooms cannot mint the same qualified pane id.
    // What a bare workspace comparison bought — matching *any* pane of the
    // room, and matching a `wsp govern -w` record that names no pane at all
    // — is recovered the same way: `who == workspace` catches the caller
    // that is asking about the room by name, and the prefix check catches
    // the caller standing in a pane of a room a `-w` record claimed without
    // ever learning that pane's id.
    governors
        .iter()
        .find(|(_, rec)| {
            let host = rec.get("host").and_then(Value::as_str).unwrap_or("");
            if !host.is_empty() && host != util::hostname() {
                return false;
            }
            let workspace = rec.get("workspace").and_then(Value::as_str).unwrap_or_default();
            if workspace.is_empty() {
                return false;
            }
            let pane = rec.get("pane").and_then(Value::as_str).unwrap_or_default();
            who == pane || who == workspace || (pane.is_empty() && who.starts_with(&format!("{workspace}:")))
        })
        .map(|(p, _)| p.clone())
}

/// The seat a caller means, from what it has in hand: the pane if there is
/// one, the workspace itself otherwise.
///
/// The bridge between the shape most callers still carry — a workspace and
/// maybe a pane, straight off a herdr record or a stored claim — and the one
/// [`governs`] now asks for. `pane` wins when it is not empty because it is
/// the more exact of the two; `workspace` alone is the room-wide question
/// `wsp govern -w` and a bare `--clear` ask, and is exactly what a caller
/// passes when it has no pane to be exact about.
pub fn seat_query(workspace: &str, pane: Option<&str>) -> crate::place::Seat {
    crate::place::Seat::new(pane.filter(|p| !p.is_empty()).unwrap_or(workspace))
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
    let held = governs(&store.governors(), &seat_query(workspace, None));
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

pub fn take(store: &Store, project: &str, workspace: &str, pane: &str) -> Option<(Seat, String)> {
    // Taking a seat somebody else is in is allowed and is said out loud. The
    // alternative is a refusal on a record whose whole content is "an agent is
    // sitting here", which goes stale every time a session ends without
    // standing down — and a seat you cannot take back after a crash is worse
    // than one that changes hands with a line of output.
    //
    // The room comes along beside the seat, read here rather than after the
    // write below — `room_of` would answer for a record this call is about to
    // overwrite, and the displaced room is exactly what the write erases.
    let displaced = store.governors().get(project).and_then(|rec| {
        let was = str_at(rec, "workspace");
        (was != workspace).then(|| seat_of(project, rec).map(|s| (s, was))).flatten()
    });

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
    if let Some(had) = governs(&store.governors(), &seat_query(workspace, None)).filter(|p| p != project) {
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
        .map(|rec| {
            (
                str_at(rec, "session"),
                str_at(rec, "cwd"),
                str_at(rec, "kind"),
                str_at(rec, "model"),
                str_at(rec, "effort"),
            )
        })
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
            // The kind travels with the session because it is half of the same
            // answer: an id nobody can say which binary to hand it to is not a
            // thread anybody can pick up.
            "kind": kept.2,
            // The tier, for the same reason and one step further: a seat is the
            // most expensive agent in a run, and a successor handed "whatever
            // the settings say now" is a rotation that quietly changes cost.
            // `wsp-117`, and `note_tier` is what puts the real answer here — a
            // `wsp govern` typed by a person takes over an agent that is already
            // running and must not restate its tier, which is why these are kept
            // rather than cleared here.
            "model": kept.3,
            "effort": kept.4,
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
/// kept and what is dropped, and then the `stood_down` sentence rides along if it
/// was there — because this function is `reconcile`'s as well as `--clear`'s, and
/// a reconciler that dropped the sentence would undo a person's decision on its
/// next visit. **The one thing that clears a stand-down is filling the seat**
/// ([`take`]), which is the point: the decision is about an empty seat, so an
/// empty seat cannot revoke it and an occupied one has already answered it.
pub fn vacate(store: &Store, project: &str) -> bool {
    let governors = store.governors();
    let Some(rec) = governors.get(project) else { return false };
    // Already empty. Said as false so a caller can report what it actually
    // changed rather than what it looked at.
    if seat_of(project, rec).is_none() && rec.get("workspace").is_none() {
        return false;
    }
    let was = rec.get("workspace").and_then(Value::as_str).unwrap_or_default().to_string();
    let mut left = json!({ "vacated": util::now_iso(), "last": stood_down(rec) });
    if let Some(at) = str_at(rec, STOOD_DOWN).into() {
        if let Some(o) = left.as_object_mut() {
            o.insert(STOOD_DOWN.into(), json!(at));
        }
    }
    store.set_governor(project, left);
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
    let governors = store.governors();
    for slot in slots(&governors) {
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
        let room = room_of(&governors, &slot.scope);
        // The pane and the room are said separately because the repairs
        // differ, and the repair is the half of this line worth reading. A room
        // that is still open can be sat in again as it stands; one that has
        // gone with its pane needs somewhere to sit first.
        let (state, fill) = match panes.iter().any(|p| p.workspace_id == room) {
            true => (
                format!("its pane {} is gone and {room} is still open", seat.pane),
                format!("`wsp govern {} -w {room}` puts somebody back in it", slot.scope),
            ),
            false => (
                format!("its pane {} and its workspace {room} are both gone", seat.pane),
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
        "kind": str_at(rec, "kind"),
        // The tier, and **this is the field that made the omission matter.**
        // `wsp-148` reads it back off `last` to seat a successor, because a
        // governor whose pane died is vacated before the reconciler looks — so
        // without these two the one record a reseat has to read is the one that
        // did not carry them, and a run's most expensive seat was replaced at the
        // runtime default every time it was replaced.
        "model": str_at(rec, "model"),
        "effort": str_at(rec, "effort"),
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
            pane: str_at(r, "pane"),
            since: str_at(r, "since"),
            session: str_at(r, "session"),
            cwd: str_at(r, "cwd"),
            kind: str_at(r, "kind"),
        })
    };
    of(rec).or_else(|| rec.get("last").and_then(of))
}

/// The seat this scope's slot is for — filled, or remembered by a vacated
/// record — and `None` where **nobody is in it**.
///
/// **One definition of that question, and `wsp-148` is why there is one.**
/// Two readers disagreed about it: this verb's `--reseat` guard asked
/// [`seat_of_scope`] and read a record with a workspace and an *empty* `pane`
/// as occupied, while the reconciler asked the same record and read it as
/// unseated. So the daemon would count a vacancy twice, claim the record, launch
/// this verb — and be told *"the seat has somebody in it - nothing to reseat"*,
/// leaving the claim on a post nobody was ever going to fill for the next
/// twenty minutes. Found live on 2026-10-05, on `tooling`, whose record reads
/// `workspace: compound, pane: ""`.
///
/// **The empty pane is the whole of it.** [`seat_of`] stops at the workspace,
/// because a workspace names a seat and a pane names an *occupant*; two records
/// in this store write the first and not the second, and this is the reader that
/// has to notice. Live first, the way back second — [`last_seat`] is not an
/// alternative here, it is the same seat at a different age.
pub fn seat_held(scope: &str, governors: &BTreeMap<String, Value>) -> Option<Seat> {
    let seat = seat_of_scope(scope, governors).or_else(|| last_seat(governors, scope))?;
    (!seat.pane.is_empty()).then_some(seat)
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

/// The rotation this pane is the named successor of, if one is in flight.
///
/// The brief's key into the handover record. A record is written the moment a
/// successor's seat exists and names both ends — the pane to end and the pane
/// the instruction is for — so the successor's first sight of its job carries
/// the predecessor's ending with it, read off the store rather than out of the
/// typed work order. That channel is the one piece of handover state that does
/// not go through the store, which is exactly why it can be dropped at all;
/// see [`crate::cmd_spawn::rotate`] for the whole argument.
///
/// `(scope, from)` — what this pane is about to hold, and whose pane to end
/// once it holds it. `None` for every pane on the machine but one.
pub fn incoming(
    handovers: &BTreeMap<String, Value>,
    pane: Option<&str>,
) -> Option<(String, String)> {
    let pane = pane?;
    handovers
        .iter()
        .find(|(_, rec)| str_at(rec, "to") == pane)
        .map(|(scope, rec)| (scope.clone(), str_at(rec, "from")))
}

/// Why the ending a handover record owes was not carried out, once
/// `wsp govern <scope> --ending` has tried and failed. `None` while it is owed
/// or running, and there is no record at all once it has succeeded, because
/// `despawn` consumes it.
///
/// This is the record's one terminal state that stays on disk. It is kept so
/// that the successor's brief can say its predecessor is still running, and
/// why. It is not an instruction: the successor is never the one who ends it
/// (`wsp-128`). The next rotation of the scope overwrites it, and a person's
/// `wsp despawn --pane <from>` consumes it.
pub fn ending_failed(handovers: &BTreeMap<String, Value>, scope: &str) -> Option<String> {
    handovers
        .get(scope)
        .and_then(|rec| rec.get("failed"))
        .and_then(Value::as_str)
        .filter(|why| !why.is_empty())
        .map(str::to_string)
}

/// Is a rotation into this pane still in flight — named successor, slot not yet
/// moved?
///
/// The window [`crate::cmd_spawn::rotate`] holds open on purpose: the record is
/// written the moment the successor's seat exists, and the slot moves last,
/// once the successor's first turn is confirmed. Between those two the
/// successor is a live agent holding an ending it has not earned yet, and the
/// pane it has been told to end is the one running `rotate`.
///
/// Written once because two surfaces ask it and would drift apart on the day it
/// mattered: the brief phrases the seat line differently either side of the
/// move, and `despawn` refuses on one side of it. It is a predicate over two
/// values its callers already hold rather than a reader of its own — the same
/// bargain [`needs_a_person`] makes, for the same reason.
///
/// `governed` is what this pane holds the slot of *now* ([`governs`]);
/// `incoming` is what [`incoming`] found. Nothing in flight, no rotation
/// pending — which is every pane on the machine but one, for the seconds a
/// handover takes.
pub fn rotation_pending(governed: Option<&str>, incoming: Option<&(String, String)>) -> bool {
    matches!(incoming, Some((scope, _)) if governed != Some(scope.as_str()))
}

/// Record against each seat what the backend says is sitting in it: the session
/// it is running under, the tree it was started in, and its kind.
///
/// The seat-shaped half of [`crate::cmd_agent::learn_sessions`], and separate
/// from it for one reason that is not tidiness: a binding is keyed on a **pane**
/// and a seat on a **workspace**, so the two cannot share a loop over the same
/// key. Everything else about them is the same judgement, and the argument for
/// both rules lives on that function: silence is not a correction, a different
/// session is. The cwd travels with the session because a transcript resumed in
/// the wrong tree is worse than one not resumed at all — `claude --resume`
/// takes the id and inherits the directory from wherever it is run. The kind
/// travels with them for `core-031`'s reason and is judged the same way; the
/// argument for observing it rather than declaring it is on
/// [`crate::cmd_agent::learn_sessions`] and is not repeated here.
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
    seen: impl Iterator<Item = (&'a str, &'a str, &'a str, &'a str, &'a str)>,
) -> usize {
    let governors = store.governors();
    // Workspace -> project, computed once: `governs` is a scan, and a machine
    // running twenty panes would otherwise scan the whole file per pane.
    let learned: Vec<(String, String, String, String)> = seen
        .filter(|(_, _, session, _, _)| !session.trim().is_empty())
        .filter_map(|(workspace, pane, session, cwd, kind)| {
            let project = governs(&governors, &seat_query(workspace, Some(pane)))?;
            let seat = seat_of(&project, governors.get(&project)?)?;
            // The session and the kind are the two fields a change is judged
            // on, and the cwd is not: a cwd that has moved under a session wsp
            // already knows is herdr answering about a pane whose shell has
            // `cd`-ed, and the tree the agent was *started* in is the one to
            // bring it back in.
            //
            // The kind joins the test rather than riding along with it because
            // of the seats that already exist: their session was learned
            // yesterday and will not change again, so a test on the session
            // alone would leave every one of them kindless for life.
            let moved =
                seat.session != session || (!kind.trim().is_empty() && seat.kind != kind);
            moved.then(|| {
                (project, session.to_string(), cwd.to_string(), kind.to_string())
            })
        })
        .collect();
    if learned.is_empty() {
        return 0;
    }
    store.locked(|| {
        let governors = store.governors();
        for (project, session, cwd, kind) in &learned {
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
            // Silence is not a correction, the rule `learn_sessions` states: a
            // pane whose agent has died reports no kind, and that is the moment
            // the recorded one is worth the most.
            if !kind.trim().is_empty() {
                o.insert("kind".to_string(), json!(kind));
            }
            store.set_governor(project, rec);
            store.log_event(
                "session-learned",
                json!({ "project": project, "session": session, "cwd": cwd, "kind": kind }),
            );
        }
    });
    learned.len()
}

// ---- what a seat is, and what is wrong with it ----------------------------
//
// Three facts about a seat that are not "who is in it", added by `wsp-148`
// because the reconciler has to decide *whether to replace* an occupant and so
// has to know how the last one was started, and because "empty" has to survive
// being counted.
//
// They live on the governor record rather than in a file of their own for the
// reason `repair`'s module docs give for a member's `## Log`: whatever state
// this is, it is a property of *this seat* and every verb that moves a seat
// already rewrites the whole record — `take`, `vacate`, `learn_seats` — so a
// second file is a second thing to keep in step with them, and the day one
// forgets is the day the reconciler reseats a seat that is working.

/// The tier the seat's agent is running at, off the record.
///
/// **`None` means "the settings tier", and it is not a failure.** A governor is
/// deliberately not given the governed default — `cmd_spawn::governed` returns
/// nothing for `--govern`, and `a_govern_spawn_onto_the_same_scope_keeps_the
/// _settings_tier` holds that line on purpose — so a seat whose settings file
/// states nothing records nothing and is handed back as `(None, None)`, which
/// is exactly what the spawn that filled it used. That is the whole of what this
/// reader buys: the successor of a seat is started the way that seat was, and
/// not the way the settings file happens to read at the moment it is replaced.
///
/// `wsp-117`. The row also asks for a *bounded* default for the most expensive
/// seat in a run, and this is deliberately not that: bounding a governor is a
/// decision about what tier a governor should be, and reversing a tested one is
/// not a side effect of recording one.
pub fn tier_of(governors: &BTreeMap<String, Value>, scope: &str) -> (Option<String>, Option<String>) {
    let Some(rec) = governors.get(scope) else { return (None, None) };
    let read = |k: &str| {
        rec.get(k)
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    let live = (read("model"), read("effort"));
    // **Under `last` as well, and a live record's top level is usually empty of
    // both.** A governor whose pane died is *vacated* before the reconciler
    // looks, and vacating moves the whole of the seat under `last` — so the
    // reading a reseat does is nearly always this one, and a reader that only
    // looked at the top level would find the tier on exactly the records nobody
    // needs it on. Mirrors [`room_of`]: live first, the way back second.
    if live.0.is_some() || live.1.is_some() {
        return live;
    }
    let Some(last) = rec.get("last") else { return live };
    let read = |k: &str| {
        last.get(k)
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    (read("model"), read("effort"))
}

/// Put the tier a seat was actually started at on its record.
///
/// **Written by the two callers that start an agent, after they have resolved
/// it, and by nobody else.** `wsp spawn --govern` knows what it passed;
/// `rotate_as` knows what it passed. `take` runs before either, and clearing
/// the field there instead would throw away the tier of a seat a person is
/// taking over — which is the case where the agent in the room is not being
/// replaced and its cost is not being changed.
pub fn note_tier(store: &Store, scope: &str, model: Option<&str>, effort: Option<&str>) -> bool {
    store.edit_governor(scope, |rec| {
        let Some(o) = rec.as_object_mut() else { return false };
        o.insert("model".into(), json!(model.unwrap_or_default()));
        o.insert("effort".into(), json!(effort.unwrap_or_default()));
        true
    })
}

/// How long this seat has been standing empty, in ticks, and whether a reseat
/// is already under way.
///
/// **Two fields and not one, because they answer two different questions.** The
/// count is the reconciler's evidence — a seat that reads `Empty` on one pass is
/// a pane between states, and one that reads it on three is a dead governor on a
/// running list. The claim is the guard: `wsp-148` requires the record to be
/// written *before* the spawn so a repeated tick cannot seat a second successor,
/// and a record that only counted would say `2` again on the tick after the
/// spawn as readily as on the tick before it.
///
/// The count is on the record and not in [`crate::repair::Pass`] because that
/// struct is in memory: the daemon `exec`s itself when an install lands
/// underneath it (`daemon::reload`), which throws the count away at exactly the
/// moment a person is most likely to be installing — mid-run, with a governor
/// dead. A counter that resets would never reach two ticks.
///
/// The claim is dated and read back through [`SEAT_CLAIMED_FOR`] rather than
/// being a bare boolean, because a bare one survives a process that was killed
/// mid-reseat — and the failure this row was pointed at by name is a rotate that
/// left a successor nobody could reach behind. A claim is taken back when the
/// reseat finishes, and an abandoned one is only honoured for this long.
/// When a reseat of this seat failed, if one did.
///
/// **A second field beside the claim, and the reason the claim is not simply
/// given back on failure.** Giving it back made the very next pass try again,
/// which is right for a failure that was a moment's bad luck and catastrophic
/// for one that is structural: on 2026-10-05 `tokenhub-spec-sync` was reseated
/// once a minute for ever, each attempt opening a compound agent whose renderer
/// never came up, and the seat empty at the end of every one. The claim is the
/// bound on that — [`SEAT_CLAIMED_FOR`] between attempts — so a failure has to
/// *keep* it and say why, and this is the why.
///
/// `wsp watch --status` reads it too, because the alternative was a sentence
/// claiming a governor was on its way for twenty minutes after the process that
/// was going to seat it had already said it could not.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Vacancy {
    /// Consecutive passes that have read this seat `Empty` or `Gone`.
    pub unseated: u64,
    /// When a reseat was claimed, if one is in flight.
    pub reseating: Option<i64>,
    /// When the reseat under way last failed, if it did.
    pub failed: Option<i64>,
}

/// How long a claim on a reseat is honoured before the next pass takes it back.
///
/// Twenty minutes, and it is a bound on a *dead* process rather than a wait: a
/// reseat that is running holds this for as long as the spawn takes, and one
/// that was killed holding it holds it for ever. Both are answered by a number
/// that is long past any real spawn and short enough that a run does not sit
/// unseated for the rest of the night.
pub const SEAT_CLAIMED_FOR: i64 = 20 * 60;

/// Read the three markers off a record.
pub fn vacancy(governors: &BTreeMap<String, Value>, scope: &str) -> Vacancy {
    governors.get(scope).map(Vacancy::of).unwrap_or_default()
}

impl Vacancy {
    /// The markers as one record carries them. `None` is a record with none of
    /// them, which is every record written before `wsp-148`.
    fn of(rec: &Value) -> Vacancy {
        Vacancy {
            unseated: rec.get("unseated").and_then(Value::as_u64).unwrap_or(0),
            reseating: at_of(rec, "reseating"),
            failed: at_of(rec, "reseating_failed"),
        }
    }

    /// Whether the claim, if there is one, has gone stale.
    pub fn claim_stale(&self, at: i64) -> bool {
        self.reseating.is_some_and(|since| since + SEAT_CLAIMED_FOR <= at)
    }

    /// Whether a reseat may be claimed now — nothing is claimed, or what is was
    /// claimed long enough ago that whatever took it is gone.
    pub fn claim_free(&self, at: i64) -> bool {
        self.reseating.is_none_or(|_| self.claim_stale(at))
    }

    /// Whether this seat has been empty long enough to replace. `n` ticks, which
    /// is the whole of the delay this row buys and the reason it is not zero: a
    /// pane reads `Empty` for a moment every time an agent is cleared and
    /// restarted, and a successor seated into that window is two governors for
    /// one run.
    pub fn overdue(&self, n: u64) -> bool {
        self.unseated >= n
    }
}

/// One more pass has found this seat empty, and how this seat now reads.
///
/// **The whole record and not just the count, and the reason is that the caller
/// decides on one and writes the other.** The reconciler asks "is this seat
/// overdue?" — [`Vacancy::overdue`] — and this is the only reading that can
/// answer it, so returning the pair rather than the number means the predicate
/// that fires and the marker that recorded it are the same read of the same
/// value. Returning the count alone would put `EMPTY_TICKS` and the record's own
/// `unseated` field on either side of a write that a concurrent `learn_seats`
/// can interleave with.
///
/// Under the lock and re-read inside it, so two passes racing cannot both decide
/// they are the first.
pub fn count_unseated(store: &Store, scope: &str) -> Vacancy {
    let mut now = Vacancy::default();
    store.edit_governor(scope, |rec| {
        let Some(o) = rec.as_object_mut() else { return false };
        o.insert(
            "unseated".into(),
            json!(o.get("unseated").and_then(Value::as_u64).unwrap_or(0) + 1),
        );
        now = Vacancy::of(rec);
        true
    });
    now
}

/// Forget that this seat was ever empty — a pass that found somebody in it.
///
/// **Clears the count and never the claim.** A live seat is the one reading that
/// proves the reseat worked, and it is the only evidence there will ever be that
/// it did; clearing a claim on it would be clearing the receipt.
pub fn forget_unseated(store: &Store, scope: &str) {
    store.edit_governor(scope, |rec| {
        let Some(o) = rec.as_object_mut() else { return false };
        if o.remove("unseated").is_none() {
            return false;
        }
        true
    });
}

/// Take the claim that stops a second pass seating a second successor.
///
/// **The record before the spawn, which is the whole of the ordering.** It is
/// written by whoever is about to open a seat and started, so a pass that finds
/// it does nothing at all — not a second seat, and not a second sentence to the
/// spool. The alternative, deciding on a field that is only written once the
/// successor is up, leaves the window that `wsp-114` was filed about open for the
/// whole length of a spawn.
///
/// A stale claim is taken back rather than refused, and refusing it is the
/// obvious thing to write: an abandoned claim would then hold a dead governor's
/// post empty until a person noticed it, which is the failure this row exists to
/// end.
pub fn claim_seat(store: &Store, scope: &str) -> bool {
    let at = util::epoch_secs();
    store.edit_governor(scope, |rec| {
        if !Vacancy::of(rec).claim_free(at) {
            return false;
        }
        let Some(o) = rec.as_object_mut() else { return false };
        o.insert("reseating".into(), json!(util::now_iso()));
        true
    })
}

/// A reseat was attempted under a claim and did not produce a seat.
///
/// **The claim is kept and the failure is recorded against it, and both halves
/// are load-bearing.** The claim is what stops the next pass trying again
/// immediately, which is the whole difference between a momentary failure being
/// retried in a minute and a structural one being retried every minute for
/// ever — `tokenhub-spec-sync` on 2026-10-05, each attempt opening a compound
/// agent whose renderer never came up. The record of the failure is what lets
/// [`crate::wake`] say `retrying` rather than `reseating` for the twenty minutes
/// in between.
///
/// **Kept for a refusal as well as a broken agent**, and for the same reason: a
/// refusal the reconciler keeps making is a refusal it will keep making, and a
/// verb that released the claim on its way out turned one into a spawn a minute.
pub fn reseat_failed(store: &Store, scope: &str) {
    store.edit_governor(scope, |rec| {
        let Some(o) = rec.as_object_mut() else { return false };
        o.insert("reseating_failed".into(), json!(util::now_iso()));
        // A claim that was never taken is not invented here: this records that
        // an attempt happened, and the reconciler is what decides whether a
        // *next* one may.
        true
    });
}

/// When a person stood this seat down, if they did.
///
/// **The marker that makes `--clear` mean something to the reconciler, and it is
/// one field because `vacate` used to carry two meanings at once.** `--clear`
/// and `reconcile` both emptied the record, and the record they left was
/// *identical*, so the reconciler could not tell a governor that had died — which
/// is the one it is there to replace — from a governor a person had deliberately
/// stood down. It read the second as the first and refilled it. On 2026-10-05
/// that put `cpd-275` back into a `tooling` seat Ed had closed by hand twenty
/// minutes earlier, two ticks after the close.
///
/// **Absent is the ordinary case and is not a default to fill in.** A record
/// without the key is a seat nobody has ever stood down, which is every record
/// written before this and every seat taken since — so a store full of them is
/// unaffected and the reconciler keeps doing its job.
///
/// A date and not a boolean, because the roster has to say *which* — a seat
/// nobody is in is either a governor that died or a position somebody decided to
/// leave empty, and those are different sentences to a person reading it.
pub fn stood_at(governors: &BTreeMap<String, Value>, scope: &str) -> Option<i64> {
    let at = str_at(governors.get(scope)?, STOOD_DOWN);
    let at = util::epoch_of(&at);
    (at > 0).then_some(at)
}

/// The key a stand-down is recorded under. `STOOD_DOWN` is spelled here rather
/// than inlined because three files now read or write it and a typo in a record
/// is silent — the reconciler would simply never see the decision.
pub const STOOD_DOWN: &str = "stood_down";

/// One ISO timestamp off a record, as an epoch — and `None` for a key that is
/// absent, empty or unparseable, which are the same answer.
///
/// **Shared because three markers now carry dates and a reader that accepted a
/// different subset of those three would be a reader whose idea of "claimed" is
/// not the reconciler's.** A hand-edited `0` and a hand-edited `"soon"` both mean
/// nothing here, and both mean the claim is not held.
fn at_of(rec: &Value, key: &str) -> Option<i64> {
    let at = util::epoch_of(&str_at(rec, key));
    (at > 0).then_some(at)
}

/// Record that a person stood this seat down, and that it is to stay down.
///
/// **Written after the vacate rather than inside it, because `vacate` is also
/// what `reconcile` calls** and the two must not converge again. `vacate`'s own
/// record is what a dead governor leaves; this is the extra sentence on it that
/// says somebody meant it.
pub fn mark_stood_down(store: &Store, scope: &str) -> bool {
    store.edit_governor(scope, |rec| {
        let Some(o) = rec.as_object_mut() else { return false };
        o.insert(STOOD_DOWN.into(), json!(util::now_iso()));
        true
    })
}

/// The seat's agent, resolved through the port — as it is *now*, which is not
/// necessarily the pane it started in.
///
/// The record names a workspace because that is the durable half, and the pane
/// on it can go stale — an agent cleared and restarted comes back on another
/// pane, possibly on another backend entirely. So anything that speaks to a
/// slot asks whichever backend answers for it at the moment of speaking, and a
/// slot answered by nobody is a vacancy rather than a stale address.
///
/// **Off `locate_seat` over `local_backends()`, the same fold `wsp tell` and
/// `wsp answer` make** (`compound-077`, `compound-091`) — not `herdr::panes()`
/// alone, which is what stopped a `compound`-hosted governor from ever being
/// found. `backends` is the caller's, not built here: it is a fan-out over
/// every backend this machine can spawn onto, and a caller that already paid
/// for one (`wsp answer`'s own `locate_seat`, a loop over several seats) is not
/// made to pay for it twice.
///
/// # The room stood in for the pane, and it no longer can
///
/// Before `compound-092` gave every agent an id of its own, an agent cleared
/// and restarted in the same herdr window was found by asking who else was
/// standing in that *workspace* — sound with one agent in the room, a guess
/// with two, and worklist-035 is the night that guess went wrong. That fallback
/// was herdr's furniture: wsp has had no workspace of its own to ask since
/// `compound-092`, and a `compound` seat has no workspace at all to fall back
/// to.
///
/// **What replaces it is a fact this process already keeps twice over.** An
/// agent gets a row in `agents.json` the moment it claims, and a claim names
/// its agent as `agent_id` (both `cd68f27`) — so *unassigned* is a join over
/// two files this process owns, no socket and no backend asked. `store` never
/// keeps a claim past the task it was on: `clear_claim` runs the moment one
/// ends, so there is no state here for "claimed, but finished" to occupy — an
/// agent with no claim in `store.claims()` is the whole of "holds no claim",
/// and the harder reading (no claim, or every claim already finished) is not a
/// second case this store can be in.
///
/// **Scoped to agents started since this seat was taken**, because the room
/// used to buy that scoping for free and an unqualified "the one unassigned
/// agent on the machine" is answering a different question — a fleet can be
/// mid-restart on more than one seat at once, and another seat's turnover is
/// not this one's replacement. More than one candidate is exactly the
/// ambiguity the old fallback refused to guess through, so it reads the same
/// way here: the caller gets *the seat is empty*, which is true, and which
/// names the repair.
pub fn occupant<'a>(
    store: &Store,
    backends: &'a [Box<dyn crate::place::Place>; 2],
    seat: &Seat,
) -> Option<(&'a Box<dyn crate::place::Place>, crate::place::Seated)> {
    // The recorded pane first, if a backend still answers for it and
    // something is sitting there.
    if let Some((place, row)) = crate::cmd_agent::locate_seat(backends, &seat.pane) {
        if !row.agent.kind.is_empty() {
            return Some((place, row));
        }
    }
    // **Then the room, and this is `compound-174`.** A record can name no
    // pane and still name the seat exactly: a compound seat has no herdr pane
    // behind it, so one id is the room AND the seat in it, and `wsp govern` in
    // one wrote the room and left the pane empty. Looking only at the pane is
    // then looking in a field that is empty by construction, and the answer
    // was *the seat is empty* for a seat with somebody sitting in it — which
    // is how a governor on a compound seat could be drawn by `wip` and told
    // nothing by every verb that reaches one.
    //
    // `room_of` and not `Seat::workspace`, because `compound-096` took that
    // field off the struct deliberately; the record's own key is the durable
    // one and every other reader here already goes through it.
    //
    // **This is a lookup, not a widening.** The match is exact, so a herdr
    // `workspace` — `w1`, a room — resolves only to a seat literally named
    // `w1`, and no seat has that name. A room that holds several panes still
    // answers *empty*, which is the despawn guard's hazard arriving by
    // another road if this ever stops being true;
    // `a_herdr_room_with_no_pane_is_still_not_guessed_at` holds that line.
    let room = room_of(&store.governors(), &seat.scope);
    if !room.is_empty() && room != seat.pane {
        if let Some((place, row)) = crate::cmd_agent::locate_seat(backends, &room) {
            if !row.agent.kind.is_empty() {
                return Some((place, row));
            }
        }
    }
    let claims = store.claims();
    let claimed: BTreeSet<String> = claims
        .values()
        .filter_map(|c| c.get("agent_id").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    let mut unassigned = store
        .agents_held()
        .into_iter()
        .filter(|(id, rec)| !claimed.contains(id) && str_at(rec, "started") > seat.since);
    let (_, rec) = match (unassigned.next(), unassigned.next()) {
        (Some(only), None) => only,
        _ => return None,
    };
    crate::cmd_agent::locate_seat(backends, &str_at(&rec, "seat")).filter(|(_, row)| !row.agent.kind.is_empty())
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
/// The scope this needle names — a worklist slug or a project id. `pub(crate)`
/// because `wsp govern <scope> --rotate` resolves its subject through this same
/// one key space and must refuse a name nothing governs before anything else
/// happens.
pub(crate) fn scope_of(store: &Store, index: &Index, needle: &str) -> Option<String> {
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

/// `wsp govern [<scope>] [--clear|--remove|--tell "…"]`, and the two flags that
/// are whole other verbs: `--rotate` and `--reseat`.
pub fn govern(store: &Store, args: &Args) -> i32 {
    // Rotation is not an edit to this seat — it ends it, by handing it to
    // somebody else. It lives with the placement machinery in `cmd_spawn`,
    // which is what seats the successor, and is routed from here because the
    // verb a custodian types names this one. See [`crate::cmd_spawn::rotate`].
    if args.has("rotate") {
        return crate::cmd_spawn::rotate(store, args);
    }
    // Filling a slot nobody is in, which is the same machinery and the same
    // successor with no predecessor. Routed here for the same reason and because
    // the reconciler runs it as this verb — `wsp-148`. A person can run it too,
    // which is the point: the daemon filling a seat and a person filling it are
    // one verb, so a "wsp will do it" is not a different action from doing it.
    if args.has("reseat") {
        let index = Index::new(store.projects());
        let Some(needle) = args.rest.first() else {
            eprintln!("usage: wsp govern <project|worklist> --reseat");
            return 2;
        };
        let Some(scope) = scope_of(store, &index, needle) else {
            eprintln!("wsp: no such project or worklist `{needle}`");
            return 1;
        };
        // **Refuses a stand-down as firmly as it refuses an occupied seat, and
        // for the same reason: this verb is how the reconciler asks, so honouring
        // the decision here is what keeps it — a person who ran `--clear` and
        // watched a governor come up anyway would be right to say the flag does
        // nothing.**
        if stood_at(&store.governors(), &scope).is_some() {
            eprintln!("wsp: the {scope} seat was stood down - leaving it down");
            eprintln!("wsp: `wsp spawn -p <project> --govern` fills it if that changes");
            return 1;
        }
        // **Refuses what the reconciler would not have asked about, and that is the
        // whole of it.** Asking "does the record name a pane" instead strands
        // every scope whose recorded pane has since died — which is the ordinary
        // shape of the thing this row exists to repair, so the daemon would count
        // the vacancy, launch this verb, and be refused once a minute for ever.
        // Live on `tokenhub-spec-sync`, whose count reached 54.
        //
        // One question, one function: [`crate::repair::reading`]. The cost is a
        // reading of the backend on a path that already opens panes, and the
        // alternative is a second opinion to keep in step with the first.
        if matches!(
            crate::repair::reading(store, &crate::cycle::Fleet, &scope),
            crate::repair::Occupancy::Occupied
        ) {
            eprintln!("wsp: the {scope} seat has somebody in it - nothing to reseat");
            // **The claim stays, and the refusal is recorded against it.** The
            // reconciler takes it before it launches this process, so giving it
            // back here meant the very next pass tried again — and a refusal
            // this verb keeps making is one it will keep making.
            reseat_failed(store, &scope);
            return 1;
        }
        return crate::cmd_spawn::reseat(store, &scope);
    }
    // The other half of a rotation, run by the pane being ended. `--rotate`
    // starts it detached; see [`crate::cmd_spawn::carry_out_ending`].
    if args.has("ending") {
        return crate::cmd_spawn::carry_out_ending(store, args);
    }
    let p = Paint::new();
    let index = Index::new(store.projects());
    let env = herdr::Env::read();
    let (workspace, pane) = room_and_pane(args.get("workspace").as_deref(), &env);
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
    let handed_back = governs(&governors, &seat_query(&ws, None)).filter(|p| p != &scope);
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
                "displaced": displaced.map(|(_, room)| room),
                "stood_down_from": handed_back,
            })
        );
        return 0;
    }
    println!("{} {}", p.cyan("▣"), p.bold(&scope));
    if let Some((_, room)) = displaced {
        println!("  {}", p.dim(&format!("taken from {room}")));
    }
    // Not in the same grey as the hint below it. One agent holds one
    // governorship, so this line is the whole of an eviction: the seat it names
    // goes open, and `wsp govern <that scope> --tell` stops reaching this
    // workspace the moment it prints — which is how a peer finds out, on
    // 2026-08-19, that the seat it was addressing had moved. The seat that lost
    // it had read this line and not weighed it, so the second half is the
    // consequence spelled out rather than left to be inferred from "open".
    if let Some(was) = &handed_back {
        println!("  {} {}", p.yellow("stood down from"), p.bold(was));
        println!("  {}", p.dim("that seat is open — --tell no longer reaches you there"));
    }
    println!("  {}", p.dim("raised hands here reach this workspace · wsp govern --clear to stand down"));
    0
}

/// The room and the pane a governor taking a seat right now should record, and
/// the whole of `compound-174`'s first fault in one function.
///
/// Three cases, in this order, and the order is the argument:
///
/// 1. **`-w` names a room this process is not standing in.** The pane is
///    `None`, deliberately: the process's own pane says nothing about who is
///    sitting in *that* room, stamping it would point the despawn guard at a
///    pane in another workspace, and asking with it would ask the wrong
///    question. Unchanged by this row.
/// 2. **herdr's variables are here, so this is a herdr pane.** They are
///    authoritative and `WSP_SEAT_ID` is ignored entirely — `place_herdr`
///    records that a stale `WSP_SEAT_ID` inherited from somewhere names a pane
///    that backend never had, so believing it here would be believing exactly
///    the value that warns against it.
/// 3. **No herdr variables at all, which is a compound seat** (`compound-105`,
///    `compound-174`). `WSP_SEAT_ID` names it, and a compound seat has no
///    herdr pane behind it, so that one id is the room *and* the seat in it.
///    Both fields are filled with it.
///
/// Case 3 is what the verb could not do before: it read herdr's environment,
/// found nothing, and refused with *"no workspace — pass -w, or run inside
/// herdr"* — advice that is wrong on a compound seat, where there is no herdr
/// to run inside. With `-w` forced, the record was then written with an empty
/// pane, which is the delivery half fixed in [`occupant`].
fn room_and_pane(named: Option<&str>, env: &herdr::Env) -> (Option<String>, Option<String>) {
    if let Some(ws) = named {
        return (Some(ws.to_string()), None);
    }
    match env.workspace_id.clone().filter(|w| !w.is_empty()) {
        Some(ws) => (Some(ws), env.pane_id.clone().filter(|p| !p.is_empty())),
        None => match crate::place::seat_from_env() {
            Some(seat) => {
                let id = seat.to_string();
                (Some(id.clone()), Some(id))
            }
            None => (None, None),
        },
    }
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
    let text = text.trim();
    if text.is_empty() {
        eprintln!("wsp: nothing to say");
        return 2;
    }
    let Some(seat) = governors.get(scope).and_then(|rec| seat_of(scope, rec)) else {
        eprintln!("wsp: no seat on `{scope}` — wsp govern {scope} fills it");
        return 1;
    };
    // The retry refusal, and it stays. **This is a person at a keyboard, and
    // this verb has never refused to deliver** — it is the other three verbs
    // `wsp-146` moved, and what it changed about this one is where the words go,
    // not whether a deliberate repeat is honoured. `wake::Tell` bypasses
    // `already_sent` for the opposite reason: a level re-raising is ordinary and
    // a retry is not.
    let sent = crate::cmd_agent::Sent::new(
        scope,
        &format!("the {scope} seat"),
        seat.pane.as_str(),
        seat.pane.as_str(),
        text,
        args,
    );
    if let Some(ago) = sent.already_sent(store) {
        if !args.has("again") {
            return crate::cmd_agent::twice(&sent, ago, &Paint::new());
        }
    }
    // **The spool first, and the daemon delivers it** — `wsp-146`. This used to
    // resolve the occupant and type at the pane here, which meant a governor in
    // the middle of a turn was told *not ready — working* and the sentence was
    // lost. "Busy" now means later and the record is the seat's spool; the gate,
    // the acknowledgement and the retry are `crate::wake`'s, one implementation,
    // and the receipt below is the same report it prints for anybody else.
    let Some(report) = crate::wake::say(store, scope, text, None) else {
        eprintln!("wsp: no seat on `{scope}` — wsp govern {scope} fills it");
        return 1;
    };
    if args.json() {
        println!(
            "{}",
            json!({ "target": scope, "pane": seat.pane, "id": sent.id, "told": report.arrived(),
                    "held": report.held, "why": report.why })
        );
    } else {
        println!("{}", report.said(&format!("the {scope} seat"), &Paint::new()));
    }
    0
}

/// `wsp govern --clear [<project>]` — this workspace stops being the seat.
///
/// Bare, it stands down from everything this workspace holds, because a session
/// ending does not end one seat at a time. Named, it stands down the scope you
/// name and leaves anything else standing.
///
/// **That plural is legacy being healed, not an arrangement to compose.** One
/// agent holds one governorship — the decision, and the two questions it makes
/// unanswerable, are in [`governs`] — and [`take`] stands a workspace down from
/// whatever it held before. So two scopes on one workspace is a store written
/// before that rule or edited around it — this one held two for a night — and
/// it heals the next time anybody takes a seat. `--clear <scope>` is here to
/// survive that state, not to build it: a seat wanting `wsp` covered while it
/// runs `robustness` is asking for the chain [`seat_for`] walks, which answers
/// with a different agent. Said here, 800 lines from the decision, because this
/// paragraph read as a description of how governorship works cost a night on
/// 2026-08-19 — a seat took a second scope not expecting the eviction that
/// [`governs`] and [`take`] both document, lost the first, and with it the
/// address `wsp govern <scope> --tell` reaches it by, so a peer could not reach
/// it at all.
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
            Some(ws) => governs(&governors, &seat_query(ws, None)),
            None => {
                eprintln!("wsp: no workspace — pass -w, or name the scope");
                return 2;
            }
        },
    };

    // **The stand-down is recorded on both branches, and unconditionally on this
    // one.** `vacate` answers whether it emptied anything, and the commonest use
    // of `--clear` is sealing a seat that is *already* empty — which is exactly
    // the case where there is nothing to vacate and the most need to say so. A
    // person who has just watched a seat refuse to stay closed is not helped by
    // being told the record was already fine.
    //
    // `--remove` leaves the sentence instead of nothing for the same reason: it
    // used to delete the record, and a deleted record reads to the reconciler as
    // a post nobody has ever filled — which is the case it seats at once. That
    // made the stronger of the two flags no stronger at all.
    let cleared = held.filter(|proj| match remove {
        true => {
            let gone = store.clear_governor(proj);
            if gone {
                store.log_event("governor-cleared", json!({ "project": proj }));
                rename_seat(store, workspace.unwrap_or_default());
            }
            mark_stood_down(store, proj);
            true
        }
        false => {
            vacate(store, proj);
            mark_stood_down(store, proj);
            true
        }
    });

    if args.json() {
        println!("{}", json!({ "cleared": cleared, "removed": remove }));
    } else {
        match (&cleared, remove) {
            (None, _) => println!("{}", p.dim("this workspace is nobody's seat")),
            (Some(proj), true) => {
                println!("{} {}", p.dim("no seat any more on —"), proj);
                println!("  {}", p.dim("recorded as stood down; wsp will not fill it"));
            }
            (Some(proj), false) => {
                println!("{} {}", p.dim("stood down, seat left open —"), proj);
                println!("  {}", p.dim("wsp will not fill it; `wsp spawn -p <project> --govern` if that changes"));
            }
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
    let mine: Option<String> = workspace.and_then(|ws| governs(&governors, &seat_query(ws, pane)));

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
                    "workspace": s.occupant.as_ref().map(|_| room_of(&governors, &s.scope)),
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
            (Some(o), _) if o.pane.is_empty() => room_of(&governors, &s.scope),
            (Some(o), _) => format!("{} · {}", room_of(&governors, &s.scope), o.pane),
            (None, true) => format!("on {}", s.host),
            (None, false) => {
                // **Both the door and the fact that wsp opens it by itself.**
                // `wsp-148` gave the daemon the job of filling a vacancy on a
                // running list, and a roster that still says only `wsp spawn ...
                // --govern fills it` would be teaching a reader to do by hand
                // what the machine now does on its own — and would say nothing
                // about the case that matters, which is whether the daemon is the
                // thing that is going to fill it.
let auto = store.worklist(&s.scope).is_some_and(|w| w.status().is_running());
                // **A stand-down outranks every other sentence here**, including
                // the one the daemon is about to make true. `wsp-148` gave the
                // reconciler the job of filling a vacancy on a running list, and
                // until this branch existed a seat Ed had closed by hand still
                // read `the daemon seats this one` — so the roster instructed the
                // reader to wait for exactly the thing the person had just
                // refused. The date is on it because *which* is the question: a
                // governor that died is worth chasing, a position somebody left
                // empty is not.
                let down = stood_at(&governors, &s.scope).map(util::local_hm).unwrap_or_default();
                if !down.is_empty() {
                    format!(
                        "stood down at {down} · wsp will not fill it — \
                         `wsp spawn -p <project> --govern` if that changes"
                    )
                } else if vacancy(&governors, &s.scope).reseating.is_some() {
                    // **A claim in flight outranks the daemon's own sentence**,
                    // because it changes what a reader should do: a claim means
                    // wsp has already opened the successor, so a person running
                    // `wsp govern <scope> --reseat` here would be opening a
                    // second one — which is `wsp-114`'s three governors for one
                    // run. It says so rather than reporting an emptiness that is
                    // already being dealt with.
                    "empty · a successor is being seated - do not reseat by hand".to_string()
                } else if auto {
                    "empty · the daemon seats this one".to_string()
                } else {
                    "empty · wsp spawn -p <project> --govern fills it".to_string()
                }
            }
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
        assert_eq!((s.scope.as_str(), room_of(&g, &s.scope).as_str()), ("robustness", "w1"));
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
        assert_eq!((s.scope.as_str(), room_of(&g, &s.scope).as_str()), ("batch", "w7"));

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

        assert_eq!(governs(&store.governors(), &seat_query("w1", Some("w1:p1"))).as_deref(), Some("batch"));
        let slots = slots(&store.governors());
        let robustness = slots.iter().find(|s| s.scope == "robustness").expect("the post stayed");
        assert!(!robustness.filled(), "and it was handed back, empty");
        assert_eq!(
            seat_for(&store.governors(), &tree(), Some("batch"), Some("robustness"))
                .map(|s| room_of(&store.governors(), &s.scope)),
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

        assert_eq!(governs(&g, &seat_query("w1", Some("w1:p2"))).as_deref(), Some("acc"), "the seat itself");
        assert_eq!(governs(&g, &seat_query("w1", Some("w1:p1"))), None, "and its neighbour, which is nobody's seat");

        // The consequence, said as the predicate an unattended run depends on:
        // a worker stopped on a `doing` task is a person's problem, and sharing
        // a room with a custodian does not make it stop being one.
        assert!(
            needs_a_person(true, true, governs(&g, &seat_query("w1", Some("w1:p1"))).is_some()),
            "the worker beside the seat is still the loudest row on the panel",
        );
        assert!(
            !needs_a_person(true, true, governs(&g, &seat_query("w1", Some("w1:p2"))).is_some()),
            "and the seat is still idle between the agents it is waiting on",
        );
    }

    /// **`compound-094`: the replacement is found by wsp's own record, not by
    /// asking who else is standing in the room** — the fallback
    /// worklist-035 put a stop to for a *co-custodian* is the same fallback
    /// `occupant` used to make for a *restarted* one, and it is gone from both.
    ///
    /// The recorded pane (`w1:p1`) answers for nobody. Three agents exist:
    /// one that held the seat before and is long since irrelevant (started
    /// before the seat was even taken), one still holding a claim (a worker,
    /// not a replacement, whatever room it is in), and one holding neither —
    /// which is the only fact `occupant` now asks about.
    #[test]
    fn a_replacement_is_found_by_its_own_unclaimed_record_not_by_the_room() {
        use crate::fake::{Fake, Spot, Stage};
        use crate::place::State;

        let (env, store) = store("replacement-found");
        store.set_governor(
            "acc",
            json!({ "workspace": "w1", "pane": "w1:p1", "host": util::hostname(), "since": util::iso_at(1_000) }),
        );
        let seat = seat_of("acc", store.governors().get("acc").unwrap()).unwrap();

        // Stale: it predates the seat, so it is not this restart's agent
        // whatever it holds.
        store.set_agent("a-old", json!({ "seat": "w1:p0", "started": util::iso_at(500) }));
        // A worker: newer than the seat, but claimed, so it answers for a task
        // and not for this seat.
        store.set_agent("a-worker", json!({ "seat": "w1:p3", "started": util::iso_at(1_100) }));
        store.set_claim("t-1", json!({ "agent_id": "a-worker" }));
        // The replacement: newer than the seat, and holds nothing.
        store.set_agent("a-new", json!({ "seat": "w1:p2", "started": util::iso_at(1_200) }));

        let mut stage = Stage::new();
        stage.put(Spot::agent("w1:p2", "claude", "acc", State::Idle));
        stage.put(Spot::agent("w1:p3", "claude", "t-1", State::Idle));
        let fake = Fake::bind(env.path("herdr.sock"), stage).expect("a socket");
        let (k, v) = fake.socket_env();
        std::env::set_var(k, v);

        let backends = crate::cmd_spawn::local_backends();
        let (_, found) = occupant(&store, &backends, &seat).expect("the unclaimed agent is the seat now");
        assert_eq!(found.seat.as_str(), "w1:p2");
    }

    /// Two candidates is the ambiguity the room used to refuse to guess
    /// through, and the record-based read refuses it the same way: a seat two
    /// restarts could each claim reads as empty rather than as either of them.
    #[test]
    fn two_unclaimed_agents_is_the_same_ambiguity_as_two_in_the_room() {
        use crate::fake::{Fake, Spot, Stage};
        use crate::place::State;

        let (env, store) = store("replacement-ambiguous");
        store.set_governor(
            "acc",
            json!({ "workspace": "w1", "pane": "w1:p1", "host": util::hostname(), "since": util::iso_at(1_000) }),
        );
        let seat = seat_of("acc", store.governors().get("acc").unwrap()).unwrap();

        store.set_agent("a-new", json!({ "seat": "w1:p2", "started": util::iso_at(1_200) }));
        store.set_agent("a-newer", json!({ "seat": "w1:p3", "started": util::iso_at(1_300) }));

        let mut stage = Stage::new();
        stage.put(Spot::agent("w1:p2", "claude", "acc", State::Idle));
        stage.put(Spot::agent("w1:p3", "claude", "acc", State::Idle));
        let fake = Fake::bind(env.path("herdr.sock"), stage).expect("a socket");
        let (k, v) = fake.socket_env();
        std::env::set_var(k, v);

        let backends = crate::cmd_spawn::local_backends();
        assert!(occupant(&store, &backends, &seat).is_none(), "neither is guessed at");
    }

    /// **The delivery half of `compound-174`, and the fault exactly as it
    /// stands on a compound seat.** A governor's record is read by pane to
    /// find out who to hand a sentence to, and a compound seat's record has
    /// no pane: it names itself with ONE id, which is the room *and* the seat
    /// in it, because there is no herdr pane behind it to name. So the record
    /// is filed, `wsp wip` draws the governor on it, and `wsp govern --tell`
    /// answers *the seat is empty — nobody is in cpd-60 to tell*.
    ///
    /// The seat is not empty. The lookup is looking in the wrong place, and the
    /// room is the other name the same seat answers to.
    #[test]
    fn a_seat_with_no_pane_on_its_record_is_still_found_through_its_room() {
        use crate::fake::{Fake, Spot, Stage};
        use crate::place::State;

        let (env, store) = store("pane-less-seat");
        store.set_governor(
            "acc",
            json!({ "workspace": "cpd-60", "pane": "", "host": util::hostname(), "since": util::iso_at(1_000) }),
        );
        let seat = seat_of("acc", store.governors().get("acc").unwrap()).unwrap();
        assert!(seat.pane.is_empty(), "the record names no pane — that is the fault, not the setup");

        let mut stage = Stage::new();
        stage.put(Spot::agent("cpd-60", "claude", "acc", State::Idle));
        let fake = Fake::bind(env.path("herdr.sock"), stage).expect("a socket");
        let (k, v) = fake.socket_env();
        std::env::set_var(k, v);

        let backends = crate::cmd_spawn::local_backends();
        let (_, found) = occupant(&store, &backends, &seat).expect("the room names the seat");
        assert_eq!(found.seat.as_str(), "cpd-60", "found through the room, and by exact match");
    }

    /// **`wsp govern --tell` no longer refuses a busy governor, and this is the
    /// sentence that used to.**
    ///
    /// It used to resolve the occupant and type at the pane, so a governor
    /// mid-turn was answered *"not ready — working"* and the sentence was gone;
    /// `cycle.rs` was hitting it on every verdict at a barrier. Now the words go
    /// into that seat's spool and the receipt says the seat is owed them, which
    /// is the first honest answer this verb has ever given about a busy seat.
    ///
    /// Driven over a socket rather than asserted on the record, because the
    /// refusal was a refusal *to type*: a fake at `Working` is the only way to
    /// see that nothing was typed and that exit code is still zero.
    #[test]
    fn a_tell_to_a_governor_mid_turn_is_held_rather_than_refused() {
        use crate::fake::{Fake, Spot, Stage};
        use crate::place::State;

        let (env, store) = store("tell-busy");
        take(&store, "wsp", "w1", "w1:p1");
        let mut stage = Stage::new();
        stage.put(Spot::agent("w1:p1", "claude", "wsp", State::Working));
        let fake = Fake::bind(env.path("herdr.sock"), stage).expect("a socket");
        let (k, v) = fake.socket_env();
        std::env::set_var(k, v);

        let args = Args::parse(vec![
            "wsp".into(),
            "wsp".into(),
            "--tell".into(),
            "go on to group 2".into(),
        ]);
        let governors = store.governors();
        assert_eq!(tell(&store, &governors, "wsp", "go on to group 2", &args), 0, "busy is not a failure");

        let typed: Vec<_> = fake
            .asked()
            .into_iter()
            .filter(|a| a.verb == crate::fake::Verb::Tell)
            .collect();
        assert!(typed.is_empty(), "a mid-turn governor is not typed at: {typed:?}");

        // The record, which is what the daemon reads and what a person runs
        // `wsp watch --drain` to see.
        let spool = crate::cmd_watch::Spool::of_json(
            &store.watches().get(&crate::wake::key_for("wsp")).and_then(|v| v.get("spool")).cloned().unwrap_or(serde_json::Value::Null),
        );
        assert_eq!(spool.depth(), 1, "and it is owed, which is the whole of the change");
    }

    /// **And the half that decides whether the fix above is safe.** A herdr
    /// `workspace` is a ROOM — `w1` — and no seat is named `w1`, so asking the
    /// backends for the room finds nothing and the answer is byte-for-byte
    /// what it was: the seat is empty, and the fallback never guesses a pane
    /// out of a room that holds several.
    ///
    /// This is the property that makes the fallback a lookup rather than a
    /// widening. If it ever fails, some `-w` record has started resolving to
    /// an arbitrary pane in the room it named, which is the despawn guard's
    /// hazard (`compound-174`) arriving by another road.
    #[test]
    fn a_herdr_room_with_no_pane_is_still_not_guessed_at() {
        use crate::fake::{Fake, Spot, Stage};
        use crate::place::State;

        let (env, store) = store("room-is-not-a-seat");
        store.set_governor(
            "acc",
            json!({ "workspace": "w1", "pane": "", "host": util::hostname(), "since": util::iso_at(1_000) }),
        );
        let seat = seat_of("acc", store.governors().get("acc").unwrap()).unwrap();

        // Two panes in the room, and neither is the room.
        let mut stage = Stage::new();
        stage.put(Spot::agent("w1:p1", "claude", "acc", State::Idle));
        stage.put(Spot::agent("w1:p2", "claude", "acc", State::Idle));
        let fake = Fake::bind(env.path("herdr.sock"), stage).expect("a socket");
        let (k, v) = fake.socket_env();
        std::env::set_var(k, v);

        let backends = crate::cmd_spawn::local_backends();
        assert!(
            occupant(&store, &backends, &seat).is_none(),
            "`w1` is a room, not a seat: the fallback must not resolve it to either pane"
        );
    }

    /// The three cases [`room_and_pane`] decides between, and the one that was
    /// missing before `compound-174` is the third.
    ///
    /// The herdr cases are here as guards rather than as novelty: case 1 is
    /// `-w`, whose refusal to stamp a pane is load-bearing (the despawn guard),
    /// and case 2 must keep ignoring `WSP_SEAT_ID` even when it is set, because
    /// a stale one names a pane herdr never had.
    #[test]
    fn a_governor_records_the_room_it_is_standing_in_on_whichever_backend_that_is() {
        let herdr = herdr::Env {
            pane_id: Some("w1:p6".to_string()),
            workspace_id: Some("w1".to_string()),
            event: None,
            event_json: None,
        };
        // A compound seat: no herdr variables at all, and one id of its own.
        let nowhere = herdr::Env {
            pane_id: None,
            workspace_id: None,
            event: None,
            event_json: None,
        };
        let isolated = util::isolated("room-and-pane");

        assert_eq!(
            room_and_pane(Some("w9"), &herdr),
            (Some("w9".to_string()), None),
            "`-w` names another room, so it stamps no pane: the despawn guard reads that field",
        );
        assert_eq!(
            room_and_pane(None, &herdr),
            (Some("w1".to_string()), Some("w1:p6".to_string())),
            "a herdr pane is a room and a pane, and that is unchanged",
        );

        // A stale WSP_SEAT_ID inside a herdr pane must be ignored, or the
        // record names a pane that backend never listed.
        std::env::set_var(crate::place::SEAT_ENV, "cpd-stale");
        assert_eq!(
            room_and_pane(None, &herdr),
            (Some("w1".to_string()), Some("w1:p6".to_string())),
            "herdr's own variables outrank a WSP_SEAT_ID inherited from somewhere",
        );
        std::env::remove_var(crate::place::SEAT_ENV);

        std::env::set_var(crate::place::SEAT_ENV, "cpd-60");
        assert_eq!(
            room_and_pane(None, &nowhere),
            (Some("cpd-60".to_string()), Some("cpd-60".to_string())),
            "a compound seat names itself with one id, and it is both the room and the seat in it",
        );
        assert_eq!(
            room_and_pane(Some("w9"), &nowhere),
            (Some("w9".to_string()), None),
            "`-w` still outranks the seat's own id: the person named the room",
        );
        drop(isolated);
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
            governs(&store.governors(), &seat_query("w1", None)).as_deref(),
            Some("wsp"),
            "the workspace holds a seat, which is what the workspace token says",
        );

        let hand_written = seated(&[("wsp", "w1")]);
        assert_eq!(
            governs(&hand_written, &seat_query("w1", Some("w1:p1"))).as_deref(),
            Some("wsp"),
            "no pane recorded is nothing to compare, and a seat with no address is still a seat",
        );
    }

    /// `governs` in the shape the row actually asked for: one seat, no
    /// separate workspace beside it — not through [`seat_query`], which every
    /// other test here uses because it still holds both. `whoami` is the
    /// caller that never has a workspace to offer any more, so this is its
    /// call written out directly.
    #[test]
    fn governs_answers_a_bare_seat_with_no_workspace_beside_it() {
        let (_env, store) = store("bare-seat");
        take(&store, "robustness", "w1", "w1:p1");
        take(&store, "wsp", "w2", "w2:p1");

        assert_eq!(
            governs(&store.governors(), &crate::place::Seat::new("w1:p1")).as_deref(),
            Some("robustness"),
        );
        assert_eq!(
            governs(&store.governors(), &crate::place::Seat::new("w2:p1")).as_deref(),
            Some("wsp"),
        );
        // A `-w` record answers for any pane of its room, exactly as it does
        // through `seat_query` — proven here from the bare pane id alone,
        // with nothing passed that names the workspace on its own.
        let room_only = seated(&[("acc", "w9")]);
        assert_eq!(
            governs(&room_only, &crate::place::Seat::new("w9:p3")).as_deref(),
            Some("acc"),
        );
        // A pane in a room nobody governs, or in no room at all, is nobody's.
        assert_eq!(governs(&store.governors(), &crate::place::Seat::new("w3:p1")), None);
        assert_eq!(governs(&store.governors(), &crate::place::Seat::default()), None);
    }

    /// The `wip` row that was wrong all night. Idle on a `doing` task is a
    /// person being the blocker for a worker and is the resting state for a
    /// seat, which spends most of its time waiting on the agents under it.
    #[test]
    fn an_idle_seat_is_not_a_person_being_the_blocker() {
        let g = seated(&[("robustness", "w1")]);
        let seat = |ws: &str| governs(&g, &seat_query(ws, Some(&format!("{ws}:p1")))).is_some();
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
            assert_eq!(needs_a_person(idle, doing, governs(&none, &seat_query("w1", Some("w1:p1"))).is_some()), idle && doing);
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
        assert_eq!(governs(&store.governors(), &seat_query("w1", Some("w1:p1"))).as_deref(), Some("robustness"));

        take(&store, "wsp", "w1", "w1:p1");
        assert_eq!(governs(&store.governors(), &seat_query("w1", Some("w1:p1"))).as_deref(), Some("wsp"), "it moved");
        let slots = slots(&store.governors());
        let robustness = slots.iter().find(|s| s.scope == "robustness").expect("the post stayed");
        assert!(!robustness.filled(), "and it is empty rather than gone");

        // Another workspace's seat is untouched by either.
        take(&store, "data", "w2", "w2:p1");
        assert_eq!(governs(&store.governors(), &seat_query("w2", Some("w2:p1"))).as_deref(), Some("data"));
        assert_eq!(governs(&store.governors(), &seat_query("w1", Some("w1:p1"))).as_deref(), Some("wsp"));
        assert_eq!(governs(&store.governors(), &seat_query("w3", Some("w3:p1"))), None);
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
            seat_for(&store.governors(), &tree(), None, Some("wsp")).map(|s| room_of(&store.governors(), &s.scope)),
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
