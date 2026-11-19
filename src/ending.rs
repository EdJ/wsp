//! A member whose verification holds has its agent ended. `wsp-193`.
//!
//! Ed, 2026-10-06: once a member's verification is complete and agreed, nothing
//! ended its agent. The member's pane and the verifier's stayed open until
//! somebody closed them by hand — every member of every group, every night.
//!
//! # The trigger, and why it is the verdict and not the row
//!
//! The member is at `review`, still holds its claim, and the newest pass in
//! its `## Verification` section holds, on the commit it last landed. A pass
//! still running, a blocks, a superseded pass, or a holds on a commit a later
//! landing moved past are all "not yet". `review` alone is not enough: a
//! member at `review` with no verdict yet is the agent a blocks verdict sends
//! work back to, and ending it would throw away the context the fix is made
//! from.
//!
//! The verdict is read through [`crate::verification::latest`], `wsp-188`'s
//! record, and nowhere else. The verifier's pane is `wsp-188`'s too:
//! [`crate::cycle::passes_finished`] ends every pass with its verdict in, once,
//! and runs before [`tick`] in the same repair pass — so this module ends the
//! member and only the member. What it lends the verifier's ending is the
//! confirmation: [`verifier_pids`] before the despawn and [`confirm_gone`]
//! after it, read through the same [`Seats`], so a pass is `ended` on the same
//! evidence a member is.
//!
//! # What is never ended, and how each is read
//!
//! - **A seat in a turn, on a prompt, or behind its own open `wsp ask`.** That
//!   is [`crate::waiting::reading`] — the one reading every other reader asks —
//!   plus the plain fact of a turn in flight, which `waiting` deliberately does
//!   not count as waiting. Checked again on the next tick, and said nowhere: it
//!   passes on its own.
//! - **A seat a person has typed to since the verdict.** A turn that began
//!   after the verdict is somebody using the agent, and the agent is theirs to
//!   end now. Read through [`Seats::turn_began_since`], and **a backend that
//!   cannot say holds too** — the cost of holding is a pane left open, the cost
//!   of guessing is a conversation a person was in.
//! - **A governor seat.** Whatever row it is bound to, a seat in
//!   `governors.json` is a post, and ending it is a reseat nobody asked for.
//!
//! The three that will not pass by themselves — typed, governor, and a seat
//! with no pid to confirm by — are said once on the row and in `cycle.log`,
//! so a pane left open has a sentence beside it saying why.
//!
//! # Ended means gone
//!
//! `wsp despawn` closes the seat and releases the claim, and an agent was
//! found alive at `ppid=1` after exactly that, still holding a conversation Ed
//! was in. So the agent's whole process tree is read **before** the despawn —
//! afterwards the parent links are gone — and the ending is recorded only once
//! none of it is running. A seat with no pid to read is not ended at all:
//! nothing could confirm it.
//!
//! The record is one line on the member's `## Log`, [`ENDED`] and the commit.
//! A despawn that fails, or leaves a pid running, is [`NOT_ENDED`] on the row,
//! a line in `cycle.log` and one sentence to the governor — and **not retried
//! on the next tick**, which is `wsp-176`'s shape: a failure tried once a
//! minute is a governor told once a minute about one pane.
//!
//! The row stays at `review`. Ending its agent is not `done`, which is Ed's.
//!
//! # Settled without a verdict, and the leg-run row's first seat (`wsp-336`)
//!
//! Ed, 2026-11-02: seventeen idle panes, sixteen of them members whose rows had
//! been at `review` for days. `wsp-193` only looked at members of a list that
//! was still open and only on a verdict, so a member of a **finished list**, of
//! a group run by hand, or of no list at all was nobody's to end.
//!
//! A member is also settled **with no pass at all** when it is landed (the
//! trunk has its commits), no verifier is owed — it is in no open list's group
//! that wsp runs — and [`SETTLE_GRACE_SECS`] have gone by since it landed and
//! reviewed, so a person reading the row has time to type to the agent first.
//! A member with a pass that does not hold keeps its pane whatever the list
//! says: a blocks verdict is answered from that context.
//!
//! The seat is the binding's, or — where a reaped binding left only the claim —
//! the seat of the agent the claim names.
//!
//! **A row run as legs has no member seat to end.** It has the seat that staged
//! it: the agent ran `wsp leg add`, `wsp release <row>` and stopped, and the
//! pane stayed, unbound. `wsp release` writes [`HANDED`] on the row before it
//! drops the binding, and [`original_seats`] ends that pane once every leg is
//! `done` or `skipped` and the row is landed — by the same rules and the same
//! confirmation as a member's.
//!
//! # An accepted exit ends its seat in the same call (`wsp-615`, E4)
//!
//! `wsp submit` (docs/exit-packet.md) is a seat's way out, and once its exit is
//! accepted the seat has nothing left to do: a **member's**, a **verifier's**
//! and a **barrier check's** alike. The submit records the seat ([`exited`]) and
//! from that call on it holds no place under the cap ([`exited_seats`], read by
//! [`crate::room`]); it is ended at its `Stop` by a helper the submit leaves
//! behind, and by the next tick where that helper is not there ([`end_exit`]).
//! The rules are this module's: never a governor seat, never one a turn began at
//! since the exit, its pids read before the despawn and confirmed gone after
//! it, the tree kept. Ended or left, it is said once on the row.
//!
//! **One ending per seat.** The record is taken before the verdict or the
//! `go`/`hold` moves anything, and from then the older endings of a verifier
//! ([`crate::cycle::passes_finished`]), of a check
//! ([`crate::cycle::checks_finished`], `go`'s `--end-check-seat` helper) and of
//! a member ([`tick`]'s own) step aside for it ([`exit_owns`]). The exit's
//! ending writes what theirs would: a verifier's goes through
//! [`crate::cycle::end_passes_keeping`], so the pass gets its end and spend; a
//! check's marks its barrier entry ended. A seat already gone or already
//! ended is said and forgotten, not read again every tick.
//!
//! **A member's seat is ended at submit, not held until the verdict** — Ed's
//! decision D1 (2026-11-12, docs/exit-packet.md §10). A blocks verdict starts
//! a rework seat from the verdict's `sent back:` reason, so the context the
//! held seat would have kept is not what a rework is made from; a seat held
//! until its verdict held a place under the cap for the whole verification.
//! The `handed`/`holds` endings above stay for rows that reach review without
//! `submit`. A `not held` exit with a decision ask ends its seat too (D3): the
//! answer reopens the row, with the answer as its `sent back:` reason, on a
//! fresh seat ([`crate::cmd_submit::answered`]).

use crate::cycle::Seats;
use crate::model::{Status, Task, Worklist, WorklistStatus};
use crate::place::State;
use crate::store::Store;
use crate::util;

/// The line on a member's row saying its agent was ended, and on which verdict.
pub(crate) const ENDED: &str = "ended: verification holds at";

/// The line on a member's row saying an ending was tried and failed.
const NOT_ENDED: &str = "wsp: could not end";

/// The line on a member's row saying its agent is being left running for a
/// reason that will not pass by itself.
const LEFT: &str = "wsp: verification holds and the agent is left running —";

/// The line on a member's row saying its verifier's pane is left running.
const VERIFIER_LEFT: &str = "wsp: the verifier is left running —";

/// The line `wsp release` writes on a staged row for the seat that handed it to
/// its legs, followed by the seat. Read by [`original_seats`].
pub(crate) const HANDED: &str = "wsp: seat";
const HANDED_TAIL: &str = "handed the row to its legs";

/// The line saying that seat was ended once the last leg had ended and the row
/// had landed.
const ORIGINAL_ENDED: &str = "wsp: original seat";
const ORIGINAL_ENDED_TAIL: &str = "ended — the last leg ended and the row is landed";
const ORIGINAL_NOT_ENDED: &str = "wsp: could not end original seat";

/// The line on a member's row saying its agent was ended with no verdict to
/// wait for: it is landed and no verifier is owed.
pub(crate) const SETTLED: &str = "ended: landed, no verifier owed, at";

/// The line on a member's row saying its agent was ended when its verifier
/// started in its tree (`wsp-430`): the seat does not stand while it reads.
pub(crate) const HANDED_OVER: &str = "ended: handed to its verifier at";

/// How long a landed member with no verifier owed is left before its agent is
/// ended: the person's chance to type to it first.
const SETTLE_GRACE_SECS: i64 = if cfg!(test) { 0 } else { 300 };

/// How long a despawned agent's processes are given to exit before the ending
/// is called failed. `compound`'s own `stop` already waits out `TERM` and sends
/// `KILL`, so this is the margin after that, not the whole grace.
const GONE_WITHIN_MS: u64 = if cfg!(test) { 0 } else { 5_000 };

/// A verification that holds and still stands — the trigger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Holds {
    /// The commit the verifier read, which is what the ending is recorded
    /// against: a later landing is a new verdict, and a new ending.
    pub(crate) commit: String,
    /// When the verdict was recorded. A turn since then is somebody's.
    pub(crate) at: i64,
    /// Whether a verdict stands behind this. `false` is a member that is landed
    /// with no verifier owed, settled by the grace instead of by a verdict.
    pub(crate) verdict: bool,
    /// A verifier's pass is open on the member's work: its seat is ended now and
    /// its **tree kept**, which the verifier reads in (`wsp-430`).
    pub(crate) handed: bool,
}

impl Holds {
    /// The row's record that this was ended, and what [`ended`] looks for.
    fn ended_line(&self) -> String {
        match (self.handed, self.verdict) {
            (true, _) => format!("{HANDED_OVER} {}", self.commit),
            (false, true) => format!("{ENDED} {}", self.commit),
            (false, false) => format!("{SETTLED} {}", self.commit),
        }
    }

    /// The reason, as a clause in a sentence.
    fn because(&self) -> String {
        match (self.handed, self.verdict) {
            (true, _) => format!("its verifier has the work at {}", self.commit),
            (false, true) => format!("verification holds at {}", self.commit),
            (false, false) => format!("it is landed at {} and no verifier is owed", self.commit),
        }
    }
}

/// The member's newest verification, if it holds and nothing has moved past it.
///
/// **The one place this module reads a verdict.** A pass at `holds` whose
/// `read` is not the member's latest landing was overtaken by one: that landing
/// has not been read, and it is a new verdict that ends the agent, if any does.
/// The time is the pass's `decided`; a pass written before `decided` existed
/// falls back to when it was opened, which only widens what counts as typed.
fn holds(tasks: &[Task], member: &Task) -> Option<Holds> {
    use crate::verification::{latest, same_commit, State as Verdict};
    let p = latest(tasks, &member.id)?;
    if p.state != Verdict::Holds {
        return None;
    }
    let now = crate::repair::landed(member);
    if let (Some(read), Some(now)) = (p.read.as_deref(), now.as_deref()) {
        if !same_commit(read, now) {
            return None;
        }
    }
    let commit = p.read.clone().or(now).unwrap_or_else(|| p.at.clone());
    Some(Holds { commit, at: util::epoch_of(p.decided.as_deref().unwrap_or(&p.at)), verdict: true, handed: false })
}

/// **The member whose verifier is reading** (`wsp-430`, compounding-122): at
/// review, with a pass open on the commit it is at. A pass opens only once the
/// row's mechanical check is clear, so this is "reviewed and green", and the
/// seat has nothing left to do: the verifier reads in the tree, which stays,
/// and a block starts a rework seat from the verdict
/// ([`crate::cycle`]'s `sent_back_unseated`). The time is the member's own
/// review, so a turn begun since is somebody's.
fn handed(tasks: &[Task], member: &Task) -> Option<Holds> {
    use crate::verification::{latest, same_commit, State as Verdict};
    let p = latest(tasks, &member.id)?;
    if p.state != Verdict::Running {
        return None;
    }
    if let (Some(read), Some(now)) = (p.read.as_deref(), crate::repair::commit_of(member).as_deref()) {
        if !same_commit(read, now) {
            return None;
        }
    }
    let at = last_stamp(member, "review:").unwrap_or_else(|| util::epoch_of(&p.at));
    let commit = format!("{} (pass {})", p.read.clone().unwrap_or_else(|| "no commit".into()), p.at);
    Some(Holds { commit, at, verdict: false, handed: true })
}

/// The member as settled: a verdict that holds, or — with no pass at all, no
/// verifier owed and the trunk holding its commits — the grace since it landed
/// and reviewed. **A pass that is not a holds is never settled**, whatever the
/// list says: a blocks verdict is answered from this agent's context.
fn settled(tasks: &[Task], member: &Task, owed: bool) -> Option<Holds> {
    if let Some(h) = holds(tasks, member) {
        return Some(h);
    }
    if let Some(h) = handed(tasks, member) {
        return Some(h);
    }
    if owed || crate::verification::latest(tasks, &member.id).is_some() {
        return None;
    }
    let commit = crate::repair::landed(member)?;
    let at = ["wsp: landed", "review:"].iter().filter_map(|n| last_stamp(member, n)).max()?;
    (util::epoch_secs() - at >= SETTLE_GRACE_SECS).then_some(Holds { commit, at, verdict: false, handed: false })
}

/// When the newest `## Log` line containing `needle` was written.
fn last_stamp(t: &Task, needle: &str) -> Option<i64> {
    let log = t.section("Log")?;
    let line = log.lines().filter(|l| l.contains(needle)).last()?;
    let stamp = line.trim_start_matches("- ").split_whitespace().next()?;
    stamp.contains('T').then(|| util::epoch_of(stamp))
}

/// Why an agent is left running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Left {
    /// Nobody can read the seat this pass.
    Unreadable,
    /// A turn in flight, or an agent still coming up.
    Turn(State),
    /// On a prompt, or behind its own open `wsp ask`.
    Waiting(crate::waiting::Wait),
    /// A turn began after the verdict.
    Typed,
    /// The backend cannot say whether a turn began after the verdict.
    Untold,
    /// The seat holds a governor post.
    Governor(String),
    /// Nothing names a process to confirm the ending by.
    NoPid,
}

impl Left {
    /// Whether this passes by itself, and is checked again silently next tick.
    fn passes(&self) -> bool {
        matches!(self, Left::Unreadable | Left::Turn(_) | Left::Waiting(_))
    }

    fn sentence(&self) -> String {
        match self {
            Left::Unreadable => "nobody can read its seat".into(),
            Left::Turn(s) => format!("its seat reads {}", s.as_str()),
            Left::Waiting(w) => w.sentence(),
            Left::Typed => "a turn began at its seat after the verdict, so somebody is using it and it is theirs to end".into(),
            Left::Untold => "its seat cannot say whether anybody has typed to it since the verdict".into(),
            Left::Governor(scope) => format!("its seat governs {scope}"),
            Left::NoPid => "nothing names its process, so an ending could not be confirmed".into(),
        }
    }
}

/// What the seat is asked, gathered so [`left`] can be decided without one.
pub(crate) struct Reading {
    pub(crate) state: Option<State>,
    pub(crate) wait: Option<crate::waiting::Wait>,
    pub(crate) governs: Option<String>,
    pub(crate) typed: Option<bool>,
    pub(crate) pids: Option<Vec<u32>>,
}

/// **The decision.** `None` is end it; `Some` is why not.
///
/// The governor first, because it is never ended whatever else is true. A seat
/// that reads `Gone` or `Empty` has no agent left in it to be using: there is
/// nobody to have typed and nothing to confirm, and ending it is releasing the
/// claim and closing the pane.
pub(crate) fn left(r: &Reading) -> Option<Left> {
    if let Some(scope) = &r.governs {
        return Some(Left::Governor(scope.clone()));
    }
    let Some(state) = r.state else { return Some(Left::Unreadable) };
    if let Some(w) = &r.wait {
        return Some(Left::Waiting(w.clone()));
    }
    match state {
        State::Gone | State::Empty => return None,
        State::Idle => {}
        other => return Some(Left::Turn(other)),
    }
    match r.typed {
        Some(true) => return Some(Left::Typed),
        None => return Some(Left::Untold),
        Some(false) => {}
    }
    match &r.pids {
        Some(p) if !p.is_empty() => None,
        _ => Some(Left::NoPid),
    }
}

/// One pass over every member at `review` that holds a seat, and then over the
/// first seats of rows run as legs.
///
/// **Every member, whatever its list is doing** (`wsp-336`): `wsp-193` read only
/// the members of a list still running, held or parked, and the seventeen idle
/// panes of 2026-11-02 were nobody's. Whether a verifier is still owed is read
/// off the **open** lists alone — their groups that wsp runs — and the verdict
/// off the member.
pub(crate) fn tick(store: &Store, seats: &dyn Seats) {
    exits(store, seats);
    // A seat its exit is still to end (a turn in flight, or left running by it)
    // is that ending's alone: the `--end-seat` helper may be ending it now.
    let owned = exit_owns(store);
    let lists = open_lists(store);
    let mut tasks = None;
    let mut asks = None;
    let mut seated = std::collections::BTreeSet::new();
    for id in seated_rows(store) {
        seated.insert(id.clone());
        let Some(m) = store.find_task(&id) else { continue };
        let Some(h) = due(store, &lists, &mut tasks, &m) else { continue };
        let Some(seat) = seat_of(store, &id) else {
            // Settled, and bound to no seat: nothing here can be asked or confirmed,
            // and `repair`'s gone-member pass is the one that says so.
            continue;
        };
        if owned.contains(&seat) {
            continue;
        }
        let w = list_of_member(&lists, &m.id);
        let asks = asks.get_or_insert_with(|| crate::waiting::Asks::read(store));
        if end_one(store, seats, w, &m.id, &seat, &h, asks) {
            record_ended(store, &m.id, &h);
        }
    }
    released_seats(store, seats, &lists, &seated, &mut tasks, &mut asks);
    original_seats(store, seats);
    // `wsp-453`: what a finished row's tree still has running goes with its
    // seat, or by the next tick where the seat went some other way. Not in a
    // test, which would signal the machine's own processes, unless the test
    // asked for it ([`tests::SCANS`]) over a tree of its own.
    if scans_finished() {
        stop_finished(store, seats);
    }
}

/// Whether the tick scans finished trees: always, but in a test only where it
/// asked to on its thread.
fn scans_finished() -> bool {
    #[cfg(test)]
    return tests::SCANS.with(std::cell::Cell::get);
    #[cfg(not(test))]
    true
}

/// [`crate::check_tree::left_in_finished`], asked of a fleet's seats.
fn left_in_finished_by(store: &Store, seats: &dyn Seats) -> Option<Vec<crate::check_tree::Left>> {
    // One `ps` for the whole scan, not one per seat (`wsp-480`); each scan takes
    // its own, so the look after a signal is a fresh one.
    crate::place_super::one_ps(|| crate::check_tree::left_in_finished(store, seats.census()?, &|seat| seats.pids(seat), &crate::legs::staged))
}

/// How long [`stop_finished`] gives a `TERM` before it looks again and sends
/// `KILL`. Short in a test, whose one process exits on `TERM` at once.
const TERM_GRACE_MS: u64 = if cfg!(test) { 100 } else { 1500 };

/// Stop what the finished rows left running (`wsp-453`): `TERM`, a moment,
/// then `KILL` for what is still there, and a `cycle.log` line for each tree.
/// Run by the tick, so a row that reaches review has nothing of its own
/// running by the next one. Returns the lines said.
///
/// **Under `WSP_HOST=manager` the scan is wip's too** (`wsp-034`): `wip` there
/// runs no scan of its own and draws [`finished_wip_lines_held`], so what this
/// scan found is left in `finished-wip.json` before anything is signalled, an
/// empty list where nothing was. Under `inproc` `wip` scans for itself and the
/// file is its alone, as it was.
pub(crate) fn stop_finished(store: &Store, seats: &dyn Seats) -> Vec<String> {
    let Some(found) = left_in_finished_by(store, seats) else { return Vec::new() };
    if crate::fold::under_manager() {
        hold_wip_lines(store, &wip_lines_of(&found));
    }
    let mut said = say_spared(store, &found);
    let found: Vec<_> = found.into_iter().filter(|l| !l.pids.is_empty()).collect();
    if found.is_empty() {
        return said;
    }
    let all = |l: &[crate::check_tree::Left]| l.iter().flat_map(|t| t.pids.clone()).collect::<Vec<u32>>();
    crate::check_tree::signal("TERM", &all(&found));
    std::thread::sleep(std::time::Duration::from_millis(TERM_GRACE_MS));
    let still = left_in_finished_by(store, seats).unwrap_or_default();
    crate::check_tree::signal("KILL", &all(&still));
    for t in &found {
        let kill: Vec<u32> = still.iter().filter(|s| s.tree == t.tree).flat_map(|s| s.pids.clone()).collect();
        let path = util::contract(std::path::Path::new(&t.tree));
        let list = |p: &[u32]| p.iter().map(u32::to_string).collect::<Vec<_>>().join(" ");
        let mut line = format!("stopped in finished row {} tree {path}: TERM {}", t.row, list(&t.pids));
        if !kill.is_empty() {
            line.push_str(&format!(", KILL {}", list(&kill)));
        }
        crate::store::log_line(store, &line);
        said.push(line);
    }
    said
}

/// What the sweep let be, said in `cycle.log` once per pid for as long as this
/// process lives (`wsp-478`). Returns the lines said.
fn say_spared(store: &Store, found: &[crate::check_tree::Left]) -> Vec<String> {
    static SAID: std::sync::Mutex<std::collections::BTreeSet<String>> = std::sync::Mutex::new(std::collections::BTreeSet::new());
    let mut said = Vec::new();
    let mut seen = SAID.lock().unwrap_or_else(|e| e.into_inner());
    for l in found {
        for (pid, why) in &l.spared {
            if seen.insert(crate::check_tree::spared_key(l, *pid, why)) {
                let line = crate::check_tree::spared_line(l, *pid, why);
                crate::store::log_line(store, &line);
                said.push(line);
            }
        }
    }
    said
}

/// How long the lines `wsp wip` draws for finished trees are good for.
const WIP_LINES_FOR_SECS: i64 = 30;

/// The lines a finished-tree scan left in `finished-wip.json`, as the last
/// scan left them, and nothing when they are stale: no scan here. `wip` under
/// `WSP_HOST=manager` runs no `ps` (`docs/agent-manager.md` 4.3) and draws
/// these; the daemon's tick scans and writes them ([`stop_finished`]).
pub(crate) fn finished_wip_lines_held(store: &Store) -> Vec<String> {
    let now = util::epoch_secs();
    std::fs::read_to_string(store.state_file("finished-wip.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .filter(|v| now - v["at"].as_i64().unwrap_or(0) < WIP_LINES_FOR_SECS)
        .and_then(|v| v["lines"].as_array().map(|l| l.iter().filter_map(|l| l.as_str().map(str::to_string)).collect()))
        .unwrap_or_default()
}

/// The lines `wsp wip` draws for the trees in `found` that still have
/// processes the tree started.
fn wip_lines_of(found: &[crate::check_tree::Left]) -> Vec<String> {
    found.iter().filter(|l| !l.pids.is_empty()).map(|l| crate::check_tree::left_line(l, crate::check_tree::rss_kb(&l.pids))).collect()
}

/// Leave `lines` in `finished-wip.json` for the next [`WIP_LINES_FOR_SECS`],
/// written whole and renamed into place.
fn hold_wip_lines(store: &Store, lines: &[String]) {
    let file = store.state_file("finished-wip.json");
    let tmp = file.with_extension(format!("json.{}", std::process::id()));
    if std::fs::write(&tmp, serde_json::json!({ "at": util::epoch_secs(), "lines": lines }).to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, &file);
    }
}

/// The lines `wsp wip` draws for finished trees that still have processes.
///
/// **One scan for every caller in the half minute** (`wsp-480`): it is an
/// `lsof` over the whole machine and four `ps` runs, 0.35 s of CPU on a
/// thermally limited one, and compound's host asks `wsp wip` every three
/// seconds. The scan leaves its lines in `finished-wip.json`; a `wip` within
/// [`WIP_LINES_FOR_SECS`] of it draws those. The tick, which stops what the
/// lines name, scans for itself and never reads this; under
/// `WSP_HOST=manager` it writes it ([`stop_finished`]).
pub(crate) fn finished_wip_lines(store: &Store) -> Vec<String> {
    let file = store.state_file("finished-wip.json");
    let now = util::epoch_secs();
    if !cfg!(test) {
        let held = std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok());
        if let Some(v) = held.filter(|v| now - v["at"].as_i64().unwrap_or(0) < WIP_LINES_FOR_SECS) {
            if let Some(lines) = v["lines"].as_array() {
                return lines.iter().filter_map(|l| l.as_str().map(str::to_string)).collect();
            }
        }
    }
    let Some(found) = left_in_finished_by(store, &crate::cycle::Fleet) else { return Vec::new() };
    let lines = wip_lines_of(&found);
    if !cfg!(test) {
        hold_wip_lines(store, &lines);
    }
    lines
}

fn open_lists(store: &Store) -> Vec<Worklist> {
    store
        .worklists()
        .into_iter()
        .filter(|w| matches!(w.status(), WorklistStatus::Running | WorklistStatus::Held | WorklistStatus::Parked))
        .collect()
}

fn list_of_member<'a>(lists: &'a [Worklist], member: &str) -> Option<&'a Worklist> {
    lists.iter().find(|w| w.groups().iter().any(|g| g.members.contains(&member.to_string())))
}

/// The member's settlement when it is one to end an agent for, and not already
/// ended or failed: at `review`, no verifier or barrier row, not a staged row.
fn due(store: &Store, lists: &[Worklist], tasks: &mut Option<Vec<Task>>, m: &Task) -> Option<Holds> {
    if m.status() != Status::Review || m.tags.iter().any(|g| g == crate::cycle::VERIFY_TAG || g == crate::cycle::BARRIER_TAG) {
        return None;
    }
    // A row run as legs or pieces: its first seat is [`original_seats`]', and
    // its parts' seats are ended by the tick, each from its entry.
    if crate::inner::run_inside(m) {
        return None;
    }
    let owed = list_of_member(lists, &m.id)
        .is_some_and(|w| w.groups().iter().any(|g| g.members.contains(&m.id) && g.policy_for(&m.id).is_some()));
    let tasks = tasks.get_or_insert_with(|| store.tasks());
    let h = settled(tasks, m, owed)?;
    // A seat that may have started servers of its own keeps its pane until the
    // project's `at_stop:` has run, which is when its tree goes.
    if h.handed && crate::at_stop::declared(store, m) {
        return None;
    }
    (!ended(m, &h) && !failed(m, &h.commit)).then_some(h)
}

/// **The members whose seat nobody holds a record of.** A pane released at
/// review has neither claim nor binding, so [`seated_rows`] cannot see it and
/// it stood open, idle, for as long as the row waited for `wsp done` — five
/// panes and 1.9 GB on 2026-11-02, `wsp-340`.
///
/// Found from the other side: each pane the backends hold whose label opens
/// with a task's id, kept only where that task's last claim was made from this
/// very pane and let go of. A pane another row has claimed or is bound to is
/// that row's. Ended by [`end_one`], with all its rules.
fn released_seats(
    store: &Store,
    seats: &dyn Seats,
    lists: &[Worklist],
    seated: &std::collections::BTreeSet<String>,
    tasks: &mut Option<Vec<Task>>,
    asks: &mut Option<crate::waiting::Asks>,
) {
    let Some(census) = seats.census() else { return };
    let held = store.held_seats();
    for (seat, label) in census {
        let id = label.split(" · ").next().unwrap_or_default().trim_start_matches('(').to_string();
        if held.contains(&seat) || seated.contains(&id) {
            continue;
        }
        let Some(m) = store.find_task(&id) else { continue };
        if !released_from(&m, &seat) {
            continue;
        }
        let Some(h) = due(store, lists, tasks, &m) else { continue };
        let w = list_of_member(lists, &m.id);
        let asks = asks.get_or_insert_with(|| crate::waiting::Asks::read(store));
        if end_one(store, seats, w, &m.id, &seat, &h, asks) {
            record_ended(store, &m.id, &h);
        }
    }
}

/// Whether the member's last claim was made from `seat` and has been let go.
fn released_from(m: &Task, seat: &str) -> bool {
    crate::cmd_attempts::attempts_of(m)
        .iter()
        .rfind(|a| a.leg.is_none())
        .is_some_and(|a| a.pane == seat && !a.until.is_empty())
}

/// Every row a seat is held for: by a claim or by a binding that is not a leg's,
/// each once.
fn seated_rows(store: &Store) -> Vec<String> {
    let mut ids: std::collections::BTreeSet<String> = store.claims().into_keys().collect();
    ids.extend(
        store
            .bindings()
            .values()
            .filter(|b| b.get("leg").is_none())
            .filter_map(|b| b.get("task_id").and_then(|t| t.as_str()).map(str::to_string)),
    );
    ids.into_iter().collect()
}

/// The member's seat: its binding, or — where a reaped binding left only the
/// claim — the seat of the agent the claim names.
fn seat_of(store: &Store, member: &str) -> Option<String> {
    let bindings = store.bindings();
    let bound = bindings
        .iter()
        .find(|(_, b)| b.get("task_id").and_then(|t| t.as_str()) == Some(member) && b.get("leg").is_none())
        .map(|(pane, _)| pane.clone());
    bound.or_else(|| {
        let agent = store.claims().get(member)?.get("agent_id")?.as_str()?.to_string();
        let seat = store.agents_held().get(&agent)?.get("seat")?.as_str()?.to_string();
        (!seat.is_empty()).then_some(seat)
    })
}

/// Whether this settlement's ending is already recorded on the member.
fn ended(m: &Task, h: &Holds) -> bool {
    let line = h.ended_line();
    m.section("Log").is_some_and(|l| l.lines().any(|x| x.ends_with(&line)))
}

/// Whether ending the member for this verdict has already been tried and
/// failed — the record that makes a failure told once and not retried.
fn failed(m: &Task, commit: &str) -> bool {
    let (who, at) = (format!("{NOT_ENDED} {} (", m.id), format!(" at {commit} — "));
    m.section("Log").is_some_and(|l| l.lines().any(|x| x.contains(&who) && x.contains(&at)))
}

/// End the member's agent if nothing says leave it. `true` when it was ended
/// and confirmed gone.
fn end_one(store: &Store, seats: &dyn Seats, w: Option<&Worklist>, member: &str, seat: &str, h: &Holds, asks: &crate::waiting::Asks) -> bool {
    let state = seats.state(seat);
    // **A shell of its own does not hold a seat whose row is verified or
    // settled** (`wsp-428`). Its work is in and landed, so the dev server or
    // watch it left running has nothing to wake the agent for: it stood 55
    // minutes after its verdict and held its group's barrier. Ending is the
    // group signalled, and that kills the shell, which is the point. A question
    // it asked, a prompt, a turn and a person typing still hold it below.
    let shells = Default::default();
    let reading = Reading {
        state,
        wait: crate::waiting::reading(state, asks, &shells, seat, member),
        governs: crate::place::governs(&store.governors(), &crate::place::Seat::new(seat)),
        typed: seats.turn_began_since(seat, h.at),
        pids: seats.pids(seat),
    };
    let by = w.map_or(member, |w| w.id.as_str());
    if let Some(why) = left(&reading) {
        if !why.passes() {
            say_left(store, w, member, seat, &why, h);
        }
        return false;
    }
    let pids = reading.pids.unwrap_or_default();
    let claimed = store.claims().contains_key(member);
    // A seat handed to its verifier leaves its tree to it.
    let keep = h.handed;
    let ending = match claimed {
        true => crate::cycle::despawned_keeping(store, member, keep),
        false => crate::cycle::despawn_seat_keeping(seat, keep),
    };
    match ending.and_then(|()| confirm_gone(seats, &pids)) {
        Ok(()) => {
            crate::store::log_line(store, &format!("{by} {member} ({seat}) ended — {}, and no pid of it is running", h.because()));
            true
        }
        Err(why) => {
            not_ended(store, w, member, seat, h, &why);
            false
        }
    }
}

/// The first seat of every row run as legs or pieces, ended once the row has no
/// part left to run and has landed.
///
/// **Its own record, because the seat has no other.** `wsp release <row>` drops
/// the binding and the claim, so the pane stays open with nothing naming it but
/// the line [`handed_to_legs`] wrote. Ended by the member's own rules — never in
/// a turn, behind a prompt or an ask, typed to since it handed the row over, or
/// holding a governor post — and confirmed by pid, once, a failure not retried.
fn original_seats(store: &Store, seats: &dyn Seats) {
    let mut asks = None;
    let bindings = store.bindings();
    for row in store.tasks_shared().iter().filter(|t| crate::inner::run_inside(t)) {
        if !matches!(row.status(), Status::Review | Status::Done) || crate::repair::landed(row).is_none() {
            continue;
        }
        let legs = crate::legs::legs(row);
        if legs.iter().any(|l| !matches!(l.state, crate::legs::State::Done | crate::legs::State::Skipped)) {
            continue;
        }
        if !crate::pieces::record(row).pieces.iter().all(crate::pieces::settled) {
            continue;
        }
        for (seat, at) in handed_seats(row) {
            if original_recorded(row, &seat) || bindings.contains_key(&seat) {
                continue;
            }
            let state = seats.state(&seat);
            // As for a member: its work has landed, a shell of its own holds nothing (`wsp-428`).
            let shells = Default::default();
            let asks = asks.get_or_insert_with(|| crate::waiting::Asks::read(store));
            let reading = Reading {
                state,
                wait: crate::waiting::reading(state, asks, &shells, &seat, &row.id),
                governs: crate::place::governs(&store.governors(), &crate::place::Seat::new(seat.as_str())),
                typed: seats.turn_began_since(&seat, at),
                pids: seats.pids(&seat),
            };
            if let Some(why) = left(&reading) {
                if !why.passes() {
                    let line = format!("wsp: original seat {seat} is left running: {}", why.sentence());
                    if once(store, &row.id, &line) {
                        crate::store::log_line(store, &format!("{}: {}", row.id, line.trim_start_matches("wsp: ")));
                    }
                }
                continue;
            }
            let pids = reading.pids.unwrap_or_default();
            let outcome = crate::cycle::despawn_seat(&seat).and_then(|()| confirm_gone(seats, &pids));
            let line = match &outcome {
                Ok(()) => format!("{ORIGINAL_ENDED} {seat} {ORIGINAL_ENDED_TAIL}"),
                Err(why) => format!("{ORIGINAL_NOT_ENDED} {seat} — {}", util::truncate(&why_not_ended(why), 200)),
            };
            if once(store, &row.id, &line) {
                crate::store::log_line(store, &format!("{}: {}", row.id, line.trim_start_matches("wsp: ")));
            }
        }
    }
}

/// Whether an ending of that original seat, successful or failed, is on the row.
fn original_recorded(row: &Task, seat: &str) -> bool {
    let (ended, failed) = (format!("{ORIGINAL_ENDED} {seat} "), format!("{ORIGINAL_NOT_ENDED} {seat} "));
    row.section("Log").is_some_and(|l| l.lines().any(|x| x.contains(&ended) || x.contains(&failed)))
}

/// The seats that handed this row to its legs, each with when.
fn handed_seats(row: &Task) -> Vec<(String, i64)> {
    let Some(log) = row.section("Log") else { return Vec::new() };
    log.lines()
        .filter_map(|l| {
            let rest = l.trim_start_matches("- ");
            let (stamp, said) = rest.split_once(' ')?;
            let seat = said.strip_prefix(HANDED)?.trim().strip_suffix(HANDED_TAIL)?.trim();
            (!seat.is_empty()).then(|| (seat.to_string(), util::epoch_of(stamp)))
        })
        .collect()
}

/// Written by `wsp release` before it drops the binding of a seat that stages a
/// row: the one record that names the seat afterwards.
pub(crate) fn handed_to_legs(store: &Store, row: &str, seat: &str, binding: &serde_json::Value) {
    if binding.get("leg").is_some() {
        return;
    }
    let Some(t) = store.find_task(row) else { return };
    if t.status() != Status::Doing || !crate::inner::run_inside(&t) {
        return;
    }
    once(store, row, &format!("{HANDED} {seat} {HANDED_TAIL}"));
}

/// `Ok` once none of `pids` is running, waiting [`GONE_WITHIN_MS`] at most.
/// The member's check, and the verifier's in [`crate::cycle::end_passes`].
pub(crate) fn confirm_gone(seats: &dyn Seats, pids: &[u32]) -> Result<(), String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(GONE_WITHIN_MS);
    loop {
        let running = seats.running(pids);
        if running.is_empty() {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            let list = running.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(" ");
            return Err(format!("despawned, and pid {list} is still running"));
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// A verifier's seat, read before [`crate::cycle::end_passes`] closes it: the
/// pids its ending is confirmed by, or why it is left.
///
/// **The member's rule, less what does not apply to a verifier.** A seat that
/// reads `Gone` or `Empty` has nothing in it to confirm; one nobody can read is
/// left silently and asked again next tick; one with no pid is [`Left::NoPid`],
/// left and said, because closing a pane that nothing confirms is how the
/// `ppid=1` survivor became invisible. No turn, prompt or typed check: a
/// verifier is ended on its own verdict, or because a later landing made its
/// reading worthless, and both stand whatever its screen shows.
pub(crate) fn verifier_pids(seats: &dyn Seats, seat: &str) -> Result<Vec<u32>, Left> {
    if let Some(pids) = seats.pids(seat).filter(|p| !p.is_empty()) {
        return Ok(pids);
    }
    match seats.state(seat) {
        Some(State::Gone | State::Empty) => Ok(Vec::new()),
        None => Err(Left::Unreadable),
        Some(_) => Err(Left::NoPid),
    }
}

/// A verifier left running for a reason that will not pass: said once per
/// pass, on the member's row, in `cycle.log`, and to the run's governor.
pub(crate) fn say_verifier_left(store: &Store, w: Option<&Worklist>, member: &str, seat: &str, opened: &str, why: &Left) {
    if why.passes() {
        return;
    }
    let line = format!("{VERIFIER_LEFT} {member} ({seat}, opened {opened}): {}", why.sentence());
    if !once(store, member, &line) {
        return;
    }
    let by = w.map_or(member, |w| w.id.as_str());
    crate::store::log_line(store, &format!("{by} {member}: {}", line.trim_start_matches("wsp: ")));
    if let Some(w) = w {
        crate::cycle::tell(store, w, &format!(
            "The verifier of {member} in {seat} is left running: {}. \
             `wsp despawn --pane {seat}` is the hand version.",
            why.sentence()
        ));
    }
}

/// A finished leg's seat left running for a reason that will not pass: said once on
/// the row and in `cycle.log`, as [`say_verifier_left`] says a verifier's.
pub(crate) fn say_leg_left(store: &Store, row: &str, leg: &str, seat: &str, why: &Left) {
    say_part_left(store, row, &format!("leg {leg}"), seat, why)
}

/// [`say_leg_left`] for any part of a row run inside: `leg build`, `piece 2`.
pub(crate) fn say_part_left(store: &Store, row: &str, part: &str, seat: &str, why: &Left) {
    if why.passes() {
        return;
    }
    let line = format!("wsp: {part}'s seat {seat} is left running: {}", why.sentence());
    if once(store, row, &line) {
        crate::store::log_line(store, &format!("{row}: {}", line.trim_start_matches("wsp: ")));
    }
}

/// The ending, on the member's row, once — re-read under the lock so two
/// passes racing write it once.
fn record_ended(store: &Store, id: &str, h: &Holds) {
    let line = h.ended_line();
    let wrote = store.locked(|| {
        let Some(mut t) = store.find_task(id) else { return false };
        if ended(&t, h) {
            return false;
        }
        t.log(&line);
        t.touch();
        store.save_task(&t).is_ok()
    });
    // Committed with the write, as every store write is (`wsp-433`): this runs
    // from the daemon's tick, whose caller commits none of it.
    if wrote {
        store.git_commit(&format!("wsp: {id}'s seat ended"));
    }
}

/// Why an ending failed, with what was not empty where the error is only
/// "Directory not empty" (`wsp-067`), which names nothing.
///
/// **Not always the tree** (`wsp-085`). A despawn that answers "is still
/// standing, so nothing was released" failed before any tree was touched: it
/// is the backend's end, removing the seat's own directory while the agent's
/// last hooks still wrote into it (`wsp-092` moves such a directory aside
/// now). This said "the tree is in use" for cpd-413 and cpd-1000 on m1-fixes-3,
/// and sent the reader to the member's tree for a cause that was never there.
pub(crate) fn why_not_ended(why: &str) -> String {
    if !(why.contains("Directory not empty") || why.contains("os error 66") || why.contains("os error 39")) {
        return why.to_string();
    }
    if why.contains("still standing, so nothing was released") {
        format!("{why} — the seat's own directory was written as it was removed (its agent's last hooks), so the seat was not ended; its tree was not touched")
    } else {
        format!("{why} — the tree is in use: a process still writes in it, so it could not be removed")
    }
}

/// A failed ending: on the row, in `cycle.log`, and to the governor — once,
/// because the row's line is what the next pass reads as "already tried".
fn not_ended(store: &Store, w: Option<&Worklist>, member: &str, seat: &str, h: &Holds, why: &str) {
    let why = &why_not_ended(why);
    let commit = &h.commit;
    let line = format!("{NOT_ENDED} {member} ({seat}) at {commit} — {}", util::truncate(why, 200));
    if !once(store, member, &line) {
        return;
    }
    crate::store::log_line(store, &format!("{} {member}: {}", w.map_or(member, |w| w.id.as_str()), line.trim_start_matches("wsp: ")));
    if let Some(w) = w {
        crate::cycle::tell(store, w, &format!(
            "{member}: {}, and ending its agent ({seat}) failed: {}. \
             It will not be tried again for this verdict; `wsp despawn {member}` is the hand version.",
            h.because(),
            util::truncate(why, 160)
        ));
    }
}

/// An agent left running for a reason that will not pass: said once per
/// reason per verdict.
fn say_left(store: &Store, w: Option<&Worklist>, member: &str, seat: &str, why: &Left, h: &Holds) {
    let line = format!("{LEFT} {member} ({seat}) at {}: {}", h.commit, why.sentence());
    if !once(store, member, &line) {
        return;
    }
    crate::store::log_line(store, &format!("{} {member}: {}", w.map_or(member, |w| w.id.as_str()), line.trim_start_matches("wsp: ")));
    if let Some(w) = w {
        crate::cycle::tell(store, w, &format!(
            "{member}: {}, and its agent ({seat}) is left running: {}.",
            h.because(),
            why.sentence()
        ));
    }
}

/// Write `line` on the row unless it is already there; whether it was written.
fn once(store: &Store, id: &str, line: &str) -> bool {
    store.locked(|| {
        let Some(mut t) = store.find_task(id) else { return false };
        if t.section("Log").is_some_and(|l| l.lines().any(|x| x.ends_with(line))) {
            return false;
        }
        t.log(line);
        t.touch();
        store.save_task(&t).is_ok()
    })
}

// ---- an accepted exit ends its seat (`wsp-615`, row E4) ----------------------

/// Where the seats of accepted exits wait for their ending: seat → the row, the
/// kind and when it was accepted, and `left` once it was left running. Read by
/// the cap ([`exited_seats`]), by the older endings that step aside for it
/// ([`exit_owns`]), and ended by [`end_exit`].
const EXITS: &str = "exits.json";

/// The line on the row saying the seat of its accepted exit was ended.
pub(crate) const EXIT_ENDED: &str = "ended: its exit was accepted at";

/// The line on the row saying that seat is left running, and why.
const EXIT_LEFT: &str = "wsp: the exit is accepted and its seat is left running —";

/// The line on the row saying ending that seat was tried and failed.
const EXIT_NOT_ENDED: &str = "wsp: could not end the seat of an accepted exit";

/// The line on the row saying the seat had gone before its exit's ending came.
const EXIT_GONE: &str = "wsp: the seat of an accepted exit had already gone —";

fn read_exits(store: &Store) -> serde_json::Map<String, serde_json::Value> {
    std::fs::read_to_string(store.state_file(EXITS))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

fn write_exits(store: &Store, all: &serde_json::Map<String, serde_json::Value>) -> bool {
    let file = store.state_file(EXITS);
    let tmp = file.with_extension(format!("json.{}", std::process::id()));
    std::fs::write(&tmp, serde_json::Value::Object(all.clone()).to_string()).is_ok() && std::fs::rename(&tmp, &file).is_ok()
}

/// `wsp submit` accepted an exit from `seat` for `row`: the seat is closing.
/// From here it holds no place under the cap ([`exited_seats`]), the older
/// endings leave it alone ([`exit_owns`]), and the tick — or the seat's own
/// `Stop`, where a turn is in flight — ends it.
pub(crate) fn exited(store: &Store, seat: &str, row: &str, kind: &str) -> bool {
    store.locked(|| {
        let mut all = read_exits(store);
        all.insert(seat.to_string(), serde_json::json!({ "row": row, "kind": kind, "at": util::epoch_secs() }));
        write_exits(store, &all)
    })
}

/// The seats of accepted exits not yet ended or left: none of them is counted
/// under the cap, seen working or not, as a settled verifier is not (`wsp-385`).
pub(crate) fn exited_seats(store: &Store) -> std::collections::BTreeSet<String> {
    read_exits(store).into_iter().filter(|(_, e)| e["left"].as_bool() != Some(true)).map(|(s, _)| s).collect()
}

/// The seats whose ending is their exit's — closing, or left running by it.
/// **One ending per seat**: [`crate::cycle::passes_finished`],
/// [`crate::cycle::checks_finished`] and a `go`'s `--end-check-seat` helper
/// skip these, so a verifier's and a check's seat are not ended twice — the
/// exit's ending writes what theirs would (the pass's end and spend, the
/// barrier entry's `ended`).
pub(crate) fn exit_owns(store: &Store) -> std::collections::BTreeSet<String> {
    read_exits(store).keys().cloned().collect()
}

/// The record of `seat`'s exit gone: an exit refused after all, or ended.
pub(crate) fn forget_exit(store: &Store, seat: &str) {
    store.locked(|| {
        let mut all = read_exits(store);
        all.remove(seat).is_some() && write_exits(store, &all)
    });
}

/// A verifier's or a check's seat left running by its exit: kept on the
/// record, so no older ending takes it after all, but counted again.
fn mark_left(store: &Store, seat: &str) {
    store.locked(|| {
        let mut all = read_exits(store);
        let Some(e) = all.get_mut(seat).and_then(|e| e.as_object_mut()) else { return false };
        e.insert("left".into(), serde_json::Value::Bool(true));
        write_exits(store, &all)
    });
}

/// Every accepted exit's seat, tried once each tick.
fn exits(store: &Store, seats: &dyn Seats) {
    for seat in exit_owns(store) {
        end_exit(store, seats, &seat);
    }
}

/// End the seat of an accepted exit — member, verifier or check — by ending.rs's
/// own rules: never a governor seat, never one a turn began at since the exit
/// was accepted ([`Seats::turn_began_since`], a backend that cannot say holds),
/// and never mid-turn. Its pids are read before the despawn and confirmed gone
/// after it ([`verifier_pids`], [`confirm_gone`]); the **tree is kept**, which a
/// verifier reads in and the landing came from.
///
/// **The only ending such a seat has.** A verifier's goes through
/// [`crate::cycle::end_passes_keeping`], so its pass is written `ended` with
/// its spend, its runs and what it did, read off the seat before the despawn
/// clears them. A check's marks its barrier entry ended
/// ([`crate::barrier::mark_ended`]) and says what became of its copy. A seat
/// some other path ended first — [`Seats::absent`] says it is gone, or its pass
/// or entry is no longer standing — is said and forgotten, not retried.
///
/// **No wait counts**: a `not held` exit raised its own decision ask, and the
/// seat is ended behind it (D3) — the answer reopens the row on a fresh seat.
///
/// A turn in flight, or a seat nobody can read, passes by itself and is tried
/// again: at the seat's `Stop` by the helper `wsp submit` arranged, else on the
/// next tick. Everything else is said once on the row and in `cycle.log`. A
/// member's record then goes, so a seat left is counted again by what it is
/// doing; a verifier's or a check's left seat stays on the record, out of the
/// older endings' reach, until it is gone. `None` is not yet; `Some` is the
/// line said.
pub(crate) fn end_exit(store: &Store, seats: &dyn Seats, seat: &str) -> Option<String> {
    let e = read_exits(store).get(seat).cloned()?;
    let row = e["row"].as_str().unwrap_or_default().to_string();
    let kind = e["kind"].as_str().unwrap_or("member").to_string();
    let at = e["at"].as_i64().unwrap_or(0);
    if e["left"].as_bool() == Some(true) {
        if seats.state(seat).is_none() && seats.absent(seat) {
            forget_exit(store, seat);
        }
        return None;
    }
    let state = seats.state(seat);
    if state.is_none() && seats.absent(seat) {
        return Some(said_exit(store, seat, &row, &kind, &format!("{EXIT_GONE} {seat} ({kind}) was ended by something else, or closed by hand")));
    }
    if !still_standing(store, seat, &row, &kind) {
        return Some(said_exit(store, seat, &row, &kind, &format!("{EXIT_GONE} {seat} ({kind}) was already ended, and its record says so")));
    }
    let reading = Reading {
        state,
        wait: None,
        governs: crate::place::governs(&store.governors(), &crate::place::Seat::new(seat)),
        typed: seats.turn_began_since(seat, at),
        pids: seats.pids(seat),
    };
    let line = match left(&reading) {
        Some(why) if why.passes() => return None,
        Some(why) => {
            let sentence = match why {
                Left::Typed => "a turn began at it after the exit was accepted, so somebody is using it and it is theirs to end".into(),
                Left::Untold => "it cannot say whether anybody has typed to it since the exit was accepted".into(),
                other => other.sentence(),
            };
            format!("{EXIT_LEFT} {seat} ({kind}): {sentence}")
        }
        None => {
            let pids = match verifier_pids(seats, seat) {
                Ok(p) => p,
                Err(why) if why.passes() => return None,
                Err(why) => return Some(said_exit(store, seat, &row, &kind, &format!("{EXIT_LEFT} {seat} ({kind}): {}", why.sentence()))),
            };
            let gone = matches!(state, Some(State::Gone | State::Empty));
            let ending = match kind.as_str() {
                "verifier" => verifier_ended(store, seats, seat, &row),
                "check" => check_ended(store, seats, seat, gone, &pids),
                _ => {
                    let claimed = store.claims().contains_key(&row);
                    match (claimed, gone) {
                        (true, _) => crate::cycle::despawned_keeping(store, &row, true),
                        (false, true) => Ok(()),
                        (false, false) => crate::cycle::despawn_seat_keeping(seat, true),
                    }
                    .and_then(|()| confirm_gone(seats, &pids))
                }
            };
            match ending {
                Ok(()) => format!("{EXIT_ENDED} {} ({kind}, {seat}), no pid of it running and its tree kept", util::iso_at(at)),
                Err(why) => format!("{EXIT_NOT_ENDED} ({kind}, {seat}) — {}", util::truncate(&why_not_ended(&why), 200)),
            }
        }
    };
    Some(said_exit(store, seat, &row, &kind, &line))
}

/// Whether the record a verifier's or a check's seat is ended by still holds it
/// standing: its pass on the member, its entry on the barrier. A member's is
/// its seat, read above.
fn still_standing(store: &Store, seat: &str, row: &str, kind: &str) -> bool {
    match kind {
        "verifier" => store
            .find_task(row)
            .is_some_and(|t| crate::verification::passes(&t).iter().any(|p| p.standing() && p.pane.as_deref() == Some(seat))),
        "check" => crate::barrier::seat_of(store, seat).is_some(),
        _ => true,
    }
}

/// A verifier's exit, ended through the pass's own ending so its end, spend,
/// runs and work are written on it — the tree kept.
fn verifier_ended(store: &Store, seats: &dyn Seats, seat: &str, row: &str) -> Result<(), String> {
    crate::cycle::end_passes_keeping(store, seats, row, true, |p| p.pane.as_deref() == Some(seat));
    let ending = store
        .find_task(row)
        .and_then(|t| crate::verification::passes(&t).into_iter().find(|p| p.pane.as_deref() == Some(seat)).map(|p| p.ending));
    match ending {
        Some(crate::verification::Ending::Ended) => Ok(()),
        Some(crate::verification::Ending::Failed(why)) => Err(why),
        _ => Err("its pass was not ended — it stays standing for `wsp despawn --pane`".into()),
    }
}

/// A check's exit: its seat ended with the tree left to [`crate::check_tree`]'s
/// sweep, its barrier entry marked ended, and what became of its copy said —
/// what `worklist advance --end-check-seat` did, which steps aside for this.
fn check_ended(store: &Store, seats: &dyn Seats, seat: &str, gone: bool, pids: &[u32]) -> Result<(), String> {
    let ended = match gone {
        true => Ok(()),
        false => crate::cycle::despawn_seat_keeping(seat, true),
    }
    .and_then(|()| confirm_gone(seats, pids));
    crate::barrier::mark_ended(store, seat, ended.is_ok());
    if !cfg!(test) {
        crate::check_tree::tidy(store, &[seat.to_string()]);
        crate::leg_tree::tidy(store, &[seat.to_string()]);
    }
    ended
}

/// Say `line` once on the row (where `row` is one) and in `cycle.log`, and let
/// the record go: what is said is the end of it, ended or left. A verifier's or
/// a check's seat left running keeps its record, marked left, so the older
/// endings still step aside for it.
fn said_exit(store: &Store, seat: &str, row: &str, kind: &str, line: &str) -> String {
    match line.starts_with(EXIT_LEFT) && kind != "member" {
        true => mark_left(store, seat),
        false => forget_exit(store, seat),
    }
    once(store, row, line);
    crate::store::log_line(store, &format!("{row}: {}", line.trim_start_matches("wsp: ")));
    if line.starts_with(EXIT_ENDED) {
        store.git_commit(&format!("wsp: {row}'s seat ended on its exit"));
    }
    line.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ending_refused_with_directory_not_empty_says_the_tree_is_in_use() {
        let why = why_not_ended("could not remove tree: Directory not empty (os error 66)");
        assert!(why.contains("Directory not empty") && why.contains("the tree is in use"), "{why}");
        // The backend's end, refused on the seat's own directory, is not the tree (`wsp-085`).
        let why = why_not_ended("wsp: cpd-413 is still standing, so nothing was released: Directory not empty (os error 66)");
        assert!(why.contains("the seat's own directory") && !why.contains("the tree is in use"), "{why}");
        assert_eq!(why_not_ended("no such seat"), "no such seat");
    }
    use crate::cycle::tests::{DESPAWN_FAILS, ENDED as DESPAWNED, SAID, TOLD};
    use crate::model::Group;
    use crate::verification::Ending;
    use std::collections::BTreeMap;

    /// Seats scripted per seat: what each reads, whether a turn began since the
    /// verdict, its pids — and which pids are still running after an ending.
    #[derive(Default)]
    struct Fake {
        state: BTreeMap<String, State>,
        typed: BTreeMap<String, bool>,
        pids: BTreeMap<String, Vec<u32>>,
        survives: Vec<u32>,
        census: Option<Vec<(String, String)>>,
        /// Seats every backend says are gone.
        absent: Vec<String>,
    }

    impl Fake {
        /// `cpd-1` the member and `cpd-2` the verifier, both idle, nobody has
        /// typed to either, each with a pid that exits when it is ended.
        fn at_rest() -> Fake {
            let mut f = Fake::default();
            for (seat, pid) in [("cpd-1", 101), ("cpd-2", 202)] {
                f.state.insert(seat.into(), State::Idle);
                f.typed.insert(seat.into(), false);
                f.pids.insert(seat.into(), vec![pid, pid + 1]);
            }
            f
        }

        fn with(mut self, seat: &str, s: State) -> Fake {
            self.state.insert(seat.into(), s);
            self
        }
    }

    impl Seats for Fake {
        fn state(&self, seat: &str) -> Option<State> {
            self.state.get(seat).copied()
        }
        fn turn_began_since(&self, seat: &str, _since: i64) -> Option<bool> {
            self.typed.get(seat).copied()
        }
        fn pids(&self, seat: &str) -> Option<Vec<u32>> {
            self.pids.get(seat).cloned()
        }
        fn running(&self, pids: &[u32]) -> Vec<u32> {
            pids.iter().copied().filter(|p| self.survives.contains(p)).collect()
        }
        fn census(&self) -> Option<Vec<(String, String)>> {
            self.census.clone()
        }
        fn absent(&self, seat: &str) -> bool {
            self.absent.iter().any(|s| s == seat)
        }
    }

    thread_local! {
        /// Set by a test that wants the tick's finished-tree scan
        /// ([`super::scans_finished`]) over a tree of its own.
        pub(super) static SCANS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    /// `WSP_HOST` for one test, put back however it ends. Taken after
    /// [`util::isolated`], whose lock it is held under.
    struct HostVar;
    impl HostVar {
        fn set(v: &str) -> HostVar {
            std::env::set_var(wsp_wire::host_tcp::ENV, v);
            HostVar
        }
    }
    impl Drop for HostVar {
        fn drop(&mut self) {
            std::env::remove_var(wsp_wire::host_tcp::ENV);
            SCANS.with(|s| s.set(false));
        }
    }

    /// `wsp-034`: under `WSP_HOST=manager` `wip` runs no scan and draws
    /// `finished-wip.json`, so the daemon's tick, which scans, writes it. A row
    /// at review with no seat and no claim has its tree's own binary standing
    /// in it: a tick under `inproc` stops it and leaves no file (wip scans for
    /// itself there, as before); a tick under the manager stops it and leaves
    /// the line `wip` draws.
    #[test]
    fn a_tick_under_the_manager_leaves_wip_its_finished_tree_heading() {
        let env = util::isolated("ending-finished-wip");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let row = format!("fin-{}", std::process::id());
        let mut t = Task::new(&row, &row);
        t.set_status(Status::Review);
        store.save_task(&t).unwrap();
        // `lsof` names the real path, and the tree's own binary is read off
        // `ps -o comm=`, which is the name it was run under: a `sleep` named
        // as the tree's build (a copied system binary is refused a start).
        let base = std::fs::canonicalize(env.home()).unwrap();
        let tree = base.join(".worktrees").join(&row);
        std::fs::create_dir_all(&tree).unwrap();
        let stand = || {
            use std::os::unix::process::CommandExt;
            std::process::Command::new("/bin/sleep").arg0(tree.join("target/debug/stand")).arg("60").current_dir(&tree).spawn().unwrap()
        };
        let gone = |c: &mut std::process::Child| {
            let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while std::time::Instant::now() < until {
                if c.try_wait().unwrap().is_some() {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            let _ = c.kill();
            false
        };
        // The manager's seats: none, so nobody holds the row.
        let seats = Fake { census: Some(vec![]), ..Fake::default() };
        let held = store.state_file("finished-wip.json");

        let mut child = stand();
        let host = HostVar::set("inproc");
        SCANS.with(|s| s.set(true));
        tick(&store, &seats);
        drop(host);
        assert!(gone(&mut child), "the tick stops what the finished tree started");
        assert!(!held.exists(), "under inproc the file is wip's own scan's, as before");
        assert!(crate::ending::finished_wip_lines_held(&store).is_empty());

        let mut child = stand();
        let host = HostVar::set("manager");
        SCANS.with(|s| s.set(true));
        tick(&store, &seats);
        drop(host);
        assert!(gone(&mut child), "the tick under the manager stops it too");
        let lines = finished_wip_lines_held(&store);
        assert_eq!(lines.len(), 1, "wip draws the tick's one line: {lines:?}");
        assert!(lines[0].starts_with(&format!("{row} is finished and has no seat, and 1 process ")), "{}", lines[0]);
        assert!(lines[0].ends_with("the tick stops them"), "{}", lines[0]);
        let log = std::fs::read_to_string(store.state_file("cycle.log")).unwrap_or_default();
        assert_eq!(log.matches(&format!("stopped in finished row {row} ")).count(), 2, "{log}");
    }

    fn drain<T>(cell: &'static std::thread::LocalKey<std::cell::RefCell<Vec<T>>>) -> Vec<T> {
        cell.with(|c| c.borrow_mut().drain(..).collect())
    }

    /// A running list whose one member `m-1` landed `abc1234` and is at
    /// `review`, claimed on `cpd-1`, with one pass in its `## Verification`
    /// that read that commit and holds — its verifier still standing in
    /// `cpd-2`, as `wsp-188` leaves it until the next tick.
    fn verified(tag: &str) -> (util::Isolated, Store) {
        drain(&DESPAWNED);
        drain(&SAID);
        drain(&TOLD);
        DESPAWN_FAILS.with(|f| *f.borrow_mut() = None);
        let env = util::isolated(&format!("ending-{tag}"));
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let mut m = Task::new("m-1", "m-1");
        m.set_status(Status::Review);
        crate::log::note(&store, &mut m, &format!("{} abc1234", crate::repair::LANDED)).unwrap();
        set_pass(&store, crate::verification::State::Holds);
        let mut w = Worklist::new("run", "run");
        w.set_status(WorklistStatus::Running);
        w.set_groups(&[Group { members: vec!["m-1".into()], agent: "claude".into(), ..Group::default() }]);
        store.save_worklist(&w).unwrap();
        store.set_claim("m-1", serde_json::json!({ "workspace": "cpd-1" }));
        store.set_binding("cpd-1", serde_json::json!({ "task_id": "m-1" }));
        (env, store)
    }

    /// `m-1`'s one pass, at `state`.
    fn set_pass(store: &Store, state: crate::verification::State) {
        let mut m = store.find_task("m-1").unwrap();
        let mut p = crate::verification::Pass::opened(Some("abc1234".into()), None);
        p.at = "2026-10-06T09:00:00Z".into();
        p.pane = Some("cpd-2".into());
        p.state = state;
        p.decided = (state != crate::verification::State::Running).then(|| "2026-10-06T09:30:00Z".into());
        crate::verification::write(&mut m, &[p]);
        store.save_task(&m).unwrap();
    }

    /// One repair pass, in its order: `wsp-188` ends the verifier seats whose
    /// verdict is in, and then this module ends the members.
    fn pass(store: &Store, seats: &Fake) {
        crate::cycle::passes_finished(store, seats);
        tick(store, seats);
    }

    fn log_of(store: &Store, id: &str) -> String {
        store.find_task(id).unwrap().section("Log").unwrap_or_default().to_string()
    }

    fn member_ended() -> bool {
        drain(&DESPAWNED).contains(&"m-1".to_string())
    }

    /// `wsp-433`: `wsp doctor` found 26 task files with release lines and 43
    /// worklist files with attempts marked `ended` on disk and in no commit.
    /// The release (`release_pane`'s claim and log line) and both endings (the
    /// row's, and the check's on its worklist) leave the store clean.
    #[test]
    fn a_release_and_an_attempts_end_leave_the_store_committed() {
        let (_env, store) = verified("committed");
        for args in [
            &["init", "-q"][..],
            &["config", "user.email", "wsp@test"],
            &["config", "user.name", "wsp test"],
            &["config", "commit.gpgsign", "false"],
            &["add", "-A"],
            &["commit", "-q", "-m", "store"],
        ] {
            let ok = wsp_base::calls::Command::new("git").arg("-C").arg(&store.root).args(args).env_remove("GIT_INDEX_FILE").status().unwrap();
            assert!(ok.success(), "git {args:?}");
        }
        let loose = || {
            let out = wsp_base::calls::Command::new("git")
                .arg("-C")
                .arg(&store.root)
                .args(["status", "--porcelain"])
                .env_remove("GIT_INDEX_FILE")
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        assert_eq!(loose(), "", "the baseline is clean");

        // The member's ending, then the release line a seat's ending writes.
        pass(&store, &Fake::at_rest());
        assert!(log_of(&store, "m-1").contains("ended: verification holds"), "{}", log_of(&store, "m-1"));
        assert_eq!(loose(), "", "the row's ending is committed");
        let (removed, task) = crate::cmd_agent::release_pane(&store, "cpd-1");
        assert!(removed && task.as_deref() == Some("m-1"));
        assert!(log_of(&store, "m-1").contains("released"), "{}", log_of(&store, "m-1"));
        assert_eq!(loose(), "", "the release line is committed");

        // And a check marked ended on its worklist.
        let mut w = store.worklist("run").unwrap();
        let mut g = w.groups();
        g[0].checks.push(crate::barrier::Check {
            at: "2026-10-06T09:00:00Z".into(),
            state: crate::barrier::State::Running,
            read: None,
            pane: Some("cpd-9".into()),
            on: Some("claude sonnet/high".into()),
            ending: crate::barrier::Ending::Standing,
            was: None,
            tries: 1,
            gave_up: false,
            suite: None,
            run: None,
            by: None,
        });
        w.set_groups(&g);
        store.save_worklist(&w).unwrap();
        store.git_commit("test: a check");
        assert_eq!(loose(), "");
        assert!(crate::barrier::mark_ended(&store, "cpd-9", true));
        assert!(loose().is_empty(), "the attempt's end is committed: {}", loose());
    }

    #[test]
    fn a_holds_verdict_ends_the_member_and_the_verifier_exactly_once() {
        let (_env, store) = verified("holds");
        let seats = Fake::at_rest();
        pass(&store, &seats);
        assert_eq!(drain(&DESPAWNED), vec!["cpd-2".to_string(), "m-1".to_string()], "the verifier's pane and the member's agent");
        assert_eq!(verifier_ending(&store), Ending::Ended, "pids 202 and 203 read before, and neither running after");
        let log = log_of(&store, "m-1");
        assert_eq!(log.matches("ended: verification holds at abc1234").count(), 1, "{log}");
        assert_eq!(store.find_task("m-1").unwrap().status(), Status::Review, "ending its agent is not done, which is Ed's");

        // `wsp despawn` would have released the claim; a second pass that
        // still finds it — a despawn that lied — ends nothing a second time.
        pass(&store, &seats);
        assert!(drain(&DESPAWNED).is_empty(), "the row's record and the pass's are what make it once");
        assert_eq!(log_of(&store, "m-1").matches("ended: verification holds").count(), 1);
    }

    /// `wsp-428`: compounding-117's seat stood 55 minutes after its verdict
    /// because a dev server of its own was running.
    #[test]
    fn a_verified_member_with_a_shell_of_its_own_running_is_ended() {
        struct Shelled(Fake);
        impl Seats for Shelled {
            fn state(&self, seat: &str) -> Option<State> {
                self.0.state(seat)
            }
            fn turn_began_since(&self, seat: &str, since: i64) -> Option<bool> {
                self.0.turn_began_since(seat, since)
            }
            fn pids(&self, seat: &str) -> Option<Vec<u32>> {
                self.0.pids(seat)
            }
            fn running(&self, pids: &[u32]) -> Vec<u32> {
                self.0.running(pids)
            }
            fn shells(&self, seats: &[&str]) -> crate::waiting::Shells {
                match seats.contains(&"cpd-1") {
                    true => crate::waiting::Shells::one("cpd-1", 1),
                    false => Default::default(),
                }
            }
        }
        let (_env, store) = verified("shelled");
        tick(&store, &Shelled(Fake::at_rest()));
        assert!(member_ended(), "its work is in; the shell goes with the seat");
    }

    #[test]
    fn a_blocks_verdict_leaves_the_member_alone() {
        let (_env, store) = verified("blocks");
        set_pass(&store, crate::verification::State::Blocks);
        pass(&store, &Fake::at_rest());
        assert!(!member_ended(), "the member is where a fix is made from");
    }

    #[test]
    fn a_verdict_a_later_landing_superseded_ends_nothing() {
        let (_env, store) = verified("superseded");
        let mut m = store.find_task("m-1").unwrap();
        crate::log::note(&store, &mut m, &format!("{} def5678", crate::repair::LANDED)).unwrap();
        pass(&store, &Fake::at_rest());
        assert!(!member_ended(), "abc1234 held, and def5678 has not been read");

        set_pass(&store, crate::verification::State::Superseded);
        pass(&store, &Fake::at_rest());
        assert!(!member_ended(), "a pass wsp-188 marked superseded is no verdict at all");
    }

    /// `wsp-430`: the member's seat ends when its verifier's pass is open, and
    /// the tree is kept for the verifier to read in; the verifier is not ended.
    #[test]
    fn a_member_is_ended_when_its_verifier_has_the_work_and_its_tree_is_kept() {
        let (_env, store) = verified("running");
        set_pass(&store, crate::verification::State::Running);
        drain(&crate::cycle::tests::KEPT);
        let seats = Fake::at_rest();
        pass(&store, &seats);
        assert_eq!(drain(&DESPAWNED), vec!["m-1".to_string()], "the member, and not the verifier that is reading");
        assert_eq!(drain(&crate::cycle::tests::KEPT), vec!["m-1".to_string()], "with --keep-tree");
        let log = log_of(&store, "m-1");
        assert_eq!(log.matches("ended: handed to its verifier at abc1234").count(), 1, "{log}");
        assert_eq!(store.find_task("m-1").unwrap().status(), Status::Review);
        pass(&store, &seats);
        assert!(drain(&DESPAWNED).is_empty(), "once");
    }

    /// `wsp-430`: a block after the seat has gone starts a rework seat, whose
    /// brief ends in the verdict as what is owed.
    #[test]
    fn a_blocked_member_whose_seat_was_handed_over_gets_a_rework_seat_with_the_verdict() {
        let (_env, store) = verified("rework");
        set_pass(&store, crate::verification::State::Running);
        pass(&store, &Fake::at_rest());
        assert!(member_ended());
        // What `wsp despawn` does after the ending the test records.
        store.clear_claim("m-1");
        store.clear_binding("cpd-1");
        crate::verification::record(&store, "m-1", crate::verification::State::Blocks, "the install is missing", Some("cpd-2"), None).unwrap();
        let w = store.worklist("run").unwrap();
        drain(&crate::cycle::tests::SPAWNED);
        crate::cycle::step(&store, &w, &crate::cycle::tests::Blind);
        assert_eq!(drain(&crate::cycle::tests::SPAWNED), vec![("m-1".to_string(), "claude".to_string())], "a rework seat, on the member's own kind");
        let log = log_of(&store, "m-1");
        assert!(log.contains("sent back:") && log.contains("the install is missing"), "{log}");
        assert_eq!(store.find_task("m-1").unwrap().status(), Status::Doing);
    }

    /// `wsp-430`: a typed-to seat is somebody's.
    #[test]
    fn a_member_someone_typed_to_since_its_review_is_not_handed_over() {
        let (_env, store) = verified("typed");
        set_pass(&store, crate::verification::State::Running);
        let mut seats = Fake::at_rest();
        seats.typed.insert("cpd-1".into(), true);
        pass(&store, &seats);
        assert!(!member_ended());
    }

    #[test]
    fn a_pass_that_read_another_commit_than_the_member_is_at_is_not_its_verifier() {
        let (_env, store) = verified("moved");
        set_pass(&store, crate::verification::State::Running);
        let mut m = store.find_task("m-1").unwrap();
        crate::log::note(&store, &mut m, &format!("{} def5678", crate::repair::LANDED)).unwrap();
        pass(&store, &Fake::at_rest());
        assert!(!member_ended(), "the pass read abc1234 and the member is at def5678");
    }

    /// "Typed since the verdict" is measured from when the verdict was
    /// recorded: a turn between opening the pass and its verdict is the member
    /// answering its own verifier, not a person picking the agent up.
    #[test]
    fn typed_since_is_measured_from_the_verdict_not_from_the_opening() {
        let (_env, store) = verified("decided");
        let m = store.find_task("m-1").unwrap();
        let h = holds(&store.tasks(), &m).unwrap();
        assert_eq!(h.at, util::epoch_of("2026-10-06T09:30:00Z"));
        assert_eq!(h.commit, "abc1234");
    }

    #[test]
    fn a_member_mid_turn_is_left_and_ended_on_the_tick_after_it_settles() {
        let (_env, store) = verified("turn");
        pass(&store, &Fake::at_rest().with("cpd-1", State::Working));
        assert_eq!(drain(&DESPAWNED), vec!["cpd-2".to_string()], "only the verifier, whose turn is over");
        assert!(drain(&TOLD).is_empty(), "a turn passes by itself and nobody is told about it");
        pass(&store, &Fake::at_rest());
        assert_eq!(drain(&DESPAWNED), vec!["m-1".to_string()], "the verifier was ended once, on the tick before");
    }

    #[test]
    fn a_member_on_a_prompt_is_left_alone() {
        let (_env, store) = verified("prompt");
        tick(&store, &Fake::at_rest().with("cpd-1", State::Blocked));
        assert!(!member_ended(), "a dialog only a person can answer");
    }

    #[test]
    fn a_member_behind_its_own_open_ask_is_left_alone() {
        let (_env, store) = verified("ask");
        let q = crate::message::Message::question(
            crate::message::Party::pane("cpd-1", ""),
            crate::message::Kind::Note,
            "which of the two?",
            crate::message::Waiting::new("cpd-1", "m-1"),
        )
        .about(crate::message::About::Task("m-1".into()));
        store.save_message(&q).unwrap();
        let asks = crate::waiting::Asks::read(&store);
        assert!(asks.of("cpd-1", "m-1").is_some(), "the fixture is an open ask the one reading sees");
        tick(&store, &Fake::at_rest());
        assert!(!member_ended(), "idle on screen, and waiting on an answer");
    }

    #[test]
    fn a_seat_somebody_typed_to_since_the_verdict_is_left_and_said_once() {
        let (_env, store) = verified("typed");
        let mut seats = Fake::at_rest();
        seats.typed.insert("cpd-1".into(), true);
        tick(&store, &seats);
        tick(&store, &seats);
        assert!(!member_ended());
        let told = drain(&TOLD);
        assert_eq!(told.len(), 1, "said once, not every minute: {told:?}");
        assert!(told[0].contains("theirs to end"), "{}", told[0]);
    }

    #[test]
    fn a_seat_that_cannot_say_whether_it_was_typed_to_is_left() {
        let (_env, store) = verified("untold");
        let mut seats = Fake::at_rest();
        seats.typed.remove("cpd-1");
        tick(&store, &seats);
        assert!(!member_ended(), "holding costs a pane, guessing costs a conversation");
    }

    #[test]
    fn a_governor_seat_is_never_ended_whatever_row_it_is_bound_to() {
        let (_env, store) = verified("governor");
        store.set_governor("run", serde_json::json!({ "workspace": "cpd-1", "pane": "cpd-1" }));
        tick(&store, &Fake::at_rest());
        assert!(!member_ended());
    }

    #[test]
    fn a_despawn_that_fails_is_logged_and_told_once_and_not_retried() {
        let (_env, store) = verified("fails");
        DESPAWN_FAILS.with(|f| *f.borrow_mut() = Some("no such seat".into()));
        let seats = Fake::at_rest();
        tick(&store, &seats);
        tick(&store, &seats);
        tick(&store, &seats);
        let told = drain(&TOLD);
        assert_eq!(told.iter().filter(|t| t.contains("ending its agent")).count(), 1, "{told:?}");
        assert!(drain(&SAID).iter().any(|l| l.contains("could not end m-1") && l.contains("no such seat")));
        let log = log_of(&store, "m-1");
        assert_eq!(log.matches("could not end m-1").count(), 1, "{log}");
        assert!(!log.contains("ended: verification holds"), "a failure is not an ending");
    }

    #[test]
    fn an_ending_is_recorded_only_once_no_pid_of_the_agent_is_running() {
        let (_env, store) = verified("pid");
        let mut seats = Fake::at_rest();
        seats.survives = vec![102];
        tick(&store, &seats);
        let log = log_of(&store, "m-1");
        assert!(!log.contains("ended: verification holds"), "the seat closed and pid 102 did not: {log}");
        assert!(log.contains("pid 102 is still running"), "{log}");
        assert_eq!(drain(&TOLD).iter().filter(|t| t.contains("102")).count(), 1);
    }

    /// `m-1` as `wsp-340` found five: landed, at review, its verdict holding, and
    /// its claim and binding let go — the pane still open at `cpd-1`.
    fn released(tag: &str) -> (util::Isolated, Store, Fake) {
        let (env, store) = verified(tag);
        let mut m = store.find_task("m-1").unwrap();
        m.log("claimed by pane cpd-1 · spawned at sonnet/high");
        crate::log::note(&store, &mut m, "released after 1h · ran sonnet-5-5/high · 9 turns").unwrap();
        store.clear_claim("m-1");
        store.clear_binding("cpd-1");
        let mut seats = Fake::at_rest();
        seats.census = Some(vec![("cpd-1".into(), "m-1 · title".into())]);
        (env, store, seats)
    }

    #[test]
    fn a_pane_released_at_review_is_ended_once() {
        let (_env, store, seats) = released("released");
        tick(&store, &seats);
        assert!(drain(&DESPAWNED).contains(&"cpd-1".to_string()), "no claim and no binding to find it by, and it is ended");
        assert_eq!(log_of(&store, "m-1").matches("ended: verification holds at abc1234").count(), 1);
        tick(&store, &seats);
        assert!(drain(&DESPAWNED).is_empty(), "the row's record makes it once");
    }

    #[test]
    fn a_released_pane_another_row_holds_is_not_the_members() {
        let (_env, store, seats) = released("released-taken");
        store.set_binding("cpd-1", serde_json::json!({ "task_id": "m-2" }));
        tick(&store, &seats);
        assert!(drain(&DESPAWNED).is_empty(), "the pane went on to another row");
    }

    #[test]
    fn a_pane_that_was_never_the_members_claim_or_is_labelled_for_another_row_is_left() {
        let (_env, store, mut seats) = released("released-other");
        seats.census = Some(vec![("cpd-1".into(), "m-2 · title".into())]);
        tick(&store, &seats);
        assert!(drain(&DESPAWNED).is_empty(), "labelled for another row");
        seats.census = Some(vec![("cpd-9".into(), "m-1 · title".into())]);
        seats.state.insert("cpd-9".into(), State::Idle);
        tick(&store, &seats);
        assert!(drain(&DESPAWNED).is_empty(), "its claim was made from cpd-1, not cpd-9");
    }

    #[test]
    fn a_released_pane_mid_turn_is_left() {
        let (_env, store, seats) = released("released-turn");
        tick(&store, &seats.with("cpd-1", State::Working));
        assert!(drain(&DESPAWNED).is_empty());
    }

    #[test]
    fn a_released_pane_with_no_census_is_left() {
        let (_env, store, mut seats) = released("released-blind");
        seats.census = None;
        tick(&store, &seats);
        assert!(drain(&DESPAWNED).is_empty(), "nobody answered, so there is nothing to sweep");
    }

    fn verifier_ending(store: &Store) -> Ending {
        crate::verification::passes(&store.find_task("m-1").unwrap())[0].ending.clone()
    }

    #[test]
    fn a_verifier_whose_pid_outlives_its_pane_is_a_failed_ending_told_once() {
        let (_env, store) = verified("vpid");
        let mut seats = Fake::at_rest();
        seats.survives = vec![203];
        pass(&store, &seats);
        assert!(
            matches!(verifier_ending(&store), Ending::Failed(why) if why.contains("pid 203 is still running")),
            "the pane closed and 203 did not: that is not an ending"
        );
        assert_eq!(drain(&TOLD).iter().filter(|t| t.contains("verifier of m-1") && t.contains("203")).count(), 1);
        drain(&DESPAWNED);
        // The pane did close: every backend says so. Another despawn would not
        // end pid 203, so it is not tried (`wsp-092` retries a seat still there).
        seats.absent = vec!["cpd-2".into()];
        pass(&store, &seats);
        assert!(!drain(&DESPAWNED).contains(&"cpd-2".to_string()), "a failure on a gone pane is not tried again every tick");
        assert!(drain(&TOLD).iter().all(|t| !t.contains("verifier of m-1")), "nor told again");
    }

    #[test]
    fn a_verifier_with_no_pid_to_confirm_by_is_left_and_said_once() {
        let (_env, store) = verified("vnopid");
        let mut seats = Fake::at_rest();
        seats.pids.remove("cpd-2");
        pass(&store, &seats);
        assert!(!drain(&DESPAWNED).contains(&"cpd-2".to_string()), "nothing could confirm it, so it is not closed");
        assert_eq!(verifier_ending(&store), Ending::Standing, "and stays standing to be asked again");
        assert_eq!(drain(&TOLD).iter().filter(|t| t.contains("verifier of m-1") && t.contains("left running")).count(), 1);
        pass(&store, &seats);
        assert!(drain(&TOLD).iter().all(|t| !t.contains("verifier of m-1")), "said once, not every tick");
        assert_eq!(log_of(&store, "m-1").matches("the verifier is left running").count(), 1);

        seats.state.insert("cpd-2".into(), State::Gone);
        pass(&store, &seats);
        assert!(drain(&DESPAWNED).contains(&"cpd-2".to_string()), "a seat with nobody left in it has nothing to confirm");
        assert_eq!(verifier_ending(&store), Ending::Ended);
    }

    #[test]
    fn a_seat_with_no_pid_to_confirm_by_is_not_ended() {
        let (_env, store) = verified("nopid");
        let mut seats = Fake::at_rest();
        seats.pids.remove("cpd-1");
        tick(&store, &seats);
        assert!(!member_ended());
    }

    #[test]
    fn a_member_whose_agent_already_exited_is_released_and_recorded() {
        let (_env, store) = verified("gone");
        let mut seats = Fake::at_rest().with("cpd-1", State::Gone);
        seats.pids.remove("cpd-1");
        seats.typed.remove("cpd-1");
        tick(&store, &seats);
        assert!(drain(&DESPAWNED).contains(&"m-1".to_string()), "nobody is left in it to be using it");
        assert!(log_of(&store, "m-1").contains("ended: verification holds at abc1234"));
    }

    fn set_list(store: &Store, status: WorklistStatus, agent: &str) {
        let mut w = Worklist::new("run", "run");
        w.set_status(status);
        w.set_groups(&[Group { members: vec!["m-1".into()], agent: agent.into(), ..Group::default() }]);
        store.save_worklist(&w).unwrap();
    }

    /// `wsp-336`: the seventeen panes of 2026-11-02 were members of lists that
    /// had finished, and `wsp-193` read only the lists still open.
    #[test]
    fn a_verified_member_of_a_finished_list_is_ended() {
        let (_env, store) = verified("done-list");
        set_list(&store, WorklistStatus::Done, "claude");
        tick(&store, &Fake::at_rest());
        assert!(member_ended());
        assert!(log_of(&store, "m-1").contains("ended: verification holds at abc1234"));
    }

    #[test]
    fn a_landed_member_in_no_list_with_no_pass_is_ended_once_and_says_why() {
        let (_env, store) = verified("no-list");
        let mut m = store.find_task("m-1").unwrap();
        crate::verification::write(&mut m, &[]);
        store.save_task(&m).unwrap();
        store.save_worklist(&Worklist::new("run", "run")).unwrap();
        let seats = Fake::at_rest();
        tick(&store, &seats);
        assert!(member_ended(), "landed, and nobody owes it a verifier");
        let log = log_of(&store, "m-1");
        assert_eq!(log.matches("ended: landed, no verifier owed, at abc1234").count(), 1, "{log}");
        tick(&store, &seats);
        assert!(!member_ended(), "once");
    }

    #[test]
    fn a_member_of_a_group_wsp_verifies_keeps_its_pane_until_a_verdict() {
        let (_env, store) = verified("owed");
        let mut m = store.find_task("m-1").unwrap();
        crate::verification::write(&mut m, &[]);
        store.save_task(&m).unwrap();
        set_list(&store, WorklistStatus::Running, "claude");
        tick(&store, &Fake::at_rest());
        assert!(!member_ended(), "its verifier has not started, and a blocks verdict is answered from this context");

        set_list(&store, WorklistStatus::Running, "manual");
        tick(&store, &Fake::at_rest());
        assert!(member_ended(), "a group run by hand is owed no verifier");
    }

    #[test]
    fn a_pass_that_does_not_hold_keeps_the_pane_whatever_the_list_says() {
        let (_env, store) = verified("blocked-done");
        set_pass(&store, crate::verification::State::Blocks);
        set_list(&store, WorklistStatus::Done, "claude");
        tick(&store, &Fake::at_rest());
        assert!(!member_ended());
    }

    #[test]
    fn an_unlanded_member_with_no_verifier_owed_is_left() {
        let (_env, store) = verified("unlanded");
        let mut m = Task::new("m-1", "m-1");
        m.set_status(Status::Review);
        store.save_task(&m).unwrap();
        tick(&store, &Fake::at_rest());
        assert!(!member_ended(), "nothing says the trunk has its commits");
    }

    #[test]
    fn a_member_whose_binding_was_reaped_is_found_by_its_claims_agent() {
        let (_env, store) = verified("reaped");
        store.clear_binding("cpd-1");
        store.set_claim("m-1", serde_json::json!({ "agent_id": "a-1" }));
        store.set_agent("a-1", serde_json::json!({ "seat": "cpd-1" }));
        tick(&store, &Fake::at_rest());
        assert!(member_ended());
    }

    /// A staged row `s-1` at `review`, landed, every leg done, handed over by
    /// `cpd-7`, which is bound to nothing now.
    fn staged_row(tag: &str) -> (util::Isolated, Store) {
        drain(&DESPAWNED);
        drain(&SAID);
        DESPAWN_FAILS.with(|f| *f.borrow_mut() = None);
        let env = util::isolated(&format!("ending-{tag}"));
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let mut t = Task::new("s-1", "s-1");
        t.set_status(Status::Doing);
        let mut leg = crate::legs::Leg::pending("finish", "land it");
        leg.state = crate::legs::State::Running;
        crate::legs::write(&mut t, &[leg]);
        store.save_task(&t).unwrap();
        store.set_binding("cpd-7", serde_json::json!({ "task_id": "s-1" }));
        handed_to_legs(&store, "s-1", "cpd-7", &store.bindings()["cpd-7"]);
        store.clear_binding("cpd-7");
        (env, store)
    }

    fn finish_row(store: &Store) {
        let mut t = store.find_task("s-1").unwrap();
        let mut legs = crate::legs::legs(&t);
        legs[0].state = crate::legs::State::Done;
        crate::legs::write(&mut t, &legs);
        t.set_status(Status::Review);
        crate::log::note(store, &mut t, &format!("{} abc1234", crate::repair::LANDED)).unwrap();
    }

    fn seat_ended() -> bool {
        drain(&DESPAWNED).contains(&"cpd-7".to_string())
    }

    fn lone() -> Fake {
        let mut f = Fake::default();
        f.state.insert("cpd-7".into(), State::Idle);
        f.typed.insert("cpd-7".into(), false);
        f.pids.insert("cpd-7".into(), vec![701, 702]);
        f
    }

    #[test]
    fn a_leg_run_rows_first_seat_is_ended_after_its_last_leg_and_the_landing() {
        let (_env, store) = staged_row("orig");
        assert!(log_of(&store, "s-1").contains("wsp: seat cpd-7 handed the row to its legs"));
        tick(&store, &lone());
        assert!(!seat_ended(), "a leg is still running");

        finish_row(&store);
        tick(&store, &lone());
        assert!(seat_ended());
        assert_eq!(log_of(&store, "s-1").matches("original seat cpd-7 ended").count(), 1);
        tick(&store, &lone());
        assert!(!seat_ended(), "once");
    }

    #[test]
    fn a_leg_run_row_that_has_not_landed_keeps_its_first_seat() {
        let (_env, store) = staged_row("orig-unlanded");
        finish_row(&store);
        let mut t = store.find_task("s-1").unwrap();
        t.body = t.body.replace(crate::repair::LANDED, "wsp: nothing");
        store.save_task(&t).unwrap();
        tick(&store, &lone());
        assert!(!seat_ended());
    }

    #[test]
    fn a_first_seat_that_is_mid_turn_or_governs_is_left() {
        let (_env, store) = staged_row("orig-left");
        finish_row(&store);
        let mut busy = lone();
        busy.state.insert("cpd-7".into(), State::Working);
        tick(&store, &busy);
        assert!(!seat_ended(), "a turn passes by itself");

        store.set_governor("run", serde_json::json!({ "workspace": "cpd-7", "pane": "cpd-7" }));
        tick(&store, &lone());
        assert!(!seat_ended(), "a governor seat is never ended");
    }

    #[test]
    fn a_first_seat_a_person_typed_to_since_it_handed_over_is_left() {
        let (_env, store) = staged_row("orig-typed");
        finish_row(&store);
        let mut typed = lone();
        typed.typed.insert("cpd-7".into(), true);
        tick(&store, &typed);
        assert!(!seat_ended());
        assert!(log_of(&store, "s-1").contains("original seat cpd-7 is left running"));
    }

    #[test]
    fn a_first_seat_that_will_not_end_is_recorded_and_not_retried() {
        let (_env, store) = staged_row("orig-fails");
        finish_row(&store);
        let mut seats = lone();
        seats.survives = vec![702];
        tick(&store, &seats);
        tick(&store, &seats);
        let log = log_of(&store, "s-1");
        assert_eq!(log.matches("could not end original seat cpd-7").count(), 1, "{log}");
        assert_eq!(drain(&DESPAWNED).iter().filter(|s| *s == "cpd-7").count(), 1, "tried once");
    }

    #[test]
    fn release_records_the_seat_only_for_a_staged_row_at_doing() {
        let (_env, store) = staged_row("handed");
        let before = log_of(&store, "s-1");
        handed_to_legs(&store, "s-1", "cpd-7", &serde_json::json!({ "leg": "finish" }));
        assert_eq!(log_of(&store, "s-1"), before, "a leg's seat is not the row's first");
        let mut plain = Task::new("p-1", "p-1");
        plain.set_status(Status::Doing);
        store.save_task(&plain).unwrap();
        handed_to_legs(&store, "p-1", "cpd-8", &serde_json::json!({}));
        assert!(!log_of(&store, "p-1").contains("handed the row"), "a row with no legs has no first seat to track");
    }

    #[test]
    fn a_scope_of_one_ps_answers_every_tree_from_the_table_it_took_first() {
        let me = std::process::id();
        let (a, b) = crate::place_super::one_ps(|| {
            let a = crate::place_super::tree_of(me);
            // A child started inside the scope is not in the table the scope holds.
            let mut child = wsp_base::calls::Command::new("sleep").arg("5").spawn().unwrap();
            let b = crate::place_super::tree_of(me);
            let _ = child.kill();
            let _ = child.wait();
            (a, b)
        });
        assert_eq!(a, b, "the second look in the scope is the first one's table");
        let mut child = wsp_base::calls::Command::new("sleep").arg("5").spawn().unwrap();
        let after = crate::place_super::tree_of(me);
        let _ = child.kill();
        let _ = child.wait();
        assert!(after.contains(&child.id()), "outside the scope every look is a fresh ps");
    }

    #[test]
    fn the_process_tree_is_read_with_its_root_even_when_ps_lists_nothing_under_it() {
        let me = std::process::id();
        assert!(crate::place_super::tree_of(me).contains(&me));
    }

    /// The case the pid check exists for, with real processes: the root of a
    /// seat exits, the agent under it does not, and what was read before the
    /// ending still names the survivor — which is now at `ppid=1`.
    #[test]
    fn a_child_that_outlives_its_root_is_still_named_and_still_running() {
        use std::os::unix::process::CommandExt;
        let mut root = wsp_base::calls::Command::new("sh")
            .args(["-c", "sleep 30 & echo $!; wait"])
            .stdout(std::process::Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap();
        let mut line = String::new();
        std::io::BufRead::read_line(&mut std::io::BufReader::new(root.stdout.take().unwrap()), &mut line).unwrap();
        let child: u32 = line.trim().parse().unwrap();
        let tree = crate::place_super::tree_of(root.id());
        assert!(tree.contains(&root.id()) && tree.contains(&child), "{tree:?} names {child}");

        root.kill().unwrap();
        root.wait().unwrap();
        let after = crate::place_super::tree_of(root.id());
        let running = crate::cycle::Fleet.running(&tree);
        // By pid, never by pattern — this test's own sleep and nothing else.
        crate::host::Host::signal(&crate::host::LOCAL, &[child], "TERM");
        assert!(after.contains(&child), "still in the group with its parent gone: {after:?}");
        assert_eq!(running, vec![child], "the seat's root is gone and its agent is not");
    }
    struct Quiet(Vec<(String, String)>);
    impl Seats for Quiet {
        fn state(&self, _seat: &str) -> Option<crate::place::State> {
            None
        }
        fn census(&self) -> Option<Vec<(String, String)>> {
            Some(self.0.clone())
        }
    }

    /// `wsp-453` where it happened: a check left a server in a row's tree (here
    /// a `sleep`, in a group of its own the way `vite` leaves one), and the row
    /// reached review. The tick's stop ends it; a row still `doing`, and a row
    /// with a seat open, keep theirs. The server is a `sleep` under a shell
    /// that stands in the tree; a plain `sleep` started from elsewhere with its
    /// cwd in the tree is not the tree's (`wsp-478`), and `f-4` is that.
    #[test]
    fn a_server_a_check_left_in_a_finished_rows_tree_is_stopped_and_a_working_rows_is_not() {
        use std::os::unix::process::CommandExt;
        let env = util::isolated("check-tree-finished");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let root = std::env::temp_dir().join(format!("wsp-453-{}-{}", std::process::id(), util::epoch_nanos()));
        let mut rows = Vec::new();
        for (id, status) in [
            ("f-1", crate::model::Status::Review),
            ("f-2", crate::model::Status::Doing),
            ("f-3", crate::model::Status::Review),
            ("f-4", crate::model::Status::Review),
        ] {
            let mut t = crate::model::Task::new(id, id);
            t.set_status(status);
            store.save_task(&t).unwrap();
            let tree = root.join(crate::cmd_checkout::WORKTREES).join(id);
            std::fs::create_dir_all(&tree).unwrap();
            let mut cmd = if id == "f-4" {
                let mut c = wsp_base::calls::Command::new("sleep");
                c.arg("60");
                c
            } else {
                let mut c = wsp_base::calls::Command::new("sh");
                c.args(["-c", "sleep 60; true"]);
                c
            };
            let child = cmd
                .current_dir(&tree)
                .stdin(std::process::Stdio::null())
                .process_group(0)
                .spawn()
                .unwrap();
            rows.push((id, child));
        }
        let seated = Quiet(vec![("cpd-9".into(), "f-3 · working".into())]);
        let said = stop_finished(&store, &seated);
        let mut ended = Vec::new();
        for (id, child) in rows.iter_mut() {
            // A signalled child of ours is reaped at once; one alive is not.
            std::thread::sleep(std::time::Duration::from_millis(50));
            ended.push((*id, child.try_wait().unwrap().is_some()));
            // The group, for the `sleep` under the shell.
            crate::host::Host::signal_group(&crate::host::LOCAL, child.id(), "KILL");
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(ended, vec![("f-1", true), ("f-2", false), ("f-3", false), ("f-4", false)], "said: {said:?}");
        assert!(said.iter().any(|l| l.starts_with("stopped in finished row f-1")), "{said:?}");
        assert!(said.iter().any(|l| l.starts_with("spared") && l.contains("row f-4")), "{said:?}");
        assert!(!said.iter().any(|l| l.contains("f-2") || l.contains("f-3")), "{said:?}");
        assert!(crate::check_tree::left_line(&crate::check_tree::Left { row: "f-1".into(), tree: "/r/.worktrees/f-1".into(), pids: vec![1, 2], spared: vec![] }, 2048).contains("2 processes (2 MB)"));
    }

    /// `wsp-633`: the children of a person's app, a new pid each cycle, were
    /// named and TERMed every cycle for a day. They are spared, and the one
    /// lineage is named once, whatever pids it has by the next cycle.
    #[test]
    fn a_persons_apps_children_in_a_removed_tree_are_named_once_and_never_stopped() {
        let env = util::isolated("check-tree-app-lineage");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let tree = "/r/.worktrees/app-633";
        let app = "/Users/e/Applications/web-chrome.app/Contents/MacOS/web-chrome";
        let mut said = Vec::new();
        for cycle in 0..4u32 {
            let (app_pid, child, grandchild) = (7000, 7100 + cycle, 7200 + cycle);
            let table = vec![
                (app_pid, 1, app.to_string()),
                (child, app_pid, "/usr/local/bin/helper".to_string()),
                (grandchild, child, "/usr/local/bin/node".to_string()),
            ];
            let lsof = format!("p{app_pid}\nn{tree}\np{child}\nn{tree}/web\np{grandchild}\nn{tree}/web\n");
            let lefts = crate::check_tree::finished_trees(&lsof, &|n| n == "app-633", &[]);
            let found = crate::check_tree::sorted(&lsof, lefts, &table);
            assert!(found.iter().all(|l| l.pids.is_empty()), "cycle {cycle}: nothing is the row's to stop: {found:?}");
            said.extend(say_spared(&store, &found));
        }
        let app_lines: Vec<_> = said.iter().filter(|l| l.contains("descends from an app")).collect();
        assert_eq!(app_lines.len(), 1, "one lineage named once: {said:?}");
        assert_eq!(said.len(), 2, "the app itself once more, by its own pid: {said:?}");
    }


    // ---- `wsp-615`: an accepted exit ends its seat ----------------------------

    /// The run's places, with every seat seen working: what the cap counts.
    fn counted_now(store: &Store) -> Vec<String> {
        let r = crate::room::Records::read(store);
        crate::room::working(&r, &|_| crate::room::Look::Working).into_iter().map(|c| c.what).collect()
    }

    /// The three kinds, as their submits leave them: a member claimed on `cpd-1`,
    /// its verifier on `cpd-2` with its verdict (holds) on the pass, and a check
    /// on `cpd-3` standing on the barrier entry of `run`'s group 1, which it
    /// passed. Each idle with a pid, nobody having typed since.
    fn three_exits(tag: &str) -> (util::Isolated, Store, Fake) {
        let (env, store) = verified(tag);
        drain(&crate::cycle::tests::KEPT);
        // What the pass's start wrote: the load, kept at its end, and a reading
        // the end replaces with what it reads off the seat.
        let mut m = store.find_task("m-1").unwrap();
        let mut ps = crate::verification::passes(&m);
        ps[0].spent = Some(crate::verification::Spent { ran: "at its start".into(), load1: Some(250), ..Default::default() });
        crate::verification::write(&mut m, &ps);
        store.save_task(&m).unwrap();
        let mut w = store.worklist("run").unwrap();
        let mut groups = w.groups();
        let mut c = crate::barrier::Check::opened(None, None);
        c.pane = Some("cpd-3".into());
        c.state = crate::barrier::State::Passes;
        groups[0].checks = vec![c];
        w.set_groups(&groups);
        store.save_worklist(&w).unwrap();
        let mut f = Fake::at_rest();
        f.state.insert("cpd-3".into(), State::Idle);
        f.typed.insert("cpd-3".into(), false);
        f.pids.insert("cpd-3".into(), vec![303]);
        (env, store, f)
    }

    /// The check entry's ending on group 1.
    fn check_ending(store: &Store) -> crate::barrier::Ending {
        store.worklist("run").unwrap().groups()[0].checks[0].ending
    }

    fn the_pass(store: &Store) -> crate::verification::Pass {
        crate::verification::passes(&store.find_task("m-1").unwrap()).remove(0)
    }

    /// All three record their exits, as `wsp submit` does, and the repair pass
    /// runs in its order: the verifiers' and checks' own endings, then this
    /// module's tick. Each seat is ended once, by its exit, its tree kept; the
    /// pass and the barrier entry are written ended as their own endings would.
    #[test]
    fn a_member_a_verifier_and_a_check_are_ended_within_one_tick_of_an_accepted_exit_and_the_tree_kept() {
        let (_env, store, f) = three_exits("exit-three");
        assert!(counted_now(&store).contains(&"m-1".to_string()), "seen working at review, the member counted before its exit");
        assert!(exited(&store, "cpd-1", "m-1", "member"));
        assert!(exited(&store, "cpd-2", "m-1", "verifier"));
        assert!(exited(&store, "cpd-3", "run barrier 1", "check"));
        assert!(!counted_now(&store).contains(&"m-1".to_string()), "the cap drops in the submit's own call, before any tick");

        crate::cycle::passes_finished(&store, &f);
        crate::cycle::checks_finished(&store, &f);
        assert!(drain(&DESPAWNED).is_empty(), "the older endings step aside for a seat its exit ends");
        assert!(crate::cycle::end_checks_when_idle(&store, "run", 1).is_empty(), "and go arranges no second ending of the check");

        exits(&store, &f);
        store.clear_claim("m-1"); // as the member's `wsp despawn` does
        tick(&store, &f);
        let ended = drain(&DESPAWNED);
        for one in ["m-1", "cpd-2", "cpd-3"] {
            assert_eq!(ended.iter().filter(|e| *e == one).count(), 1, "{one} ended once: {ended:?}");
        }
        let kept = drain(&crate::cycle::tests::KEPT);
        assert_eq!(kept.len(), 3, "every one with --keep-tree: {kept:?}");
        let p = the_pass(&store);
        assert_eq!(p.ending, Ending::Ended, "the verifier's pass is written ended by its exit's ending");
        let spent = p.spent.clone().expect("a spend on the pass");
        assert!(spent.ran != "at its start" && spent.load1 == Some(250), "its spend written at its end, the start's load kept: {p:?}");
        assert_eq!(check_ending(&store), crate::barrier::Ending::Ended, "the check's entry is marked ended");
        let log = log_of(&store, "m-1");
        assert_eq!(log.matches(EXIT_ENDED).count(), 2, "the member's and its verifier's, once each: {log}");
        assert!(exit_owns(&store).is_empty(), "nothing is left to end");

        pass(&store, &f);
        crate::cycle::checks_finished(&store, &f);
        assert!(drain(&DESPAWNED).is_empty(), "and nothing is ended twice, by either path");
        assert_eq!(the_pass(&store).ending, Ending::Ended);
    }

    /// A verifier or a check whose exit is recorded while its turn is still in
    /// flight is not ended by the older endings either: it waits for its `Stop`.
    #[test]
    fn a_verifier_and_a_check_mid_turn_are_left_for_their_stop_by_every_ending() {
        let (_env, store, f) = three_exits("exit-stop");
        let f = f.with("cpd-2", State::Working).with("cpd-3", State::Working);
        exited(&store, "cpd-2", "m-1", "verifier");
        exited(&store, "cpd-3", "run barrier 1", "check");
        crate::cycle::passes_finished(&store, &f);
        crate::cycle::checks_finished(&store, &f);
        exits(&store, &f);
        assert!(drain(&DESPAWNED).is_empty(), "nothing ends a seat mid-turn");
        assert_eq!(the_pass(&store).ending, Ending::Standing);
        let f = f.with("cpd-2", State::Idle).with("cpd-3", State::Idle);
        assert!(end_exit(&store, &f, "cpd-2").is_some_and(|l| l.starts_with(EXIT_ENDED)), "ended at its Stop");
        assert!(end_exit(&store, &f, "cpd-3").is_some_and(|l| l.starts_with(EXIT_ENDED)), "ended at its Stop");
        assert_eq!(drain(&DESPAWNED), vec!["cpd-2".to_string(), "cpd-3".to_string()]);
        assert_eq!(the_pass(&store).ending, Ending::Ended);
        assert_eq!(check_ending(&store), crate::barrier::Ending::Ended);
    }

    #[test]
    fn a_governor_seat_and_one_typed_to_since_are_not_ended_and_each_is_said_once() {
        let (_env, store, mut f) = three_exits("exit-left");
        store.set_governor("run", serde_json::json!({ "workspace": "cpd-1", "pane": "cpd-1" }));
        f.typed.insert("cpd-2".into(), true);
        exited(&store, "cpd-1", "m-1", "member");
        exited(&store, "cpd-2", "m-1", "verifier");
        exits(&store, &f);
        exits(&store, &f);
        pass(&store, &f);
        assert!(drain(&DESPAWNED).is_empty(), "neither is ended, and the older endings leave them to the person too");
        let log = log_of(&store, "m-1");
        assert_eq!(log.matches("cpd-1 (member): its seat governs run").count(), 1, "{log}");
        assert_eq!(log.matches("cpd-2 (verifier): a turn began at it after the exit").count(), 1, "{log}");
        assert!(!exited_seats(&store).contains("cpd-2"), "a seat left is counted again by what it does");
        f.state.remove("cpd-2");
        f.absent.push("cpd-2".into());
        exits(&store, &f);
        assert!(exit_owns(&store).is_empty(), "and its record goes once the seat has gone");
    }

    #[test]
    fn a_seat_mid_turn_is_left_for_its_stop_and_counts_for_nothing_meanwhile() {
        let (_env, store, f) = three_exits("exit-turn");
        let f = f.with("cpd-1", State::Working);
        exited(&store, "cpd-1", "m-1", "member");
        assert_eq!(end_exit(&store, &f, "cpd-1"), None, "a turn in flight passes by itself");
        assert!(drain(&DESPAWNED).is_empty());
        assert!(!counted_now(&store).contains(&"m-1".to_string()), "closing, so no place under the cap while it turns");
        let f = f.with("cpd-1", State::Idle);
        assert!(end_exit(&store, &f, "cpd-1").is_some_and(|l| l.starts_with(EXIT_ENDED)), "ended at its Stop");
        assert_eq!(drain(&DESPAWNED), vec!["m-1".to_string()]);
    }

    #[test]
    fn a_seat_whose_pid_outlives_the_despawn_is_said_not_ended_once_and_not_retried() {
        let (_env, store, mut f) = three_exits("exit-survives");
        f.survives = vec![202];
        exited(&store, "cpd-2", "m-1", "verifier");
        exits(&store, &f);
        // The despawn did close the pane, and every backend says so: only pid
        // 202 outlived it, which another despawn would not end (`wsp-092`
        // retries a failed ending only while the seat is still there).
        f.absent.push("cpd-2".into());
        exits(&store, &f);
        crate::cycle::passes_finished(&store, &f);
        assert_eq!(drain(&DESPAWNED), vec!["cpd-2".to_string()], "tried once");
        assert_eq!(log_of(&store, "m-1").matches(EXIT_NOT_ENDED).count(), 1);
        assert!(matches!(the_pass(&store).ending, Ending::Failed(_)), "written on the pass, so its own ending does not retry it");
    }

    /// A seat every backend reports gone, or whose pass something else already
    /// ended, is said once and forgotten — not read again every tick.
    #[test]
    fn a_seat_already_gone_or_already_ended_is_said_and_forgotten() {
        let (_env, store, mut f) = three_exits("exit-gone");
        f.state.remove("cpd-1");
        f.absent.push("cpd-1".into());
        exited(&store, "cpd-1", "m-1", "member");
        exited(&store, "cpd-2", "m-1", "verifier");
        crate::cycle::end_passes(&store, &f, "m-1", |_| true);
        let _ = drain(&DESPAWNED);
        exits(&store, &f);
        assert!(drain(&DESPAWNED).is_empty(), "neither is ended again");
        assert!(exit_owns(&store).is_empty(), "both records go");
        assert_eq!(log_of(&store, "m-1").matches(EXIT_GONE).count(), 2, "each said once");
        exits(&store, &f);
        assert_eq!(log_of(&store, "m-1").matches(EXIT_GONE).count(), 2);
    }

    /// **`wsp-152`: on Linux, a verifier seat is still ended after an install
    /// replaced the wsp the daemon is running.** The daemon ends a seat by
    /// running `wsp despawn` again at its own path. Linux names a replaced
    /// binary `<path> (deleted)`, and spawning that name failed with "No such
    /// file or directory (os error 2)" for cpd-10, cpd-14 and cpd-15 every tick
    /// on Omarchy 2026-11-19. Here a real process loses its binary to an
    /// "install", the name its `/proc/<pid>/exe` gives is shown to fail as it
    /// did, and [`util::on_disk`] gives the path that runs the new binary, as
    /// `cycle::despawn_seat_keeping` runs it (cwd `/`).
    #[cfg(target_os = "linux")]
    #[test]
    fn a_seat_is_ended_by_the_wsp_on_disk_after_an_install_replaced_the_running_one() {
        use std::process::{Command, Stdio};
        let dir = std::env::temp_dir().join(format!("wsp-152-{}-{}", std::process::id(), util::epoch_nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("sleep");
        // Copied by a child, never written by this process (ETXTBSY: see
        // `wsp_testkit::script`).
        let cp = Command::new("sh").arg("-c").arg("cp -L \"$(command -v sleep)\" \"$1\"").arg("sh").arg(&bin).status().unwrap();
        assert!(cp.success(), "copying sleep to {}", bin.display());
        let mut running = Command::new(&bin).arg("60").stdin(Stdio::null()).spawn().unwrap();

        // The install: the binary that is running is unlinked and a new one
        // takes its path.
        std::fs::remove_file(&bin).unwrap();
        wsp_testkit::script(&bin, "#!/bin/sh\necho \"ended $*\"\n");
        let linked = std::fs::read_link(format!("/proc/{}/exe", running.id()));
        let _ = running.kill();
        let _ = running.wait();
        let linked = linked.unwrap();
        assert!(linked.to_string_lossy().ends_with(" (deleted)"), "Linux names the replaced binary so: {}", linked.display());
        let before = Command::new(&linked).current_dir("/").stdin(Stdio::null()).output().unwrap_err();
        assert_eq!(before.kind(), std::io::ErrorKind::NotFound, "the name that failed each tick: {before}");

        let exe = util::on_disk(&linked);
        assert_eq!(exe, bin);
        let out = Command::new(&exe).args(["despawn", "--pane", "cpd-10"]).current_dir("/").stdin(Stdio::null()).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(String::from_utf8_lossy(&out.stdout), "ended despawn --pane cpd-10\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
