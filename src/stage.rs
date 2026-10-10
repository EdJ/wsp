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
            (t.id.clone(), of_member(&m))
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
}
