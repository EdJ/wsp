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
//! # Per hierarchy, and the chain that makes absence cheap
//!
//! `wsp` has a seat; `robustness` has its own; a sub-project may have one. That
//! is the rule tags, decisions and the handbook already follow — inherited down
//! the chain, specialised at each level — and it falls out of keying the record
//! on the project: [`seat_for`] asks the task's project, then its parent, then
//! its parent, and takes the first answer.
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

/// Every project this workspace is the seat for, in id order.
///
/// No workspace is no seat, never every seat. A pane herdr answered for without
/// a workspace id, or a caller outside herdr entirely, would otherwise match a
/// record whose own field failed to parse — and the answer to "am I the seat"
/// would come back yes for a process that is not in a workspace at all.
pub fn governed_by(governors: &BTreeMap<String, Value>, workspace: &str) -> Vec<String> {
    if workspace.is_empty() {
        return Vec::new();
    }
    governors
        .iter()
        .filter_map(|(p, rec)| seat_of(p, rec).filter(|s| s.workspace == workspace).map(|_| p.clone()))
        .collect()
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

/// `wsp govern [<project>] [--clear]` — take the seat, report it, or stand down.
pub fn govern(store: &Store, args: &Args) -> i32 {
    let p = Paint::new();
    let index = Index::new(store.projects());
    let env = herdr::Env::read();
    let workspace = args.get("workspace").or(env.workspace_id.clone());
    let governors = store.governors();

    if args.has("clear") {
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
    let Some(ws) = workspace else {
        eprintln!("wsp: no workspace — pass -w, or run inside herdr");
        return 2;
    };

    // Taking a seat somebody else is in is allowed and is said out loud. The
    // alternative is a refusal on a record whose whole content is "an agent is
    // sitting here", which goes stale every time a session ends without
    // standing down — and a seat you cannot take back after a crash is worse
    // than one that changes hands with a line of output.
    let displaced = governors.get(&proj.id).and_then(|rec| seat_of(&proj.id, rec)).filter(|s| s.workspace != ws);

    store.set_governor(
        &proj.id,
        json!({
            "workspace": ws,
            "pane": env.pane_id.clone().unwrap_or_default(),
            "host": util::hostname(),
            "since": util::now_iso(),
        }),
    );
    store.log_event("governor-set", json!({ "project": proj.id, "workspace": ws }));

    if args.json() {
        println!(
            "{}",
            json!({ "project": proj.id, "workspace": ws, "displaced": displaced.map(|s| s.workspace) })
        );
        return 0;
    }
    println!("{} {}", p.cyan("▣"), p.bold(&proj.id));
    if let Some(was) = displaced {
        println!("  {}", p.dim(&format!("taken from {}", was.workspace)));
    }
    println!("  {}", p.dim("raised hands here reach this workspace · wsp govern --clear to stand down"));
    0
}

/// `wsp govern --clear [<project>]` — this workspace stops being the seat.
///
/// Bare, it stands down from everything this workspace holds, because a session
/// ending does not end one seat at a time. Named, it gives up one and keeps the
/// rest, which is how a seat that governs `wsp` and `robustness` hands the
/// second one on.
fn stand_down(store: &Store, index: &Index, args: &Args, workspace: Option<&str>) -> i32 {
    let p = Paint::new();
    let governors = store.governors();
    let held: Vec<String> = match args.rest.first() {
        Some(needle) => match index.find(needle) {
            Some(proj) => vec![proj.id.clone()],
            None => {
                eprintln!("wsp: no such project `{needle}`");
                return 1;
            }
        },
        None => match workspace {
            Some(ws) => governed_by(&governors, ws),
            None => {
                eprintln!("wsp: no workspace — pass -w, or name the project");
                return 2;
            }
        },
    };

    let cleared: Vec<String> = held.into_iter().filter(|proj| store.clear_governor(proj)).collect();
    for proj in &cleared {
        store.log_event("governor-cleared", json!({ "project": proj }));
    }

    if args.json() {
        println!("{}", json!({ "cleared": cleared }));
    } else if cleared.is_empty() {
        println!("{}", p.dim("this workspace is nobody's seat"));
    } else {
        println!("{} {}", p.dim("stood down —"), cleared.join(" "));
    }
    0
}

/// What is seated: this workspace's own, the seat above it, or the whole roster.
fn report(store: &Store, index: &Index, args: &Args, workspace: Option<&str>) -> i32 {
    let p = Paint::new();
    let governors = store.governors();
    let mine: Vec<String> = workspace.map(|ws| governed_by(&governors, ws)).unwrap_or_default();

    if args.json() {
        let seats: Vec<Value> = governors
            .iter()
            .filter_map(|(proj, rec)| seat_of(proj, rec))
            .map(|s| json!({ "project": s.project, "workspace": s.workspace, "pane": s.pane, "since": s.since }))
            .collect();
        println!("{}", json!({ "workspace": workspace, "governs": mine, "seats": seats }));
        return 0;
    }

    if governors.is_empty() {
        println!("{}", p.dim("no seats — wsp govern <project> takes one"));
        return 0;
    }
    for (proj, rec) in &governors {
        let Some(s) = seat_of(proj, rec) else { continue };
        let here = mine.contains(proj);
        let mark = if here { p.cyan("▣") } else { p.dim("·") };
        let seat = match s.pane.is_empty() {
            true => s.workspace.clone(),
            false => format!("{} · {}", s.workspace, s.pane),
        };
        println!("{} {}  {}", mark, p.bold(proj), p.dim(&seat));
    }
    if mine.is_empty() {
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
        let seat = |ws: &str| !governed_by(&g, ws).is_empty();
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
            assert_eq!(needs_a_person(idle, doing, !governed_by(&none, "w1").is_empty()), idle && doing);
        }
    }

    /// One workspace may hold several seats — a night that coordinates
    /// `robustness` and answers for `wsp` above it is one agent, not two — and
    /// standing down bare gives up all of them, because a session ending does
    /// not end one seat at a time.
    #[test]
    fn a_workspace_can_hold_more_than_one_seat() {
        let g = seated(&[("robustness", "w1"), ("wsp", "w1"), ("data", "w2")]);
        assert_eq!(governed_by(&g, "w1"), ["robustness", "wsp"]);
        assert_eq!(governed_by(&g, "w2"), ["data"]);
        assert!(governed_by(&g, "w3").is_empty());
    }
}
