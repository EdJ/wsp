//! Stages: where a task or a worklist group is, as a caption — a **word** wsp
//! derives and a **tail** somebody wrote at the verb that took the step.
//! compound's `docs/ux-design.md` §17.2 and the two tables in §17.5; `wsp-242`.
//!
//! **No new record for any word.** Every word below is read off something wsp
//! already keeps — a status, a `## Verification` entry, a `## Log` line, the
//! list's position, a barrier check, a group's history — and the tails are
//! what `verified`, `go`, `hold`, `block`, `park`, `reopen` and `say` were
//! already given. So a stage cannot go stale against the record it reads, and
//! there is no second place for the word to be wrong in.
//!
//! **The word is never authored** (§17.2), so a caption cannot say `passed` on
//! a barrier that held: the tail is the only half a person wrote, and it is
//! only ever put after the word wsp chose.
//!
//! **First match wins, in the table's order.** The order is the argument: a
//! member at `review` whose newest pass blocks is `sent back`, not `at
//! review`, because the verdict is newer than the status and is what is owed;
//! a group whose barrier check is open is `being checked` whatever its
//! members read as, because the check is what the run is waiting on.
//!
//! **The derivation is pure.** [`of_member`] and [`of_group`] take the facts
//! and touch no store, so each word has a test that builds exactly the record
//! that makes it. [`census`] is the one live read — whether an agent is
//! turning — and a caller that has none passes [`Agent::Unknown`], which
//! reads the say as the only evidence there is.
//!
//! **And when** (`wsp-255`, §19.3 and §19.9): [`history`] replays a member's
//! log and passes into the stages it went through, each at the instant of
//! the line or entry that moved it, and [`times`] gives a group when it was
//! reached, settled and decided. The same rule: nothing is written for it,
//! so a timeline is never behind the records a tooltip would otherwise read.

use std::collections::BTreeMap;

use crate::caption::{self, Told};
use crate::model::{Landed, Status, Task};
use crate::store::Store;
use crate::verification::{Pass, State as Verdict};
use crate::worklist::Position;
use crate::util;

/// What the seat holding a `doing` task is doing, as far as anyone looked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Agent {
    /// Nobody asked, or no backend answered. Not "idle": not knowing whether a
    /// seat stopped is not knowing it stopped (`wip`'s rule).
    #[default]
    Unknown,
    /// A turn is in flight, or the agent is running and not stopped.
    Working,
    /// Stopped while holding `doing` work, in a seat that governs nothing —
    /// `cmd_govern::needs_a_person`, the one definition `wip` publishes.
    WantsYou,
}

/// Everything a member's stage is read from.
pub(crate) struct Member<'a> {
    pub task: &'a Task,
    /// [`crate::verification::all`], oldest first.
    pub passes: &'a [Pass],
    /// The list's landing record for this member, if it is on one that has.
    pub landed: Option<&'a Landed>,
    /// What its agent last said about this task, whole.
    pub said: Option<&'a str>,
    pub claimed: bool,
    pub agent: Agent,
}

/// §17.5's first table.
///
/// One reading beyond the table's words, and it is `cycle`'s own:
/// **a sending back is answered by a return to review.** `sent back` is the
/// newest `sent back:` line in `## Log` with no `review:` below it — the log's
/// order, not its dates, which are days (`cycle::reviewed_since_sent_back`
/// argues it). Read that way, "a reopen came after the last entry" needs no
/// instants: a pass is opened on a return, so a reopen nobody has answered is
/// newer than every pass that answered one. And a block the member has
/// answered falls through to what the member is now — at review, or being
/// verified again — rather than reading `sent back` until a verifier comes.
pub(crate) fn of_member(m: &Member) -> Told {
    let t = m.task;
    let log = heads(t);
    match t.status() {
        Status::Done => return bare("done", &t.updated, "status"),
        Status::Parked => return reason("parked", &log, "parked:", t),
        Status::Blocked => return reason("blocked", &log, "blocked:", t),
        _ => {}
    }

    let latest = m.passes.iter().rev().find(|p| p.state != Verdict::Superseded);
    let back = log.iter().rposition(|(_, h)| h.starts_with(crate::cycle::SENT_BACK));
    let answered = back.is_some_and(|b| log[b + 1..].iter().any(|(_, h)| h.starts_with("review:") || h == "→ review"));
    if let (Some(b), false) = (back, answered) {
        let (date, head) = &log[b];
        // The verifier's own `sent back:` line names it; anything else under
        // that mark is a `wsp reopen`, whose reason is the tail.
        if !head.contains("the verifier blocked") {
            let why = head[crate::cycle::SENT_BACK.len()..].trim();
            return told("sent back", Some(caption::say(why, Some(&t.id))), date, "reason");
        }
        if let Some(p) = latest.filter(|p| p.state == Verdict::Blocks) {
            return verdict("sent back", p);
        }
        return told("sent back", None, date, "stage");
    }
    if let Some(p) = latest {
        match p.state {
            // A block with no `sent back:` line under it is one migrated off a
            // Verify row, from before the verdict wrote one: still owed.
            Verdict::Blocks if back.is_none() => return verdict("sent back", p),
            Verdict::Holds => return verdict("holds", p),
            Verdict::Running => {
                let again = m.passes.iter().any(|q| matches!(q.state, Verdict::Holds | Verdict::Blocks));
                return told("verifying", again.then(|| "again".to_string()), &p.at, "stage");
            }
            _ => {}
        }
    }
    if t.status() == Status::Review {
        return bare("at review", &t.updated, "status");
    }
    if let Some(l) = m.landed.filter(|l| l.commit.is_some()) {
        let n = l.files.len();
        return told("landed", Some(format!("{n} file{}", if n == 1 { "" } else { "s" })), &t.updated, "stage");
    }
    let said = m.said.map(|s| caption::say(s, Some(&t.id))).filter(|s| !s.is_empty());
    match (t.status(), m.agent) {
        (Status::Doing, Agent::WantsYou) => told("wants you", said, &t.updated, "say"),
        (Status::Doing, Agent::Working) => told("working", said, &t.updated, "say"),
        (Status::Doing, Agent::Unknown) if said.is_some() => told("working", said, &t.updated, "say"),
        (Status::Doing, _) => bare("started", &t.updated, "status"),
        _ if m.claimed => bare("started", &t.updated, "status"),
        _ => bare("not started", &t.created, "status"),
    }
}

/// Everything a group's stage is read from, off the walk `worklist show`
/// already made: the position, and the group's own records.
pub(crate) struct Group<'a> {
    /// 1-based.
    pub ordinal: usize,
    pub pos: &'a Position,
    /// A draft has reached no group, whatever its members read as.
    pub draft: bool,
    /// [`crate::barrier::running`]: a check's seat is open on this barrier.
    pub checking: bool,
    /// The hold, when the newest barrier event on this group is one.
    pub held: Option<Told>,
    /// The verdict's own caption, `Passed — …` or `Passed · <date> · N lines`.
    pub verdict: Option<Told>,
    /// Each member's stage word, in the group's order; `None` for a member no
    /// task answers to.
    pub members: Vec<Option<&'static str>>,
}

/// The word under compound's `Stop` for the group's barrier, and its tail.
///
/// **`standing` never quotes the stop condition** — compound's worklist-053
/// rule, §17.10. A standing barrier is the one that wants a sentence, and a
/// caption drawn from the stop would put the question's own words where a
/// glance reads the answer, beside a barrier nobody has answered. Its tail is
/// the fact, always: nobody has written.
pub(crate) fn of_group(g: &Group) -> Told {
    if g.draft {
        return bare("not reached", "", "stage");
    }
    let gone = g.members.iter().filter(|w| w.is_none()).count();
    let unfinished = g.pos.passed.iter().filter(|b| b.group == g.ordinal && !b.member.finished()).count();
    let unwritten = g.pos.unwritten.contains(&g.ordinal);
    let behind = match g.pos.at {
        Some(at) => at > g.ordinal,
        None => true,
    };
    if behind && (unwritten || unfinished > 0 || gone > 0) {
        let mut what = Vec::new();
        if unwritten {
            what.push("no verdict".to_string());
        }
        if gone > 0 {
            what.push(format!("{} gone", members(gone)));
        }
        if unfinished > 0 {
            what.push(format!("{} unfinished", members(unfinished)));
        }
        return told("crossed unwritten", Some(what.join(" · ")), "", "stage");
    }
    if behind {
        return g.verdict.clone().unwrap_or_else(|| bare("passed", "", "stage"));
    }
    if g.pos.at != Some(g.ordinal) {
        return bare("not reached", "", "stage");
    }
    if g.checking {
        return bare("being checked", "", "stage");
    }
    if let Some(h) = &g.held {
        return h.clone();
    }
    if g.pos.at_barrier() {
        return told("standing", Some("nobody has written".into()), "", "stage");
    }
    let settled = g.pos.members.iter().filter(|s| s.finished()).count();
    let mut tail = format!("{settled} of {} settled", g.pos.members.len());
    let back = g.members.iter().filter(|w| **w == Some("sent back")).count();
    if back > 0 {
        tail.push_str(&format!(" · {back} sent back"));
    }
    told("working", Some(tail), "", "stage")
}

fn members(n: usize) -> String {
    format!("{n} member{}", if n == 1 { "" } else { "s" })
}

/// The word and a tail. A blank tail is none, and the word stands alone.
fn told(word: &'static str, tail: Option<String>, at: &str, from: &'static str) -> Told {
    match tail.map(|t| t.trim().to_string()).filter(|t| !t.is_empty()) {
        Some(t) => Told { word, text: format!("{} — {t}", caption::capital(word)), tail: Some(t), from, at: at.to_string() },
        None => bare(word, at, if from == "status" { "status" } else { "stage" }),
    }
}

fn bare(word: &'static str, at: &str, from: &'static str) -> Told {
    Told { word, tail: None, text: caption::capital(word), from, at: at.to_string() }
}

/// A verifier's verdict under the stage's word: its caption, or with none the
/// word and its clock, as [`caption::of_pass`] writes a pass.
fn verdict(word: &'static str, p: &Pass) -> Told {
    let at = p.decided.as_deref().unwrap_or(&p.at);
    let pass = caption::of_pass(p.state.as_str(), p.caption.as_deref(), at);
    match pass.tail {
        Some(tail) => told(word, Some(tail), at, "verdict"),
        None => {
            let clock = pass.text.split_once(" · ").map(|(_, c)| format!(" · {c}")).unwrap_or_default();
            Told { word, tail: None, text: format!("{}{clock}", caption::capital(word)), from: "stage", at: at.to_string() }
        }
    }
}

/// `parked` and `blocked`: the reason's first clause off the newest line the
/// verb wrote, dated by it.
fn reason(word: &'static str, log: &[(String, String)], mark: &str, t: &Task) -> Told {
    match log.iter().rev().find(|(_, h)| h.starts_with(mark)) {
        Some((date, h)) => told(word, Some(caption::say(h[mark.len()..].trim(), Some(&t.id))), date, "reason"),
        None => bare(word, &t.updated, "status"),
    }
}

/// `## Log`'s entries as (date, what was said), oldest first — the bullets
/// only, as `cycle` reads them.
fn heads(t: &Task) -> Vec<(String, String)> {
    let log = t.section("Log").unwrap_or_default();
    log.lines()
        .filter_map(|l| l.trim().strip_prefix("- "))
        .map(|l| match l.split_once(' ') {
            Some((stamp, said)) if util::is_stamp(stamp) => (stamp.to_string(), said.to_string()),
            _ => (String::new(), l.to_string()),
        })
        .collect()
}

/// One step of a member's timeline: the stage word it entered, and when.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Step {
    pub word: &'static str,
    /// Empty where nothing dated it.
    pub at: String,
    /// Only a tail wsp itself derives — `again`, `workspace closed`. The
    /// prose tails are the current stage's, and each verdict's own entry has
    /// its caption; a timeline repeating them is the same sentence twice per
    /// step in a payload carrying every member of every group.
    pub tail: Option<&'static str>,
}

impl Step {
    pub fn json(&self) -> serde_json::Value {
        let mut v = serde_json::json!({ "word": self.word });
        if !self.at.is_empty() {
            v["at"] = serde_json::json!(self.at);
        }
        if let Some(t) = self.tail {
            v["tail"] = serde_json::json!(t);
        }
        v
    }
}

/// When a member entered each stage, oldest first: `wsp-255`, for "verifying
/// since 10:36" and "sent back 09:13, held 09:16".
///
/// **Read off the records each step already wrote**, as [`of_member`] is:
/// the `## Log` (claims, starts, landings, reviews, blocks, parks, reopens,
/// put-backs, releases) and the `## Verification` entries (a pass opens at
/// its header's instant and is decided at its `decided`). A verifier's block
/// is dated by its entry, not by the `sent back:` line `cycle` writes beside
/// it at the same second, so a verdict is one step and not two.
///
/// **`dropped` is a word only history has.** An agent let go of the work
/// before it reached review — a release, or `repair` putting it back — and
/// the task is `not started` again until somebody takes it up; the live
/// stage reads the second half, and the timeline keeps both, so a member
/// with two attempts shows both (compound-395).
///
/// **The say is not dated**, so a claim dates the whole of being on it: the
/// claim is `working`, and the last step takes the live word where that is
/// `started` (nothing said since) — a timeline ending in a word the node does
/// not show is two answers to one question. `wants you` is appended at the
/// stage's own instant, since when the agent stopped is in no record.
pub(crate) fn history(t: &Task, passes: &[Pass], now: Option<&Told>) -> Vec<Step> {
    // (at, word, tail), the log's then the passes', merged by a stable sort
    // so a tie keeps the log first: the review a pass was opened on.
    let mut events: Vec<(String, &'static str, Option<&'static str>)> = Vec::new();
    let mut claimed = false;
    // What the log alone says the member is at, so a release can tell work
    // put down from work handed in.
    let mut state = "not started";
    for (at, head) in heads(t) {
        if at.is_empty() {
            continue;
        }
        let h = head.as_str();
        let word = if h.starts_with("claimed by pane ") || h.starts_with("taken over from pane ") {
            claimed = true;
            Some(("working", None))
        } else if h.starts_with(crate::cycle::STARTED_BY) || h.starts_with("wsp: started by this run") || h == "→ doing" {
            Some((if claimed { "working" } else { "started" }, None))
        } else if h == "→ todo" {
            Some(("not started", None))
        } else if h == "→ review" || h.starts_with("review:") {
            Some(("at review", None))
        } else if h == "→ done" {
            Some(("done", None))
        } else if h == "blocked" || h.starts_with("blocked:") {
            Some(("blocked", None))
        } else if h.starts_with("parked:") {
            Some(("parked", None))
        } else if h.starts_with("wsp: landed ") {
            Some(("landed", None))
        } else if let Some(why) = h.strip_prefix(crate::cycle::SENT_BACK) {
            // The verifier's is its pass's; only the state moves here.
            if why.contains("the verifier blocked") {
                state = "sent back";
                None
            } else {
                Some(("sent back", None))
            }
        } else if h.starts_with("wsp put this back") {
            claimed = false;
            Some(("dropped", None))
        } else if h.starts_with("released") {
            claimed = false;
            let put_down = matches!(state, "started" | "working" | "landed" | "sent back");
            let reaped = h.split(" · ").any(|c| c.trim() == "workspace closed");
            put_down.then_some(("dropped", reaped.then_some("workspace closed")))
        } else {
            None
        };
        if let Some((w, tail)) = word {
            state = w;
            events.push((at, w, tail));
        }
    }
    let mut ordered: Vec<&Pass> = passes.iter().collect();
    ordered.sort_by(|a, b| a.at.cmp(&b.at));
    let mut answered = false;
    for p in ordered {
        events.push((p.at.clone(), "verifying", answered.then_some("again")));
        let decided = || p.decided.clone().unwrap_or_else(|| p.at.clone());
        match p.state {
            Verdict::Holds => events.push((decided(), "holds", None)),
            Verdict::Blocks => events.push((decided(), "sent back", None)),
            _ => {}
        }
        answered |= matches!(p.state, Verdict::Holds | Verdict::Blocks);
    }
    events.sort_by(|a, b| a.0.cmp(&b.0));

    let mut out = vec![Step { word: "not started", at: t.created.clone(), tail: None }];
    for (at, word, tail) in events {
        if out.last().is_some_and(|s| s.word == word && s.tail == tail) {
            continue;
        }
        out.push(Step { word, at, tail });
    }
    if let (Some(now), Some(last)) = (now, out.last_mut()) {
        let on_it = |w: &str| matches!(w, "started" | "working");
        if on_it(last.word) && on_it(now.word) {
            last.word = now.word;
        } else if on_it(last.word) && now.word == "wants you" {
            let at = now.at.clone().max(last.at.clone());
            out.push(Step { word: "wants you", at, tail: None });
        }
    }
    out
}

/// When a group started, settled and was decided (`wsp-255`). Each is `None`
/// until it has happened, and none is a record of its own.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Times {
    pub started: Option<String>,
    pub settled: Option<String>,
    pub decided: Option<String>,
}

impl Times {
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({ "started_at": self.started, "settled_at": self.settled, "decided_at": self.decided })
    }
}

/// A group's times, off records the walk already has.
///
/// - **started**: when the run reached it — the pass of the barrier before
///   it, or the list's start for the first group — or when it was made, if
///   that is later. Not the first member's claim, which is the run's spawn
///   and seconds behind; and not its making, which for a planned group is
///   long before (gpui-migration's group 13 was made at 08:47:14 and reached
///   at 08:48:01).
/// - **settled**: when the last member to reach review did, while every
///   member is there — a member sent back unsettles the group, and the
///   instant it returns is the one that counts.
/// - **decided**: the barrier's last word — its pass, or a hold nothing has
///   come after — exactly what `stage`'s `passed` and `held` read.
///
/// `members` is each member's status and timeline; `None` for one no task
/// answers to, which settles nothing and holds nothing up, since nothing it
/// does can be recorded.
pub(crate) fn times(arrived: Option<&str>, made: Option<&str>, members: &[Option<(Status, &[Step])>], decided: Option<&str>) -> Times {
    let started = arrived.filter(|a| !a.is_empty()).map(|a| made.filter(|m| *m > a).unwrap_or(a).to_string());
    let known: Vec<&(Status, &[Step])> = members.iter().flatten().collect();
    let settled = match !known.is_empty() && known.iter().all(|(s, _)| matches!(s, Status::Review | Status::Done)) {
        true => known
            .iter()
            .map(|(_, steps)| steps.iter().rev().find(|s| matches!(s.word, "at review" | "done")).map(|s| s.at.as_str()))
            .collect::<Option<Vec<&str>>>()
            .and_then(|ats| ats.into_iter().max().filter(|a| !a.is_empty()).map(str::to_string)),
        false => None,
    };
    Times { started, settled, decided: decided.filter(|d| !d.is_empty()).map(str::to_string) }
}

/// The sentence each task's agent last said about it, by task id: the seat
/// bound to the task first, else any seat whose say was about it.
pub(crate) fn sayings(store: &Store) -> BTreeMap<String, String> {
    let said = store.said();
    let mut out = BTreeMap::new();
    for entry in said.values() {
        if let (Some(task), Some(s)) = (entry.get("task").and_then(|v| v.as_str()), entry.get("said").and_then(|v| v.as_str())) {
            if !s.is_empty() {
                out.insert(task.to_string(), s.to_string());
            }
        }
    }
    for (seat, b) in store.bindings() {
        if let Some(task) = b.get("task_id").and_then(|v| v.as_str()) {
            if let Some(s) = crate::cmd_agent::sentence(&said, &seat, Some(task)) {
                out.insert(task.to_string(), s);
            }
        }
    }
    out
}

/// Which `doing` tasks have an agent working on them, and which are stopped
/// on a person, from every backend wsp can spawn onto — the same port and the
/// same predicate `wip` uses. One census per call, and none at all when
/// nothing asked about is `doing`, so a `--json` read of finished work never
/// opens a socket.
///
/// Never in a test: herdr is found by socket, and a test asking it would read
/// whatever this machine's agents are doing.
pub(crate) fn census(store: &Store, doing: &[&Task]) -> BTreeMap<String, Agent> {
    let mut out = BTreeMap::new();
    if doing.is_empty() || cfg!(test) {
        return out;
    }
    let bindings = store.bindings();
    let governors = store.governors();
    for backend in crate::cmd_spawn::local_backends() {
        let Ok(c) = backend.census() else { continue };
        for s in c.seats() {
            let seat = s.seat.as_str();
            let Some(task) = bindings.get(seat).and_then(|b| b.get("task_id")).and_then(|t| t.as_str()) else { continue };
            if !doing.iter().any(|t| t.id == task) || !s.state.is_running() {
                continue;
            }
            let workspace = seat.split(':').next().unwrap_or(seat);
            let governs = crate::cmd_govern::governs(&governors, &crate::cmd_govern::seat_query(workspace, Some(seat))).is_some();
            let agent = match crate::cmd_govern::needs_a_person(s.state.stopped(), true, governs) {
                true => Agent::WantsYou,
                false => Agent::Working,
            };
            // A task held in two seats is wanting you if either is stopped on
            // you: the one waiting is the one that needs saying.
            let e = out.entry(task.to_string()).or_insert(agent);
            if agent == Agent::WantsYou {
                *e = agent;
            }
        }
    }
    out
}

/// Every stage `ls` and `show` give, for the tasks asked about: one read of
/// each source for the whole set.
pub(crate) fn of_tasks(store: &Store, asked: &[&Task]) -> BTreeMap<String, Told> {
    read(store, asked, false).into_iter().map(|(id, (now, _))| (id, now)).collect()
}

/// [`of_tasks`] and each task's [`history`] beside it, off the same reads —
/// for `show` and `worklist show --json`, and not `ls`, which carries a stage
/// and no timeline.
pub(crate) fn with_history(store: &Store, asked: &[&Task]) -> BTreeMap<String, (Told, Vec<Step>)> {
    read(store, asked, true)
}

fn read(store: &Store, asked: &[&Task], timed: bool) -> BTreeMap<String, (Told, Vec<Step>)> {
    let all = store.tasks();
    let claims = store.claims();
    let said = sayings(store);
    let mut landed: BTreeMap<String, Landed> = BTreeMap::new();
    for w in store.worklists() {
        for g in w.groups() {
            for l in g.landed {
                landed.insert(l.member.clone(), l);
            }
        }
    }
    let doing: Vec<&Task> = asked.iter().copied().filter(|t| t.status() == Status::Doing).collect();
    let agents = census(store, &doing);
    asked
        .iter()
        .map(|t| {
            let passes = crate::verification::all(&all, t);
            let m = Member {
                task: t,
                passes: &passes,
                landed: landed.get(&t.id),
                said: said.get(&t.id).map(String::as_str),
                claimed: claims.contains_key(&t.id),
                agent: agents.get(&t.id).copied().unwrap_or_default(),
            };
            let now = of_member(&m);
            let steps = if timed { history(t, &passes, Some(&now)) } else { Vec::new() };
            (t.id.clone(), (now, steps))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worklist::{Behind, Settlement, Standing};

    fn task(status: &str, log: &[&str]) -> Task {
        let mut t = Task::new("A member", "m-1");
        t.status_raw = status.into();
        t.created = "2026-10-01T08:00:00Z".into();
        t.updated = "2026-10-08T09:00:00Z".into();
        if !log.is_empty() {
            t.body = format!("## Log\n{}\n", log.iter().map(|l| format!("- {l}")).collect::<Vec<_>>().join("\n"));
        }
        t
    }

    fn pass(state: &str, caption: Option<&str>) -> Pass {
        let mut p = Pass::opened(None, None);
        p.at = "2026-10-08T09:10:00Z".into();
        p.state = match state {
            "holds" => Verdict::Holds,
            "blocks" => Verdict::Blocks,
            "superseded" => Verdict::Superseded,
            _ => Verdict::Running,
        };
        if p.state != Verdict::Running {
            p.decided = Some("2026-10-08T09:16:00Z".into());
        }
        p.caption = caption.map(str::to_string);
        p
    }

    fn member<'a>(t: &'a Task, passes: &'a [Pass]) -> Member<'a> {
        Member { task: t, passes, landed: None, said: None, claimed: false, agent: Agent::Unknown }
    }

    fn stage(m: &Member) -> (&'static str, Option<String>, &'static str) {
        let s = of_member(m);
        (s.word, s.tail, s.from)
    }

    #[test]
    fn a_done_member_is_done_whatever_else_it_carries() {
        let t = task("done", &["2026-10-08 sent back: wire it"]);
        let ps = [pass("blocks", Some("no test"))];
        assert_eq!(stage(&member(&t, &ps)), ("done", None, "status"));
    }

    #[test]
    fn a_parked_member_carries_its_reasons_first_clause() {
        let t = task("parked", &["2026-10-07 parked: when compound-460 lands. Then wire the chart"]);
        assert_eq!(stage(&member(&t, &[])), ("parked", Some("When compound-460 lands".into()), "reason"));
        assert_eq!(of_member(&member(&t, &[])).at, "2026-10-07", "dated by the line the verb wrote");
    }

    #[test]
    fn a_blocked_member_carries_its_questions_first_clause() {
        let t = task("blocked", &["2026-10-07 blocked: which of the two tables wins? Ed's call"]);
        assert_eq!(stage(&member(&t, &[])).0, "blocked");
        assert_eq!(stage(&member(&t, &[])).1.as_deref(), Some("Which of the two tables wins? Ed's call"), "a ? is not a cut");
    }

    /// The verifier's block, with its caption, and with none its clock.
    #[test]
    fn a_member_whose_verifier_blocked_is_sent_back_with_the_verdicts_caption() {
        let t = task("doing", &["2026-10-08 review: landed", "2026-10-08 sent back: the verifier blocked at abc1234: …"]);
        let ps = [pass("blocks", Some("the wiring between the two has no test"))];
        let s = of_member(&member(&t, &ps));
        assert_eq!(s.text, "Sent back — the wiring between the two has no test");
        assert_eq!((s.word, s.from), ("sent back", "verdict"));
        let bare = of_member(&member(&t, &[pass("blocks", None)]));
        assert!(bare.text.starts_with("Sent back · "), "the word and its clock, never the verdict: {}", bare.text);
    }

    /// A reopen after a pass that held: the reopen is newer, so it is what is owed.
    #[test]
    fn a_reopen_after_a_hold_is_sent_back_with_the_reopens_reason() {
        let t = task("doing", &["2026-10-08 review: landed", "2026-10-09 sent back: install it and drive it live; then review"]);
        let ps = [pass("holds", Some("the wiring is tested now"))];
        assert_eq!(stage(&member(&t, &ps)), ("sent back", Some("Install it and drive it live".into()), "reason"));
    }

    /// Answered by a return to review, a block is no longer what the member is.
    #[test]
    fn a_member_that_returned_to_review_after_a_block_is_no_longer_sent_back() {
        let t = task("review", &["2026-10-08 sent back: the verifier blocked: …", "2026-10-09 review: the test is there"]);
        assert_eq!(stage(&member(&t, &[pass("blocks", None)])).0, "at review");
        let again = [pass("blocks", None), pass("running", None)];
        assert_eq!(stage(&member(&t, &again)), ("verifying", Some("again".into()), "stage"));
    }

    #[test]
    fn a_member_whose_newest_pass_holds_is_holds_with_the_verdicts_caption() {
        let t = task("review", &["2026-10-08 review: landed"]);
        let s = of_member(&member(&t, &[pass("holds", Some("the wiring is tested now"))]));
        assert_eq!((s.word, s.text.as_str()), ("holds", "Holds — the wiring is tested now"));
    }

    #[test]
    fn a_member_with_a_pass_open_is_verifying_and_again_only_after_an_earlier_verdict() {
        let t = task("review", &["2026-10-08 review: landed"]);
        assert_eq!(stage(&member(&t, &[pass("running", None)])), ("verifying", None, "stage"));
        let ps = [pass("holds", None), pass("superseded", None), pass("running", None)];
        assert_eq!(of_member(&member(&t, &ps)).text, "Verifying — again");
    }

    #[test]
    fn a_member_at_review_with_no_pass_is_at_review() {
        let t = task("review", &["2026-10-08 review: landed"]);
        assert_eq!(stage(&member(&t, &[])), ("at review", None, "status"));
    }

    #[test]
    fn a_member_the_list_landed_before_its_review_is_landed_with_its_file_count() {
        let t = task("doing", &[]);
        let l = Landed { member: "m-1".into(), commit: Some("abc1234".into()), files: vec!["a.rs".into(), "b.rs".into(), "c.rs".into()] };
        let mut m = member(&t, &[]);
        m.landed = Some(&l);
        assert_eq!(of_member(&m).text, "Landed — 3 files");
        let unplaced = Landed { commit: None, ..l.clone() };
        m.landed = Some(&unplaced);
        assert_ne!(of_member(&m).word, "landed", "no commit named is nothing landed");
    }

    #[test]
    fn a_doing_member_stopped_on_a_person_wants_you_with_its_say() {
        let t = task("doing", &[]);
        let mut m = member(&t, &[]);
        m.agent = Agent::WantsYou;
        m.said = Some("m-1: which table wins? asked Ed. Waiting");
        assert_eq!(stage(&m), ("wants you", Some("Which table wins? asked Ed".into()), "say"));
    }

    #[test]
    fn a_doing_member_with_a_say_is_working_with_its_first_clause() {
        let t = task("doing", &[]);
        let mut m = member(&t, &[]);
        m.said = Some("wiring the stages into show — tests next");
        assert_eq!(stage(&m), ("working", Some("Wiring the stages into show".into()), "say"));
        m.agent = Agent::Working;
        m.said = None;
        assert_eq!(stage(&m), ("working", None, "stage"), "a turning agent with nothing said is still working");
    }

    #[test]
    fn a_claimed_member_with_nothing_said_is_started() {
        let t = task("doing", &[]);
        assert_eq!(stage(&member(&t, &[])), ("started", None, "status"));
        let todo = task("todo", &[]);
        let mut m = member(&todo, &[]);
        m.claimed = true;
        assert_eq!(stage(&m).0, "started");
    }

    #[test]
    fn an_unclaimed_todo_member_is_not_started() {
        let t = task("todo", &[]);
        let s = of_member(&member(&t, &[]));
        assert_eq!((s.word, s.text.as_str(), s.at.as_str()), ("not started", "Not started", "2026-10-01T08:00:00Z"));
    }

    fn standing(id: &str, status: &str) -> Standing {
        let mut t = Task::new(id, id);
        t.status_raw = status.into();
        Standing { id: id.into(), settlement: Settlement::of(&t), landing: None }
    }

    fn pos(at: Option<usize>, members: Vec<Standing>) -> Position {
        Position { at, of: 3, members, passed: vec![], slipped: vec![], unwritten: vec![], reading: crate::worklist::Reading::Settled }
    }

    fn group<'a>(ordinal: usize, p: &'a Position) -> Group<'a> {
        Group { ordinal, pos: p, draft: false, checking: false, held: None, verdict: None, members: vec![Some("working")] }
    }

    #[test]
    fn a_group_ahead_of_the_run_or_on_a_draft_is_not_reached() {
        let p = pos(Some(1), vec![standing("a", "doing")]);
        assert_eq!(of_group(&group(2, &p)).text, "Not reached");
        let mut g = group(1, &p);
        g.draft = true;
        assert_eq!(of_group(&g).word, "not reached");
    }

    #[test]
    fn a_group_with_work_running_is_working_with_its_counts() {
        let p = pos(Some(2), vec![standing("a", "review"), standing("b", "doing"), standing("c", "done")]);
        let mut g = group(2, &p);
        g.members = vec![Some("holds"), Some("sent back"), Some("done")];
        assert_eq!(of_group(&g).text, "Working — 2 of 3 settled · 1 sent back");
    }

    /// The worklist-053 rule: a standing barrier's tail is the fact and never
    /// a word of the stop condition, which is the question it is waiting on.
    #[test]
    fn a_standing_barriers_tail_is_nobody_has_written_and_no_word_of_its_stop() {
        let stop = "if any of the three spikes goes badly, flag Ed and stop the run";
        let p = pos(Some(2), vec![standing("a", "review"), standing("b", "done")]);
        let s = of_group(&group(2, &p));
        assert_eq!((s.word, s.tail.as_deref()), ("standing", Some("nobody has written")));
        let words: Vec<&str> = s.text.split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() > 2).collect();
        for w in stop.split_whitespace().map(|w| w.trim_matches(|c: char| !c.is_alphanumeric())) {
            assert!(!words.iter().any(|x| x.eq_ignore_ascii_case(w)), "{w:?} from the stop is in {:?}", s.text);
        }
    }

    #[test]
    fn a_barrier_with_a_check_open_is_being_checked() {
        let p = pos(Some(2), vec![standing("a", "review")]);
        let mut g = group(2, &p);
        g.checking = true;
        g.held = Some(caption::of_barrier("held", Some("waiting on Ed"), "2026-10-08", 1, "hold"));
        assert_eq!(of_group(&g).text, "Being checked", "the check is newer than the hold it re-reads");
    }

    #[test]
    fn a_held_barrier_is_held_with_the_holds_caption() {
        let p = pos(Some(2), vec![standing("a", "review")]);
        let mut g = group(2, &p);
        g.held = Some(caption::of_barrier("held", Some("waiting on Ed's read of the design"), "2026-10-08", 3, "hold"));
        let s = of_group(&g);
        assert_eq!((s.word, s.text.as_str(), s.from), ("held", "Held — waiting on Ed's read of the design", "hold"));
    }

    #[test]
    fn a_group_behind_the_run_is_passed_with_the_verdicts_caption_or_its_date_and_length() {
        let p = pos(Some(3), vec![]);
        let mut g = group(1, &p);
        g.verdict = Some(caption::of_barrier("passed", None, "2026-10-08T07:16:00Z", 11, "verdict"));
        let s = of_group(&g);
        assert_eq!(s.word, "passed");
        assert_eq!(s.text, format!("Passed · {} · 11 lines", util::local_ymd("2026-10-08T07:16:00Z")));
        let done = pos(None, vec![]);
        let mut last = group(3, &done);
        last.verdict = Some(caption::of_barrier("passed", Some("all three landed clean"), "2026-10-08", 1, "verdict"));
        assert_eq!(of_group(&last).text, "Passed — all three landed clean");
    }

    #[test]
    fn a_barrier_crossed_without_a_verdict_or_over_a_missing_member_is_crossed_unwritten() {
        let mut p = pos(Some(3), vec![]);
        p.unwritten = vec![1];
        assert_eq!(of_group(&group(1, &p)).text, "Crossed unwritten — no verdict");
        let mut g = group(2, &p);
        g.members = vec![Some("done"), None];
        assert_eq!(of_group(&g).text, "Crossed unwritten — 1 member gone");
        p.passed = vec![Behind { group: 2, member: standing("b", "doing") }];
        let mut g = group(2, &p);
        g.members = vec![Some("done"), Some("working")];
        assert_eq!(of_group(&g).tail.as_deref(), Some("1 member unfinished"));
    }

    #[test]
    fn say_takes_the_first_clause_and_the_speakers_own_name_off() {
        assert_eq!(caption::say("wsp-242: reading the spec; stages not started yet", Some("wsp-242")), "Reading the spec");
        assert_eq!(caption::say("surveying", None), "Surveying");
        assert_eq!(caption::say("parity.md lists every gap. measured twice", None), "parity.md lists every gap", "a name keeps its case");
        assert_eq!(caption::say("first line — rest\nsecond line", None), "First line");
    }

    // --- `wsp-255`: when each stage was entered ---

    fn steps(t: &Task, passes: &[Pass], now: Option<&Told>) -> Vec<(&'static str, String, Option<&'static str>)> {
        history(t, passes, now).into_iter().map(|s| (s.word, s.at, s.tail)).collect()
    }

    fn at(word: &'static str, at: &str) -> (&'static str, String, Option<&'static str>) {
        (word, at.to_string(), None)
    }

    fn step(word: &'static str, at: &str) -> Step {
        Step { word, at: at.into(), tail: None }
    }

    fn decided(state: &str, at: &str, decided: &str) -> Pass {
        let mut p = pass(state, None);
        p.at = at.into();
        p.decided = Some(decided.into());
        p
    }

    /// compound-393 on gpui-migration, at its own instants: worked, landed,
    /// reviewed, verified and sent back, landed again, verified again and
    /// held. The verifier's block is one step dated by its entry, not two
    /// with the `sent back:` line `cycle` wrote at the same second.
    #[test]
    fn a_member_s_timeline_is_each_stage_it_entered_at_the_instant_its_record_gives() {
        let t = task(
            "review",
            &[
                "2026-10-08T08:48:08Z started by wsp: gpui-migration group 13",
                "2026-10-08T08:48:10Z claimed by pane cpd-574 · spawned at sonnet/high",
                "2026-10-08T08:48:10Z wsp: started by this run gpui-migration group 13",
                "2026-10-08T09:00:31Z wsp: this member's seat reads idle with this row still at doing — told it to finish",
                "2026-10-08T09:00:38Z wsp: landed eb277994f233",
                "2026-10-08T09:00:49Z review: The IME candidate window now anchors to the cursor",
                "2026-10-08T09:13:08Z sent back: the verifier blocked at eb277994f233: Blocks, on one thing",
                "2026-10-08T09:14:58Z wsp: landed 2d7ed5bdf13e",
                "2026-10-08T09:15:03Z review: Addressed the block",
                "2026-10-08T09:16:54Z released after 28m · ran sonnet-5-5/high · 40 turns",
            ],
        );
        let passes = [
            decided("blocks", "2026-10-08T09:01:01Z", "2026-10-08T09:13:08Z"),
            decided("holds", "2026-10-08T09:15:12Z", "2026-10-08T09:16:18Z"),
        ];
        // `verification::all` puts a migrated pass after the body's own,
        // whatever its instant.
        let shuffled = [passes[1].clone(), passes[0].clone()];
        let mut want = vec![
            at("not started", "2026-10-01T08:00:00Z"),
            at("started", "2026-10-08T08:48:08Z"),
            at("working", "2026-10-08T08:48:10Z"),
            at("landed", "2026-10-08T09:00:38Z"),
            at("at review", "2026-10-08T09:00:49Z"),
            at("verifying", "2026-10-08T09:01:01Z"),
            at("sent back", "2026-10-08T09:13:08Z"),
            at("landed", "2026-10-08T09:14:58Z"),
            at("at review", "2026-10-08T09:15:03Z"),
            ("verifying", "2026-10-08T09:15:12Z".into(), Some("again")),
            at("holds", "2026-10-08T09:16:18Z"),
        ];
        assert_eq!(steps(&t, &passes, None), want);
        assert_eq!(steps(&t, &shuffled, None), want, "the passes are read in time order, whatever order they come in");
        // A release after review is the agent stepping away from work it
        // handed in, not putting it down.
        assert!(!steps(&t, &passes, None).iter().any(|s| s.0 == "dropped"));
        // And the timeline ends where the stage says it is.
        let now = of_member(&member(&t, &passes));
        assert_eq!((now.word, want.pop().map(|s| s.0)), ("holds", Some("holds")));
    }

    /// compound-395: the first agent went and the run put it back, and the
    /// second finished it. Both attempts are in the timeline, and the drop
    /// is dated by the put-back, the record that it happened.
    #[test]
    fn an_attempt_put_down_before_review_is_dropped_and_the_next_one_follows_it() {
        let t = task(
            "review",
            &[
                "2026-10-08T08:48:24Z started by wsp: gpui-migration group 13",
                "2026-10-08T08:48:25Z claimed by pane cpd-576 · spawned at sonnet/high",
                "2026-10-08T09:14:00Z wsp put this back: the agent it was started on is gone, and the run will start it again",
                "2026-10-08T09:14:01Z released after 25m · ran sonnet-5-5/high · 48 turns",
                "2026-10-08T09:15:02Z started by wsp: gpui-migration group 13",
                "2026-10-08T09:15:03Z claimed by pane cpd-580 · spawned at sonnet/high",
                "2026-10-08T09:55:37Z review: A held submenu is now named",
            ],
        );
        let words: Vec<(&str, String)> = steps(&t, &[], None).into_iter().map(|s| (s.0, s.1)).collect();
        assert_eq!(
            words[1..],
            [
                ("started", "2026-10-08T08:48:24Z".to_string()),
                ("working", "2026-10-08T08:48:25Z".into()),
                ("dropped", "2026-10-08T09:14:00Z".into()),
                ("started", "2026-10-08T09:15:02Z".into()),
                ("working", "2026-10-08T09:15:03Z".into()),
                ("at review", "2026-10-08T09:55:37Z".into()),
            ],
            "the release a second after the put-back is the same drop: {words:?}"
        );
    }

    /// A release with nothing handed in is a drop even with no put-back, and
    /// one whose workspace was closed under it says so — the agent did not
    /// put it down, which `wsp attempts` keeps apart as `reaped`.
    #[test]
    fn a_release_before_review_is_a_drop_and_a_closed_workspace_says_so() {
        let t = task(
            "todo",
            &[
                "2026-10-08T08:00:00Z claimed by pane w1:p1",
                "2026-10-08T08:30:00Z released after 30m",
                "2026-10-08T09:00:00Z claimed by pane w1:p2",
                "2026-10-08T09:30:00Z released after 30m · workspace closed",
            ],
        );
        let h = steps(&t, &[], None);
        assert_eq!(h[2], at("dropped", "2026-10-08T08:30:00Z"));
        assert_eq!(h[4], ("dropped", "2026-10-08T09:30:00Z".into(), Some("workspace closed")));
    }

    /// Blocks, parks, a reopen and the verbs' bare status moves are steps
    /// too, each at its own line; a reopen's `sent back:` is one, because no
    /// pass dates it.
    #[test]
    fn a_block_a_park_a_reopen_and_done_are_steps_at_their_own_lines() {
        let t = task(
            "done",
            &[
                "2026-10-08T08:00:00Z claimed by pane w1:p1",
                "2026-10-08T08:10:00Z blocked: which socket?",
                "2026-10-08T08:20:00Z → doing",
                "2026-10-08T08:30:00Z review: built it",
                "2026-10-08T08:40:00Z sent back: the second half is missing",
                "2026-10-08T08:50:00Z parked: until the fixture exists",
                "2026-10-08T09:00:00Z → doing",
                "2026-10-08T09:05:00Z → review",
                "2026-10-08T09:10:00Z → done",
            ],
        );
        let words: Vec<&str> = steps(&t, &[], None).into_iter().map(|s| s.0).collect();
        assert_eq!(
            words,
            ["not started", "working", "blocked", "working", "at review", "sent back", "parked", "working", "at review", "done"],
            "an unblock by an agent still holding the claim is it working again"
        );
    }

    /// The say is not dated, so the claim dates being on it; the timeline's
    /// last word agrees with the stage the node shows.
    #[test]
    fn the_timeline_ends_in_the_word_the_stage_shows_while_an_agent_is_on_it() {
        let t = task("doing", &["2026-10-08T08:00:00Z claimed by pane w1:p1"]);
        let mut m = member(&t, &[]);
        m.claimed = true;
        let quiet = of_member(&m);
        assert_eq!(steps(&t, &[], Some(&quiet)).last(), Some(&at("started", "2026-10-08T08:00:00Z")), "claimed, nothing said");

        m.agent = Agent::WantsYou;
        m.said = Some("which socket?");
        let waiting = of_member(&m);
        let h = steps(&t, &[], Some(&waiting));
        assert_eq!(h[h.len() - 2], at("working", "2026-10-08T08:00:00Z"));
        assert_eq!(h.last().map(|s| s.0), Some("wants you"), "appended, since when it stopped is in no record: {h:?}");
    }

    /// group 13's three times, in its shapes: reached when group 12 passed,
    /// not when it was made; settled when the follow-up that joined it last
    /// reached review; decided at the pass.
    #[test]
    fn a_group_starts_when_the_run_reaches_it_settles_at_its_last_review_and_is_decided_at_its_barrier() {
        let first = [step("at review", "2026-10-08T09:15:03Z")];
        let last = [step("at review", "2026-10-08T09:00:00Z"), step("sent back", "2026-10-08T09:30:00Z"), step("at review", "2026-10-08T10:45:18Z")];
        let members = [Some((Status::Review, &first[..])), Some((Status::Done, &last[..])), None];
        let t = times(Some("2026-10-08T08:48:01Z"), Some("2026-10-08T08:47:14Z"), &members, Some("2026-10-08T11:01:30Z"));
        assert_eq!(
            t,
            Times {
                started: Some("2026-10-08T08:48:01Z".into()),
                settled: Some("2026-10-08T10:45:18Z".into()),
                decided: Some("2026-10-08T11:01:30Z".into()),
            },
            "the last return to review counts, and a member no task answers to holds nothing up"
        );
        assert_eq!(times(Some("2026-10-08T08:48:01Z"), Some("2026-10-08T09:00:00Z"), &members, None).started.as_deref(), Some("2026-10-08T09:00:00Z"), "a group put in after the run reached its place started when it was made");

        let working = [step("working", "2026-10-08T09:00:00Z")];
        let unsettled = [Some((Status::Review, &first[..])), Some((Status::Doing, &working[..]))];
        assert_eq!(times(None, None, &unsettled, None), Times::default(), "not reached, a member still out, and nothing decided");
    }
}
