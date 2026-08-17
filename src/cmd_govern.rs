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
//! # …and what it turned out to be, once a person had to talk to one
//!
//! The record above routes a raised hand and it is not a **position**. Ed,
//! 2026-08-17, on holding two governorships from one workspace: *"I don't see
//! you as governor, I still see you as robustness/078 — and you've not been
//! moved to sit below the wsp line as I would expect."* The decision on
//! t-260817-021 settles the shape:
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
//! been claimed onto t-260817-021, begin work" but "you are the custodian of
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
//! store renamed the live governor's own window. `wsp sandbox` (t-260816-056)
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

/// A seat, resolved: which project it governs and where it is sitting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seat {
    /// The project the record is filed under — which is the project *governed*,
    /// and not necessarily the one that was asked about. [`seat_for`] walks up,
    /// so a flag on a `data` task can resolve to the `wsp` seat, and the reader
    /// wants to be told which.
    pub project: String,
    pub workspace: String,
    /// The pane the agent was in when it took the seat. Display and the despawn
    /// guard only — it goes stale the moment that agent is restarted, and
    /// everything that must stay correct reads [`Seat::workspace`].
    pub pane: String,
    pub since: String,
}

/// A slot on a project: the position, and whoever is in it now.
///
/// [`Seat`] is the occupancy and this is the post. The difference is the whole
/// of "it outlives its occupant": a slot whose agent has gone is a slot with
/// `occupant: None`, which still has a project, a place in the tree and a row
/// you can stand on — where before, the record was deleted and the position
/// went with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    pub project: String,
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
        .map(|(project, rec)| Slot {
            occupant: seat_of(project, rec),
            host: rec.get("host").and_then(Value::as_str).unwrap_or_default().to_string(),
            since: rec
                .get("since")
                .or_else(|| rec.get("vacated"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            project: project.clone(),
        })
        .collect()
}

/// One record, if it belongs to this machine.
///
/// A workspace id is herdr's and means nothing on another host — the same
/// reason a claim and a mandate each carry one. A seat on another machine is
/// not a seat you can reach, so it reads as no seat rather than as a wrong one.
fn seat_of(project: &str, rec: &Value) -> Option<Seat> {
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
        project: project.to_string(),
        workspace: workspace.to_string(),
        pane: rec.get("pane").and_then(Value::as_str).unwrap_or_default().to_string(),
        since: rec.get("since").and_then(Value::as_str).unwrap_or_default().to_string(),
    })
}

/// The seat responsible for work in `project`: its own, or the nearest one
/// above it.
///
/// The escalation the task asked about, and it is an ancestor walk rather than
/// a policy. `robustness` has a seat and `wsp` has a seat: a hand raised in
/// `robustness` reaches the first, and the same hand raised while that seat is
/// away reaches the second. Nothing decides to escalate — the walk simply does
/// not stop at a level that has nobody in it, which is also why standing down
/// needs no hand-over.
pub fn seat_for(
    governors: &BTreeMap<String, Value>,
    index: &Index,
    project: Option<&str>,
) -> Option<Seat> {
    let project = project?;
    if governors.is_empty() {
        return None;
    }
    std::iter::once(project.to_string())
        .chain(index.ancestors(project))
        .find_map(|p| governors.get(&p).and_then(|rec| seat_of(&p, rec)))
}

/// The project this workspace is the custodian of, if it is the custodian of
/// one.
///
/// **One agent, one governorship.** Ed, 2026-08-17, reversing what
/// t-260817-013 built for. That task argued a night coordinating `robustness`
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
pub fn governs(governors: &BTreeMap<String, Value>, workspace: &str) -> Option<String> {
    if workspace.is_empty() {
        return None;
    }
    governors
        .iter()
        .find(|(p, rec)| seat_of(p, rec).is_some_and(|s| s.workspace == workspace))
        .map(|(p, _)| p.clone())
}

/// Is this idle agent a person's problem?
///
/// The rule `wip` and the panel have both always applied — an idle process on a
/// `doing` task means whoever is working it has stopped and a person is the
/// blocker — with the one exception the seat creates. A governor is idle
/// *between* the agents it is sequencing, which is most of the time, and
/// reading that as a stall marked the busiest agent on the machine as stuck.
///
/// Here rather than at the three call sites so the exception is stated once. It
/// was already the same expression written out three times; it is now the same
/// expression with a reason attached, written out none. Whether the pane is a
/// seat comes in as a bool because the two callers know it differently — `wip`
/// has the map in hand, the panel has already joined it onto the row — and
/// neither should have to hold the other's shape to ask the question.
pub fn needs_a_person(idle: bool, doing: bool, seat: bool) -> bool {
    idle && doing && !seat
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
    let held = governs(&store.governors(), workspace);
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
    if let Some(had) = governs(&store.governors(), workspace).filter(|p| p != project) {
        vacate(store, &had);
    }

    store.set_governor(
        project,
        json!({
            "workspace": workspace,
            "pane": pane,
            "host": util::hostname(),
            "since": util::now_iso(),
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
pub fn vacate(store: &Store, project: &str) -> bool {
    let governors = store.governors();
    let Some(rec) = governors.get(project) else { return false };
    // Already empty. Said as false so a caller can report what it actually
    // changed rather than what it looked at.
    if seat_of(project, rec).is_none() && rec.get("workspace").is_none() {
        return false;
    }
    let was = rec.get("workspace").and_then(Value::as_str).unwrap_or_default().to_string();
    store.set_governor(project, json!({ "vacated": util::now_iso() }));
    store.log_event("governor-vacated", json!({ "project": project }));
    // The room keeps the name of whatever it still answers for, and gets its
    // own back when that is nothing.
    rename_seat(store, &was);
    true
}

/// The pane the slot's agent is in *now*, which is not the pane it started in.
///
/// The record names a workspace because that is the durable half, and the pane
/// on it is display only — an agent cleared and restarted comes back on another
/// pane in the same room. So anything that speaks to a slot asks the runner who
/// is in that room at the moment of speaking, and a slot answered by nobody is
/// a vacancy rather than a stale address.
pub fn occupant(seat: &Seat) -> Option<herdr::Pane> {
    let panes = herdr::panes().ok()?;
    // The recorded pane first when it is still there and still has an agent —
    // a workspace with two agents in it should keep answering as the one that
    // took the seat rather than alternating with its neighbour.
    panes
        .iter()
        .find(|p| p.pane_id == seat.pane && !p.agent.is_empty())
        .or_else(|| panes.iter().find(|p| p.workspace_id == seat.workspace && !p.agent.is_empty()))
        .cloned()
}

/// `wsp govern [<project>] [--clear|--remove|--tell "…"]`
pub fn govern(store: &Store, args: &Args) -> i32 {
    let p = Paint::new();
    let index = Index::new(store.projects());
    let env = herdr::Env::read();
    let workspace = args.get("workspace").or(env.workspace_id.clone());
    let governors = store.governors();

    if args.has("clear") || args.has("remove") {
        return stand_down(store, &index, args, workspace.as_deref());
    }

    // Nothing named: report. Inside a workspace that is the answer to "what am
    // I the seat for, and who is the seat above me"; outside one there is no
    // such question, so it is the roster. Neither changes anything — a command
    // that only reports is one an agent can run without having decided.
    let Some(needle) = args.rest.first().cloned() else {
        return report(store, &index, args, workspace.as_deref());
    };

    let Some(proj) = index.find(&needle) else {
        eprintln!("wsp: no such project `{needle}`");
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
        let text = match args.get("tell") {
            Some(t) if t != "true" => t,
            _ => args.rest[1..].join(" "),
        };
        return tell(&governors, &proj.id, &text, args);
    }

    let Some(ws) = workspace else {
        eprintln!("wsp: no workspace — pass -w, or run inside herdr");
        return 2;
    };

    // The pane is the environment's only when the workspace is too. `-w` names
    // a room this process is not standing in, and stamping the caller's own
    // pane onto that record would point the despawn guard at a pane in another
    // workspace entirely.
    let pane = match args.get("workspace") {
        Some(_) => String::new(),
        None => env.pane_id.clone().unwrap_or_default(),
    };
    // What this workspace is giving up by taking this, read before the write
    // rather than after it. One agent holds one governorship, so `take` hands
    // the old one back — and a hand-over that happens silently is one you find
    // out about from a panel a day later.
    let handed_back = governs(&governors, &ws).filter(|p| p != &proj.id);
    let displaced = take(store, &proj.id, &ws, &pane);

    if args.json() {
        println!(
            "{}",
            json!({
                "project": proj.id,
                "workspace": ws,
                "displaced": displaced.map(|s| s.workspace),
                "stood_down_from": handed_back,
            })
        );
        return 0;
    }
    println!("{} {}", p.cyan("▣"), p.bold(&proj.id));
    if let Some(was) = displaced {
        println!("  {}", p.dim(&format!("taken from {}", was.workspace)));
    }
    if let Some(was) = &handed_back {
        println!("  {}", p.dim(&format!("stood down from {was}, and its seat is open")));
    }
    println!("  {}", p.dim("raised hands here reach this workspace · wsp govern --clear to stand down"));
    0
}

/// `wsp govern <project> --tell "…"` — a sentence for whoever is in the slot.
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
fn tell(governors: &BTreeMap<String, Value>, project: &str, text: &str, args: &Args) -> i32 {
    use crate::place::Seat as Pane;

    let text = text.trim();
    if text.is_empty() {
        eprintln!("wsp: nothing to say");
        return 2;
    }
    let Some(seat) = governors.get(project).and_then(|rec| seat_of(project, rec)) else {
        eprintln!("wsp: no seat on `{project}` — wsp govern {project} fills it");
        return 1;
    };
    let Some(pane) = occupant(&seat) else {
        eprintln!("wsp: the {project} seat is empty — nobody is in {} to tell", seat.workspace);
        return 1;
    };

    let place = crate::place_herdr::Herdr::new();
    let how = crate::agent_commands::of(&pane.agent);
    match how.tell(&place, &Pane::new(&pane.pane_id), text) {
        Ok(()) => {
            if args.json() {
                println!("{}", json!({ "project": project, "pane": pane.pane_id, "told": true }));
            } else {
                println!("{}", Paint::new().dim(&format!("→ {} · {}", project, pane.pane_id)));
            }
            0
        }
        Err(e) => {
            eprintln!("wsp: the {project} seat was not told: {e}");
            1
        }
    }
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
        Some(needle) => match index.find(needle) {
            Some(proj) => Some(proj.id.clone()),
            None => {
                eprintln!("wsp: no such project `{needle}`");
                return 1;
            }
        },
        None => match workspace {
            Some(ws) => governs(&governors, ws),
            None => {
                eprintln!("wsp: no workspace — pass -w, or name the project");
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
fn report(store: &Store, index: &Index, args: &Args, workspace: Option<&str>) -> i32 {
    let p = Paint::new();
    let governors = store.governors();
    let mine: Option<String> = workspace.and_then(|ws| governs(&governors, ws));

    let slots = slots(&governors);

    if args.json() {
        let seats: Vec<Value> = slots
            .iter()
            .map(|s| {
                json!({
                    "project": s.project,
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
        println!("{}", p.dim("no seats — wsp govern <project> takes one"));
        return 0;
    }
    // Vacant slots draw too, and that is the point of the list: a position
    // nobody is standing in is the row you are reading this to find.
    for s in &slots {
        let here = mine.as_deref() == Some(s.project.as_str());
        let mark = if here { p.cyan("▣") } else { p.dim("·") };
        let who = match (&s.occupant, s.elsewhere()) {
            (Some(o) , _) if o.pane.is_empty() => o.workspace.clone(),
            (Some(o), _) => format!("{} · {}", o.workspace, o.pane),
            (None, true) => format!("on {}", s.host),
            (None, false) => "empty · wsp spawn -p <project> --govern fills it".to_string(),
        };
        println!("{} {}  {}", mark, p.bold(&s.project), p.dim(&who));
    }
    if mine.is_none() {
        // Where a hand raised *here* would go. The question an agent asks is
        // never "who are the seats" — it is "who is mine" — and the roster
        // above answers the first without answering the second.
        let project = crate::cmd_agent::current_project(store, args, index).ok().flatten();
        match seat_for(&governors, index, project.as_deref()) {
            Some(s) => println!("{}", p.dim(&format!("work here reaches the {} seat", s.project))),
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
        let s = seat_for(&g, &tree(), Some("robustness")).unwrap();
        assert_eq!((s.project.as_str(), s.workspace.as_str()), ("robustness", "w1"));
    }

    /// The escalation question the overview asks, and the answer is that there
    /// is no escalation step — the walk simply does not stop at an empty level.
    #[test]
    fn a_hand_raised_where_the_local_seat_is_empty_reaches_the_one_above() {
        let g = seated(&[("wsp", "w9")]);
        let s = seat_for(&g, &tree(), Some("data")).unwrap();
        assert_eq!(s.project, "wsp", "past robustness, which has nobody in it");
    }

    /// The normal state, and the one that has to stay cheap: no seat anywhere
    /// is not an error, a default or a fallback seat. It is `None`, which every
    /// caller already draws as today's behaviour.
    #[test]
    fn no_seat_anywhere_above_a_project_is_simply_no_seat() {
        assert_eq!(seat_for(&BTreeMap::new(), &tree(), Some("data")), None);
        assert_eq!(seat_for(&seated(&[("robustness", "w1")]), &tree(), Some("tooling")), None);
    }

    /// A seat is reachable from below and from nowhere else. `robustness` is
    /// inside `wsp`, so the `wsp` seat answers for it; `tooling` is outside
    /// `robustness`, and a seat that answered downward would have every hand in
    /// the tree arriving at every seat in it.
    #[test]
    fn a_seat_answers_for_what_is_under_it_and_not_for_its_siblings() {
        let g = seated(&[("robustness", "w1")]);
        let index = tree();
        assert!(seat_for(&g, &index, Some("data")).is_some());
        assert_eq!(seat_for(&g, &index, Some("wsp")), None);
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
        assert_eq!(seat_for(&g, &tree(), Some("robustness")), None);
    }

    /// The `wip` row that was wrong all night. Idle on a `doing` task is a
    /// person being the blocker for a worker and is the resting state for a
    /// seat, which spends most of its time waiting on the agents under it.
    #[test]
    fn an_idle_seat_is_not_a_person_being_the_blocker() {
        let g = seated(&[("robustness", "w1")]);
        let seat = |ws: &str| governs(&g, ws).is_some();
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
            assert_eq!(needs_a_person(idle, doing, governs(&none, "w1").is_some()), idle && doing);
        }
    }

    /// **One agent, one governorship** — the reversal of what t-260817-013
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
        assert_eq!(governs(&store.governors(), "w1").as_deref(), Some("robustness"));

        take(&store, "wsp", "w1", "w1:p1");
        assert_eq!(governs(&store.governors(), "w1").as_deref(), Some("wsp"), "it moved");
        let slots = slots(&store.governors());
        let robustness = slots.iter().find(|s| s.project == "robustness").expect("the post stayed");
        assert!(!robustness.filled(), "and it is empty rather than gone");

        // Another workspace's seat is untouched by either.
        take(&store, "data", "w2", "w2:p1");
        assert_eq!(governs(&store.governors(), "w2").as_deref(), Some("data"));
        assert_eq!(governs(&store.governors(), "w1").as_deref(), Some("wsp"));
        assert_eq!(governs(&store.governors(), "w3"), None);
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
        assert!(seat_for(&store.governors(), &tree(), Some("wsp")).is_some());

        assert!(vacate(&store, "wsp"), "there was somebody in it");
        let g = store.governors();
        assert!(g.contains_key("wsp"), "the position went with the agent");
        assert_eq!(seat_for(&g, &tree(), Some("wsp")), None, "an empty seat routes nothing");

        let slots = slots(&g);
        assert_eq!(slots.len(), 1);
        assert!(!slots[0].filled(), "drawn, and drawn as empty");
        assert!(!slots[0].elsewhere(), "empty here is not held elsewhere");

        // Filled again by the next agent, which is the whole point of keeping
        // it, and vacating twice changes nothing the second time.
        assert!(!vacate(&store, "wsp"), "already empty");
        take(&store, "wsp", "w2", "w2:p1");
        assert_eq!(
            seat_for(&store.governors(), &tree(), Some("wsp")).map(|s| s.workspace),
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
