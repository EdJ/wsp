//! The run's steps, taken by wsp: `wsp-134`.
//!
//! A worklist used to advance only when its governor did something — spawned
//! each member, read each finished one, passed the barrier, spawned the next
//! group, rotated. So the governor had to be awake at every step, and a
//! harness has one way of keeping an agent awake: a conversation held open,
//! watchers and wakeups inside it, or a person typing "cpd-144 needs input"
//! into its pane. Ed, 2026-09-30: **wsp runs the steps, and agents are for
//! decisions and action.** They do not coordinate the steps.
//!
//! # Driven by the verbs, not by a daemon
//!
//! The wake path in [`crate::wake`] already existed and had been dead for three
//! days when this row was written: its only launcher was herdr's `[[startup]]`,
//! and the fleet had moved to compound. Ed's answer to "how does the daemon
//! run without herdr" was the better question — *do we need one?* — and the
//! answer is no. Every event a run advances on is a wsp verb run in some
//! agent's own process: a member's `review` and `land`, a verifier's
//! `review` or `block`, the barrier agent's `go` or `hold`. So the verb is
//! the trigger. [`poke`] sits at the seam every status verb already shares
//! (`cmd_task::mutate_saying`), and at `land`, `go` and `hold`.
//!
//! What that gives up is anything with no verb behind it: an agent that dies,
//! or stalls on a prompt, before it runs `wsp review` never triggers the next
//! step. That stays the person's to see, on the panel, as it always was.
//!
//! # The chain
//!
//! For a running list whose current group carries a [`Policy`]:
//!
//! 1. Members not yet started are spawned, up to the group's cap, each on its
//!    own `agent <id>:` line where it has one and the group's otherwise
//!    ([`Group::policy_for`], `wsp-150`).
//! 2. A member that has reached `review` **and landed** gets a read-only
//!    verifier, on the member's policy: a child row tagged [`VERIFY_TAG`]
//!    whose overview is its work order. It notes its verdict on the member and
//!    reviews its own row, or blocks it.
//! 3. When every member is landed and every verifier is at `review`, one agent
//!    checks the barrier — a row tagged [`BARRIER_TAG`] — on the group's own
//!    line, since it reads the group as a whole, and ends it with
//!    `wsp worklist go` or `hold`.
//! 4. A pass rotates the governing seat onto a fresh successor and starts the
//!    next group (step 1 again). A governor is *told* what happened at each
//!    point that asks for a decision: a block, a hold, a pass. It never has to
//!    act for the run to move.
//!
//! # Levels, so a lost trigger costs a delay and nothing else
//!
//! [`advance`] reads where the run is from the store and git alone and does
//! whatever is owed; it does not remember what it did last time. So it is
//! idempotent, a second trigger while the first is running finds the work
//! already done, and `wsp worklist advance` by hand repairs a trigger that was
//! lost. Each thing it starts is keyed on a record written *before* the spawn,
//! inside the store lock — the member moved to `doing`, the verifier or
//! barrier row created — so two advances racing find the record and start
//! nothing twice.
//!
//! # Detached, because the trigger is somebody else's command
//!
//! A member's `wsp review` must return in a second, and a spawn waits for an
//! agent to come up. So [`poke`] starts `wsp worklist advance` in a process
//! group of its own — the reason `cmd_spawn::arrange_ending` gives: an agent's
//! ending signals its group — with its output in `cycle.log` in the state
//! directory, since nobody is reading the pane it started from.

use std::io::Write;
use std::process::{Command, Stdio};

use crate::model::{Group, Policy, Status, Task, Worklist, WorklistStatus};
use crate::store::Store;
use crate::util;
use crate::worklist::{self, Reading, Settlement};
use crate::Args;

/// The tag on a verifier's row. Its parent is the member it verifies.
pub(crate) const VERIFY_TAG: &str = "verify";
/// The tag on the row of the agent that checks a barrier.
pub(crate) const BARRIER_TAG: &str = "barrier";

/// Set in the environment of anything [`poke`] starts, and read by nothing but
/// a test or a person who wants a verb to leave the run alone.
const OFF: &str = "WSP_NO_CYCLE";

/// After a status verb: start the steps this may have made due.
///
/// Cheap and in-process up to the one decision that matters — is this row
/// part of a run at all? — so the ordinary `wsp review` on a task nobody
/// listed pays for one read of `worklists/` and starts nothing.
pub(crate) fn poke_task(store: &Store, id: &str, verb: &str) {
    let Some(t) = store.find_task(id) else { return };
    let ours = t.tags.iter().any(|g| g == VERIFY_TAG || g == BARRIER_TAG);
    let member = worklist::Running::read(store).list_of(&t.id).is_some();
    if !ours && !member {
        return;
    }
    launch(store, &["worklist", "advance", "--task", &t.id, "--verb", verb]);
}

/// After `go` or `hold` on a list: tell whoever governs it, and take the steps
/// the new position owes. `passed` is the barrier crossed, if one was.
pub(crate) fn poke_list(store: &Store, list: &str, event: &str, passed: Option<usize>) {
    let n = passed.map(|n| n.to_string()).unwrap_or_default();
    let mut argv = vec!["worklist", "advance", list, "--event", event];
    if passed.is_some() {
        argv.extend(["--passed", n.as_str()]);
    }
    launch(store, &argv);
}

fn launch(store: &Store, argv: &[&str]) {
    // A test's `current_exe` is the test harness, and handing it `worklist
    // advance` would run the suite again from inside itself.
    if cfg!(test) || std::env::var_os(OFF).is_some() {
        return;
    }
    use std::os::unix::process::CommandExt;
    let Ok(exe) = std::env::current_exe() else { return };
    let log = store.state_file("cycle.log");
    let Ok(out) = std::fs::OpenOptions::new().create(true).append(true).open(&log) else { return };
    let Ok(err) = out.try_clone() else { return };
    let mut cmd = Command::new(exe);
    cmd.args(argv)
        // Nobody's pane: a spawn started from here must not read itself as
        // the agent whose verb triggered it.
        .env_remove(crate::place::SEAT_ENV)
        .env_remove("HERDR_PANE_ID")
        .env_remove("HERDR_WORKSPACE_ID")
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .process_group(0);
    let _ = cmd.spawn();
}

/// `wsp worklist advance [<slug>] [--task ID --verb V] [--event go|hold --passed N]`
///
/// Take every step the store says is owed, on one list or on every running
/// one. The event flags are what the trigger saw, and they only decide what a
/// governor is told: the steps themselves are read off the store.
pub fn advance(store: &Store, args: &Args) -> i32 {
    let named = args.rest.get(1).cloned();
    stamp(&format!("advance {}", args.rest[1..].join(" ")));

    if let (Some(id), Some(verb)) = (args.get("task"), args.get("verb")) {
        told_about_task(store, &id, &verb);
    }
    if let (Some(list), Some(event)) = (named.as_deref(), args.get("event")) {
        let passed = args.get("passed").and_then(|n| n.parse::<usize>().ok());
        if let Some(w) = store.worklist(list) {
            match event.as_str() {
                "hold" => tell(store, &w, &format!(
                    "The {} run was held at its barrier: `wsp worklist show {}` has the reason. \
                     wsp starts nothing more until somebody runs `wsp worklist go {}`.",
                    w.id, w.id, w.id
                )),
                "go" => {
                    // Whose `go` this was decides what follows it. A group
                    // run by hand was passed by its governor, who ends its
                    // agents and rotates itself under the manual order — doing
                    // either here as well is a double rotation on a live run
                    // (wsp-135's FAIL, on ux-revamp held at barrier 16). The
                    // step is still taken: a hand-run group may be followed
                    // by one wsp runs.
                    if !ran_by_wsp(&w, passed) {
                        let _ = step(store, &w);
                        return 0;
                    }
                    // Rotate first, so what is told next reaches the successor
                    // rather than a seat that is about to be ended. Not on the
                    // last pass: there is nothing left to govern.
                    if let Some(n) = passed {
                        end_behind(store, &w, n);
                        if n < w.groups().len() {
                            rotate(store, &w);
                        }
                    }
                    let _ = step(store, &w);
                    tell(store, &w, &passed_sentence(store, &w, passed));
                    return 0;
                }
                _ => {}
            }
        }
    }

    let lists: Vec<Worklist> = match &named {
        Some(n) => store.worklist(n).into_iter().collect(),
        None => store.worklists(),
    };
    for w in lists.iter().filter(|w| w.status() == WorklistStatus::Running) {
        let _ = step(store, w);
    }
    0
}

/// Whether the group a `go` concerns is one wsp runs: the group just passed,
/// or on a start, the first group the run stands at.
fn ran_by_wsp(w: &Worklist, passed: Option<usize>) -> bool {
    let groups = w.groups();
    let g = match passed {
        Some(n) => groups.get(n - 1),
        None => groups.iter().find(|g| g.verdict.trim().is_empty()),
    };
    g.is_some_and(|g| g.policy().is_some())
}

/// What the run looks like after a `go`, in the words a governor is told.
fn passed_sentence(store: &Store, w: &Worklist, passed: Option<usize>) -> String {
    let w = store.worklist(&w.id).unwrap_or_else(|| w.clone());
    let groups = w.groups();
    let head = match passed {
        Some(n) => {
            let verdict = groups.get(n - 1).map(|g| util::truncate(g.verdict.trim(), 400)).unwrap_or_default();
            format!("Barrier {n} of {} passed. The verdict: {verdict}", w.id)
        }
        None => format!("{} started.", w.id),
    };
    let pos = worklist::position(store, &w, Reading::Settled);
    let tail = match pos.at.and_then(|at| groups.get(at - 1).map(|g| (at, g))) {
        None => format!(" Nothing is left in the run: `wsp worklist done {}` closes it.", w.id),
        Some((at, g)) if g.policy().is_some() => {
            format!(" wsp is running group {at}: {}. You are told if one of them needs a decision.", g.members.join(" "))
        }
        Some((at, g)) => format!(
            " Group {at} ({}) has no `agent:` line, so it is run by hand: `wsp worklist next {}`.",
            g.members.join(" "),
            w.id
        ),
    };
    format!("{head}{tail}")
}

/// A block on anything in a run is a decision somebody owes, so the seat
/// governing the run is told. A review is not: it is the next step, and wsp
/// takes it — unless the member's work is not on the trunk, when there is no
/// step wsp can take and nobody else will notice.
fn told_about_task(store: &Store, id: &str, verb: &str) {
    let Some(t) = store.find_task(id) else { return };
    let Some(w) = list_of(store, &t) else { return };
    if verb == "review" {
        return told_unlanded(store, &w, &t);
    }
    if verb != "blocked" {
        return;
    }
    let what = if t.tags.iter().any(|g| g == VERIFY_TAG) {
        "A verifier found a problem"
    } else if t.tags.iter().any(|g| g == BARRIER_TAG) {
        "The barrier check stopped on a question"
    } else {
        "A member is blocked"
    };
    tell(store, &w, &format!(
        "{what} in the {} run: {} ({}) is blocked. `wsp show {}` says on what. \
         The run waits here until it is unblocked.",
        w.id, t.id, util::truncate(&t.title, 60), t.id
    ));
}

/// A member that reached `review` with commits its trunk has not got.
///
/// `review` is an agent's terminal verb — it lands first — so a member that
/// did not land has stopped, and its verifier is keyed on the landing. Only a
/// group wsp runs: by hand, the governor is the one reading reviews.
fn told_unlanded(store: &Store, w: &Worklist, t: &Task) {
    if t.tags.iter().any(|g| g == VERIFY_TAG || g == BARRIER_TAG) {
        return;
    }
    let pos = worklist::position(store, w, Reading::Landed);
    let Some(g) = pos.at.and_then(|at| w.groups().get(at - 1).cloned()) else { return };
    if g.policy().is_none() {
        return;
    }
    let Some(s) = pos.members.iter().find(|s| s.id == t.id) else { return };
    if s.finished() || !s.settlement.settled() {
        return;
    }
    tell(store, w, &format!("In the {} run, {}", w.id, unlanded(&s.id, &s.note())));
}

/// The sentence for a reviewed member that has not landed, in the log and
/// to the seat alike.
fn unlanded(id: &str, note: &str) -> String {
    format!(
        "{id} is at review with {note}, so it has no verifier yet. \
         `wsp land {id}` puts it there, and wsp starts the verifier on the land."
    )
}

/// The running list a row belongs to: itself a member, a verifier under one,
/// or the barrier row named after the list.
fn list_of(store: &Store, t: &Task) -> Option<Worklist> {
    let running = worklist::Running::read(store);
    let id = match t.tags.iter().any(|g| g == VERIFY_TAG) {
        true => t.parent.clone().unwrap_or_default(),
        false => t.id.clone(),
    };
    if let Some(l) = running.list_of(&id) {
        return store.worklist(l);
    }
    let slug = t.title.strip_prefix(BARRIER_TITLE)?.split_whitespace().next()?.to_string();
    store.worklist(&slug)
}

/// Everything the current group owes, taken in order. Returns what it started,
/// for the log.
fn step(store: &Store, w: &Worklist) -> Vec<String> {
    let mut started = Vec::new();
    let w = match store.worklist(&w.id) {
        Some(w) => w,
        None => return started,
    };
    if w.status() != WorklistStatus::Running {
        return started;
    }
    let groups = w.groups();
    let pos = worklist::position(store, &w, Reading::Landed);
    let Some(at) = pos.at else { return started };
    let Some(g) = groups.get(at - 1) else { return started };
    let Some(policy) = g.policy() else { return started };

    // 1. Members nobody has started, up to the cap — counted again inside
    // the lock by `take_member`, because the two advances a pass can start
    // (the barrier agent's `go` and its own `review`) both read this list.
    let cap = g.parallelism(None);
    for s in &pos.members {
        let Some(t) = store.find_task(&s.id) else { continue };
        if let Some((why, own)) = take_member(store, &t, &w.id, at, &g.members, cap) {
            stamp(&format!("{} group {at}: starting {} on {} ({why})", w.id, s.id, own.kind));
            if spawn(store, &s.id, &own, Undo::Member) {
                started.push(s.id.clone());
            } else {
                failed(store, &w, &s.id);
            }
        }
    }

    // 2. A verifier for each member that is finished — reviewed and landed.
    for s in pos.members.iter().filter(|s| s.finished()) {
        let Some(member) = store.find_task(&s.id) else { continue };
        if let Some(v) = open_verifier(store, &member, &w.id, at, g) {
            // On the kind the work ran on — `wsp-134` d1 (C) — which in a
            // mixed group is the member's own line and not the group's.
            let on = g.policy_for(&member.id).unwrap_or_else(|| policy.clone());
            if spawn(store, &v, &on, Undo::Row) {
                started.push(v);
            } else {
                failed(store, &w, &v);
            }
        }
    }

    // 2a. And one that is reviewed and *not* landed gets nothing, which the
    // log has to say: tokenhub-003 went to review with its commit still on
    // its branch, this started nothing and wrote nothing, and its governor
    // went looking for the cause in the wrong record (`wsp-142`).
    for s in pos.members.iter().filter(|s| s.settlement.settled() && !s.finished()) {
        stamp(&format!("{} group {at}: {}", w.id, unlanded(&s.id, &s.note())));
    }

    // 3. The barrier, once every member is landed and every verdict is in.
    let tasks = store.tasks();
    let verified = |id: &str| {
        latest_verifier(&tasks, id).is_some_and(|v| matches!(v.status(), Status::Review | Status::Done))
    };
    if pos.at_barrier() && pos.members.iter().all(|s| verified(&s.id)) {
        if let Some(b) = open_barrier(store, &w, at, g) {
            if spawn(store, &b, &policy, Undo::Row) {
                started.push(b);
            } else {
                failed(store, &w, &b);
            }
        }
    }
    if !started.is_empty() {
        stamp(&format!("{} group {at}: started {}", w.id, started.join(" ")));
    }
    started
}

/// How long a start may take to show up as a claim before wsp reads it as
/// one that never happened — an advance killed between its record and its
/// spawn, or a spawn that died without saying so. `wsp spawn` claims within a
/// minute when it works; ten leaves a slow one alone.
const WEDGED_AFTER: i64 = 10 * 60;

/// Unclaimed, and written long enough ago that a start in flight would have
/// claimed it by now.
fn wedged(t: &Task, claims: &std::collections::BTreeMap<String, serde_json::Value>) -> bool {
    !claims.contains_key(&t.id) && util::epoch_secs() - util::epoch_of(&t.updated) > WEDGED_AFTER
}

/// The line `take_member` writes, and how it recognises its own record.
const STARTED_BY: &str = "started by wsp:";

/// Move a member to `doing` under the store lock, if it may start now, and
/// say why it may and what it starts on. `None` is "leave it".
///
/// **The policy is read inside the lock too**, from the list as it stands
/// then, and not off the group [`step`] read before it. A member's line is
/// editable until the member starts (`wsp worklist member`, which writes
/// under the same lock and refuses once the member is `doing`), so the start
/// and the edit are ordered by the lock: an edit that lands first is the line
/// the member starts on, and one that comes after finds it started and is
/// refused. Read outside, an edit in between would be accepted and then
/// silently not be what ran.
///
/// It may when it is `todo` and unclaimed, or when it is a start of wsp's
/// own that never took (`doing`, unclaimed, [`STARTED_BY`] its last word, and
/// [`wedged`]). Either way **the cap is counted here, inside the lock**: two
/// advances run at a pass, each with its own reading of the group, and only a
/// count taken under the lock both of them write through can hold the cap.
fn take_member(
    store: &Store,
    t: &Task,
    list: &str,
    at: usize,
    members: &[String],
    cap: Option<usize>,
) -> Option<(&'static str, Policy)> {
    let id = t.id.clone();
    let took = store.locked(|| {
        let t = store.find_task(&id)?;
        let claims = store.claims();
        let ours = t.section("Log").and_then(|l| l.lines().last().map(|x| x.contains(STARTED_BY))).unwrap_or(false);
        let why = match t.status() {
            Status::Todo if !claims.contains_key(&id) => "new",
            Status::Doing if ours && wedged(&t, &claims) => "again: the last start never claimed it",
            _ => return None,
        };
        let going = members
            .iter()
            .filter(|m| **m != id)
            .filter_map(|m| store.find_task(m))
            .filter(|m| !Settlement::of(m).settled())
            .filter(|m| m.status() == Status::Doing || claims.contains_key(&m.id))
            .count();
        if cap.is_some_and(|c| going >= c) {
            return None;
        }
        let policy = store.worklist(list)?.groups().get(at - 1)?.policy_for(&id)?;
        let mut t = t;
        t.set_status(Status::Doing);
        t.log(&format!("{STARTED_BY} {list} group {at}"));
        t.touch();
        store.save_task(&t).ok()?;
        Some((why, policy))
    })?;
    store.git_commit(&format!("wsp: {list} group {at} starts {id}"));
    Some(took)
}

/// The newest verifier row under a member.
fn latest_verifier<'a>(tasks: &'a [Task], member: &str) -> Option<&'a Task> {
    tasks
        .iter()
        .filter(|t| t.parent.as_deref() == Some(member) && t.tags.iter().any(|g| g == VERIFY_TAG))
        .max_by(|a, b| a.created.cmp(&b.created).then_with(|| a.id.cmp(&b.id)))
}

/// Create the verifier row for a member, if one is owed, and hand back its id.
///
/// Owed when there is none, or when the last one blocked — found a problem —
/// and the member has been touched since: that is the member coming back
/// with a fix, and it is verified again by a fresh agent rather than by the
/// one that already made up its mind.
fn open_verifier(store: &Store, member: &Task, list: &str, at: usize, g: &Group) -> Option<String> {
    let made = store.locked(|| {
        let tasks = store.tasks();
        let owed = match latest_verifier(&tasks, &member.id) {
            None => true,
            Some(v) if v.status() == Status::Todo && wedged(v, &store.claims()) => {
                return restart(store, v);
            }
            Some(v) => v.status() == Status::Blocked && member.updated > v.updated,
        };
        if !owed {
            return None;
        }
        let id = store.alloc_task_id(member.project.as_deref()).ok()?;
        let mut t = Task::new(&format!("Verify {}: {}", member.id, util::truncate(&member.title, 60)), &id);
        t.project = member.project.clone();
        t.parent = Some(member.id.clone());
        t.tags = vec![VERIFY_TAG.to_string()];
        t.status_raw = Status::Todo.as_str().to_string();
        crate::model::set_section_in(&mut t.body, "Overview", &verifier_order(member, list, at, g, &id));
        store.save_task(&t).ok()?;
        Some(id)
    })?;
    store.git_commit(&format!("wsp: verify {} for {list} group {at}", member.id));
    Some(made)
}

/// A row wsp made whose agent never arrived, taken again: touched, so a
/// second advance in the next ten minutes leaves it to this one.
fn restart(store: &Store, t: &Task) -> Option<String> {
    let mut t = t.clone();
    t.log("wsp is starting an agent on this again: the last one never claimed it");
    t.touch();
    store.save_task(&t).ok()?;
    Some(t.id)
}

/// How a barrier row's title starts; the list's slug follows it.
const BARRIER_TITLE: &str = "Barrier: ";

fn barrier_title(list: &str, at: usize) -> String {
    format!("{BARRIER_TITLE}{list} group {at}")
}

/// Create the barrier row, if nobody has, and hand back its id.
fn open_barrier(store: &Store, w: &Worklist, at: usize, g: &Group) -> Option<String> {
    let title = barrier_title(&w.id, at);
    let project = g.members.iter().find_map(|m| store.find_task(m)).and_then(|t| t.project);
    let made = store.locked(|| {
        let tasks = store.tasks();
        if let Some(b) = tasks.iter().find(|t| t.title == title && t.tags.iter().any(|g| g == BARRIER_TAG)) {
            return match b.status() == Status::Todo && wedged(b, &store.claims()) {
                true => restart(store, b),
                false => None,
            };
        }
        let id = store.alloc_task_id(project.as_deref()).ok()?;
        let mut t = Task::new(&title, &id);
        t.project = project.clone();
        t.tags = vec![BARRIER_TAG.to_string()];
        t.status_raw = Status::Todo.as_str().to_string();
        crate::model::set_section_in(&mut t.body, "Overview", &barrier_order(w, at, g, &id));
        store.save_task(&t).ok()?;
        Some(id)
    })?;
    store.git_commit(&format!("wsp: {title}"));
    Some(made)
}

/// A verifier's work order, which is its row's overview: the brief every
/// spawned agent reads already carries it, so no kind needs a second channel.
fn verifier_order(member: &Task, list: &str, at: usize, g: &Group, me: &str) -> String {
    let stop = match g.stop.trim() {
        "" => String::new(),
        s => format!("\n\nThe barrier after this group asks: {s}"),
    };
    format!(
        "Verify **{m}** ({title}), a member of group {at} of the `{list}` worklist. wsp spawned \
         you when {m} reached review and landed; nobody else is going to read this work before \
         the barrier.\n\n\
         **You are read-only.** Do not edit, commit, land, rebase, or change {m}'s status. \
         Read what was asked and what was done with `wsp show {m}`, then check it against the \
         code on the trunk rather than against the report: the commits it landed, whether they \
         do what the overview asked, and the tests that cover them where those are cheap to run.\
         {stop}\n\n\
         **Finish in two commands.** Write your verdict as a note on {m}, three lines — what \
         you checked, what held, what did not — with `wsp note {m} --from FILE`. Then:\n\
         - it holds: `wsp review {me} -` with one line saying so;\n\
         - it does not: `wsp block {me} --from FILE`, saying what has to change.\n\n\
         wsp takes the next step from either; there is nobody to tell.",
        m = member.id,
        title = util::truncate(&member.title, 80),
    )
}

/// The barrier agent's work order.
fn barrier_order(w: &Worklist, at: usize, g: &Group, me: &str) -> String {
    let stop = match g.stop.trim() {
        "" => "There is no stop condition written at this barrier.".to_string(),
        s => format!("The stop condition, which your verdict answers:\n\n> {s}"),
    };
    format!(
        "Check the barrier after group {at} of the `{list}` worklist. Every member has landed \
         and each has a verifier's verdict, as a note on the member: {members}.\n\n\
         {stop}\n\n\
         **Do not edit, commit, land or rebase.** The per-member reviews are done; what is \
         left is what only the group as a whole can show. `wsp worklist next {list}` gives \
         what the group landed and which members touched one file. Read the verdicts, check \
         the group together on the trunk — it builds, its suites pass together, no two members \
         undid each other — and whether the next group has what it depends on.\n\n\
         **Then decide, and wsp does the rest.** Write the verdict in under 300 words to a \
         file, then:\n\
         - it passes: `wsp worklist go {list} --from FILE`. wsp starts the next group and \
         rotates the governor;\n\
         - it does not: `wsp worklist hold {list} --from FILE`, saying what has to happen. \
         The governor is told.\n\n\
         Either way, finish with `wsp review {me} -` and one line.",
        list = w.id,
        members = g.members.join(" "),
    )
}

/// What a failed start puts back.
#[derive(Clone, Copy)]
enum Undo {
    /// A member wsp moved to `doing`: back to `todo`, so the next advance —
    /// or a governor's own `wsp spawn` — finds it as it was.
    Member,
    /// A verifier or barrier row: left at `todo`, which the next advance
    /// takes again once it is [`wedged`].
    Row,
}

/// Start an agent on a row, and wait for `wsp spawn` to say how it went.
/// A failure is written on the row and put back per [`Undo`]; the caller
/// tells the seat. Never silent: a run that cannot start its next agent is
/// a run that has stopped.
fn spawn(store: &Store, id: &str, policy: &Policy, undo: Undo) -> bool {
    let failed = start(id, policy);
    let Some(why) = failed else { return true };
    stamp(&format!("FAILED spawn {id}: {why}"));
    let saved = store.locked(|| {
        let Some(mut t) = store.find_task(id) else { return false };
        t.log(&format!("wsp could not spawn an agent on this: {}", util::truncate(&why, 300)));
        if matches!(undo, Undo::Member) && t.status() == Status::Doing {
            t.set_status(Status::Todo);
        }
        t.touch();
        store.save_task(&t).is_ok()
    });
    if saved {
        store.git_commit(&format!("wsp: spawn failed on {id}"));
    }
    false
}

/// `wsp spawn <id> --agent …`, and its refusal if it refused.
fn start(id: &str, policy: &Policy) -> Option<String> {
    // A test's binary is the harness: record what would have started instead.
    if cfg!(test) {
        #[cfg(test)]
        return tests::SPAWNED.with(|s| {
            s.borrow_mut().push((id.to_string(), policy.kind.clone()));
            tests::FAIL.with(|f| f.borrow().clone())
        });
    }
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => return Some(e.to_string()),
    };
    let mut argv: Vec<String> = vec!["spawn".into(), id.into(), "--agent".into()];
    argv.extend(policy.spawn_flags());
    stamp(&format!("spawn {}", argv[1..].join(" ")));
    match Command::new(exe).args(&argv).stdin(Stdio::null()).output() {
        Ok(o) if o.status.success() => None,
        Ok(o) => Some(String::from_utf8_lossy(&o.stderr).trim().to_string()),
        Err(e) => Some(e.to_string()),
    }
}

/// Tell the seat a start failed, so a stopped run is somebody's to see.
fn failed(store: &Store, w: &Worklist, id: &str) {
    tell(store, w, &format!(
        "wsp could not start an agent on {id} in the {} run, so the run is waiting there. \
         `wsp show {id}` has the reason; `wsp worklist advance {}` tries again.",
        w.id, w.id
    ));
}

/// End the agents a passed group no longer needs: its members, their
/// verifiers, and the check on the barrier *before* it.
///
/// Not this barrier's own check. Its agent is the one running the `go` that
/// got us here, and ending it now would cut the turn in which it reviews its
/// own row; it is ended at the next pass instead, when it has long been idle.
/// The governor used to do all of this by hand, and a run wsp starts has to be
/// a run wsp tidies, or every group leaves its agents standing.
fn end_behind(store: &Store, w: &Worklist, passed: usize) {
    let groups = w.groups();
    let Some(g) = groups.get(passed - 1) else { return };
    let tasks = store.tasks();
    let mut ids: Vec<String> = g.members.clone();
    ids.extend(
        tasks
            .iter()
            .filter(|t| t.tags.iter().any(|x| x == VERIFY_TAG))
            .filter(|t| t.parent.as_ref().is_some_and(|p| g.members.contains(p)))
            .map(|t| t.id.clone()),
    );
    if passed > 1 {
        let before = barrier_title(&w.id, passed - 1);
        ids.extend(tasks.iter().filter(|t| t.title == before).map(|t| t.id.clone()));
    }
    let claims = store.claims();
    for id in ids.iter().filter(|id| claims.contains_key(*id)) {
        despawn(id);
    }
}

fn despawn(id: &str) {
    if cfg!(test) {
        #[cfg(test)]
        tests::ENDED.with(|s| s.borrow_mut().push(id.to_string()));
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let out = Command::new(exe).args(["despawn", id]).stdin(Stdio::null()).output();
    let said = match out {
        Ok(o) if o.status.success() => "ended".to_string(),
        Ok(o) => format!("not ended: {}", String::from_utf8_lossy(&o.stderr).trim()),
        Err(e) => format!("not ended: {e}"),
    };
    stamp(&format!("despawn {id}: {said}"));
}

/// The seat that answers for a run: the list's own, when somebody holds it,
/// and otherwise the seat routing reaches for its first member.
fn governing_scope(store: &Store, w: &Worklist) -> Option<String> {
    let governors = store.governors();
    if crate::cmd_govern::seat_of_scope(&w.id, &governors).is_some() {
        return Some(w.id.clone());
    }
    let index = crate::resolve::Index::new(store.projects());
    let first = w.groups().iter().flat_map(|g| g.members.clone()).find_map(|m| store.find_task(&m))?;
    let p = first.project?;
    std::iter::once(p.clone())
        .chain(index.ancestors(&p))
        .find(|s| crate::cmd_govern::seat_of_scope(s, &governors).is_some())
}

/// Tell whoever governs this run. With nobody seated, the sentence is raised
/// as a hand on the run's first member instead, where a person's panel draws
/// it — Ed, 2026-09-30: "assuming a governor is in place".
fn tell(store: &Store, w: &Worklist, text: &str) {
    if cfg!(test) {
        #[cfg(test)]
        tests::TOLD.with(|s| s.borrow_mut().push(text.to_string()));
        return;
    }
    stamp(&format!("tell {}: {}", w.id, util::truncate(text, 120)));
    if let Some(scope) = governing_scope(store, w) {
        let args = Args::synth("govern", &[scope.as_str()], &[("tell", text)]);
        if crate::cmd_govern::govern(store, &args) == 0 {
            return;
        }
    }
    // On a member of the group the run stands at, where a person looking at
    // the run is looking — not group 1's, long since finished.
    let w = store.worklist(&w.id).unwrap_or_else(|| w.clone());
    let groups = w.groups();
    let here = groups.iter().find(|g| g.verdict.trim().is_empty()).or_else(|| groups.last());
    let Some(first) = here.and_then(|g| g.members.first()).cloned() else { return };
    let args = Args::synth("flag", &[first.as_str(), text], &[]);
    let _ = crate::cmd_agent::flag(store, &args);
}

/// A pass hands the governing seat to a fresh successor, on wsp's own
/// initiative: `cmd_spawn::rotate_on_behalf` carries the argument.
fn rotate(store: &Store, w: &Worklist) {
    if cfg!(test) {
        #[cfg(test)]
        tests::ROTATED.with(|s| s.borrow_mut().push(w.id.clone()));
        return;
    }
    let Some(scope) = governing_scope(store, w) else { return };
    let code = crate::cmd_spawn::rotate_on_behalf(store, &scope);
    stamp(&format!("rotate {scope}: exit {code}"));
}

/// One dated line in `cycle.log`, which is this process's stdout.
fn stamp(line: &str) {
    if cfg!(test) {
        return;
    }
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{} {line}", util::now_iso());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    thread_local! {
        pub(super) static SPAWNED: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
        pub(super) static ENDED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
        pub(super) static TOLD: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
        pub(super) static ROTATED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
        /// Set, and every start in this thread fails with it.
        pub(super) static FAIL: RefCell<Option<String>> = const { RefCell::new(None) };
    }

    fn spawned() -> Vec<(String, String)> {
        SPAWNED.with(|s| s.borrow_mut().drain(..).collect())
    }

    fn scratch(tag: &str) -> (util::Isolated, Store) {
        let env = util::isolated(&format!("cycle-{tag}"));
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        (env, store)
    }

    fn task(store: &Store, id: &str, status: Status) {
        let mut t = Task::new(id, id);
        t.status_raw = status.as_str().into();
        store.save_task(&t).unwrap();
    }

    fn set(store: &Store, id: &str, status: Status) {
        let mut t = store.find_task(id).unwrap();
        t.set_status(status);
        t.touch();
        store.save_task(&t).unwrap();
    }

    fn list(store: &Store, groups: &[(&[&str], &str)]) -> Worklist {
        let mut w = Worklist::new("run", "run");
        w.set_status(WorklistStatus::Running);
        w.set_groups(
            &groups
                .iter()
                .map(|(m, a)| Group {
                    members: m.iter().map(|x| x.to_string()).collect(),
                    agent: a.to_string(),
                    ..Group::default()
                })
                .collect::<Vec<_>>(),
        );
        store.save_worklist(&w).unwrap();
        w
    }

    fn tagged(store: &Store, tag: &str) -> Vec<Task> {
        store.tasks().into_iter().filter(|t| t.tags.iter().any(|g| g == tag)).collect()
    }

    /// The whole chain, one step at a time, and each step taken once however
    /// often it is asked: the run advances off the store, not off a memory of
    /// what was done.
    #[test]
    fn a_group_with_a_policy_is_started_verified_and_brought_to_its_barrier_by_wsp() {
        let (_env, store) = scratch("chain");
        task(&store, "m-1", Status::Todo);
        task(&store, "m-2", Status::Todo);
        let w = list(&store, &[(&["m-1", "m-2"], "opencode some/model")]);

        step(&store, &w);
        assert_eq!(
            spawned(),
            vec![("m-1".into(), "opencode".into()), ("m-2".into(), "opencode".into())],
            "both members start, on the group's policy"
        );
        assert_eq!(store.find_task("m-1").unwrap().status(), Status::Doing, "the record that stops a second start");
        step(&store, &w);
        assert!(spawned().is_empty(), "a second advance starts nothing twice");

        set(&store, "m-1", Status::Review);
        step(&store, &w);
        let v = tagged(&store, VERIFY_TAG);
        assert_eq!(v.len(), 1, "one verifier, for the member that finished");
        assert_eq!(v[0].parent.as_deref(), Some("m-1"));
        assert!(v[0].section("Overview").unwrap().contains("read-only"), "its order is its overview");
        assert_eq!(spawned(), vec![(v[0].id.clone(), "opencode".into())], "on the same policy: the floor holds");
        step(&store, &w);
        assert!(spawned().is_empty() && tagged(&store, VERIFY_TAG).len() == 1, "and only once");

        set(&store, "m-2", Status::Review);
        step(&store, &w);
        let _ = spawned();
        assert!(tagged(&store, BARRIER_TAG).is_empty(), "no barrier check while a verdict is outstanding");

        for v in tagged(&store, VERIFY_TAG) {
            set(&store, &v.id, Status::Review);
        }
        step(&store, &w);
        let b = tagged(&store, BARRIER_TAG);
        assert_eq!(b.len(), 1, "every member verified: one barrier check");
        assert_eq!(b[0].title, "Barrier: run group 1");
        assert!(b[0].section("Overview").unwrap().contains("wsp worklist go run --from FILE"));
        assert_eq!(spawned().len(), 1);
        step(&store, &w);
        assert!(spawned().is_empty(), "the barrier row is the key; nothing starts twice");
    }

    /// `wsp-150`: one claude member beside an opencode one, in one group and
    /// behind one barrier. Each starts on its own kind, each verifier on the
    /// kind its member ran on, and the barrier check on the group's line.
    #[test]
    fn a_mixed_group_starts_and_verifies_each_member_on_its_own_kind() {
        let (_env, store) = scratch("mixed");
        task(&store, "m-1", Status::Todo);
        task(&store, "m-2", Status::Todo);
        let mut w = list(&store, &[(&["m-1", "m-2"], "opencode some/model-free")]);
        let mut g = w.groups();
        g[0].member_agents.insert("m-2".into(), "claude opus high".into());
        w.set_groups(&g);
        store.save_worklist(&w).unwrap();

        step(&store, &w);
        assert_eq!(
            spawned(),
            vec![("m-1".into(), "opencode".into()), ("m-2".into(), "claude".into())],
            "the member with a line of its own starts on it, and the other on the group's"
        );

        set(&store, "m-1", Status::Review);
        set(&store, "m-2", Status::Review);
        step(&store, &w);
        let by_parent: Vec<(String, String)> = spawned()
            .into_iter()
            .map(|(id, kind)| (store.find_task(&id).unwrap().parent.unwrap(), kind))
            .collect();
        assert_eq!(
            by_parent,
            vec![("m-1".into(), "opencode".into()), ("m-2".into(), "claude".into())],
            "a verifier runs on the kind its member's work ran on"
        );

        for v in tagged(&store, VERIFY_TAG) {
            set(&store, &v.id, Status::Review);
        }
        step(&store, &w);
        assert_eq!(
            spawned(),
            vec![(tagged(&store, BARRIER_TAG)[0].id.clone(), "opencode".into())],
            "the barrier reads the group as a whole, on the group's line"
        );
    }

    /// A verifier that found a problem blocks, the member comes back with a
    /// fix, and it is read again by a fresh agent.
    #[test]
    fn a_member_fixed_after_a_failed_verdict_gets_a_fresh_verifier() {
        let (_env, store) = scratch("again");
        task(&store, "m-1", Status::Review);
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w);
        let first = tagged(&store, VERIFY_TAG).remove(0);
        set(&store, &first.id, Status::Blocked);
        step(&store, &w);
        assert_eq!(tagged(&store, VERIFY_TAG).len(), 1, "blocked, and nothing new until the member moves");

        std::thread::sleep(std::time::Duration::from_millis(1100));
        set(&store, "m-1", Status::Review);
        step(&store, &w);
        assert_eq!(tagged(&store, VERIFY_TAG).len(), 2, "the fix is verified again");
        let _ = spawned();
    }

    /// Everything written before `wsp-134`, and a group somebody turned off,
    /// is left to its governor.
    #[test]
    fn a_group_with_no_policy_or_a_manual_one_is_left_alone() {
        for agent in ["", "manual"] {
            let (_env, store) = scratch(&format!("hand{}", agent.len()));
            task(&store, "m-1", Status::Todo);
            let w = list(&store, &[(&["m-1"], agent)]);
            step(&store, &w);
            assert!(spawned().is_empty(), "`{agent}` is run by hand");
            assert_eq!(store.find_task("m-1").unwrap().status(), Status::Todo);
        }
    }

    /// The cap is on the work, and a member already going counts against it.
    #[test]
    fn a_capped_group_starts_no_more_than_its_cap() {
        let (_env, store) = scratch("cap");
        for m in ["m-1", "m-2", "m-3"] {
            task(&store, m, Status::Todo);
        }
        let mut w = list(&store, &[(&["m-1", "m-2", "m-3"], "claude")]);
        let mut g = w.groups();
        g[0].cap = Some(2);
        w.set_groups(&g);
        store.save_worklist(&w).unwrap();
        step(&store, &w);
        assert_eq!(spawned().len(), 2);
        step(&store, &w);
        assert!(spawned().is_empty(), "two are going, and the cap is two");
    }

    /// A pass ends what the group needed and nothing the run still does: the
    /// check that is passing it is left to finish its own turn.
    #[test]
    fn a_pass_ends_the_groups_agents_and_the_last_barriers_check_but_not_its_own() {
        let (_env, store) = scratch("ends");
        task(&store, "m-1", Status::Review);
        task(&store, "m-2", Status::Review);
        let w = list(&store, &[(&["m-1"], "claude"), (&["m-2"], "claude")]);
        let mut v1 = Task::new("v", "v-1");
        v1.parent = Some("m-2".into());
        v1.tags = vec![VERIFY_TAG.into()];
        store.save_task(&v1).unwrap();
        for (id, title) in [("b-1", barrier_title("run", 1)), ("b-2", barrier_title("run", 2))] {
            let mut b = Task::new(&title, id);
            b.tags = vec![BARRIER_TAG.into()];
            store.save_task(&b).unwrap();
        }
        for id in ["m-1", "m-2", "v-1", "b-1", "b-2"] {
            store.set_claim(id, serde_json::json!({ "workspace": "w" }));
        }
        end_behind(&store, &w, 2);
        let ended = ENDED.with(|s| s.borrow_mut().drain(..).collect::<Vec<_>>());
        assert_eq!(ended, vec!["m-2", "v-1", "b-1"]);
    }

    fn drained<T>(k: &'static std::thread::LocalKey<RefCell<Vec<T>>>) -> Vec<T> {
        k.with(|s| s.borrow_mut().drain(..).collect())
    }

    fn go(store: &Store, passed: usize) {
        let args = Args::synth("worklist", &["advance", "run"], &[("event", "go"), ("passed", &passed.to_string())]);
        assert_eq!(advance(store, &args), 0);
    }

    /// wsp-135's FAIL: ux-revamp is a hand-run list, and its governor's own
    /// `go` must not have wsp end its members and rotate it on top of its own
    /// `--rotate`.
    #[test]
    fn a_go_on_a_hand_run_group_ends_nothing_rotates_nothing_and_tells_nobody() {
        for agent in ["", "manual"] {
            let (_env, store) = scratch(&format!("handgo{}", agent.len()));
            task(&store, "m-1", Status::Review);
            task(&store, "m-2", Status::Todo);
            let mut w = list(&store, &[(&["m-1"], agent), (&["m-2"], agent)]);
            let mut g = w.groups();
            g[0].verdict = "passed by hand".into();
            w.set_groups(&g);
            store.save_worklist(&w).unwrap();
            store.set_claim("m-1", serde_json::json!({ "workspace": "w" }));
            go(&store, 1);
            assert!(drained(&ENDED).is_empty(), "`{agent}`: nothing ended");
            assert!(drained(&ROTATED).is_empty(), "`{agent}`: nobody rotated");
            assert!(drained(&TOLD).is_empty(), "`{agent}`: the governor passed it and needs no telling");
            assert!(spawned().is_empty(), "`{agent}`: and the next hand-run group is not started");
        }
    }

    /// The other half: a group wsp runs is tidied, the seat moves on, and the
    /// next group starts — and on the last pass, nobody is rotated onto a run
    /// with nothing left in it.
    #[test]
    fn a_go_on_a_group_wsp_runs_ends_its_agents_rotates_and_starts_the_next() {
        let (_env, store) = scratch("autogo");
        task(&store, "m-1", Status::Review);
        task(&store, "m-2", Status::Todo);
        let mut w = list(&store, &[(&["m-1"], "claude"), (&["m-2"], "claude")]);
        let mut g = w.groups();
        g[0].verdict = "passed".into();
        w.set_groups(&g);
        store.save_worklist(&w).unwrap();
        store.set_claim("m-1", serde_json::json!({ "workspace": "w" }));
        go(&store, 1);
        assert_eq!(drained(&ENDED), vec!["m-1"]);
        assert_eq!(drained(&ROTATED), vec!["run"]);
        assert_eq!(drained(&TOLD).len(), 1);
        assert_eq!(spawned(), vec![("m-2".into(), "claude".into())]);

        let mut g = w.groups();
        g[1].verdict = "passed".into();
        w.set_groups(&g);
        store.save_worklist(&w).unwrap();
        go(&store, 2);
        assert!(drained(&ROTATED).is_empty(), "the last pass has nothing left to govern");
        let _ = (drained(&ENDED), drained(&TOLD));
    }

    /// The count is taken under the lock, so an advance holding a stale
    /// reading of the group cannot start past the cap.
    #[test]
    fn a_start_is_refused_under_the_lock_once_the_cap_is_full() {
        let (_env, store) = scratch("lockcap");
        task(&store, "m-1", Status::Doing);
        task(&store, "m-2", Status::Todo);
        list(&store, &[(&["m-1", "m-2"], "claude")]);
        let members = vec!["m-1".to_string(), "m-2".to_string()];
        let m2 = store.find_task("m-2").unwrap();
        let took = |cap| take_member(&store, &m2, "run", 1, &members, cap).map(|(why, _)| why);
        assert_eq!(took(Some(1)), None, "one going, cap one");
        assert_eq!(took(Some(2)), Some("new"));
        assert_eq!(took(Some(2)), None, "taken already");
    }

    /// A start that fails puts the member back and says so; one that never
    /// arrived is taken again once it has had its ten minutes.
    #[test]
    fn a_failed_or_lost_start_is_put_back_told_and_taken_again() {
        let (_env, store) = scratch("wedge");
        task(&store, "m-1", Status::Todo);
        let w = list(&store, &[(&["m-1"], "claude")]);
        FAIL.with(|f| *f.borrow_mut() = Some("no compound session".into()));
        step(&store, &w);
        FAIL.with(|f| *f.borrow_mut() = None);
        let _ = spawned();
        let t = store.find_task("m-1").unwrap();
        assert_eq!(t.status(), Status::Todo, "put back, so nothing is wedged at doing");
        assert!(t.section("Log").unwrap().contains("no compound session"), "and the reason is on the row");
        assert_eq!(drained(&TOLD).len(), 1, "and the seat is told");

        // Lost: `doing`, unclaimed, wsp's own start its last word, long ago.
        step(&store, &w);
        assert_eq!(spawned().len(), 1);
        step(&store, &w);
        assert!(spawned().is_empty(), "a start in flight is left alone");
        let mut t = store.find_task("m-1").unwrap();
        t.updated = "2026-01-01T00:00:00Z".into();
        store.save_task(&t).unwrap();
        step(&store, &w);
        assert_eq!(spawned().len(), 1, "a start that never claimed it is taken again");

        // And a verifier row whose agent never came.
        set(&store, "m-1", Status::Review);
        step(&store, &w);
        let _ = spawned();
        let mut v = tagged(&store, VERIFY_TAG).remove(0);
        step(&store, &w);
        assert!(spawned().is_empty());
        v.updated = "2026-01-01T00:00:00Z".into();
        store.save_task(&v).unwrap();
        step(&store, &w);
        assert_eq!(spawned(), vec![(v.id.clone(), "claude".into())], "the same row, started again");
        assert_eq!(tagged(&store, VERIFY_TAG).len(), 1, "and not a second one");
    }

    #[test]
    fn a_verifier_and_a_barrier_row_find_the_run_they_belong_to() {
        let (_env, store) = scratch("belong");
        task(&store, "m-1", Status::Review);
        let w = list(&store, &[(&["m-1"], "claude")]);
        step(&store, &w);
        let v = tagged(&store, VERIFY_TAG).remove(0);
        assert_eq!(list_of(&store, &v).map(|w| w.id), Some("run".into()));
        set(&store, &v.id, Status::Review);
        step(&store, &w);
        let b = tagged(&store, BARRIER_TAG).remove(0);
        assert_eq!(list_of(&store, &b).map(|w| w.id), Some("run".into()));
        let _ = spawned();
    }

    /// `wsp-142`: tokenhub-003 went to review with its commit on its branch,
    /// the advance its review started did nothing and said nothing, and the
    /// governor went looking for a cause in the claim. The verifier is keyed
    /// on the landing, so the seat is told the member is waiting on one.
    #[test]
    fn a_member_reviewed_without_landing_gets_no_verifier_and_the_seat_is_told_why() {
        let (env, store) = scratch("unlanded");
        let repo = env.path("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .env_remove("GIT_INDEX_FILE")
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        git(&["init", "--quiet", "-b", "master"]);
        git(&["commit", "--quiet", "--allow-empty", "-m", "first"]);
        git(&["checkout", "--quiet", "-b", "m-1"]);
        git(&["commit", "--quiet", "--allow-empty", "-m", "the member's work"]);
        git(&["checkout", "--quiet", "master"]);
        let mut p = crate::model::Project::new("p");
        p.roots = vec![repo.display().to_string()];
        store.save_project(&p).unwrap();
        let mut t = Task::new("m-1", "m-1");
        t.project = Some("p".into());
        t.status_raw = Status::Review.as_str().into();
        store.save_task(&t).unwrap();
        let w = list(&store, &[(&["m-1"], "claude")]);

        told_about_task(&store, "m-1", "review");
        step(&store, &w);
        assert!(spawned().is_empty() && tagged(&store, VERIFY_TAG).is_empty(), "no verifier on unlanded work");
        let told = drained(&TOLD);
        assert_eq!(told.len(), 1, "{told:?}");
        assert!(told[0].contains("m-1 is at review with 1 commit not on master"), "{}", told[0]);
        assert!(told[0].contains("`wsp land m-1`"), "{}", told[0]);

        git(&["merge", "--ff-only", "--quiet", "m-1"]);
        told_about_task(&store, "m-1", "review");
        step(&store, &w);
        assert_eq!(tagged(&store, VERIFY_TAG).len(), 1, "landed: the verifier starts");
        assert!(drained(&TOLD).is_empty(), "and a landed review is the next step, not news");
        let _ = spawned();
    }
}
