//! `wsp timeline <row> [--json]` — where a row's wall time goes, from the
//! spawn that was asked for to the barrier that passed it.
//!
//! In-session wall has always been measured. What a person waits for is the
//! whole of it: the seat starting, the warm-up, each leg and the gap before
//! it, the review, the verifier, the verdict, the barrier. This reads that off
//! what wsp already writes — `events.jsonl`, `cycle.log`, the row's legs and
//! the first prompt in a seat's transcript — and prints one line per phase.
//!
//! **A gap is a phase where nothing was running.** It is named for what was
//! being waited on: a hand-off (a few seconds, the poke), the daemon's tick,
//! the seat's cap, or nothing the logs can name. `wsp-366`.

use serde_json::{json, Value};

use crate::host::Host;
use crate::store::Store;
use crate::util::{self, Paint};
use crate::base::args::Args;

/// The daemon's tick, which a step nobody poked waits for.
pub(crate) const TICK: i64 = 20;
/// Longest a poke takes to show as a start: a process, a lock, a pane.
pub(crate) const HAND_OFF: i64 = 8;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Phase {
    pub label: String,
    pub start: i64,
    pub end: i64,
    /// Nothing was running.
    pub gap: bool,
    /// For a gap: what was being waited for.
    pub waiting_for: Option<String>,
    /// The group's, not the member's: a barrier phase is one run for every
    /// member of the group, and a sum over members counts it once per group.
    pub shared: bool,
}

impl Phase {
    fn work(label: impl Into<String>, start: i64, end: i64) -> Phase {
        Phase { label: label.into(), start, end: end.max(start), gap: false, waiting_for: None, shared: false }
    }
    fn gap(label: impl Into<String>, start: i64, end: i64, why: String) -> Phase {
        Phase { label: label.into(), start, end: end.max(start), gap: true, waiting_for: Some(why), shared: false }
    }
    fn shared(mut self) -> Phase {
        self.shared = true;
        self
    }
    pub(crate) fn seconds(&self) -> i64 {
        self.end - self.start
    }
}

/// One leg, as the row records it.
#[derive(Debug, Clone, Default)]
pub(crate) struct LegIn {
    pub name: String,
    /// The legs it waits on.
    pub after: Vec<String>,
    pub skipped: bool,
}

#[derive(Debug, Default)]
pub(crate) struct Inputs {
    pub id: String,
    /// `(epoch, kind, data)`, for the row and for the members of its group.
    pub events: Vec<(i64, String, Value)>,
    /// `(epoch, line)` of `cycle.log`, for the row and its list.
    pub cycle: Vec<(i64, String)>,
    pub legs: Vec<LegIn>,
    /// The first prompt of the row's own seat, when the transcript says.
    pub first_request: Option<i64>,
    /// The list and group the row runs in, and the group's members.
    pub group: Option<(String, usize, Vec<String>)>,
    /// `(epoch, kind, row)` of the events that end a seat, for the rows a cap
    /// line named as holders (`wsp-394`).
    pub holder_events: Vec<(i64, String, String)>,
    /// The first line of the row's own seat's transcript, whatever it is.
    pub seat_first: Option<i64>,
    /// The verifier seats the row's sessions name, read off their transcripts.
    pub verifiers: Vec<VerifierSeat>,
    /// The stretches a person parked the row's whole list (`worklist-parked` to
    /// `worklist-resumed`), which no row's own log says.
    pub parks: Vec<Park>,
    /// The stretches the row's list stood held (`worklist-held` to the
    /// `worklist-go` or `worklist-resumed` that took it back), `(start, end,
    /// why)`: inside a barrier's check seat, a person's step and not the seat's
    /// work (`wsp-090`).
    pub holds: Vec<(i64, i64, String)>,
    /// The group's barrier check seats as their own readings have them, `(group,
    /// opened, ended)`: `ended` is the seat's stop (its burn reading's `ended`,
    /// what `standing_seats` reads), 0 where none is read. A hold written while
    /// the seat was still open starts at that stop (`wsp-090`).
    pub check_ends: Vec<(usize, i64, i64)>,
    /// The row's decisions still in force (none superseded them), `(instant,
    /// id, text)`, oldest first: one that says the row holds for the person
    /// names the stretch after a review as a person's step, not rework (`wsp-311`).
    pub decisions: Vec<(i64, String, String)>,
    /// The row's own seat's transcripts found where no `session-learned` names
    /// one, `(first stamp, transcript path or session id)`: from the burn ledger
    /// of the panes that claimed the row and from Claude Code's directory for
    /// the row's tree (`wsp-313`: a store with no `session-learned` read every
    /// seat as reading nothing, model 0 and tools none).
    pub found: Vec<(i64, String)>,
    /// The row's leg seats as its own log has them, `leg X started by pane P`
    /// to `leg X ended after N` (`wsp-335`): the record that a seat came up for
    /// a leg where no `session-learned` was written for it.
    pub leg_seats: Vec<LegSeat>,
}

/// One seat a leg ran in, off the row's log (`wsp-335`).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct LegSeat {
    pub leg: String,
    pub pane: String,
    /// The `started by pane` line.
    pub start: i64,
    /// The `ended after` line; `None` while the entry reads running.
    pub end: Option<i64>,
}

/// One park of a list, from its park event to the resume that took it back.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Park {
    pub start: i64,
    pub end: i64,
    /// Who parked it where the event names one; `somebody` where it names no one.
    pub by: String,
    pub why: String,
    /// Still parked at the end of what was read: `end` is where reading stopped.
    pub open: bool,
}

/// The reason a report quotes, whole where it has `cap` characters or fewer and
/// else cut between two words and ended with `…`: never mid-word (`wsp-856`). A
/// first word longer than the cap leaves the `…` alone.
fn cut_at_word(text: &str, cap: usize) -> String {
    let text = text.trim();
    let Some((end, next)) = text.char_indices().nth(cap) else { return text.to_string() };
    let mut head = &text[..end];
    if !next.is_whitespace() {
        head = head.rfind(char::is_whitespace).map_or("", |i| &head[..i]);
    }
    format!("{}…", head.trim_end())
}

impl Park {
    /// What it was, for a cause or a bucket: who, when, why, until when.
    pub(crate) fn words(&self) -> String {
        let at = |t: i64| util::iso_at(t).get(11..16).unwrap_or_default().to_string();
        let why = cut_at_word(&self.why, 140);
        let until = if self.open { "still parked".to_string() } else { format!("resumed at {}", at(self.end)) };
        format!("the list was parked by {} at {}, {until}{}", self.by, at(self.start), if why.is_empty() { String::new() } else { format!(": {why}") })
    }
}

/// The parks of a list, read off the event log.
pub(crate) fn list_parks(store: &Store, list: &str) -> Vec<Park> {
    parks_of(&list_events(store, &["worklist-parked", "worklist-resumed"]), list, util::epoch_secs())
}

/// The events of the kinds named, off the event log, for any list.
pub(crate) fn list_events(store: &Store, kinds: &[&str]) -> Vec<(i64, String, Value)> {
    let mut ev: Vec<(i64, String, Value)> = Vec::new();
    if let Ok(text) = std::fs::read_to_string(store.state_file("events.jsonl")) {
        for line in text.lines().filter(|l| kinds.iter().any(|k| l.contains(&format!("\"{k}\"")))) {
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            if let (Some(ts), Some(kind)) = (v["ts"].as_str(), v["kind"].as_str()) {
                ev.push((util::epoch_of(ts), kind.to_string(), v["data"].clone()));
            }
        }
    }
    ev
}

/// The holds of `list` in an event log's lines, in order, `(start, end, why)`:
/// from a `worklist-held` to the first `worklist-go` or `worklist-resumed`
/// after it; one with neither after it runs to `now`. A hold taken again while
/// held is the same hold.
pub(crate) fn holds_of(events: &[(i64, String, Value)], list: &str, now: i64) -> Vec<(i64, i64, String)> {
    let mut at: Vec<&(i64, String, Value)> = events.iter().filter(|(_, k, d)| matches!(k.as_str(), "worklist-held" | "worklist-go" | "worklist-resumed") && d["id"].as_str() == Some(list)).collect();
    at.sort_by_key(|e| e.0);
    let mut out: Vec<(i64, i64, String)> = Vec::new();
    let mut open: Option<(i64, String)> = None;
    for (t, kind, d) in at {
        if kind == "worklist-held" {
            if open.is_none() {
                open = Some((*t, d["why"].as_str().unwrap_or_default().trim().to_string()));
            }
        } else if let Some((s, why)) = open.take() {
            out.push((s, *t, why));
        }
    }
    if let Some((s, why)) = open {
        out.push((s, now.max(s), why));
    }
    out
}

/// The parks of `list` in an event log's lines, in order. A park with no resume
/// after it runs to `now`.
pub(crate) fn parks_of(events: &[(i64, String, Value)], list: &str, now: i64) -> Vec<Park> {
    let mut out: Vec<Park> = Vec::new();
    let mut at: Vec<&(i64, String, Value)> = events.iter().filter(|(_, k, d)| (k == "worklist-parked" || k == "worklist-resumed") && d["id"].as_str() == Some(list)).collect();
    at.sort_by_key(|e| e.0);
    for (t, kind, d) in at {
        if kind == "worklist-parked" {
            if out.last().is_some_and(|p| p.open) {
                continue;
            }
            let by = d["by"].as_str().filter(|b| !b.is_empty()).unwrap_or("somebody").to_string();
            out.push(Park { start: *t, end: now.max(*t), by, why: d["why"].as_str().unwrap_or_default().to_string(), open: true });
        } else if let Some(p) = out.last_mut().filter(|p| p.open) {
            p.end = *t;
            p.open = false;
        }
    }
    out
}

/// One verifier seat as its own transcript has it (`wsp-429`). wsp's
/// `started <row> · verifying` line is written when the step that spawned the
/// seat finishes, after every `wsp spawn` it ran has returned and each of those
/// waits for its agent: it lags the seat's real start by that long (100 s or
/// more on 9 of 17 verifiers in react-pass-1). The transcript's first line is
/// the start.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct VerifierSeat {
    /// When wsp learned the session.
    pub learned: i64,
    /// The first line of its transcript.
    pub first: i64,
    /// Seconds its test and build commands ran ([`is_test_command`]).
    pub test_secs: i64,
}

impl Inputs {
    fn kind(&self, kind: &str, id: &str) -> Vec<(i64, &Value)> {
        self.events
            .iter()
            .filter(|(_, k, d)| k == kind && d.get("id").and_then(|v| v.as_str()) == Some(id))
            .map(|(t, _, d)| (*t, d))
            .collect()
    }
    fn first_line(&self, from: i64, pred: impl Fn(&str) -> bool) -> Option<i64> {
        self.cycle.iter().find(|(t, l)| *t >= from && pred(l)).map(|(t, _)| *t)
    }
    fn first_learned_after(&self, at: i64) -> Option<i64> {
        self.kind("session-learned", &self.id).into_iter().map(|(t, _)| t).find(|t| *t >= at)
    }
}

/// Why nothing was running between `from` and `to`, for `who`: the row's id, or
/// `<list> · barrier <n>` for the barrier. A cap line names the one that
/// waits before ` waits —`; whoever it lists after the colon holds a place.
fn wait_name(i: &Inputs, from: i64, to: i64, who: &str) -> String {
    let span = to - from;
    let cap = i.cycle.iter().find_map(|(t, l)| {
        if *t < from || *t > to {
            return None;
        }
        let (before, after) = l.split_once(" waits — the seat is at its cap")?;
        let what = before.rsplit_once(": ").map_or(before, |(_, w)| w);
        names(what, who).then(|| after.to_string())
    });
    // The cycle log said the barrier was held: by an open question, a verifier,
    // a member (`group N: barrier waits: …`). That is what the stretch was, not
    // a barrier running (`compounding-468`).
    let hold = crate::barrier::parse_label(who).and_then(|(list, at)| {
        let head = format!("{list} group {at}: barrier waits: ");
        i.cycle.iter().filter(|(t, _)| *t >= from && *t <= to).filter_map(|(_, l)| l.split_once(&head).map(|(_, w)| w.trim().to_string())).last()
    });
    if let Some(held) = hold.filter(|h| !h.is_empty()) {
        let held = cut_at_word(&held, 300);
        format!("{}{held}", crate::wallbuckets::BARRIER_HOLD)
    } else if let Some(after) = cap {
        let held = cap_holders(i, &after, to);
        if held.is_empty() { "the seat's cap (a place to come free)".into() } else { format!("the seat's cap (a place to come free) — held by {held}") }
    } else if let Some(park) = i.parks.iter().filter(|p| parked_within(p, from, to) * 2 > span).max_by_key(|p| parked_within(p, from, to)) {
        // A person parked the whole list and nothing ran: the park is what it
        // waited on, by whom and why, not nothing the logs name (`wsp-645`:
        // wsp-635's 65m between its check and the verifier's spawn).
        format!("{PARKED_LIST} — {}", park.words())
    } else if let Some((_, _, why)) = (span > TICK + HAND_OFF && who == i.id).then(|| capped_reopen(i, from, to)).flatten() {
        // The log has the reopen and the cap: that is what the step waited on.
        why
    } else if span <= HAND_OFF {
        "the hand-off (a poke and a spawn)".into()
    } else if span <= TICK + HAND_OFF {
        format!("a daemon tick (every {TICK}s) — nothing poked the step")
    } else {
        // Named for its row, so the bucket in a list's report says whose.
        format!("nothing the logs name — the step of {who} did not run for longer than a tick")
    }
}

/// The cause of a stretch a person's park of the whole list covered.
pub(crate) const PARKED_LIST: &str = "the list parked";

/// The cause of the stretch from a leg's seat ending to the leg's close
/// (`wsp-016`): the seat came up, worked and ended, and the leg's line stayed
/// `running` with nobody in it. Not work.
pub(crate) const LEG_SEAT_ENDED: &str = "a leg's seat ended, the leg not closed";

/// The cause of the stretch from a reopen of a row at the leg cap to the next
/// seat on it (`wsp-016`, the case in `wsp-011`): no rework leg can be added and
/// the run seats no member, so nothing runs until a person starts one.
pub(crate) const CAPPED_REOPEN: &str = "reopened at the leg cap with no member seated";

/// The seat that came up for the row at `up` (its `session-learned`), and when
/// it ended, where the cycle log says: the time its exit was accepted, or the
/// line that ended it.
fn seat_ended_after(i: &Inputs, up: i64) -> Option<(String, i64)> {
    let pane = i.kind("session-learned", &i.id).into_iter().find(|(t, _)| *t == up).and_then(|(_, d)| d["pane"].as_str().map(str::to_string))?;
    let exit = format!("{} ", crate::ending::EXIT_ENDED);
    let named = format!(", {pane})");
    let ended = format!("ended {pane}");
    let at = i.cycle.iter().filter(|(t, _)| *t >= up).find_map(|(t, l)| {
        if let Some((_, rest)) = l.split_once(&exit) {
            let (at, tail) = rest.split_once(" (")?;
            let accepted = util::epoch_of(at);
            // The exit was accepted before the line that ended its seat.
            let when = if accepted >= up && accepted <= *t { accepted } else { *t };
            return tail.split_once(')').is_some_and(|(who, _)| format!(", {who})").ends_with(&named)).then_some(when);
        }
        (l == &ended || l.starts_with(&format!("{ended},"))).then_some(*t)
    })?;
    Some((pane, at))
}

/// The seat the row's log names for leg `name` started at `s` and closed at
/// `f`: the last one started before the close whose end, if it has one, is
/// not before the start. A seat that ended before the leg started was an
/// earlier run of it.
fn leg_seat<'a>(i: &'a Inputs, name: &str, s: i64, f: i64) -> Option<&'a LegSeat> {
    i.leg_seats.iter().filter(|l| l.leg == name && !l.pane.is_empty() && l.start <= f && l.end.is_none_or(|e| e >= s)).last()
}

/// The first reopen of the row at the leg cap inside `from..to`: the reopen,
/// where the stretch with no member seated ends (the next claim or seat up,
/// else `to`), and its cause. A second reopen before a seat is the same wait.
fn capped_reopen(i: &Inputs, from: i64, to: i64) -> Option<(i64, i64, String)> {
    if i.legs.iter().filter(|l| !l.skipped).count() < crate::legs::MAX_LEGS {
        return None;
    }
    let id = i.id.as_str();
    let at = i.events.iter().filter(|(t, k, d)| (k == "task-reopen" || k == "task-reopened") && d["id"] == id && *t >= from && *t < to).map(|(t, _, _)| *t).min()?;
    let seated = i.events.iter().filter(|(t, k, d)| (k == "task-claimed" || k == "session-learned") && d["id"] == id && *t >= at).map(|(t, _, _)| *t).min();
    let end = seated.map_or(to, |s| s.min(to));
    let until = match seated.filter(|s| *s <= to) {
        Some(s) => format!("until a seat was claimed at {}", clock(s)),
        None => format!("still unseated at {}", clock(to)),
    };
    Some((at, end, format!("{CAPPED_REOPEN} — reopened at {} at the {}-leg cap: no rework leg could be added and the run seats no member, {until}", clock(at), crate::legs::MAX_LEGS)))
}

/// Seconds of `from..to` a park covers.
fn parked_within(p: &Park, from: i64, to: i64) -> i64 {
    (p.end.min(to) - p.start.max(from)).max(0)
}

/// One place a cap line lists after its colon (`x in cpd-1, y starting, 2
/// more`), and what kind of seat it is.
enum Held {
    /// A row's work seat.
    Work(String),
    /// A row's step seat (`r · verifying`).
    Step(String),
    /// A list's barrier check seat (`list · barrier 2`): the label and the list.
    Check(String, String, usize),
    /// `N more`: places nobody named, so nothing to say of them.
    More,
}

fn held_in(after: &str) -> Vec<(String, Held)> {
    let list = after.split_once(": ").map_or("", |(_, l)| l);
    list.split(", ")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|item| {
            let what = item.strip_suffix(" starting").or_else(|| item.rsplit_once(" in ").map(|(w, _)| w)).unwrap_or(item);
            let held = if item.strip_suffix(" more").is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())) {
                Held::More
            } else if let Some((list, at)) = crate::barrier::parse_label(what) {
                Held::Check(what.trim().to_string(), list.to_string(), at)
            } else if let Some((row, _)) = what.split_once(" · ") {
                Held::Step(row.to_string())
            } else {
                Held::Work(what.to_string())
            };
            (item.to_string(), held)
        })
        .collect()
}

/// The holders a cap line lists, each with whether its seat had finished by
/// `to`: a work seat finishes at its row's review or release, a step's seat
/// (`r · verifying`) at the row's verdict, a barrier check seat when its
/// barrier passed.
fn cap_holders(i: &Inputs, after: &str, to: i64) -> String {
    let mut out = Vec::new();
    for (item, held) in held_in(after) {
        let ended = |pred: &dyn Fn(&str, &str) -> bool| {
            i.holder_events.iter().filter(|(t, k, r)| *t <= to && pred(k, r)).map(|(t, _, _)| *t).min()
        };
        let ended = match &held {
            Held::More => {
                out.push(item);
                continue;
            }
            Held::Work(row) => ended(&|k, r| r == row && (k == "task-review" || k == "task-released")),
            Held::Step(row) => ended(&|k, r| r == row && k == "task-verified"),
            Held::Check(label, _, _) => ended(&|k, r| r == label && k == "barrier-passed"),
        };
        out.push(match ended {
            Some(t) => format!("{item} (finished {})", clock(t)),
            None => format!("{item} (running)"),
        });
    }
    out.join(", ")
}

/// When the row's own seat was asked for.
fn spawn_line(i: &Inputs) -> Option<i64> {
    let id = i.id.as_str();
    i.first_line(0, |l| {
        (l.contains(&format!("starting {id} on ")) || (l.starts_with(&format!("spawn {id} --agent")) && !l.contains("--leg") && !l.contains("--verify")))
            || l.contains(&format!(": starting {id} on "))
    })
}

const VERIFIER_START: &str = "verifier seat start (spawn → its first transcript line)";
const VERIFIER_START_LINE: &str = "verifier seat start (spawn → wsp's started line; the seat's first line unread, so this lags it)";
const VERIFIER_HELD: &str = "verdict → verifier seat ended (clean-up, alongside what follows)";

/// One seating of a row's seat (`compounding-217`): the claim, and the release
/// that let the row be claimed again. A row released and started again is two of
/// these, and what lies between them is neither the first seat finishing nor the
/// second one idle.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Attempt {
    /// The spawn request that made this seat, where the cycle log has it.
    pub spawn: Option<i64>,
    pub claim: i64,
    /// The release that ended it; `None` for the last.
    pub end: Option<i64>,
    /// Its first review, when it got that far.
    pub review: Option<i64>,
}

impl Attempt {
    /// Where the seat's own span begins: its spawn request, else its claim.
    pub(crate) fn begin(&self) -> i64 {
        self.spawn.unwrap_or(self.claim)
    }
}

/// The cause of the stretch between an attempt that was released before it
/// reached review and the next one.
pub(crate) const PARKED_CAUSE: &str = "the row parked or released between attempts";
/// The cause of the stretch between a row's own checks that timed out or failed
/// and the rerun, after its list resumed: the check waiting for the load to fall.
pub(crate) const CHECK_WAIT_CAUSE: &str = "the row's check timed out and waited to rerun";
/// The cause of the stretch from an attempt's review to the next attempt: the
/// row was sent back and reopened. The row's `rework` block books the attempt.
pub(crate) const REWORK_CAUSE: &str = "rework: the row sent back after review and reopened";
/// The cause of a stretch after a review where the row held for something only
/// the person could do (`wsp-311`): a decision in force on the row that says it
/// holds for the person, or a question the seat put to the person and had no
/// answer to. Not rework: no verdict asked the seat to change anything.
pub(crate) const PERSON_STEP_CAUSE: &str = "the row waits for a person's step";

/// Whether a decision says the row holds for the person: it names the person
/// and says `hold`, or that it waits until the person or for the person
/// (`wsp-303`'s d5: "hold until the person detaches").
/// A message's `for` that names the person: `"person"`, or `"person:<user>"` of
/// an allocated row's question escalated past its user's primary (`wsp-418`).
fn for_the_person(f: &serde_json::Value) -> bool {
    f.as_str().is_some_and(|s| s == "person" || s.starts_with("person:"))
}

fn holds_for_person(text: &str) -> bool {
    let t = text.to_lowercase();
    if !t.contains("person") {
        return false;
    }
    t.split(|c: char| !c.is_alphanumeric()).any(|w| w == "hold")
        || ["until the person", "waits on the person", "waiting on the person", "waits for the person", "waiting for the person"].iter().any(|p| t.contains(p))
}

/// The person's step inside the stretch `from..to` after a review, `(start, end,
/// words)` (`wsp-311`). `since` is the claim of the attempt that was reviewed: a
/// decision taken in its seat counts. A decision in force that says the row
/// holds for the person holds it from the first such decision to the next seat,
/// named by the latest one (the person's step, done, is what lets the row be
/// seated again). Else a question the seat put to the person, unanswered when
/// the review was typed, holds it from the question to its answer or the next
/// seat. `None` where neither says so: the stretch is rework.
fn person_step(i: &Inputs, since: i64, from: i64, to: i64) -> Option<(i64, i64, String)> {
    let at = |t: i64| util::iso_at(t).get(11..16).unwrap_or_default().to_string();
    let holds: Vec<&(i64, String, String)> = i.decisions.iter().filter(|(t, _, x)| *t >= since && *t < to && holds_for_person(x)).collect();
    if let (Some(first), Some(last)) = (holds.first(), holds.last()) {
        let text = cut_at_word(&last.2, 160);
        let id = if last.1.is_empty() { "a decision".to_string() } else { format!("decision {}", last.1) };
        let words = format!("{id} at {}: {text}, until the next seat was claimed at {}", at(last.0), at(to));
        return (first.0.max(from) < to).then(|| (first.0.max(from), to, words));
    }
    let id = i.id.as_str();
    let msgs: Vec<&(i64, String, Value)> = i.events.iter().filter(|(_, k, d)| k == "message-raised" && d["about"]["task"] == id).collect();
    for (t, _, d) in msgs.iter().filter(|(t, _, d)| *t >= since && *t < to && d["shape"] == "question" && for_the_person(&d["for"]) && d["from"]["pane"].is_string()) {
        let Some(mid) = d["id"].as_str() else { continue };
        let answer = msgs.iter().filter(|(a, _, r)| *a > *t && r["reply_to"] == mid).map(|(a, _, _)| *a).min();
        if answer.is_some_and(|a| a <= from) {
            continue;
        }
        let (start, end) = ((*t).max(from), answer.unwrap_or(to).min(to));
        if start >= end {
            continue;
        }
        let until = match answer.filter(|a| *a < to) {
            Some(a) => format!("until the person answered at {}", at(a)),
            None => format!("unanswered when the next seat was claimed at {}", at(to)),
        };
        return Some((start, end, format!("question {mid} to the person, asked at {}, {until}", at(*t))));
    }
    None
}

/// The attempts of a row's seat, in order. A release counts as the end of one
/// only when the row was claimed again afterwards and before its last passing
/// verdict (a claim after that is a reopen of a landed row, which is not this
/// run's wall). A row with legs started is one attempt: its release is the seat
/// handing the row to them.
pub(crate) fn attempts(i: &Inputs) -> Vec<Attempt> {
    let id = i.id.as_str();
    let mut claims: Vec<i64> = i.kind("task-claimed", id).into_iter().map(|(t, _)| t).collect();
    claims.sort_unstable();
    let Some(&first) = claims.first() else { return Vec::new() };
    let spawn_pred = |l: &str| {
        l.contains(&format!("starting {id} on ")) || (l.starts_with(&format!("spawn {id} --agent")) && !l.contains("--leg") && !l.contains("--verify"))
    };
    let reviews: Vec<i64> = i.kind("task-review", id).into_iter().map(|(t, _)| t).collect();
    let single = |claim: i64| Attempt { spawn: spawn_line(i), claim, end: None, review: reviews.iter().copied().find(|r| *r >= claim) };
    if i.legs.iter().any(|l| !l.skipped) {
        return vec![single(first)];
    }
    let cut = i.kind("task-verified", id).into_iter().filter(|(_, d)| d["verdict"] == "holds").map(|(t, _)| t).max().unwrap_or(i64::MAX);
    let mut releases: Vec<i64> = i.kind("task-released", id).into_iter().map(|(t, _)| t).collect();
    releases.sort_unstable();
    let mut out: Vec<Attempt> = Vec::new();
    let mut claim = first;
    let mut from = 0i64;
    loop {
        let next = releases.iter().copied().filter(|r| *r >= claim).find_map(|r| claims.iter().copied().find(|c| *c > r && *c < cut).map(|c| (r, c)));
        let spawn = if out.is_empty() { spawn_line(i) } else { i.first_line(from, spawn_pred).filter(|s| *s <= claim) };
        let review = reviews.iter().copied().find(|r| *r >= claim && next.is_none_or(|(e, _)| *r <= e));
        match next {
            Some((end, again)) => {
                out.push(Attempt { spawn, claim, end: Some(end), review });
                claim = again;
                from = end;
            }
            None => {
                out.push(Attempt { spawn, claim, end: None, review });
                return out;
            }
        }
    }
}

/// What the stretch from `prev`'s end to the next attempt was, for the row.
fn between_attempts(i: &Inputs, prev: &Attempt, to: i64) -> (String, String) {
    let id = i.id.as_str();
    let at = |t: i64| util::iso_at(t).get(11..16).unwrap_or_default().to_string();
    // A person parking the whole list releases every seat in it and writes no
    // line on any row: the list's park record says it. A park that covers most of
    // the stretch is what it was, whether the row was released or sent back and
    // reopened (`compounding-464`).
    let from = prev.review.unwrap_or_else(|| prev.end.unwrap_or(prev.claim));
    let parked_for = |p: &Park| (p.end.min(to) - p.start.max(from)).max(0);
    if let Some(p) = i.parks.iter().filter(|p| parked_for(p) * 2 > to - from).max_by_key(|p| parked_for(p)) {
        return (format!("parked at {} → seat started again", at(p.start.max(from))), format!("{PARKED_CAUSE} — {}, until the seat was started again at {}", p.words(), at(to)));
    }
    if let Some(rv) = prev.review {
        return (
            format!("reviewed at {} → sent back → seat started again", at(rv)),
            format!("{REWORK_CAUSE} (the next seat was claimed at {})", at(to)),
        );
    }
    let end = prev.end.unwrap_or(prev.claim);
    // What the row rested on: the latest `after` set in the stretch that names a
    // row; the park, where the log wrote one.
    let waits: Option<String> = i
        .kind("task-after", id)
        .into_iter()
        .filter(|(t, _)| *t >= prev.claim && *t <= to)
        .max_by_key(|(t, _)| *t)
        .and_then(|(_, d)| d["after"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")))
        .filter(|s| !s.is_empty());
    let parked = i.kind("task-parked", id).into_iter().any(|(t, _)| t >= prev.claim && t <= end);
    let head = if parked { "parked" } else { "released" };
    let why = match waits {
        Some(w) => format!("{head}, waiting on {w}, until it was started again at {}", at(to)),
        None => format!("{head}, waiting to be restarted, until {}", at(to)),
    };
    (format!("{head} at {} → seat started again", at(end)), format!("{PARKED_CAUSE} — {why}"))
}

/// The stretch `from..to` between two attempts, as phases (`wsp-645`). A stretch
/// that is not the park-or-release one (a rework stretch) stays one gap. The row's
/// own checks that timed out under load and were rerun (`task-checked` with state
/// `waiting`) ran in it: each is work, from its start to the event. A park that
/// covers most of it ends where the list resumed: what follows the resume is the
/// row waiting to be started again, not the park (`wsp-629`: the park ended at
/// 12:58, and 40m more of checks and a restart were booked to it).
fn between_phases(i: &Inputs, from: i64, to: i64, label: String, why: String) -> Vec<Phase> {
    let mut out = Vec::new();
    if to <= from {
        return out;
    }
    // Only a parked or released stretch has checks of the row's own in it. A
    // sent-back stretch (review, reopened) holds the verifier's checks, and the
    // wait after one is the rework wait, not the check waiting to rerun.
    if !why.starts_with(PARKED_CAUSE) {
        out.push(Phase::gap(label, from, to, why));
        return out;
    }
    let at = |t: i64| util::iso_at(t).get(11..16).unwrap_or_default().to_string();
    // Where the park the cause names ends, when it ends inside the stretch.
    let resumed = i
        .parks
        .iter()
        .filter(|p| !p.open && why.contains(&p.words()))
        .map(|p| p.end)
        .find(|e| *e > from && *e < to);
    let mut cursor = from;
    let (mut label, mut why) = (label, why);
    if let Some(r) = resumed {
        out.push(Phase::gap(label, from, r, why));
        cursor = r;
        label = format!("resumed at {} → seat started again", at(r));
        why = format!("{PARKED_CAUSE} — the list resumed at {}, and the row was not started again until {}", at(r), at(to));
    }
    // The row's own checks that did not pass (`waiting`: timed out under load, to
    // be rerun; `failed`): each ran from its start to the event, and what lies
    // between one and the next, or the next claim, is the row waiting to rerun
    // it, not the park nor a release.
    let mut checks: Vec<(i64, i64, &Value)> = i
        .kind("task-checked", &i.id)
        .into_iter()
        .filter(|(t, d)| *t > cursor && *t <= to && (d["state"] == "waiting" || d["state"] == "failed"))
        .map(|(t, d)| (t, d["took"].as_i64().unwrap_or(0).max(0), d))
        .collect();
    checks.sort_by_key(|(t, _, _)| *t);
    let wait_why = check_wait_cause(&checks, to - checks.first().map_or(to, |c| (c.0 - c.1).max(cursor)));
    let mut first = true;
    for (c, took, event) in checks {
        let start = (c - took).max(cursor);
        if start > cursor {
            if first {
                out.push(Phase::gap(label.clone(), cursor, start, why.clone()));
            } else {
                out.push(Phase::gap("waiting to rerun the check".to_string(), cursor, start, wait_why.clone()));
            }
        }
        first = false;
        let what = if event["state"] == "failed" { "the row's own check failed" } else { "the row's own check timed out under load, to be rerun once the load falls" };
        out.push(Phase::work(format!("{what}{}", check_start_note(event)), start, c));
        cursor = c;
    }
    if to > cursor {
        if first {
            out.push(Phase::gap(label, cursor, to, why));
        } else {
            out.push(Phase::gap("waiting to rerun the check → seat started again".to_string(), cursor, to, wait_why));
        }
    }
    out
}

/// What the row waited on between its checks: they timed out or failed and were
/// rerun when the load fell (`wsp-645`). Counted by how many, over how long, and
/// the load at the first one's start and the last one's end.
fn check_wait_cause(checks: &[(i64, i64, &Value)], secs: i64) -> String {
    let load = |v: &Value| v.as_f64().map(|l| format!("{l:.0}"));
    let a = checks.first().and_then(|c| load(&c.2["start"]["load"]));
    let b = checks.last().and_then(|c| load(&c.2["load_end"]));
    let loads = match (a, b) {
        (Some(a), Some(b)) => format!(", load {a} → {b}"),
        _ => String::new(),
    };
    format!("{CHECK_WAIT_CAUSE} ({} checks, {}{loads})", checks.len(), util::duration_human(secs))
}

/// The phases of a row, in time order.
pub(crate) fn build(i: &Inputs) -> Vec<Phase> {
    let id = i.id.as_str();
    let mut out: Vec<Phase> = Vec::new();
    let claimed = i.kind("task-claimed", id).first().map(|(t, _)| *t);
    let reviews: Vec<i64> = i.kind("task-review", id).into_iter().map(|(t, _)| t).collect();
    // A release before the first review is a seat handing its row to legs. One
    // after it is the seat's end, long after its verdict: it is not where the
    // seat's work stopped (`wsp-429`: compounding-114 and 117 read as one seat
    // phase to the release, with the verifier and the barrier inside it).
    let released = i.kind("task-released", id).first().map(|(t, _)| *t).filter(|t| reviews.first().is_none_or(|r| t <= r));
    let released_last = i.kind("task-released", id).last().map(|(t, _)| *t);
    let learned = i.kind("session-learned", id).first().map(|(t, _)| *t);

    // Spawn → the seat is up → its first request.
    let spawn = spawn_line(i);
    let begin = spawn.or(claimed);
    if let (Some(s), Some(c)) = (spawn, claimed) {
        out.push(Phase::work("spawn requested → seat claimed", s, c));
    }
    if let (Some(c), Some(l)) = (claimed, learned) {
        out.push(Phase::work("seat up (session started)", c, l));
    }
    let work_from = match (learned, i.first_request) {
        (Some(l), Some(r)) if r >= l => {
            out.push(Phase::work("warm-up → first request", l, r));
            Some(r)
        }
        (l, _) => l.or(claimed),
    };
    let seat_end = released.or(reviews.first().copied());
    let seated = attempts(i);
    let of = |n: usize| if seated.len() > 1 { format!(", attempt {n} of {}", seated.len()) } else { String::new() };
    if let (Some(a), Some(b)) = (work_from, seat_end) {
        out.push(Phase::work(format!("{}{}", if released.is_some() { "the seat's work until release" } else { "the seat's work until review" }, of(1)), a, b));
    }
    // A row released and started again (`compounding-217`): each seat gets its
    // own spawn → release or review, and the stretch between them is named for
    // what it was — the row parked or waiting to be restarted, or sent back after
    // a review — not the first seat finishing, nor the second one idle.
    let mut attempts_end: Option<i64> = None;
    for (k, a) in seated.iter().enumerate().skip(1) {
        let prev = &seated[k - 1];
        let Some(from) = prev.review.or(prev.end) else { continue };
        let (label, why) = between_attempts(i, prev, a.claim);
        // A stretch after a review that held for the person is the person's step
        // (`wsp-311`): only what lies outside it stays rework.
        match person_step(i, prev.claim, from, a.begin()).filter(|_| why.starts_with(REWORK_CAUSE)) {
            Some((s, e, words)) => {
                let at = |t: i64| util::iso_at(t).get(11..16).unwrap_or_default().to_string();
                if s > from {
                    out.push(Phase::gap(label.clone(), from, s, why.clone()));
                }
                out.push(Phase::gap(format!("held for a person's step from {} → {}", at(s), if e < a.begin() { "answered" } else { "seat started again" }), s, e, format!("{PERSON_STEP_CAUSE} — {words}")));
                if e < a.begin() {
                    out.push(Phase::gap(format!("the person answered at {} → seat started again", at(e)), e, a.begin(), why));
                }
            }
            None => out.extend(between_phases(i, from, a.begin(), label, why)),
        }
        if let Some(s) = a.spawn {
            out.push(Phase::work(format!("spawn requested → seat claimed{}", of(k + 1)), s, a.claim));
        }
        let up = i.first_learned_after(a.claim).filter(|u| a.end.is_none_or(|e| *u <= e));
        if let Some(u) = up {
            out.push(Phase::work(format!("seat up (session started){}", of(k + 1)), a.claim, u));
        }
        let end = a.end.or(a.review);
        if let Some(e) = end {
            out.push(Phase::work(format!("the seat's work until {}{}", if a.end.is_some() { "release" } else { "review" }, of(k + 1)), up.unwrap_or(a.claim), e));
            attempts_end = Some(e);
        }
    }

    // Legs.
    let mut done: Vec<(String, i64)> = Vec::new();
    let mut last_end = attempts_end.or(released);
    for (n, leg) in i.legs.iter().enumerate() {
        if leg.skipped {
            continue;
        }
        let added = i.events.iter().find(|(_, k, d)| k == "leg-added" && d["id"] == id && d["leg"] == leg.name.as_str()).map(|(t, _, _)| *t);
        let waits: Vec<String> = if leg.after.is_empty() && n > 0 && !i.legs[n - 1].skipped { vec![i.legs[n - 1].name.clone()] } else { leg.after.clone() };
        let started = i.first_line(added.unwrap_or(0), |l| l.contains(&format!("starting leg {} of {id}", leg.name)));
        let Some(s) = started else { continue };
        // The waits as the row has them now: one that finished after this leg
        // had started was edited since (`wsp leg after`), and is not what held
        // it back.
        let mut ready = added.unwrap_or(0).max(released.unwrap_or(0));
        for w in &waits {
            if let Some((_, t)) = done.iter().find(|(d, t)| d == w && *t <= s) {
                ready = ready.max(*t);
            }
        }
        // The leg that ends the row ends with the review, not a `leg done`.
        let done_event = i.events.iter().find(|(t, k, d)| k == "leg-done" && d["id"] == id && d["leg"] == leg.name.as_str() && *t >= s);
        // A close that came late (`wsp-372`) carries when the baton was written:
        // the leg's work ended then, and the rest is the stall, not work.
        let baton_at = done_event.and_then(|(t, _, d)| d["baton_at"].as_i64().filter(|b| *b >= s && b < t));
        let finished = baton_at
            .or_else(|| done_event.map(|(t, _, _)| *t))
            .or_else(|| reviews.iter().copied().find(|r| *r >= s));
        if ready > 0 && s > ready {
            // A review typed between two legs ran the row's own check, which
            // failed and sent the row back: that run is work, not a wait
            // (`compounding-506`: 299s and 650s booked as nothing the logs name).
            let mut from = ready;
            for (r, c, event) in own_checks(i, &reviews, ready, s) {
                if r > from {
                    out.push(Phase::gap(format!("gap before leg {}", leg.name), from, r, wait_name(i, from, r, id)));
                }
                out.push(Phase::work(format!("the row's own check (wsp check, run by review — sent the row back){}", check_start_note(event)), r, c));
                from = c;
            }
            if s > from {
                let label = if from > ready { format!("row's check ended → leg {} started", leg.name) } else { format!("gap before leg {}", leg.name) };
                out.push(Phase::gap(label, from, s, wait_name(i, from, s, id)));
            }
        }
        // A seat that came up only after the leg had ended is not this leg's.
        let up = i.first_learned_after(s).filter(|u| finished.is_none_or(|f| *u <= f));
        // No `session-learned`, but the leg's own entry names the seat that ran
        // it (`wsp-335`: wsp-030's six legs, 1614 s booked as no seat): the leg
        // is work until that seat ended, and only the rest is the gap.
        if let Some((f, seat)) = finished.filter(|_| up.is_none()).and_then(|f| leg_seat(i, &leg.name, s, f).map(|l| (f, l))) {
            let label = format!("leg {} (seat {}, from its entry)", leg.name, seat.pane);
            match seat.end.filter(|e| *e < f) {
                Some(e) => {
                    out.push(Phase::work(label, s, e.max(s)));
                    out.push(Phase::gap(
                        format!("leg {}: its seat {} ended at {}, the leg not closed", leg.name, seat.pane, clock(e)),
                        e.max(s),
                        f,
                        format!("{LEG_SEAT_ENDED} — {} ended at {} (not work)", seat.pane, util::iso_at(e)),
                    ));
                }
                None => out.push(Phase::work(label, s, f)),
            }
            done.push((leg.name.clone(), f));
            last_end = Some(last_end.map_or(f, |e| e.max(f)));
            continue;
        }
        if let (None, Some(f)) = (up, finished) {
            // No seat ever came up for the leg: the stretch is not work.
            out.push(Phase::gap(format!("leg {} (no seat came up)", leg.name), s, f, format!("leg {}: no seat came up (not work)", leg.name)));
            done.push((leg.name.clone(), f));
            last_end = Some(last_end.map_or(f, |e| e.max(f)));
            continue;
        }
        let label = match up {
            Some(u) => format!("leg {} (seat up after {}s)", leg.name, u - s),
            None => format!("leg {}", leg.name),
        };
        if let Some(f) = finished {
            // A leg runs only while its seat is alive: a seat that ended before
            // the leg was closed leaves the rest of the leg's line `running`
            // with nobody in it (wsp-001's `finish`: cpd-121 ended at its Stop,
            // the line read `running` an hour more).
            match up.and_then(|u| seat_ended_after(i, u)).filter(|(_, e)| *e < f) {
                Some((seat, e)) => {
                    out.push(Phase::work(label, s, e));
                    out.push(Phase::gap(
                        format!("leg {}: its seat {seat} ended at {}, the leg not closed", leg.name, clock(e)),
                        e,
                        f,
                        format!("{LEG_SEAT_ENDED} — {seat} ended at {} (not work)", util::iso_at(e)),
                    ));
                }
                None => out.push(Phase::work(label, s, f)),
            }
            done.push((leg.name.clone(), f));
            last_end = Some(last_end.map_or(f, |e| e.max(f)));
            if let (Some(b), Some((closed, _, d))) = (baton_at, done_event) {
                let by = d["closed_by"].as_str().map(str::to_string).unwrap_or_else(|| "the leg's own seat".into());
                out.push(Phase::gap(
                    format!(
                        "closed late: leg {} done at {}, closed at {}",
                        leg.name,
                        util::iso_at(b).get(11..16).unwrap_or_default(),
                        util::iso_at(*closed).get(11..16).unwrap_or_default()
                    ),
                    b,
                    *closed,
                    format!("the close — {by} (not work)"),
                ));
            }
        }
    }

    // Review → verifier → verdict.
    // The last review that a verdict followed: a row typed `wsp review` again
    // after it had landed (to say what it did for the barrier, say) has a later
    // review nothing verified.
    let verdicts: Vec<i64> = i.kind("task-verified", id).into_iter().map(|(t, _)| t).collect();
    let review = reviews.iter().copied().rev().find(|r| verdicts.iter().any(|v| v >= r)).or_else(|| reviews.last().copied());
    if let (Some(r), Some(e)) = (review, last_end) {
        if r > e {
            let finishing = || "the row's own seat finishing (not wsp)".to_string();
            match capped_reopen(i, e, r).filter(|(at, end, _)| end > at) {
                // A reopen at the leg cap in it: from the reopen to the next seat
                // nobody was on the row, and the log says why (`wsp-016`).
                Some((at, end, why)) => {
                    if at > e {
                        out.push(Phase::gap("after the last leg → reopened", e, at, finishing()));
                    }
                    out.push(Phase::gap("reopened at the leg cap → a seat started", at, end, why));
                    if r > end {
                        out.push(Phase::gap("seat started → review typed", end, r, finishing()));
                    }
                }
                None => out.push(Phase::gap("after the last leg → review typed", e, r, finishing())),
            }
        }
    }
    let verdict = verdicts.iter().copied().find(|t| review.is_none_or(|r| *t >= r));
    if let Some(r) = review {
        let spawned = i.first_line(r.saturating_sub(1), |l| l.starts_with(&format!("spawn {id} --agent --verify")));
        let up = spawned.and_then(|s| i.first_line(s, |l| l.contains(&format!("started {id} · verifying"))));
        if let Some(s) = spawned {
            // `wsp review` runs the row's own check before it spawns the verifier;
            // the last `task-checked` between the two ends it.
            let checked = i.kind("task-checked", id).into_iter().filter(|(t, _)| *t >= r && *t <= s).max_by_key(|(t, _)| *t);
            match checked {
                Some((c, event)) => {
                    out.push(Phase::work(format!("the row's own check (wsp check, run by review){}", check_start_note(event)), r, c));
                    if s > c {
                        out.push(Phase::gap("row's check ended → verifier spawn requested", c, s, wait_name(i, c, s, id)));
                    }
                }
                None => out.push(Phase::gap("review typed → verifier spawn requested", r, s, wait_name(i, r, s, id))),
            }
            // The seat's own first line, when its transcript was read; wsp's line
            // is the fallback, and says it lags.
            let first = i.verifiers.iter().map(|v| v.first).find(|f| *f >= s && verdict.is_none_or(|v| *f <= v));
            let started = first.map(|f| (f, VERIFIER_START)).or(up.map(|u| (u, VERIFIER_START_LINE)));
            if let Some((u, label)) = started {
                out.push(Phase::work(label, s, u));
                if let Some(v) = verdict {
                    out.push(Phase::work("verifier reads → verdict recorded (landed on a hold)", u, v));
                }
            }
        }
    }
    if let Some(v) = verdict {
        if let Some(e) = i.first_line(v, |l| l.starts_with("ended ") && l.contains(&format!("which verified {id}"))) {
            out.push(Phase::work(VERIFIER_HELD, v, e));
        }
    }

    // The seat released after its verdict: it stayed up, and was asked for the
    // barrier's fix or let go late. Alongside the barrier, which it overlaps.
    if let (Some(v), Some(rel)) = (verdict, released_last) {
        if rel > v && released != Some(rel) {
            let again = reviews.iter().copied().find(|r| *r > v);
            let label = match again {
                Some(r) => format!("the row's own seat after the verdict → released (reviewed again at {}; alongside what follows)", util::iso_at(r).get(11..16).unwrap_or_default()),
                None => "the row's own seat after the verdict → released (alongside what follows)".to_string(),
            };
            out.push(Phase::work(label, v, rel));
        }
    }

    // The barrier, for a list member.
    if let (Some((list, at, members)), Some(_)) = (&i.group, verdict) {
        let last = members
            .iter()
            .filter_map(|m| i.kind("task-verified", m).into_iter().map(|(t, _)| t).max())
            .max()
            .unwrap_or(0);
        let from = last.max(verdict.unwrap_or(0));
        let req = i.first_line(from.saturating_sub(1), |l| l.starts_with(&format!("spawn {list} --agent --check {at}")) || l.contains(&format!("barrier-run {at}")));
        let seat_up = i.first_line(from, |l| l.contains(&format!("started {list} · barrier {at}")));
        // The run's own line is written when it ENDS, with its elapsed seconds
        // (lock wait included): it began that long before.
        // Copies of a run share one line of the log, or follow one another; the
        // longest of those that end together began first, and is the run.
        let who = format!("{list} · barrier {at}");
        let head = format!("{list} group {at}: barrier run — ");
        // Every run of the group, not only the one after the last verdict: a
        // run that failed, a fix row's verdict, and the run after it are all
        // the barrier's wall (`wsp-568`). No run precedes the first verdict.
        let first_from = members
            .iter()
            .filter_map(|m| i.kind("task-verified", m).into_iter().map(|(t, _)| t).min())
            .min()
            .unwrap_or(from)
            .min(from);
        let verdicts_of_group: Vec<i64> = members.iter().flat_map(|m| i.kind("task-verified", m).into_iter().map(|(t, _)| t)).collect();
        let mut runs: Vec<(i64, &str)> = Vec::new();
        for (t, l) in i.cycle.iter().filter(|(t, l)| *t >= first_from && l.contains(&head)) {
            runs.extend(l.split(&head).skip(1).map(|seg| (*t, seg)));
        }
        // Copies of a run share one line of the log, or follow one another; the
        // longest of those that end together began first, and is the run.
        let mut found: Vec<(i64, &str)> = Vec::new();
        let mut k = 0;
        while k < runs.len() {
            let f = runs[k].0;
            let mut n = k;
            while n < runs.len() && runs[n].0 <= f + 120 {
                n += 1;
            }
            if let Some(r) = runs[k..n].iter().max_by_key(|(_, seg)| run_seconds(seg).unwrap_or(0)).copied() {
                found.push(r);
            }
            k = n;
        }
        let pass = i.first_line(from, |l| l.starts_with(&format!("tell {list}: Barrier {at} of {list}")));
        let others = if last > verdict.unwrap_or(0) { " (other members' verdicts came later)" } else { "" };
        if let Some(&(end, _)) = found.last() {
            let mut prev = first_from.max(0).min(from);
            let begins: Vec<i64> = {
                let mut at_least = prev;
                found
                    .iter()
                    .map(|(e, line)| {
                        let st = (e - run_seconds(line).unwrap_or(0)).max(at_least);
                        at_least = *e;
                        st
                    })
                    .collect()
            };
            for (n, (e, line)) in found.iter().enumerate() {
                let start = begins[n];
                // The wait before a run, from the last verdict, is the barrier's;
                // what lies between a failed run and that wait is the follow-up's
                // own time, in no barrier phase (`wsp-568`).
                // The first run's wait begins at the last verdict before it
                // started, not the first: until then a member is still at work,
                // its legs running (`wsp-218`: group 4's 2051s were
                // compounding-204's legs). A verdict after the run is the
                // follow-up's own time.
                let waited_from = if n == 0 { verdicts_of_group.iter().copied().filter(|v| *v <= start).max().unwrap_or(prev).max(prev) } else { from.max(prev) };
                if waited_from < start && (n == 0 || from < start) {
                    out.push(Phase::gap(format!("last verdict → barrier {at} run started{others}"), waited_from, start, wait_name(i, waited_from, start, &who)).shared());
                }
                out.push(Phase::work(run_label(*at, line), start, *e).shared());
                // A run that failed had its own check seat, before the next
                // run: from its start to the line that settles the barrier
                // (the follow-ups added), whether or not a row joined after.
                if let Some(next) = begins.get(n + 1) {
                    let is_seat = |l: &str| l.starts_with(&format!("spawn {list} --agent --check {at}")) || l.contains(&format!("barrier-run {at}")) || l.contains(&format!("started {list} · barrier {at}"));
                    let up = i.first_line(*e, is_seat).filter(|b| b < next);
                    let settled = up.and_then(|u| i.first_line(u, |l| l.contains(&format!("{list}: the check in ")) && l.contains("settled its barrier"))).filter(|d| d <= next);
                    if let (Some(u), Some(d)) = (up, settled) {
                        if u > *e {
                            out.push(Phase::gap(format!("barrier {at} run ended → check seat started"), *e, u, wait_name(i, *e, u, &who)).shared());
                        }
                        out.extend(check_seat(i, format!("barrier {at} check seat → settled (follow-ups added)"), u, d, &list, *at));
                    }
                }
                prev = *e;
            }
            // A check seat after the run: its own wait, then its work.
            let seat = req.or(seat_up).filter(|b| *b >= end);
            if let Some(b) = seat {
                if b > end {
                    out.push(Phase::gap(format!("barrier {at} run ended → check seat started"), end, b, wait_name(i, end, b, &who)).shared());
                }
                if let Some(p) = pass.filter(|p| *p >= b) {
                    out.extend(check_seat(i, format!("barrier {at} check seat → passed"), b, p, &list, *at));
                }
            } else if let Some(p) = pass.filter(|p| *p >= end) {
                out.push(Phase::work(format!("barrier {at} run ended → passed"), end, p).shared());
            }
        } else if let Some(b) = req.or(seat_up) {
            out.push(Phase::gap(format!("last verdict → barrier {at} started{others}"), from, b, wait_name(i, from, b, &who)).shared());
            if let Some(p) = pass.filter(|p| *p >= b) {
                out.push(Phase::work(format!("barrier {at} runs → passed"), b, p).shared());
            }
        }
    }
    let _ = begin;
    out.sort_by_key(|p| (p.start, p.end));
    out
}

/// A barrier's check seat from `from` to `to`, less the stretches its list
/// stood held inside it (`wsp-090`): a check that holds the barrier for a
/// person's step has done its work, and the night until a person's `go` is the
/// group waiting on that step under its own cause, not the check seat's time
/// (s0-state-core's barrier 3 booked 741 min as its check seat, 23 of them the
/// seat's).
fn check_seat(i: &Inputs, label: String, from: i64, to: i64, list: &str, at: usize) -> Vec<Phase> {
    let hhmm = |t: i64| util::iso_at(t).get(11..16).unwrap_or_default().to_string();
    let mut out = Vec::new();
    let mut cur = from;
    for (a, b, why) in i.holds.iter().filter(|(a, b, _)| *b > from && *a < to) {
        let (written, b) = ((*a).max(cur), (*b).min(to));
        // The seat that wrote the hold stays the check seat until it stops.
        let a = seat_stop(i, list, at, written, b).map_or(written, |e| e.max(written));
        if b <= a {
            continue;
        }
        let stopped = if a > written { format!(" (the check seat stopped at {})", hhmm(a)) } else { String::new() };
        if a > cur {
            out.push(Phase::work(label.clone(), cur, a).shared());
        }
        let why = cut_at_word(why, 200);
        let until = if b < to { format!("started again at {}", hhmm(b)) } else { "still held at the pass".to_string() };
        let cause = format!(
            "{}{HELD_FOR_STEP} — barrier {at} of {list} held at {}{stopped}, {until}{}",
            crate::wallbuckets::BARRIER_HOLD,
            hhmm(written),
            if why.is_empty() { String::new() } else { format!(": {why}") }
        );
        out.push(Phase::gap(format!("barrier {at} held → started again"), a, b, cause).shared());
        cur = b;
    }
    if to > cur || out.is_empty() {
        out.push(Phase::work(label, cur, to).shared());
    }
    out
}

/// When the check seat open at `held` stopped, before `until`: the latest stop
/// its reading records for a seat of barrier `at` opened by then, else the first
/// `stopped in barrier check tree …<list>-check-<at>` line at or after `held`.
/// `None`: no seat was open, or none is read to have stopped.
fn seat_stop(i: &Inputs, list: &str, at: usize, held: i64, until: i64) -> Option<i64> {
    let read = i.check_ends.iter().filter(|(g, o, e)| *g == at && *o <= held && *e > held).map(|(_, _, e)| *e).max();
    let tree = format!("/{list}-check-{at}");
    let line = || i.first_line(held, |l| l.starts_with(STOPPED_CHECK) && l.split(':').next().is_some_and(|p| p.ends_with(&tree)));
    read.or_else(line).map(|e| e.min(until))
}

/// The line the sweep writes when it stops a barrier check tree's processes.
const STOPPED_CHECK: &str = "stopped in barrier check tree ";

/// What a barrier's hold is booked under, after [`crate::wallbuckets::BARRIER_HOLD`]:
/// the list held inside its check seat, for a person's step (`wsp-090`).
pub(crate) const HELD_FOR_STEP: &str = "held for a person's step";

/// The row's own check runs that began with a review inside `from..to`: each
/// review typed there, with the `task-checked` that ended its run before the
/// next review or `to`.
fn own_checks<'a>(i: &'a Inputs, reviews: &[i64], from: i64, to: i64) -> Vec<(i64, i64, &'a Value)> {
    let inside: Vec<i64> = reviews.iter().copied().filter(|r| *r >= from && *r <= to).collect();
    let checked = i.kind("task-checked", &i.id);
    let mut out = Vec::new();
    for (n, r) in inside.iter().enumerate() {
        let until = inside.get(n + 1).copied().unwrap_or(to);
        if let Some((c, event)) = checked.iter().copied().filter(|(t, _)| *t >= *r && *t <= until).max_by_key(|(t, _)| *t) {
            out.push((*r, c, event));
        }
    }
    out
}

/// What a `task-checked` event says of how the check began (`wsp-566`): the
/// state of its `target/` and the load, and each step that ran with its seconds.
fn check_start_note(event: &Value) -> String {
    let Some(start) = event.get("start") else { return String::new() };
    let target = start["target"].as_str().unwrap_or("?");
    let load = match (start["load"].as_f64(), start["cores"].as_u64()) {
        (Some(l), Some(c)) => format!(", load {l:.1} of {c} cores"),
        _ => String::new(),
    };
    let steps: Vec<String> = event["step_secs"]
        .as_array()
        .map(|a| a.iter().filter(|s| s["reused"] != true).filter_map(|s| Some(format!("{} {}s", s["name"].as_str()?, s["secs"].as_u64()?))).collect())
        .unwrap_or_default();
    let steps = if steps.is_empty() { String::new() } else { format!(" — per step: {}", steps.join("; ")) };
    format!(" — started on target {target}{load}{steps}")
}

/// The seconds a `barrier run —` segment reports: the run's elapsed time.
fn run_seconds(seg: &str) -> Option<i64> {
    let after = seg.split(" failed, ").nth(1)?;
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    if after[digits.len()..].starts_with('s') { digits.parse().ok() } else { None }
}

/// The run's label: its counts, and the seconds of each step that ran.
fn run_label(at: usize, seg: &str) -> String {
    let counts = match (seg.split(" failed, ").next(), run_seconds(seg)) {
        (Some(head), Some(secs)) => format!("{head} failed, {secs}s"),
        _ => seg.split(" · ").next().unwrap_or("").to_string(),
    };
    let steps = seg.split(" · per step: ").nth(1).map(|s| s.split(" · ").next().unwrap_or(s).trim_end()).map(|s| format!(" — per step: {s}")).unwrap_or_default();
    let start = seg.split(" · start: ").nth(1).map(|s| s.split(" · ").next().unwrap_or(s).trim_end()).map(|s| format!(" — start: {s}")).unwrap_or_default();
    format!("barrier {at} run ({counts}){steps}{start}")
}

fn clock(t: i64) -> String {
    util::iso_at(t).get(11..19).unwrap_or("").to_string()
}

fn render(i: &Inputs, phases: &[Phase], p: &Paint) -> String {
    let mut s = String::new();
    let (Some(a), Some(b)) = (phases.iter().map(|x| x.start).min(), phases.iter().map(|x| x.end).max()) else {
        return format!("{}: nothing recorded to time\n", i.id);
    };
    s.push_str(&format!("{} · {} start to end · times UTC\n", p.bold(&i.id), util::duration_human(b - a)));
    for x in phases {
        let line = format!("{}  {}  {:>6}  {}", clock(x.start), clock(x.end), format!("{}s", x.seconds()), x.label);
        if x.gap {
            s.push_str(&p.yellow(&format!("{line}  ← waiting for {}", x.waiting_for.as_deref().unwrap_or("?"))));
        } else {
            s.push_str(&line);
        }
        s.push('\n');
    }
    let gaps: i64 = phases.iter().filter(|x| x.gap).map(|x| x.seconds()).sum();
    s.push_str(&format!("gaps total {}s of {}s\n", gaps, b - a));
    let (member, verifier, tests, held) = fixed_of(i, phases);
    let secs = |v: Option<i64>| v.map_or("not read".to_string(), |v| format!("{v}s"));
    s.push_str(&format!(
        "fixed parts: seat spawn → first line {} · verifier spawn → first line {} · verifier test and build {} · verdict → despawn {}\n",
        secs(member),
        secs(verifier),
        if i.verifiers.is_empty() { "not read".to_string() } else { format!("{tests}s") },
        secs(held)
    ));
    let seats = holds_after_verdict(i, phases);
    if seats.len() > 1 {
        let each: Vec<String> = seats.iter().map(|(seat, secs)| format!("{seat} {secs}s")).collect();
        s.push_str(&format!("verdict → despawn by seat: {}\n", each.join(" · ")));
    }
    s
}

fn to_json(i: &Inputs, phases: &[Phase]) -> Value {
    json!({
        "id": i.id,
        "phases": phases.iter().map(|x| json!({
            "label": x.label,
            "start": util::iso_at(x.start),
            "end": util::iso_at(x.end),
            "seconds": x.seconds(),
            "gap": x.gap,
            "waiting_for": x.waiting_for,
            "shared": x.shared,
        })).collect::<Vec<_>>(),
        "gap_seconds": phases.iter().filter(|x| x.gap).map(|x| x.seconds()).sum::<i64>(),
        "fixed": fixed_json(i, phases),
    })
}

fn fixed_json(i: &Inputs, phases: &[Phase]) -> Value {
    let (member, verifier, tests, held) = fixed_of(i, phases);
    json!({
        "spawn_to_first_line_seconds": member,
        "verifier_spawn_to_first_line_seconds": verifier,
        "verifier_test_seconds": tests,
        "verdict_to_despawn_seconds": held,
        "verdict_to_despawn_by_seat": holds_after_verdict(i, phases).into_iter().map(|(seat, secs)| json!({"seat": seat, "seconds": secs})).collect::<Vec<_>>(),
    })
}

/// Whether `line` names `id` as a whole token (`wsp-36` is not `wsp-366`).
fn names(line: &str, id: &str) -> bool {
    let mut from = 0;
    while let Some(p) = line[from..].find(id) {
        let (a, b) = (from + p, from + p + id.len());
        let before = line[..a].chars().next_back().is_none_or(|c| !(c.is_alphanumeric() || c == '-'));
        let after = line[b..].chars().next().is_none_or(|c| !(c.is_alphanumeric() || c == '-'));
        if before && after {
            return true;
        }
        from = b;
    }
    false
}

/// A session's Claude Code transcript, whole, wherever it ran: the session id
/// alone finds it, so no cwd is given.
fn transcript(session: &str) -> Option<String> {
    transcript_at(&crate::host::LOCAL.transcript_of(session, "")?)
}

/// A transcript file, whole, read through the host.
fn transcript_at(file: &std::path::Path) -> Option<String> {
    String::from_utf8(crate::host::LOCAL.transcript_bytes(file)?).ok()
}

/// A seat's first act (`wsp-066`): the first stamp in its transcript, named
/// by an absolute path (a seat's `burn.json`) or a bare session id (the
/// ledger's). `None` where neither is found.
pub(crate) fn first_act(transcript_or_session: &str) -> Option<i64> {
    let text = match transcript_or_session {
        "" => return None,
        t if t.starts_with('/') => transcript_at(std::path::Path::new(t))?,
        s => transcript(s)?,
    };
    first_stamp(&text)
}

/// The first line of a transcript that carries a time: the seat's first act.
fn first_stamp(text: &str) -> Option<i64> {
    text.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find_map(|v| v.get("timestamp").and_then(|t| t.as_str()).map(util::epoch_of).filter(|t| *t > 0))
}

/// A command a verifier runs to test or build: what `scripts/row-cost/measure.py`
/// counts as test and build seconds (compounding-122), so that the two agree.
pub(crate) fn is_test_command(cmd: &str) -> bool {
    let c = cmd.trim_start();
    if c.starts_with("echo") || c.starts_with("cat >") {
        return false;
    }
    ["check:box", "vitest", "cargo test", "cargo build", "playwright"].iter().any(|k| c.contains(k))
}

/// Seconds the Bash calls in a transcript that [`is_test_command`] names took,
/// from a call to its result.
pub(crate) fn test_secs(text: &str) -> i64 {
    let mut pending: std::collections::HashMap<String, (i64, bool)> = Default::default();
    let mut total = 0;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let Some(t) = v["timestamp"].as_str().map(util::epoch_of).filter(|t| *t > 0) else { continue };
        for b in v["message"]["content"].as_array().into_iter().flatten() {
            match (v["type"].as_str(), b["type"].as_str()) {
                (Some("assistant"), Some("tool_use")) if b["name"] == "Bash" => {
                    if let Some(id) = b["id"].as_str() {
                        pending.entry(id.to_string()).or_insert((t, is_test_command(b["input"]["command"].as_str().unwrap_or(""))));
                    }
                }
                (Some("user"), Some("tool_result")) => {
                    if let Some((at, test)) = b["tool_use_id"].as_str().and_then(|id| pending.remove(id)) {
                        if test {
                            total += (t - at).max(0);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    total
}

/// The first prompt a session was given, from its Claude Code transcript.
fn first_request(session: &str) -> Option<i64> {
    let text = transcript(session)?;
    for line in text.lines() {
        if !line.contains("\"type\":\"user\"") {
            continue;
        }
        let v: Value = serde_json::from_str(line).ok()?;
        if let Some(ts) = v.get("timestamp").and_then(|t| t.as_str()) {
            return Some(util::epoch_of(ts)).filter(|t| *t > 0);
        }
    }
    None
}

/// The leg seats a row's log names (`wsp-335`). A start that never took
/// (`Dropped`) ran nothing; any other end is the seat's.
fn leg_seats_of(t: &crate::model::Task) -> Vec<LegSeat> {
    crate::cmd_attempts::attempts_of(t)
        .into_iter()
        .filter(|a| a.leg.as_deref().is_some_and(|l| !l.starts_with("piece ")) && !a.pane.is_empty() && a.outcome != crate::cmd_attempts::Outcome::Dropped)
        .map(|a| LegSeat {
            leg: a.leg.unwrap_or_default(),
            pane: a.pane,
            start: util::epoch_of(&a.at),
            end: Some(util::epoch_of(&a.until)).filter(|t| !a.until.is_empty() && *t > 0),
        })
        .collect()
}

pub(crate) fn gather(store: &Store, id: &str) -> Inputs {
    let mut i = Inputs { id: id.to_string(), ..Default::default() };
    let group = store.worklists().into_iter().find_map(|w| {
        w.groups().iter().enumerate().find(|(_, g)| g.members.iter().any(|m| m == id)).map(|(n, g)| (w.id.clone(), n + 1, g.members.clone()))
    });
    let mut ids = vec![id.to_string()];
    if let Some((_, _, m)) = &group {
        ids.extend(m.iter().cloned());
    }
    if let Ok(text) = std::fs::read_to_string(store.state_file("events.jsonl")) {
        for line in text.lines() {
            if !ids.iter().any(|x| line.contains(x.as_str())) {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            let (Some(ts), Some(kind)) = (v["ts"].as_str(), v["kind"].as_str()) else { continue };
            let data = v["data"].clone();
            let mine = data["id"].as_str().is_some_and(|d| ids.iter().any(|x| x == d));
            // A question the seat asked, and the answer to one (`compounding-465`): about the row, under the message's own id.
            let message = kind == "message-raised" && data["about"]["task"].as_str() == Some(id) && (data["shape"] == "question" || data["reply_to"].is_string());
            let keep = if data["id"].as_str() == Some(id) { true } else { message || (mine && kind == "task-verified") };
            if keep {
                i.events.push((util::epoch_of(ts), kind.to_string(), data));
            }
        }
    }
    let list = group.as_ref().map(|(l, _, _)| l.clone());
    if let Ok(text) = std::fs::read_to_string(store.state_file("cycle.log")) {
        for line in text.lines() {
            if line.len() < 21 || line.as_bytes()[19] != b'Z' || !line.is_char_boundary(20) {
                continue;
            }
            let (ts, rest) = line.split_at(20);
            let rest = rest.trim_start();
            let stopped = |l: &str| rest.starts_with(STOPPED_CHECK) && rest.contains(&format!("{l}-check-"));
            if names(rest, id) || list.as_deref().is_some_and(|l| stopped(l) || (names(rest, l) && (rest.contains("barrier") || rest.contains("Barrier") || rest.contains("--check")))) {
                i.cycle.push((util::epoch_of(ts), rest.to_string()));
            }
        }
    }
    if let Some(t) = store.find_task(id) {
        let all = crate::model::decisions_of(&t.body);
        let gone = crate::model::supersessions(&all);
        i.decisions = all
            .into_iter()
            .zip(gone)
            .filter(|(d, g)| g.is_none() && d.at.contains('T'))
            .map(|(d, _)| (util::epoch_of(&d.at), d.id, d.text))
            .collect();
        i.leg_seats = leg_seats_of(&t);
        i.legs = crate::legs::legs(&t)
            .into_iter()
            .map(|l| LegIn { skipped: l.skipped.is_some(), after: l.after.clone().unwrap_or_default(), name: l.name })
            .collect();
    }
    if let Some(l) = &list {
        let ev = list_events(store, &["worklist-parked", "worklist-resumed", "worklist-held", "worklist-go"]);
        i.parks = parks_of(&ev, l, util::epoch_secs());
        i.holds = holds_of(&ev, l, util::epoch_secs());
        if let Some(w) = store.worklist(l) {
            for (n, g) in w.groups().iter().enumerate() {
                for c in &g.checks {
                    let Some(pane) = c.pane.as_deref() else { continue };
                    let ended = crate::cmd_burn::reading_in(&store.state, pane).and_then(|r| r["ended"].as_str().map(util::epoch_of)).filter(|t| *t > 0).unwrap_or(0);
                    i.check_ends.push((n + 1, util::epoch_of(&c.at), ended));
                }
            }
        }
    }
    i.group = group;
    // The seats a cap line named as holders, and when they ended.
    let mut holders: Vec<String> = Vec::new();
    let mut checks: Vec<(String, String, usize)> = Vec::new();
    for (_, l) in &i.cycle {
        if let Some((_, after)) = l.split_once(" waits — the seat is at its cap") {
            for (_, held) in held_in(after) {
                match held {
                    Held::Work(r) | Held::Step(r) if !r.is_empty() && !holders.contains(&r) => holders.push(r),
                    Held::Check(label, list, at) if !checks.iter().any(|c| c.0 == label) => checks.push((label, list, at)),
                    _ => {}
                }
            }
        }
    }
    if !holders.is_empty() {
        if let Ok(text) = std::fs::read_to_string(store.state_file("events.jsonl")) {
            for line in text.lines() {
                if !holders.iter().any(|h| line.contains(h.as_str())) {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
                let (Some(ts), Some(kind)) = (v["ts"].as_str(), v["kind"].as_str()) else { continue };
                if !matches!(kind, "task-review" | "task-released" | "task-verified") {
                    continue;
                }
                if let Some(row) = v["data"]["id"].as_str().filter(|r| holders.iter().any(|h| h == r)) {
                    i.holder_events.push((util::epoch_of(ts), kind.to_string(), row.to_string()));
                }
            }
        }
    }
    if !checks.is_empty() {
        // A check seat ends with its barrier: the line `tell <list>: Barrier <n> of <list> passed`.
        if let Ok(text) = std::fs::read_to_string(store.state_file("cycle.log")) {
            for line in text.lines() {
                if line.len() < 21 || line.as_bytes()[19] != b'Z' || !line.is_char_boundary(20) {
                    continue;
                }
                let (ts, rest) = line.split_at(20);
                let rest = rest.trim_start();
                for (label, list, at) in &checks {
                    if rest.starts_with(&format!("tell {list}: Barrier {at} of {list}")) && rest.contains("passed") {
                        i.holder_events.push((util::epoch_of(ts), "barrier-passed".to_string(), label.clone()));
                    }
                }
            }
        }
    }
    // The seat's own first line, and the verifier seats': the sessions learned
    // for the row after its first review that are not the seat's.
    let first_review = i.kind("task-review", id).first().map(|(t, _)| *t);
    let learned: Vec<(i64, String)> = i
        .kind("session-learned", id)
        .into_iter()
        .filter(|(_, d)| d.get("leg").is_none())
        .filter_map(|(t, d)| d["session"].as_str().map(|s| (t, s.to_string())))
        .collect();
    let mine: Vec<&String> = learned.iter().filter(|(t, _)| first_review.is_none_or(|r| *t <= r)).map(|(_, s)| s).collect();
    i.seat_first = mine.first().and_then(|s| transcript(s)).as_deref().and_then(first_stamp);
    if learned.is_empty() {
        i.found = found_transcripts(store, &i);
    }
    // The verifier seats' own first lines and test seconds, which wsp wrote on
    // each pass when it ended the seat (the binding that names the session is
    // gone after the despawn).
    if let Some(t) = store.find_task(id) {
        for p in crate::verification::passes(&t) {
            let Some(sp) = p.spent.as_ref() else { continue };
            let Some(first) = sp.first_line.as_deref().map(util::epoch_of).filter(|t| *t > 0) else { continue };
            i.verifiers.push(VerifierSeat { learned: util::epoch_of(&p.at), first, test_secs: sp.test_secs });
        }
    }
    i.first_request = i.kind("session-learned", id).first().and_then(|(_, d)| d["session"].as_str().map(String::from)).and_then(|s| first_request(&s));
    i
}

// ---- the seat's own model time and tokens ------------------------------------

/// What the seat spent in one phase of the row.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct SeatPhase {
    pub label: String,
    pub start: i64,
    pub end: i64,
    pub calls: u64,
    pub model_secs: i64,
    /// Fresh input plus cache writes.
    pub tokens_in: u64,
    pub cache_read: u64,
    pub tokens_out: u64,
    /// Seconds from a tool call to its result, by tool.
    pub tools: std::collections::BTreeMap<String, i64>,
}

impl SeatPhase {
    fn holds(&self, t: i64, last: bool) -> bool {
        t >= self.start && (t < self.end || (last && t == self.end))
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SeatReport {
    pub sessions: Vec<String>,
    /// Sessions whose transcript could not be found.
    pub missing: Vec<String>,
    pub phases: Vec<SeatPhase>,
    /// Where the leg boundaries came from.
    pub source: String,
    /// When the seat was in a model call or a tool, from the transcripts read
    /// (not merged): what the wall-time buckets call a member working.
    pub active: Vec<(i64, i64)>,
    /// Sessions read whose transcript has model calls and no time on them: what
    /// they hold is not timed, and the seat idle says so (`wsp-313`).
    pub untimed: Vec<String>,
}

/// One model call, once per requestId.
#[derive(Debug, Default)]
struct Call {
    at: i64,
    start: i64,
    end: i64,
    tin: u64,
    cache_read: u64,
    out: u64,
    /// What its tool uses ran: a Bash call's command, another tool's name.
    cmds: Vec<String>,
}

const TOOL_KINDS: [&str; 4] = ["Bash", "Read", "Edit", "Write"];

/// The model calls and tool times in one transcript. A call is a requestId:
/// one transcript line per content block repeats its usage, so the figures are
/// kept once (the largest seen). A call's model seconds run from the
/// `api-request` line that sent it to its last line; a tool's from its call to
/// its result. A transcript that writes no `api-request` lines times a call
/// from the line before it: there an `attachment` or `file-history-delta` line
/// stamped when the reply came back would make a long call read as 0 s
/// (`wsp-313`), which is why the request's own line is taken where it is.
fn read_transcript(text: &str) -> (Vec<Call>, Vec<(i64, String, i64)>) {
    let mut calls: Vec<Call> = Vec::new();
    let mut index: std::collections::HashMap<String, usize> = Default::default();
    let mut pending: std::collections::HashMap<String, (i64, String)> = Default::default();
    let mut tools: Vec<(i64, String, i64)> = Vec::new();
    let mut prev: Option<i64> = None;
    // The last request sent since the last user line, not yet answered.
    let mut sent: Option<i64> = None;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let Some(t) = v["timestamp"].as_str().map(util::epoch_of).filter(|t| *t > 0) else { continue };
        match v["type"].as_str() {
            Some("api-request") => sent = Some(t),
            Some("assistant") => {
                let m = &v["message"];
                let key = v["requestId"].as_str().or_else(|| m["id"].as_str()).map(String::from);
                let n = match key.as_ref().and_then(|k| index.get(k)) {
                    Some(n) => *n,
                    None => {
                        let start = sent.take().map(|s| s.min(t)).or(prev).unwrap_or(t);
                        calls.push(Call { at: t, start, end: t, ..Default::default() });
                        if let Some(k) = key {
                            index.insert(k, calls.len() - 1);
                        }
                        calls.len() - 1
                    }
                };
                let c = &mut calls[n];
                c.end = c.end.max(t);
                let u = &m["usage"];
                let num = |k: &str| u[k].as_u64().unwrap_or(0);
                c.tin = c.tin.max(num("input_tokens") + num("cache_creation_input_tokens"));
                c.cache_read = c.cache_read.max(num("cache_read_input_tokens"));
                c.out = c.out.max(num("output_tokens"));
                for b in m["content"].as_array().into_iter().flatten() {
                    if b["type"] == "tool_use" {
                        let ran = b["input"]["command"].as_str().or_else(|| b["name"].as_str()).unwrap_or("");
                        c.cmds.push(ran.trim().to_string());
                        if let (Some(id), Some(name)) = (b["id"].as_str(), b["name"].as_str()) {
                            pending.entry(id.to_string()).or_insert((t, name.to_string()));
                        }
                    }
                }
            }
            Some("user") => {
                sent = None;
                for b in v["message"]["content"].as_array().into_iter().flatten() {
                    if b["type"] == "tool_result" {
                        if let Some((at, name)) = b["tool_use_id"].as_str().and_then(|id| pending.remove(id)) {
                            tools.push((at, name, (t - at).max(0)));
                        }
                    }
                }
            }
            _ => {}
        }
        prev = Some(t);
    }
    (calls, tools)
}

/// A transcript with a model call on a line that carries no time: its calls
/// cannot be timed (`wsp-313`).
fn untimed_calls(text: &str) -> bool {
    text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).any(|v| v["type"] == "assistant" && v["timestamp"].as_str().map(util::epoch_of).is_none_or(|t| t <= 0))
}

/// A session's test and build seconds and the time of its first line, as
/// `util::iso_at` writes it: what a verifier's pass keeps (`wsp-429`).
pub(crate) fn session_fixed(session: &str) -> Option<(i64, String)> {
    let text = transcript(session)?;
    Some((test_secs(&text), util::iso_at(first_stamp(&text)?)))
}

/// A session's model seconds and tool seconds from `since` on, read off its
/// Claude Code transcript (`compounding-104`: a verifier's usage lands on its
/// pass). `None` for a session with no transcript to read.
pub(crate) fn session_secs(session: &str, since: i64) -> Option<(i64, i64)> {
    let (calls, tools) = read_transcript(&transcript(session)?);
    let model = calls.iter().filter(|c| c.at >= since).map(|c| (c.end - c.start).max(0)).sum();
    let tool = tools.iter().filter(|(at, _, _)| *at >= since).map(|(_, _, s)| *s).sum();
    Some((model, tool))
}

/// The governor's turns inside one window of the run: from a group's barrier
/// pass to the next group's first start (`wsp-398`).
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct GovernorWindow {
    /// The window's length, seconds.
    pub seconds: i64,
    /// Model calls the governor began inside it.
    pub turns: u64,
    pub model_secs: i64,
    pub tool_secs: i64,
    /// The window under neither the governor's model nor its tools: the run
    /// waiting on something that is not the governor.
    pub idle_secs: i64,
    /// Turns that only looked (a watch, a listing, a bare reply) and made no
    /// change: what a governor that skips a turn when nothing needs judgement
    /// would not take, and the model seconds they held.
    pub looking_turns: u64,
    pub looking_secs: i64,
}

/// A command that only reads the run: no choice was made by running it.
fn only_looks(cmd: &str) -> bool {
    let c = cmd.trim_start();
    ["wsp watch", "wsp ls", "wsp status", "wsp wip", "wsp worklist next", "wsp worklist show", "wsp timeline", "sleep "]
        .iter()
        .any(|p| c.starts_with(p))
}

/// One transcript of the governor, and the stretches of the run its pane held
/// the slot: a turn outside them is not the governor's (`wsp-398`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GovSource {
    /// An absolute path to the transcript (a seat's `burn.json` names one), or a
    /// bare session id looked for under `~/.claude/projects` (older events).
    pub transcript: String,
    /// `(from, to)` epoch seconds, `to == 0` for still held; empty = unclipped.
    pub spans: Vec<(i64, i64)>,
}

fn read_source(src: &GovSource) -> Option<String> {
    if src.transcript.starts_with('/') {
        transcript_at(std::path::Path::new(&src.transcript))
    } else {
        transcript(&src.transcript)
    }
}

/// The governor's figures over `[from, to)` out of the transcripts of its
/// sessions. `None` when none could be read.
pub(crate) fn governor_window(sources: &[GovSource], from: i64, to: i64) -> Option<GovernorWindow> {
    let texts: Vec<(String, Vec<(i64, i64)>)> = sources.iter().filter_map(|s| read_source(s).map(|t| (t, s.spans.clone()))).collect();
    governor_window_of(&texts, from, to)
}

fn governor_window_of(texts: &[(String, Vec<(i64, i64)>)], from: i64, to: i64) -> Option<GovernorWindow> {
    if texts.is_empty() || to < from {
        return None;
    }
    let mut w = GovernorWindow { seconds: to - from, ..Default::default() };
    let now = util::epoch_secs();
    let held = |spans: &[(i64, i64)], at: i64| spans.is_empty() || spans.iter().any(|&(a, b)| at >= a && at <= if b == 0 { now } else { b });
    for (t, spans) in texts {
        let (calls, tools) = read_transcript(t);
        for c in calls.iter().filter(|c| c.at >= from && c.at < to && held(spans, c.at)) {
            let secs = (c.end - c.start).max(0);
            w.turns += 1;
            w.model_secs += secs;
            if c.cmds.iter().all(|x| only_looks(x)) {
                w.looking_turns += 1;
                w.looking_secs += secs;
            }
        }
        w.tool_secs += tools.iter().filter(|(at, _, _)| *at >= from && *at < to && held(spans, *at)).map(|(_, _, s)| *s).sum::<i64>();
    }
    w.idle_secs = (w.seconds - w.model_secs - w.tool_secs).max(0);
    Some(w)
}

/// The seat's own sessions: those learned for the row outside its legs' windows
/// and before it was first reviewed (a leg's seat and the verifier's are not it).
fn seat_sessions(i: &Inputs, windows: &[(i64, i64)]) -> Vec<String> {
    let review = i.kind("task-review", &i.id).first().map(|(t, _)| *t);
    let mut out: Vec<String> = Vec::new();
    for (t, d) in i.kind("session-learned", &i.id) {
        let Some(s) = d["session"].as_str() else { continue };
        // A session learned after the first review is the row's own seat only
        // where the row was claimed again by that pane in between (a seat started
        // again, `compounding-217`); otherwise it is something else's.
        let reseated = review.is_some_and(|r| i.kind("task-claimed", &i.id).into_iter().any(|(c, cd)| c > r && c <= t && cd["pane"] == d["pane"] && !d["pane"].is_null()));
        if windows.iter().any(|(a, b)| t >= *a && t <= *b) || review.is_some_and(|r| t > r && !reseated) || d.get("leg").is_some() {
            continue;
        }
        if !out.iter().any(|x| x == s) {
            out.push(s.to_string());
        }
    }
    if out.is_empty() {
        // No session learned for the seat (`wsp-313`): a transcript found for it
        // is the seat's where it began inside one of the row's own seated spans,
        // a claim (less a spawn's start) to the review or release after it, and
        // not inside a leg.
        let ends: Vec<i64> = i.events.iter().filter(|(_, k, d)| d["id"] == i.id.as_str() && (k == "task-review" || k == "task-released")).map(|(t, _, _)| *t).collect();
        let spans: Vec<(i64, i64)> = i
            .kind("task-claimed", &i.id)
            .into_iter()
            .map(|(c, _)| (c - FOUND_SLACK, ends.iter().copied().filter(|e| *e >= c).min().unwrap_or(i64::MAX)))
            .collect();
        for (first, r) in &i.found {
            let seated = spans.iter().any(|(a, b)| first >= a && first < b);
            let in_leg = windows.iter().any(|(a, b)| first >= a && first <= b);
            if seated && !in_leg && !out.contains(r) {
                out.push(r.clone());
            }
        }
    }
    out
}

/// How long before its claim a found transcript may begin and still be the
/// seat's: the spawn's start, which is before the seat claims.
const FOUND_SLACK: i64 = 120;

/// The transcripts that may be the row's own seat's where no `session-learned`
/// names one (`wsp-313`), each with its first stamp: the burn ledger's and live
/// record's for every pane that claimed the row (a path, else a session id), and
/// every transcript in Claude Code's directory for the row's tree, which keeps
/// its name after the tree is gone. [`seat_sessions`] keeps those that began
/// inside the seat's own span; a verifier reading the same tree began after the
/// review and is not kept.
fn found_transcripts(store: &Store, i: &Inputs) -> Vec<(i64, String)> {
    let mut refs: Vec<String> = Vec::new();
    let mut add = |r: String| {
        if !r.is_empty() && !refs.contains(&r) {
            refs.push(r);
        }
    };
    for (_, d) in i.kind("task-claimed", &i.id) {
        let Some(pane) = d["pane"].as_str().filter(|p| !p.is_empty()) else { continue };
        let mut lines = crate::cmd_burn::ledger_lines(&store.state, pane);
        lines.extend(crate::cmd_burn::reading_in(&store.state, pane));
        for l in lines {
            let path = l["transcript"].as_str().filter(|t| t.starts_with('/')).map(String::from);
            let by_id = || l["session_id"].as_str().and_then(|s| crate::host::LOCAL.transcript_of(s, "")).map(|p| p.to_string_lossy().to_string());
            if let Some(r) = path.or_else(by_id) {
                add(r);
            }
        }
    }
    let tree = format!("/{}/{}", crate::cmd_checkout::WORKTREES, i.id);
    for f in crate::host::LOCAL.transcripts_of_tree(&tree) {
        add(f.to_string_lossy().to_string());
    }
    refs.into_iter().filter_map(|r| first_act(&r).map(|t| (t, r))).collect()
}

/// A transcript named by its path or by its session id, whole.
fn transcript_ref(r: &str) -> Option<String> {
    if r.starts_with('/') { transcript_at(std::path::Path::new(r)) } else { transcript(r) }
}

/// One leg's span: `(name, start, end)`.
pub(crate) type Span = (String, i64, i64);

/// The legs `run-leg.sh` timed: every `legs/*/timeline.json` under `dirs`,
/// whose `start` and `end` events fall inside `[from, to]` (the row's own
/// time, so another row's legs in the same directory are left out).
pub(crate) fn run_legs(dirs: &[std::path::PathBuf], from: i64, to: i64) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(dir) else { continue };
        for e in rd.flatten() {
            let Ok(text) = std::fs::read_to_string(e.path().join("timeline.json")) else { continue };
            let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
            let at = |ev: &str| v["events"].as_array().into_iter().flatten().find(|x| x["ev"] == ev).and_then(|x| x["at"].as_f64());
            let (Some(a), Some(b)) = (at("start"), at("end")) else { continue };
            let (a, b) = (a.floor() as i64, b.ceil() as i64);
            if a < from.saturating_sub(1) || b > to + 1 {
                continue;
            }
            let name = match (v["component"].as_str(), v["leg"].as_str()) {
                (Some(c), Some(l)) => format!("{c}-{l}"),
                _ => e.file_name().to_string_lossy().to_string(),
            };
            out.push((name, a, b));
        }
    }
    out.sort_by_key(|(_, a, _)| *a);
    out
}

/// Where run-leg timelines may be: `legs` directories one to three levels under
/// the row's tree (`spikes/react-ports/legs`), never inside a dependency or
/// another worktree.
pub(crate) fn legs_dirs(tree: &std::path::Path) -> Vec<std::path::PathBuf> {
    let skip = |n: &str| n.starts_with('.') || n == "node_modules" || n == "target";
    let mut out = Vec::new();
    let mut level = vec![tree.to_path_buf()];
    for _ in 0..3 {
        let mut next = Vec::new();
        for d in &level {
            if d.join("legs").is_dir() {
                out.push(d.join("legs"));
            }
            for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
                if e.path().is_dir() && !skip(&e.file_name().to_string_lossy()) && e.file_name() != "legs" {
                    next.push(e.path());
                }
            }
        }
        level = next;
    }
    out
}

/// The `cwd` the first line of a transcript records: the tree the seat worked in.
fn cwd_of(text: &str) -> Option<std::path::PathBuf> {
    text.lines().take(50).find_map(|l| serde_json::from_str::<Value>(l).ok()?["cwd"].as_str().map(std::path::PathBuf::from))
}

/// The seat's phases: before the first leg, between each pair, after the last
/// up to review typed; one phase for a row with no legs. Boundaries are the
/// given legs, which run-leg's timelines or wsp's own leg entries supply.
fn seat_bounds(i: &Inputs, phases: &[Phase], legs: &[Span]) -> Vec<(String, i64, i64)> {
    let first_review = |from: i64| {
        let r: Vec<i64> = i.kind("task-review", &i.id).into_iter().map(|(t, _)| t).collect();
        r.iter().copied().find(|t| *t >= from).or_else(|| r.first().copied())
    };
    let last_leg = legs.iter().map(|l| l.2).max();
    let begin = phases.iter().map(|p| p.start).chain(legs.iter().map(|l| l.1)).min();
    let all_end = phases.iter().map(|p| p.end).chain(legs.iter().map(|l| l.2)).max();
    let end = first_review(last_leg.unwrap_or(0)).or(all_end);
    let (Some(begin), Some(end)) = (begin, end) else { return Vec::new() };
    let end = end.max(last_leg.unwrap_or(0));
    if legs.is_empty() {
        // A row released and started again: one span per seat (`compounding-217`).
        let seated = attempts(i);
        if seated.len() > 1 {
            let n = seated.len();
            let out: Vec<(String, i64, i64)> = seated
                .iter()
                .enumerate()
                .map(|(k, a)| {
                    let (stop, to) = if a.end.is_some() { ("release", a.end) } else { ("review", a.review) };
                    (format!("the seat, attempt {} of {n}, spawn to {stop}", k + 1), a.begin(), to.unwrap_or(end).max(a.begin()))
                })
                .collect();
            return out;
        }
        return vec![("the seat, spawn to review".into(), begin, end)];
    }
    let mut out = vec![(format!("before leg {}", legs[0].0), begin.min(legs[0].1), legs[0].1)];
    for w in legs.windows(2) {
        out.push((format!("between leg {} and leg {}", w[0].0, w[1].0), w[0].2, w[1].1.max(w[0].2)));
    }
    let l = &legs[legs.len() - 1];
    out.push((format!("after leg {} → review typed", l.0), l.2, end));
    out
}

/// The seat's tokens and model time per phase. `load` reads a session's
/// transcript; `None` is a transcript that cannot be found. `run_legs` are the
/// legs `run-leg.sh` timed; with none, the legs are wsp's own.
pub(crate) fn seat_report(i: &Inputs, phases: &[Phase], run_legs: &[Span], load: &dyn Fn(&str) -> Option<String>) -> SeatReport {
    let (legs, source): (Vec<Span>, &str) = if run_legs.is_empty() {
        let l = phases
            .iter()
            .filter(|p| !p.gap && p.label.starts_with("leg "))
            .map(|p| (p.label["leg ".len()..].split(" (").next().unwrap_or("").to_string(), p.start, p.end))
            .collect();
        (l, SEAT_SOURCE_WSP)
    } else {
        (run_legs.to_vec(), SEAT_SOURCE_RUN)
    };
    let windows: Vec<(i64, i64)> = legs.iter().map(|(_, a, b)| (*a, *b)).collect();
    let mut r = SeatReport { sessions: seat_sessions(i, &windows), source: source.to_string(), ..Default::default() };
    r.phases = seat_bounds(i, phases, &legs).into_iter().map(|(label, start, end)| SeatPhase { label, start, end, ..Default::default() }).collect();
    let last = r.phases.len().saturating_sub(1);
    let mut during = SeatPhase {
        label: "while a leg ran (the seat's own calls inside a leg; its wait on the leg is not counted)".into(),
        start: windows.iter().map(|w| w.0).min().unwrap_or(0),
        end: windows.iter().map(|w| w.1).max().unwrap_or(0),
        ..Default::default()
    };
    for s in r.sessions.clone() {
        let Some(text) = load(&s) else {
            r.missing.push(s);
            continue;
        };
        let (calls, tools) = read_transcript(&text);
        if calls.is_empty() && untimed_calls(&text) {
            r.untimed.push(s.clone());
        }
        r.active.extend(calls.iter().filter(|c| c.end > c.start).map(|c| (c.start, c.end)));
        r.active.extend(tools.iter().filter(|(_, _, secs)| *secs > 0).map(|(at, _, secs)| (*at, at + secs)));
        for c in calls {
            let inside = windows.iter().any(|(a, b)| c.at >= *a && c.at < *b);
            let found = r.phases.iter_mut().enumerate().find(|(n, p)| p.holds(c.at, *n == last)).map(|(_, p)| p);
            if let Some(p) = found.or(if inside { Some(&mut during) } else { None }) {
                p.calls += 1;
                p.model_secs += (c.end - c.start).max(0);
                p.tokens_in += c.tin;
                p.cache_read += c.cache_read;
                p.tokens_out += c.out;
            }
        }
        // A tool's time is the part of its call-to-result span inside a phase:
        // the call that starts a leg and waits for it counts nothing of the leg.
        for (at, name, secs) in tools {
            let kind = if TOOL_KINDS.contains(&name.as_str()) { name } else { "other".to_string() };
            for p in r.phases.iter_mut() {
                let o = (at + secs).min(p.end) - at.max(p.start);
                if o > 0 {
                    *p.tools.entry(kind.clone()).or_insert(0) += o;
                }
            }
        }
    }
    if during.calls > 0 {
        r.phases.push(during);
    }
    r
}

const SEAT_SOURCE_WSP: &str = "phase boundaries from wsp's leg entries (cycle.log 'starting leg', leg-done events, the review)";
const SEAT_SOURCE_RUN: &str = "phase boundaries from run-leg.sh's timeline.json (start and end per leg), the end at the review";

fn render_seat(r: &SeatReport) -> String {
    let mut s = String::new();
    if r.sessions.is_empty() {
        return "the seat's own work: no session recorded for this row\n".into();
    }
    if r.missing.len() == r.sessions.len() {
        return format!("the seat's own work: transcript not found for session {}\n", r.missing.join(", "));
    }
    s.push_str(&format!("the seat's own work (times UTC; {}); tokens: in = fresh + cache write\n", r.source));
    for p in &r.phases {
        let tools: Vec<String> = p.tools.iter().map(|(k, v)| format!("{k} {v}s")).collect();
        s.push_str(&format!(
            "{}  {}  {:>6}  {}  {} calls · model {}s · in {} (cache read {}) out {} · tools {}\n",
            clock(p.start),
            clock(p.end),
            format!("{}s", p.end - p.start),
            p.label,
            p.calls,
            p.model_secs,
            p.tokens_in,
            p.cache_read,
            p.tokens_out,
            if tools.is_empty() { "none".to_string() } else { tools.join(", ") }
        ));
    }
    for m in &r.missing {
        s.push_str(&format!("transcript not found for session {m}; its calls are not counted\n"));
    }
    for u in &r.untimed {
        s.push_str(&format!("transcript of session {u} has no timestamps on its model calls; they are not timed\n"));
    }
    s
}

fn seat_json(r: &SeatReport) -> Value {
    let all_missing = r.sessions.is_empty() || r.missing.len() == r.sessions.len();
    json!({
        "source": r.source,
        "utc": true,
        "sessions": r.sessions,
        "missing": r.missing,
        "untimed": r.untimed,
        "note": if r.sessions.is_empty() { json!("no session recorded for this row") } else if all_missing { json!(format!("transcript not found for session {}", r.missing.join(", "))) } else { Value::Null },
        "phases": if all_missing { Vec::new() } else { r.phases.iter().map(|p| {
            let mut tools = serde_json::Map::new();
            for k in TOOL_KINDS.iter().chain(["other"].iter()) {
                tools.insert((*k).to_string(), json!(p.tools.get(*k).copied().unwrap_or(0)));
            }
            json!({
                "label": p.label,
                "start": util::iso_at(p.start),
                "end": util::iso_at(p.end),
                "wall_seconds": p.end - p.start,
                "calls": p.calls,
                "model_seconds": p.model_secs,
                "tokens_in": p.tokens_in,
                "tokens_cache_read": p.cache_read,
                "tokens_out": p.tokens_out,
                "tool_seconds": tools,
            })
        }).collect::<Vec<_>>() },
    })
}

/// The legs `run-leg.sh` timed for this row: in `--legs DIR`, or else in the
/// `legs` directories of the tree the seat's transcript says it worked in.
fn run_legs_for(i: &Inputs, phases: &[Phase], flag: Option<String>) -> Vec<Span> {
    let from = phases.iter().map(|p| p.start).min().or_else(|| i.kind("task-claimed", &i.id).first().map(|(t, _)| *t)).unwrap_or(0);
    let to = i.kind("task-review", &i.id).last().map(|(t, _)| *t).or_else(|| phases.iter().map(|p| p.end).max()).unwrap_or(i64::MAX - 2);
    let dirs = match flag {
        Some(d) => vec![util::expand(&d)],
        None => {
            let tree = i.kind("session-learned", &i.id).into_iter().find_map(|(_, d)| d["session"].as_str().and_then(transcript)).as_deref().and_then(cwd_of);
            tree.map(|t| legs_dirs(&t)).unwrap_or_default()
        }
    };
    run_legs(&dirs, from, to)
}

/// One row's timeline figures, read by the same functions `wsp timeline` prints
/// from, for the effort report (`wsp-375`): nothing here is computed a second way.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct RowFigures {
    /// First phase start to last phase end, seconds.
    pub wall: i64,
    pub start: i64,
    pub end: i64,
    /// Sum of the gap phases.
    pub gap: i64,
    /// Gap seconds by what was being waited for.
    pub waits: std::collections::BTreeMap<String, i64>,
    /// The seat's own calls, model seconds, tokens, and tool seconds by kind.
    pub calls: u64,
    pub model_secs: i64,
    pub tokens_in: u64,
    pub cache_read: u64,
    pub tokens_out: u64,
    pub tools: std::collections::BTreeMap<String, i64>,
    /// Sessions whose transcript was not found: their calls are not counted.
    pub missing: usize,
    /// Sessions read whose model calls carry no time (`wsp-313`).
    pub untimed: usize,
    pub sessions: usize,
    /// The barrier phases (the group's, one run for all its members): their
    /// seconds, the gap among them, and that gap by what it waited for. They
    /// are inside `wall`, `gap` and `waits` too, for the row's own view.
    pub shared: i64,
    pub shared_gap: i64,
    pub shared_waits: std::collections::BTreeMap<String, i64>,
    /// The fixed parts the phases above hide (`wsp-429`): the row's seat from
    /// the spawn to its first transcript line; the verifier's from its spawn to
    /// its first line; the seconds its test and build commands ran, summed over
    /// its passes; and the seat's hold from the verdict to its despawn. `None`
    /// is not read (no transcript, no verifier, no despawn line yet).
    pub spawn_to_first: Option<i64>,
    pub verifier_spawn_to_first: Option<i64>,
    pub verifier_test_secs: i64,
    pub verdict_to_despawn: Option<i64>,
    /// Each seat that hold is read from, by id ([`holds_after_verdict`]).
    pub verdict_to_despawn_seats: Vec<(String, i64)>,
    /// What the wall-time buckets read (`wsp-470`): the row's own phases by what
    /// kind of time they are, when its seat was in a model call or a tool
    /// (merged), and when it stood blocked.
    pub spans: Vec<crate::wallbuckets::Span>,
    pub active: Vec<(i64, i64)>,
    pub blocked: Vec<(i64, i64)>,
    /// Every transcript the seat's calls come from was found: where one was
    /// not, its seat time cannot be told from idle and is not called waiting.
    pub measured: bool,
    /// What the row's log says it was waiting on, from each moment (`wsp-559`):
    /// the seat idle is named by the latest one.
    pub markers: Vec<(i64, String)>,
    /// When the seat waited on a shell of its own, `(from, line)` ([`own_shells`]).
    pub shells: Vec<(i64, i64)>,
    /// Seconds between a row's attempts (`compounding-217`): parked or waiting
    /// to be restarted, or sent back after a review and reopened. Named gaps
    /// of their own, so neither the seat finishing nor the seat idle.
    pub between_attempts: i64,
}

/// Who a question the seat asked is waiting on.
#[derive(Clone, Copy)]
enum Whom {
    Governor,
    Person,
}

impl Whom {
    /// The reason the seat idle is booked under while the question is open.
    fn waiting(self, what: &str) -> String {
        match self {
            Whom::Governor => format!("waiting on the governor's answer ({what})"),
            Whom::Person => format!("waiting on a person's answer ({what})"),
        }
    }
}

/// The run's line that holds a seat for an answer (`waiting::Waiting::sentence`) —
/// `<list> group <n>: <row> (<seat>) is waiting on the react-chat governor to answer its question
/// <id> — not told and not ended`, `… is held in a tool call for a person to answer <id> — …`, or
/// `… is stopped on a prompt only a person can answer — …` — as who `row`'s seat waits on, and the
/// question where it names one.
fn held_wait<'a>(l: &'a str, row: &str) -> Option<(Whom, Option<&'a str>)> {
    let (_, rest) = l.split_once(&format!(": {row} ("))?;
    let (_, rest) = rest.split_once(") is ")?;
    let rest = rest.split(" — ").next().unwrap_or(rest);
    let whom = |w: &str| if w.ends_with("governor") { Whom::Governor } else { Whom::Person };
    if let Some(r) = rest.strip_prefix("waiting on ") {
        let (w, q) = r.split_once(" to answer its question ")?;
        return Some((whom(w), Some(q.trim())));
    }
    if let Some(r) = rest.strip_prefix("held in a tool call for ") {
        let (w, q) = r.split_once(" to answer ")?;
        return Some((whom(w), Some(q.trim())));
    }
    let w = rest.strip_prefix("stopped on a prompt only ")?.strip_suffix(" can answer")?;
    Some((whom(w), None))
}

/// What the lines the log already writes for a row say it was waiting on, from
/// the moment each is written: a verdict sent back, the tell reaching the seat
/// or not, a reopen, a rebase refused, a hand-off to another row, a block, an
/// `after` it is owed. Sorted by time; a moment with none before it is "none
/// recorded".
pub(crate) fn idle_markers(i: &Inputs) -> Vec<(i64, String)> {
    let id = i.id.as_str();
    let mut out: Vec<(i64, String)> = Vec::new();
    for (t, k, d) in i.events.iter().filter(|(_, _, d)| d["id"] == id) {
        let why = match k.as_str() {
            "task-claimed" | "session-learned" => crate::wallbuckets::NONE_RECORDED.to_string(),
            "task-review" => "review typed: the check and the verifier's hand-off".into(),
            "task-checked" => "the check done: the verifier's hand-off".into(),
            "task-verified" if d["verdict"] == "blocks" => "sent back: the verdict given, the seat not yet told or moving".into(),
            "task-verified" => "the verdict holds: the seat left standing, nothing owed".into(),
            "task-reopen" | "task-doing" => "reopened: the seat is on the send-back".into(),
            "task-blocked" => "blocked on a person or a fix".into(),
            "task-parked" => "parked".into(),
            "task-released" => "the seat released".into(),
            "task-handoff" => match d["to"].as_str() {
                Some(to) => format!("handed to another row ({to}): the remainder is owed there"),
                None => "handed to another row".into(),
            },
            _ => continue,
        };
        out.push((*t, why));
    }
    // The seat asked a question and waits on the governor or a person (`compounding-465`): from the
    // run's line that holds it, or from the question's own event where that is earlier, and until the
    // answer to it is written. The run's line says who the question reached; the event, who it was
    // addressed to, so a question the run routed on is one reason, not two.
    let held: Vec<(i64, Whom, Option<&str>)> = i.cycle.iter().filter_map(|(t, l)| held_wait(l, id).map(|(w, q)| (*t, w, q))).collect();
    let mut asked: Vec<&str> = Vec::new();
    for (t, _, d) in i.events.iter().filter(|(_, k, d)| k == "message-raised" && d["about"]["task"] == id) {
        let Some(mid) = d["id"].as_str() else { continue };
        if d["shape"] == "question" && d["from"]["pane"].is_string() {
            let routed = held.iter().find(|h| h.2 == Some(mid)).map(|h| h.1);
            let addressed = if d["for"]["seat"].is_string() { Some(Whom::Governor) } else if for_the_person(&d["for"]) { Some(Whom::Person) } else { None };
            if let Some(w) = routed.or(addressed) {
                asked.push(mid);
                out.push((*t, w.waiting(&format!("question {mid}"))));
            }
        } else if d["reply_to"].as_str().is_some_and(|q| asked.contains(&q)) {
            out.push((*t, "answered: the seat has the answer and has not moved yet".into()));
        }
    }
    for (t, w, q) in held {
        out.push((t, w.waiting(&q.map_or("a prompt on its screen".to_string(), |q| format!("question {q}")))));
    }
    for (t, l) in &i.cycle {
        let why = if own_shell_line(l, id).is_some() {
            crate::wallbuckets::OWN_SHELLS.to_string()
        } else if l.contains("A verifier found a problem") && l.starts_with("tell ") {
            "sent back: the verdict with the governor".into()
        } else if l.starts_with(&format!("tell {id}: not delivered")) || l.starts_with(&format!("told {id}: not delivered")) {
            "sent back: the tell not delivered, the seat busy".into()
        } else if l.starts_with(&format!("tell {id}: it has the sentence"))
            || l.starts_with(&format!("told {id}: it has the sentence"))
            || l.starts_with(&format!("told {id}: delivered, acknowledged"))
            || l.contains("was typed to it, now it reads idle")
        {
            "told: the seat has the sentence".into()
        } else if l.starts_with("told ") && l.contains("typed at the seat") {
            "told: typed at the seat, no turn seen yet".into()
        } else if l.contains("sent back — wsp: the rebase") || (l.contains("sent back —") && l.contains("rebase")) {
            "rebase refused: sent back to the seat".into()
        } else if l.contains("again: it was sent back") {
            "sent back: the seat started again".into()
        } else if l.contains(&format!("{id} sent back")) {
            "sent back by wsp".into()
        } else if l.contains("its verifier blocked") && l.contains("sent back or answered") {
            "sent back or answered? the barrier waits on it".into()
        } else if l.contains(&format!("{id} waits on ")) && l.contains("it has not landed") {
            "owed to another row (an after that has not landed)".into()
        } else if l.contains("verification holds and the agent is left running") {
            "the verdict read: the seat left running".into()
        } else {
            continue;
        };
        out.push((*t, why));
    }
    out.sort_by_key(|m| m.0);
    out
}

/// How long the shell a run line says the row's seat waits on has run, where
/// the line is one of the two the run writes for it: the held line (`… r-1
/// (cpd-1) is waiting on 2 shells of its own still running`, no age: 0) and the
/// turn-over line the governor is told (`r-1 (cpd-1): turn over, waiting on its
/// own `sh` (`11m`)`).
fn own_shell_line(l: &str, id: &str) -> Option<i64> {
    let named = l.contains(&format!("{id} ("));
    if named && l.contains("of its own still running") && l.contains(" is waiting on ") {
        return Some(0);
    }
    let rest = l.split_once(&format!("{id} ("))?.1.split_once("): turn over, waiting on its own `")?.1;
    let age = rest.split_once("` (`").and_then(|(_, a)| a.split_once('`')).map(|(a, _)| a).unwrap_or("");
    Some(age_secs(age).unwrap_or(0))
}

/// The seconds in an age as [`util::duration_human`] writes it (`45s`, `11m`,
/// `1h5m`, `2d3h`); `None` for anything else (`just now`).
fn age_secs(s: &str) -> Option<i64> {
    let (mut total, mut n, mut any) = (0i64, String::new(), false);
    for c in s.trim().chars() {
        if c.is_ascii_digit() {
            n.push(c);
            continue;
        }
        let unit = match c {
            'd' => 86_400,
            'h' => 3600,
            'm' => 60,
            's' => 1,
            _ => return None,
        };
        total += n.parse::<i64>().ok()? * unit;
        n.clear();
        any = true;
    }
    (any && n.is_empty()).then_some(total)
}

/// When the row's seat waited on a shell of its own, `(from, line)`: from when
/// the shell started (the line less the age it gives) to the run's line
/// (`compound-react-042`). The sweep runs each on to the seat's next turn.
pub(crate) fn own_shells(i: &Inputs) -> Vec<(i64, i64)> {
    let mut out: Vec<(i64, i64)> = i.cycle.iter().filter_map(|(t, l)| own_shell_line(l, &i.id).map(|age| (t - age, *t))).collect();
    out.sort_unstable();
    out
}

/// When the row stood blocked: from each `task-blocked` to the next event that
/// moves its status (a release, a todo, a reopen, a review, a park), or to the
/// row's last phase end.
pub(crate) fn blocked_spans(i: &Inputs, end: i64) -> Vec<(i64, i64)> {
    let mut out = Vec::new();
    let mut from: Option<i64> = None;
    let mut evs: Vec<&(i64, String, Value)> = i.events.iter().filter(|(_, _, d)| d["id"] == i.id.as_str()).collect();
    evs.sort_by_key(|e| e.0);
    for (t, k, _) in evs {
        match k.as_str() {
            "task-blocked" => {
                from.get_or_insert(*t);
            }
            "task-todo" | "task-doing" | "task-reopen" | "task-review" | "task-released" | "task-parked" | "task-done" | "task-claimed" => {
                if let Some(f) = from.take() {
                    out.push((f, (*t).max(f)));
                }
            }
            _ => {}
        }
    }
    if let Some(f) = from {
        out.push((f, end.max(f)));
    }
    out
}

/// The fixed parts of a row, from what [`gather`] read and the phases [`build`] made.
pub(crate) fn fixed_of(i: &Inputs, phases: &[Phase]) -> (Option<i64>, Option<i64>, i64, Option<i64>) {
    let spawn = spawn_line(i).or_else(|| phases.iter().map(|p| p.start).min());
    let member = match (spawn, i.seat_first) {
        (Some(a), Some(b)) if b >= a => Some(b - a),
        _ => None,
    };
    // Only a phase that ends at the seat's own first line is that figure.
    let verifier = phases.iter().find(|p| p.label == VERIFIER_START).map(|p| p.seconds());
    let held = holds_after_verdict(i, phases).into_iter().map(|(_, s)| s).max();
    (member, verifier, i.verifiers.iter().map(|v| v.test_secs).sum(), held)
}

/// Every seat left standing past a verdict that holds, and for how long
/// (`compound-react-040`): the verifier, from the verdict to its `ended …,
/// which verified` line; and the row's own seat, from the last holding verdict
/// it was left running at to its first release or `ended` line after it. `(seat, seconds)`,
/// the verifier first. The verdict-to-despawn figure is the longest of these:
/// compound-react-040's verifier was ended 14 s after its verdict while its own
/// seat, cpd-115, stood 396 min more, and the figure read the 14 s alone.
pub(crate) fn holds_after_verdict(i: &Inputs, phases: &[Phase]) -> Vec<(String, i64)> {
    let id = i.id.as_str();
    let mut out = Vec::new();
    if let Some(p) = phases.iter().find(|p| p.label == VERIFIER_HELD) {
        let line = i.cycle.iter().find(|(t, l)| *t == p.end && l.starts_with("ended ") && l.contains(&format!("which verified {id}")));
        let seat = line.and_then(|(_, l)| l.strip_prefix("ended ")?.split([',', ' ']).next().map(str::to_string)).filter(|s| !s.is_empty());
        out.push((seat.map_or_else(|| "the verifier".to_string(), |s| format!("verifier {s}")), p.seconds()));
    }
    let verdicts: Vec<(i64, bool)> = i.kind("task-verified", id).into_iter().map(|(t, d)| (t, d["verdict"] != "blocks")).collect();
    let claims: Vec<(i64, Option<String>)> = i.kind("task-claimed", id).into_iter().map(|(t, d)| (t, d["pane"].as_str().map(str::to_string))).collect();
    let pane_at = |t: i64| claims.iter().filter(|(c, _)| *c <= t).max_by_key(|(c, _)| *c).and_then(|(_, p)| p.clone());
    // Where the row's own seat stopped standing: each release, and each line that ended a pane it was claimed by.
    let mut ends: Vec<(i64, Option<String>)> = i.kind("task-released", id).into_iter().map(|(t, _)| (t, None)).collect();
    for pane in claims.iter().filter_map(|(_, p)| p.as_deref()) {
        let e = format!("ended {pane}");
        ends.extend(i.cycle.iter().filter(|(_, l)| *l == e || l.starts_with(&format!("{e},")) || l.starts_with(&format!("{e} "))).map(|(t, _)| (*t, Some(pane.to_string()))));
    }
    // The first end after a verdict is where that seat stopped standing: a second
    // release with no claim between (compound-react-012's, days after its seat
    // went) ends nothing, so each verdict is read to its first end alone.
    ends.sort_by_key(|(t, _)| *t);
    let mut ended_at: Vec<i64> = Vec::new();
    let mut own: Option<(String, i64)> = None;
    for (end, pane) in ends {
        // The last verdict before the end, in the same seating, and only if it held:
        // a seat sent back after it was working again, not left standing.
        let Some(&(v, held)) = verdicts.iter().filter(|(t, _)| *t <= end).max_by_key(|(t, _)| *t) else { continue };
        if !held || claims.iter().any(|(c, _)| *c > v && *c <= end) {
            continue;
        }
        let seat = pane_at(v);
        if pane.is_some() && pane != seat {
            continue;
        }
        if ended_at.contains(&v) {
            continue;
        }
        ended_at.push(v);
        let secs = end - v;
        if own.as_ref().is_none_or(|(_, s)| secs > *s) {
            own = Some((seat.unwrap_or_else(|| "the row's own seat".to_string()), secs));
        }
    }
    out.extend(own);
    out
}

pub(crate) fn row_figures(store: &Store, id: &str) -> RowFigures {
    let i = gather(store, id);
    let phases = build(&i);
    let run = run_legs_for(&i, &phases, None);
    let seat = seat_report(&i, &phases, &run, &transcript_ref);
    let mut f = figures_of(&phases, &seat);
    f.blocked = blocked_spans(&i, f.end);
    f.markers = idle_markers(&i);
    f.shells = own_shells(&i);
    (f.spawn_to_first, f.verifier_spawn_to_first, f.verifier_test_secs, f.verdict_to_despawn) = fixed_of(&i, &phases);
    f.verdict_to_despawn_seats = holds_after_verdict(&i, &phases);
    f
}

/// The figures from the phases and the seat report, which `wsp timeline` prints
/// from too: [`to_json`] and [`seat_json`] are what a test holds this against.
pub(crate) fn figures_of(phases: &[Phase], seat: &SeatReport) -> RowFigures {
    let mut f = RowFigures::default();
    let a = phases.iter().map(|p| p.start).min().unwrap_or(0);
    let b = phases.iter().map(|p| p.end).max().unwrap_or(0);
    f.wall = (b - a).max(0);
    f.start = a;
    f.end = b;
    for p in phases.iter().filter(|p| p.shared) {
        f.shared += p.seconds();
    }
    for p in phases.iter().filter(|p| p.gap) {
        f.gap += p.seconds();
        let why = p.waiting_for.clone().unwrap_or_else(|| "unnamed".into());
        if p.shared {
            f.shared_gap += p.seconds();
            *f.shared_waits.entry(why.clone()).or_insert(0) += p.seconds();
        }
        if !p.shared && (why.starts_with(PARKED_CAUSE) || why.starts_with(REWORK_CAUSE) || why.starts_with(PERSON_STEP_CAUSE)) {
            f.between_attempts += p.seconds();
        }
        *f.waits.entry(why).or_insert(0) += p.seconds();
    }
    f.sessions = seat.sessions.len();
    f.missing = seat.missing.len();
    f.untimed = seat.untimed.len();
    f.measured = f.missing == 0 && f.sessions > 0;
    f.active = crate::wallbuckets::merge(&seat.active);
    f.spans = phases.iter().map(crate::wallbuckets::Span::of).collect();
    for p in &seat.phases {
        f.calls += p.calls;
        f.model_secs += p.model_secs;
        f.tokens_in += p.tokens_in;
        f.cache_read += p.cache_read;
        f.tokens_out += p.tokens_out;
        for (k, v) in &p.tools {
            *f.tools.entry(k.clone()).or_insert(0) += v;
        }
    }
    f
}

const USAGE: &str = "wsp timeline <row> [--json] [--legs DIR]";

pub fn timeline(store: &Store, args: &Args) -> i32 {
    let Some(needle) = args.rest.first() else {
        eprintln!("usage: {USAGE}");
        return 2;
    };
    let t = match store.task_or_why(needle) {
        Ok(t) => t,
        Err(why) => {
            eprintln!("{why}");
            return 1;
        }
    };
    let i = gather(store, &t.id);
    let phases = build(&i);
    let seat_run_legs = run_legs_for(&i, &phases, args.get("legs"));
    let seat = seat_report(&i, &phases, &seat_run_legs, &transcript_ref);
    if args.json() {
        let mut v = to_json(&i, &phases);
        v["utc"] = json!(true);
        v["seat"] = seat_json(&seat);
        println!("{v}");
    } else {
        print!("{}", render(&i, &phases, &Paint::new()));
        print!("{}", render_seat(&seat));
    }
    0
}

/// The verbs this module answers to, each with the help it carries. `main` and
/// the help text are assembled from these; nothing in `main.rs` names them.
pub const VERBS: &[crate::verbs::Verb] = &[
    crate::verbs::Verb {
        names: &["timeline"],
        run: timeline,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Agents,
                order: 620,
                text: r##"  wsp timeline <row> [--json] [--legs DIR]
                                    where a row's wall time goes, spawn to barrier:
                                    one line per phase, and the gaps where nothing
                                    ran named for what they waited on
                                    then the seat's own model time and tokens per
                                    phase, split at the row's legs (times UTC);
                                    legs from run-leg.sh's timeline.json under the
                                    row's tree, or --legs DIR, else wsp's own"##,
            },
        ],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(t: i64, kind: &str, d: Value) -> (i64, String, Value) {
        (t, kind.into(), d)
    }

    fn row() -> Inputs {
        let id = "r-1";
        Inputs {
            id: id.into(),
            events: vec![
                ev(110, "task-claimed", json!({"id": id})),
                ev(112, "session-learned", json!({"id": id, "session": "s"})),
                ev(160, "leg-added", json!({"id": id, "leg": "a"})),
                ev(165, "task-released", json!({"id": id})),
                ev(200, "session-learned", json!({"id": id})),
                ev(300, "leg-done", json!({"id": id, "leg": "a"})),
                ev(305, "task-review", json!({"id": id})),
                ev(380, "task-verified", json!({"id": id})),
            ],
            cycle: vec![
                (100, "starting r-1 on claude (new)".into()),
                (190, "list group 1: starting leg a of r-1".into()),
                (306, "spawn r-1 --agent --verify --kind claude".into()),
                (310, "list group 1: started r-1 · verifying".into()),
            ],
            legs: vec![LegIn { name: "a".into(), ..Default::default() }],
            first_request: Some(118),
            group: None,
            holder_events: Vec::new(),
            ..Default::default()
        }
    }

    fn two_seatings(parked: bool) -> Inputs {
        let id = "r-1";
        let mut events = vec![
            ev(110, "task-claimed", json!({"id": id, "pane": "p1"})),
            ev(112, "session-learned", json!({"id": id, "pane": "p1", "session": "s1"})),
        ];
        let mut cycle = vec![(100, "starting r-1 on claude (new)".to_string())];
        if parked {
            events.push(ev(130, "task-after", json!({"id": id, "after": ["r-9"]})));
            events.push(ev(140, "task-parked", json!({"id": id})));
            events.push(ev(150, "task-released", json!({"id": id})));
        } else {
            events.push(ev(200, "task-review", json!({"id": id})));
            events.push(ev(260, "task-verified", json!({"id": id, "verdict": "blocks"})));
            events.push(ev(400, "task-released", json!({"id": id})));
        }
        cycle.push((1990, "starting r-1 on claude (new)".to_string()));
        events.push(ev(2000, "task-claimed", json!({"id": id, "pane": "p2"})));
        events.push(ev(2002, "session-learned", json!({"id": id, "pane": "p2", "session": "s2"})));
        events.push(ev(2300, "task-review", json!({"id": id})));
        events.push(ev(2400, "task-verified", json!({"id": id, "verdict": "holds"})));
        Inputs { id: id.into(), events, cycle, ..Default::default() }
    }

    /// A row released before its review and started again is two seats, and the
    /// stretch between them is the row parked, not its seat finishing (`compounding-217`).
    #[test]
    fn a_parked_row_started_again_is_two_attempts_and_a_named_wait_between() {
        let i = two_seatings(true);
        let a = attempts(&i);
        assert_eq!(a.len(), 2);
        assert_eq!((a[0].claim, a[0].end, a[0].review), (110, Some(150), None));
        assert_eq!((a[1].spawn, a[1].claim, a[1].end, a[1].review), (Some(1990), 2000, None, Some(2300)));
        let ph = build(&i);
        let work: Vec<&str> = ph.iter().filter(|p| p.label.starts_with("the seat's work")).map(|p| p.label.as_str()).collect();
        assert_eq!(work, ["the seat's work until release, attempt 1 of 2", "the seat's work until review, attempt 2 of 2"]);
        let gap = ph.iter().find(|p| p.gap && p.label.starts_with("parked at")).expect("the stretch between is named");
        assert_eq!((gap.start, gap.end), (150, 1990));
        let why = gap.waiting_for.as_deref().unwrap();
        assert!(why.starts_with(PARKED_CAUSE) && why.contains("waiting on r-9"), "{why}");
        assert!(!ph.iter().any(|p| p.waiting_for.as_deref().is_some_and(|w| w.contains("seat finishing"))), "{ph:?}");
        let f = figures_of(&ph, &SeatReport::default());
        assert_eq!(f.between_attempts, 1840);
        let bounds = seat_bounds(&i, &ph, &[]);
        assert_eq!(bounds.iter().map(|b| (b.1, b.2)).collect::<Vec<_>>(), [(100, 150), (1990, 2300)], "{bounds:?}");
        assert!(bounds[0].0.contains("attempt 1 of 2") && bounds[1].0.contains("spawn to review"));
    }

    /// A row reviewed, sent back and reseated is rework between the review and
    /// the next seat, and the second seat's session is the row's own.
    #[test]
    fn a_row_sent_back_after_review_and_reseated_has_a_rework_stretch_not_a_seat_idle() {
        let i = two_seatings(false);
        let ph = build(&i);
        let gap = ph.iter().find(|p| p.gap && p.waiting_for.as_deref().is_some_and(|w| w.starts_with(REWORK_CAUSE))).expect("rework named");
        assert_eq!((gap.start, gap.end), (200, 1990));
        let work = ph.iter().find(|p| p.label == "the seat's work until review, attempt 2 of 2").unwrap();
        assert_eq!((work.start, work.end), (2002, 2300));
        assert_eq!(seat_sessions(&i, &[]), ["s1", "s2"], "a session learned after the first review by a pane that claimed the row since is the row's seat");
    }

    #[test]
    fn a_claim_after_the_last_passing_verdict_is_a_reopen_of_a_landed_row_not_an_attempt() {
        let mut i = two_seatings(true);
        assert_eq!(attempts(&i).len(), 2, "the claim at 2000 is before the verdict at 2400");
        // A later reopen, after the row had landed.
        i.events.push(ev(5000, "task-released", json!({"id": "r-1"})));
        i.events.push(ev(6000, "task-claimed", json!({"id": "r-1", "pane": "p3"})));
        assert_eq!(attempts(&i).len(), 2, "the claim at 6000 follows the passing verdict at 2400");
        i.events.retain(|(t, k, _)| !(*t == 2000 && k == "task-claimed"));
        assert_eq!(attempts(&i).len(), 1, "with no claim before the verdict there is one seat");
    }

    /// A sent-back row's idle is named by the lines the log already writes (`wsp-559`).
    #[test]
    fn the_logs_lines_name_what_a_sent_back_row_waited_on() {
        let mut i = row();
        i.events.push(ev(390, "task-verified", json!({"id": "r-1", "verdict": "blocks"})));
        i.events.push(ev(520, "task-reopen", json!({"id": "r-1"})));
        i.events.push(ev(700, "task-handoff", json!({"id": "r-1", "to": "r-9"})));
        i.cycle.push((391, "tell g: A verifier found a problem in the g run: r-1 (x)".into()));
        i.cycle.push((395, "tell r-1: not delivered — wsp: cannot say whether it is at a prompt".into()));
        i.cycle.push((500, "tell r-1: it has the sentence".into()));
        i.cycle.push((600, "g group 1: r-1 sent back — wsp: the rebase onto master does not apply cleanly".into()));
        i.cycle.push((650, "g group 1: r-1 (cpd-1) is waiting on 2 shells of its own still running, which wakes it".into()));
        i.cycle.push((660, "g group 2: r-7 waits on r-1 — after: it has not landed".into()));
        let m = idle_markers(&i);
        let at = |t: i64| m.iter().filter(|x| x.0 <= t).last().map(|x| x.1.as_str()).unwrap_or("-");
        assert!(at(390).starts_with("sent back: the verdict given"), "{m:?}");
        assert!(at(391).contains("with the governor"));
        assert!(at(395).contains("not delivered"));
        assert!(at(510).contains("has the sentence"));
        assert!(at(520).starts_with("reopened"));
        assert!(at(600).starts_with("rebase refused"));
        assert!(at(655).contains("shells of its own"));
        assert!(at(700).contains("r-9"), "the hand-off names the row the remainder went to");
        assert_eq!(at(100), "-", "nothing before the claim: the sweep reads it as none recorded");
        assert_eq!(at(115), crate::wallbuckets::NONE_RECORDED);
        assert!(m.windows(2).all(|w| w[0].0 <= w[1].0), "sorted");
        assert!(!m.iter().any(|x| x.1.contains("r-7")), "another row's line is not this row's reason");
    }

    /// A seat that asked a question and waits on the governor or a person reads as that, from the
    /// moment of the question, to the answer (`compounding-465`), in the lines the real log writes.
    #[test]
    fn a_seat_waiting_on_the_governors_or_a_persons_answer_is_named_with_the_question() {
        let mut i = row();
        i.events.push(ev(500, "message-raised", json!({"id": "m-g", "about": {"task": "r-1"}, "for": {"seat": "g"}, "from": {"pane": "cpd-1"}, "shape": "question"})));
        i.events.push(ev(800, "message-raised", json!({"id": "m-a", "about": {"task": "r-1"}, "for": null, "from": {"human": true}, "reply_to": "m-g", "shape": "notification"})));
        i.events.push(ev(900, "message-raised", json!({"id": "m-p", "about": {"task": "r-1"}, "for": "person", "from": {"pane": "cpd-1"}, "shape": "question"})));
        i.events.push(ev(901, "message-raised", json!({"id": "m-other", "about": {"task": "r-9"}, "for": "person", "from": {"pane": "cpd-9"}, "shape": "question"})));
        i.cycle.push((560, "g group 1: r-1 (cpd-1) is waiting on the g governor to answer its question m-g — not told and not ended, and held for as long as it waits".into()));
        i.cycle.push((561, "tell g: r-1 in the g run is waiting on the g governor to answer its question m-g (waiting · p…".into()));
        i.cycle.push((950, "g group 1: r-1 (cpd-1) is held in a tool call for a person to answer m-p — not told and not ended, and held for as long as it waits".into()));
        i.cycle.push((1200, "g group 1: r-1 (cpd-1) is stopped on a prompt only a person can answer — not told and not ended".into()));
        i.cycle.push((1300, "g group 1: r-7 (cpd-7) is waiting on a person to answer its question m-x — not told".into()));
        let m = idle_markers(&i);
        let at = |t: i64| m.iter().filter(|x| x.0 <= t).last().map(|x| x.1.as_str()).unwrap_or("-");
        assert_eq!(at(500), "waiting on the governor's answer (question m-g)", "from the question, before the run's line");
        assert_eq!(at(700), "waiting on the governor's answer (question m-g)");
        assert!(at(800).starts_with("answered:"), "{m:?}");
        assert_eq!(at(900), "waiting on a person's answer (question m-p)");
        assert_eq!(at(960), "waiting on a person's answer (question m-p)", "a tool call held for a person");
        assert_eq!(at(1250), "waiting on a person's answer (a prompt on its screen)");
        assert_eq!(at(1400), "waiting on a person's answer (a prompt on its screen)", "another row's wait is not this row's");
        assert!(!m.iter().any(|x| x.1.contains("m-x") || x.1.contains("m-other")), "{m:?}");
        assert_eq!(m.iter().filter(|x| x.1.contains("m-g")).count(), 2, "the question and its held line are one reason");
    }

    /// compound-react-042: 024's seat ran five baseline trials in a tracked
    /// background shell. The run wrote its turn-over line (22:21:04, the shell
    /// `11m` old), told it at 22:23:07, and wrote nothing more until 23:10:31,
    /// when the shell ended and the seat moved: the 47 minutes are the seat's
    /// own shells, not "told" and not "none recorded".
    #[test]
    fn a_seat_on_its_own_trial_shell_is_its_own_shells_not_the_tell_or_none_recorded() {
        use crate::wallbuckets::{group_wall, seat_idle_reasons, Class, MemberIn, Span, NONE_RECORDED, OWN_SHELLS};
        let at = |hms: &str| util::epoch_of(&format!("2026-11-19T{hms}Z"));
        let id = "compound-react-024";
        let mut i = Inputs { id: id.into(), ..Default::default() };
        i.events.push(ev(at("22:05:00"), "task-claimed", json!({"id": id})));
        i.events.push(ev(at("23:14:00"), "task-review", json!({"id": id})));
        i.cycle.push((at("22:21:04"), "tell retro-8: compound-react-024 (cpd-79): turn over, waiting on its own `sh` (`11m`); its last words: \"Five 10-minute baseline trials are running".into()));
        i.cycle.push((at("22:23:07"), "told compound-react-024: it has the sentence".into()));
        i.cycle.push((at("22:23:07"), "tell retro-8: compound-react-024 (cpd-79): this member's seat reads idle with this row still at doing — told it to finish".into()));
        assert_eq!(own_shells(&i), vec![(at("22:10:04"), at("22:21:04"))], "from the shell's start, the line less its age");
        assert_eq!(own_shell_line("tell l: r-1 (cpd-1): turn over, waiting on its own `cargo test` (`1h5m`); its", "r-1"), Some(3900));
        assert_eq!(own_shell_line("tell l: r-1 (cpd-1): turn over, waiting on its own `a shell` (`just now`); its", "r-1"), Some(0));
        assert_eq!(own_shell_line("tell l: r-12 (cpd-1): turn over, waiting on its own `sh` (`2m`); its", "r-1"), None, "another row's line");
        let m = MemberIn {
            id: id.into(),
            spans: vec![Span { start: at("22:05:00"), end: at("22:05:10"), class: Class::Start }, Span { start: at("22:05:10"), end: at("23:15:00"), class: Class::Seat }],
            active: vec![(at("22:05:10"), at("22:21:00")), (at("23:10:31"), at("23:15:00"))],
            measured: true,
            markers: idle_markers(&i),
            shells: own_shells(&i),
            ..Default::default()
        };
        let g = group_wall(&[m], None, Some(at("23:15:00"))).unwrap();
        let by = seat_idle_reasons(g.waits.iter().map(|((_, c), s)| (c.as_str(), *s)));
        assert_eq!(by.get(OWN_SHELLS), Some(&(at("23:10:31") - at("22:21:00"))), "{by:?}");
        assert!(!by.keys().any(|k| k.starts_with("told") || k == NONE_RECORDED), "{by:?}");
        assert_eq!(by.values().sum::<i64>(), at("23:10:31") - at("22:21:00"), "every idle second carries one reason");
    }

    /// The run's lines say `told`, not `tell`, for a row's own seat (`compounding-465`).
    #[test]
    fn the_told_lines_for_the_row_read_as_the_tell_pending_or_delivered() {
        let mut i = row();
        i.cycle.push((395, "told r-1: not delivered — wsp: r-1 was not told: cpd-1: delivery still pending after 30s".into()));
        i.cycle.push((500, "told r-1: it has the sentence".into()));
        let m = idle_markers(&i);
        let at = |t: i64| m.iter().filter(|x| x.0 <= t).last().map(|x| x.1.as_str()).unwrap_or("-");
        assert!(at(400).contains("not delivered"), "{m:?}");
        assert!(at(510).contains("has the sentence"), "{m:?}");
    }

    /// The verifier's seat is up long before wsp says `started` (`wsp-429`): the
    /// line is written when the step that spawned it ends.
    #[test]
    fn the_verifier_phase_starts_at_the_seats_first_line_not_wsps_started_line() {
        let mut i = row();
        i.verifiers = vec![VerifierSeat { learned: 306, first: 313, test_secs: 21 }];
        i.cycle[3].0 = 430;
        let ph = build(&i);
        let start = ph.iter().find(|p| p.label == VERIFIER_START).expect("from the first line");
        assert_eq!((start.start, start.end), (306, 313));
        let reads = ph.iter().find(|p| p.label.starts_with("verifier reads")).unwrap();
        assert_eq!(reads.start, 313, "the reading starts at the first line");
        assert!(!ph.iter().any(|p| p.label == VERIFIER_START_LINE));
        let (member, verifier, tests, _) = fixed_of(&i, &ph);
        assert_eq!((verifier, tests), (Some(7), 21));
        assert_eq!(member, None, "no transcript of the row's own seat was read");
    }

    #[test]
    fn without_the_first_line_wsps_started_line_is_the_start_and_says_it_lags() {
        let ph = build(&row());
        let start = ph.iter().find(|p| p.label == VERIFIER_START_LINE).expect("the fallback");
        assert_eq!((start.start, start.end), (306, 310));
        assert_eq!(fixed_of(&row(), &ph).1, None, "that is not the seat's first line, so it is not read");
    }

    #[test]
    fn the_seat_s_spawn_to_first_line_and_the_hold_after_the_verdict_are_read() {
        let mut i = row();
        i.seat_first = Some(104);
        i.cycle.push((385, "ended cpd-9, which verified r-1 (holds)".into()));
        let ph = build(&i);
        let (member, _, _, held) = fixed_of(&i, &ph);
        assert_eq!((member, held), (Some(4), Some(5)));
    }

    /// compound-react-040: the verifier was ended 14 s after its verdict, and
    /// the row's own seat, left running at that verdict, was released 6 h later.
    /// The figure is the worker seat's hold, and both seats are named by id.
    #[test]
    fn verdict_to_despawn_is_the_longest_seat_left_standing_not_the_verifiers_alone() {
        let mut i = row();
        i.events[0] = ev(110, "task-claimed", json!({"id": "r-1", "pane": "cpd-115"}));
        i.events.retain(|(_, k, _)| k != "task-released");
        i.cycle.push((394, "ended cpd-9, which verified r-1 (holds)".into()));
        i.events.push(ev(380 + 6 * 3600, "task-released", json!({"id": "r-1"})));
        let ph = build(&i);
        assert_eq!(holds_after_verdict(&i, &ph), vec![("verifier cpd-9".to_string(), 14), ("cpd-115".to_string(), 6 * 3600)]);
        assert_eq!(fixed_of(&i, &ph).3, Some(6 * 3600), "the worker seat's figure, not the verifier's 14 s");
        let j = fixed_json(&i, &ph);
        assert_eq!(j["verdict_to_despawn_seconds"], 6 * 3600);
        assert_eq!(j["verdict_to_despawn_by_seat"][1]["seat"], "cpd-115");
        assert!(render(&i, &ph, &Paint::new()).contains("verdict → despawn by seat: verifier cpd-9 14s · cpd-115 21600s"));

        // The run's own `ended <pane>` line ends the seat as a release does.
        i.events.retain(|(_, k, _)| k != "task-released");
        i.cycle.push((380 + 3600, "ended cpd-115, its verdict held".into()));
        assert_eq!(fixed_of(&i, &build(&i)).3, Some(3600));
    }

    /// compound-react-012: the seat was released an hour after its holding
    /// verdict, and the row released again days later with no claim between.
    /// The seat's figure is the hour; the later release ends nothing.
    #[test]
    fn a_second_release_after_the_seat_went_is_not_the_seat_standing() {
        let mut i = row();
        i.events[0] = ev(110, "task-claimed", json!({"id": "r-1", "pane": "cpd-9"}));
        i.events.retain(|(_, k, _)| k != "task-released");
        i.events.push(ev(380 + 4 * 86400, "task-released", json!({"id": "r-1"})));
        i.events.push(ev(380 + 3600, "task-released", json!({"id": "r-1"})));
        let ph = build(&i);
        let own: Vec<_> = holds_after_verdict(&i, &ph).into_iter().filter(|(s, _)| s == "cpd-9").collect();
        assert_eq!(own, vec![("cpd-9".to_string(), 3600)]);

        // An `ended <pane>` line after the release is no later end of that seat either.
        i.cycle.push((380 + 2 * 86400, "ended cpd-9".into()));
        assert_eq!(fixed_of(&i, &build(&i)).3, Some(3600));
    }

    /// A seat sent back after the verdict was working again: its later release is no hold.
    #[test]
    fn a_seat_released_after_a_blocking_verdict_held_nothing() {
        let mut i = row();
        i.events.retain(|(_, k, _)| k != "task-released");
        i.events.push(ev(390, "task-verified", json!({"id": "r-1", "verdict": "blocks"})));
        i.events.push(ev(5000, "task-released", json!({"id": "r-1"})));
        let ph = build(&i);
        assert!(holds_after_verdict(&i, &ph).iter().all(|(seat, _)| seat.starts_with("verifier") || seat == "the verifier"), "{:?}", holds_after_verdict(&i, &ph));
    }

    /// compounding-114 and 117: reviewed, verified, then reviewed again (a fix
    /// for the barrier) and released. The release is the seat's end, not where its
    /// work stopped, and the last review is not the one the verdict followed.
    #[test]
    fn a_row_reviewed_again_after_its_verdict_keeps_its_verifier_and_barrier_phases() {
        let mut i = row();
        i.events.retain(|(_, k, _)| k != "task-released" && k != "leg-added");
        i.legs.clear();
        i.events.push(ev(500, "task-review", json!({"id": "r-1"})));
        i.events.push(ev(900, "task-released", json!({"id": "r-1"})));
        let ph = build(&i);
        assert!(ph.iter().any(|p| p.label.starts_with("verifier reads") && p.end == 380), "{ph:?}");
        assert!(ph.iter().any(|p| p.label.starts_with("the seat's work until review") && p.end == 305), "{ph:?}");
        let after = ph.iter().find(|p| p.label.starts_with("the row's own seat after the verdict → released")).expect("named");
        assert_eq!((after.start, after.end), (380, 900));
    }

    #[test]
    fn test_and_build_commands_are_what_measure_py_counts_and_their_seconds_are_summed() {
        assert!(is_test_command("cd app && cargo test cmd_spawn::"));
        assert!(is_test_command("npm run check:box"));
        assert!(!is_test_command("echo cargo test"));
        assert!(!is_test_command("cat > t.sh <<EOF\ncargo test\nEOF"));
        assert!(!is_test_command("git status"));
        let text = [
            line(10, json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": "a", "name": "Bash", "input": {"command": "cargo test x"}}]}})),
            line(40, json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "a"}]}})),
            line(41, json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": "b", "name": "Bash", "input": {"command": "git diff"}}]}})),
            line(50, json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "b"}]}})),
        ]
        .join("\n");
        assert_eq!(test_secs(&text), 30);
        assert_eq!(first_stamp(&text), Some(10));
    }

    #[test]
    fn a_row_reads_as_phases_and_the_wait_after_a_release_is_named_a_gap() {
        let ph = build(&row());
        let gap = ph.iter().find(|p| p.label.starts_with("gap before leg a")).expect("a gap before the leg");
        assert_eq!((gap.start, gap.end, gap.seconds()), (165, 190, 25));
        assert!(gap.gap && gap.waiting_for.as_deref().unwrap().contains("tick"), "{gap:?}");
        assert!(ph.iter().any(|p| p.label.starts_with("leg a (seat up after 10s)") && p.seconds() == 110), "{ph:?}");
        assert!(ph.windows(2).all(|w| w[0].start <= w[1].start), "in time order");
        let ver = ph.iter().find(|p| p.label.starts_with("review typed → verifier")).unwrap();
        assert_eq!(ver.seconds(), 1);
        assert!(ver.waiting_for.as_deref().unwrap().contains("hand-off"));
    }

    #[test]
    fn a_leg_with_no_seat_is_a_gap_not_work() {
        let mut i = row();
        i.events.retain(|(_, k, _)| k != "session-learned");
        let ph = build(&i);
        let leg = ph.iter().find(|p| p.label.starts_with("leg a (no seat")).expect("the leg is named");
        assert!(leg.gap && leg.seconds() > 0, "{leg:?}");
        assert!(!ph.iter().any(|p| !p.gap && p.label.starts_with("leg a")), "{ph:?}");
    }

    /// `wsp-335`: a leg with no `session-learned` whose entry names its pane
    /// and ends `ended after 47s` (wsp-030's `finish`, cpd-169) is 47 s of the
    /// leg's own work, not "no seat came up"; the entry is read off the log.
    #[test]
    fn a_leg_whose_entry_names_its_seat_is_work_not_no_seat() {
        let mut t = crate::model::Task::new("a task", "r-1");
        t.body = "## Log\n- 1970-01-01T00:03:10Z leg a started by pane cpd-169 · spawned at opus/high\n- 1970-01-01T00:03:57Z leg a ended after 47s · 3 turns\n".into();
        let mut i = row();
        i.events.retain(|(_, k, _)| k != "session-learned");
        for e in i.events.iter_mut().filter(|e| e.1 == "leg-done") {
            e.0 = 237;
        }
        i.leg_seats = leg_seats_of(&t);
        assert_eq!(i.leg_seats, vec![LegSeat { leg: "a".into(), pane: "cpd-169".into(), start: 190, end: Some(237) }]);
        let ph = build(&i);
        let leg = ph.iter().find(|p| p.label.starts_with("leg a")).expect("the leg is there");
        assert!(!leg.gap && leg.seconds() == 47, "{ph:?}");
        assert!(!ph.iter().any(|p| p.waiting_for.as_deref().is_some_and(|w| w.contains("no seat came up"))), "{ph:?}");
    }

    /// `wsp-335` keeps `wsp-016`'s case: the seat the entry names ended and the
    /// entry read running 60 min more: 47 s of work, the 60 min a gap.
    #[test]
    fn a_leg_whose_named_seat_ended_long_before_the_close_is_a_gap_after_it() {
        let mut i = row();
        i.events.retain(|(_, k, _)| k != "session-learned");
        for e in i.events.iter_mut() {
            match e.1.as_str() {
                "leg-done" => e.0 = 237 + 3600,
                "task-review" => e.0 = 237 + 3605,
                "task-verified" => e.0 = 237 + 3680,
                _ => {}
            }
        }
        i.leg_seats = vec![LegSeat { leg: "a".into(), pane: "cpd-169".into(), start: 190, end: Some(237) }];
        let ph = build(&i);
        let work: Vec<&Phase> = ph.iter().filter(|p| p.label.starts_with("leg a")).collect();
        assert!(work.len() == 2 && !work[0].gap && work[0].seconds() == 47, "{ph:?}");
        assert!(work[1].gap && work[1].seconds() == 3600 && work[1].waiting_for.as_deref().is_some_and(|w| w.starts_with(LEG_SEAT_ENDED)), "{ph:?}");
    }

    /// A leg whose seat came up, worked and ended before the leg was closed
    /// (`wsp-016`): the leg is work up to the seat's end, and the rest is a gap
    /// named for the seat and when it ended.
    #[test]
    fn a_leg_whose_seat_ended_before_its_close_is_work_to_the_end_then_a_gap() {
        let mut i = row();
        for e in i.events.iter_mut().filter(|(t, k, _)| *t == 200 && k == "session-learned") {
            e.2 = json!({"id": "r-1", "pane": "cpd-121"});
        }
        i.cycle.push((250, format!("r-1: {} {} (member, cpd-121), no pid of it running and its tree kept", crate::ending::EXIT_ENDED, util::iso_at(240))));
        i.cycle.sort_by_key(|(t, _)| *t);
        let ph = build(&i);
        let work = ph.iter().find(|p| !p.gap && p.label.starts_with("leg a")).expect("the leg's work");
        assert_eq!((work.start, work.end), (190, 240), "{ph:?}");
        let gap = ph.iter().find(|p| p.gap && p.waiting_for.as_deref().is_some_and(|w| w.starts_with(LEG_SEAT_ENDED))).expect("the stretch after the seat is named");
        assert_eq!((gap.start, gap.end), (240, 300));
        let why = gap.waiting_for.as_deref().unwrap();
        assert!(why.contains("cpd-121") && why.contains(&util::iso_at(240)), "{why}");
    }

    /// A seat that ended after the leg was closed changes nothing.
    #[test]
    fn a_leg_whose_seat_ended_after_its_close_is_work_to_the_close() {
        let mut i = row();
        for e in i.events.iter_mut().filter(|(t, k, _)| *t == 200 && k == "session-learned") {
            e.2 = json!({"id": "r-1", "pane": "cpd-121"});
        }
        i.cycle.push((302, "ended cpd-121, which ran leg a of r-1".into()));
        i.cycle.sort_by_key(|(t, _)| *t);
        let ph = build(&i);
        let work = ph.iter().find(|p| !p.gap && p.label.starts_with("leg a")).expect("the leg's work");
        assert_eq!((work.start, work.end), (190, 300));
        assert!(!ph.iter().any(|p| p.waiting_for.as_deref().is_some_and(|w| w.starts_with(LEG_SEAT_ENDED))), "{ph:?}");
    }

    /// wsp-004 (`wsp-016`, the case in `wsp-011`): a row at the 8-leg cap is
    /// reopened, no rework leg can be added and no member is seated, so the
    /// step does not run. The log has the reopen: that is what it waited on,
    /// not nothing the logs name.
    #[test]
    fn a_reopen_at_the_leg_cap_with_no_member_seated_is_named_from_the_log() {
        let mut i = row();
        i.legs = (0..crate::legs::MAX_LEGS).map(|n| LegIn { name: format!("l{n}"), ..Default::default() }).collect();
        i.events.push(ev(1000, "task-reopen", json!({"id": "r-1"})));
        let why = wait_name(&i, 990, 1545, "r-1");
        assert!(why.starts_with(CAPPED_REOPEN) && why.contains("still unseated"), "{why}");
        i.events.push(ev(1500, "task-claimed", json!({"id": "r-1"})));
        let why = wait_name(&i, 990, 1545, "r-1");
        assert!(why.starts_with(CAPPED_REOPEN) && why.contains(&format!("claimed at {}", clock(1500))), "{why}");
        // Under the cap a reopen adds a rework leg and the run starts it: not this case.
        i.legs.truncate(3);
        assert!(wait_name(&i, 990, 1545, "r-1").starts_with("nothing the logs"));
    }

    /// The stretch after the last leg that holds a capped reopen: from the reopen
    /// to the next seat it is named for the reopen, not the seat finishing.
    #[test]
    fn a_capped_reopen_after_the_last_leg_is_split_out_of_the_seat_finishing() {
        let mut i = row();
        i.legs.extend((1..crate::legs::MAX_LEGS).map(|n| LegIn { name: format!("l{n}"), skipped: false, after: vec!["x".into()] }));
        // Only leg a starts; the review comes late, after a reopen and a claim.
        i.events.retain(|(_, k, _)| k != "task-review" && k != "task-verified");
        i.events.push(ev(400, "task-reopened", json!({"id": "r-1"})));
        i.events.push(ev(700, "task-claimed", json!({"id": "r-1"})));
        i.events.push(ev(800, "task-review", json!({"id": "r-1"})));
        let ph = build(&i);
        let capped = ph.iter().find(|p| p.waiting_for.as_deref().is_some_and(|w| w.starts_with(CAPPED_REOPEN))).expect("the reopen is named");
        assert_eq!((capped.start, capped.end), (400, 700), "{ph:?}");
        assert!(ph.iter().any(|p| p.gap && (p.start, p.end) == (300, 400)), "{ph:?}");
        assert!(ph.iter().any(|p| p.gap && (p.start, p.end) == (700, 800)), "{ph:?}");
    }

    #[test]
    fn a_leg_closed_late_is_timed_to_its_baton_and_the_stall_is_a_gap_not_work() {
        let mut i = row();
        for e in i.events.iter_mut().filter(|(_, k, d)| k == "leg-done" && d["leg"] == "a") {
            e.2["baton_at"] = json!(e.0 - 60);
            e.2["closed_by"] = json!("wsp: seat idle 9m");
        }
        let ph = build(&i);
        let leg = ph.iter().find(|p| p.label.starts_with("leg a (")).expect("the leg is a phase");
        let late = ph.iter().find(|p| p.label.starts_with("closed late: leg a done at")).expect("the stall is named");
        assert_eq!(leg.end, late.start, "work ends at the baton");
        assert_eq!(late.seconds(), 60);
        assert!(late.gap && late.waiting_for.as_deref().unwrap().contains("wsp: seat idle 9m"), "{late:?}");
    }

    #[test]
    fn a_cap_line_in_the_gap_names_the_cap() {
        let mut i = row();
        i.cycle.push((170, "list group 1: r-1 waits — the seat is at its cap of 4: x".into()));
        let ph = build(&i);
        let gap = ph.iter().find(|p| p.label.starts_with("gap before leg a")).unwrap();
        assert!(gap.waiting_for.as_deref().unwrap().contains("cap"), "{gap:?}");
    }

    #[test]
    fn a_cap_wait_names_its_holders_and_says_which_seat_had_finished() {
        let mut i = row();
        i.cycle.push((170, "list group 1: r-1 waits — the seat is at its cap of 4: r-9 · verifying in cpd-1, r-8 in cpd-2, r-7 starting".into()));
        i.holder_events.push((100, "task-verified".into(), "r-9".into()));
        i.holder_events.push((120, "task-review".into(), "r-8".into()));
        i.holder_events.push((900, "task-review".into(), "r-7".into()));
        let ph = build(&i);
        let why = ph.iter().filter_map(|p| p.waiting_for.clone()).find(|w| w.contains("cap")).expect("a cap wait");
        assert!(why.contains("r-9 · verifying in cpd-1 (finished "), "{why}");
        assert!(why.contains("r-8 in cpd-2 (finished "), "{why}");
        assert!(why.contains("r-7 starting (running)"), "{why}");
    }

    #[test]
    fn a_barrier_check_holder_that_passed_reads_finished_and_a_more_item_is_left_unlabelled() {
        let mut i = row();
        i.cycle.push((170, "list group 1: r-1 waits — the seat is at its cap of 4: other · barrier 2 in cpd-3, list · barrier 3 in cpd-4, 2 more".into()));
        i.holder_events.push((100, "barrier-passed".into(), "other · barrier 2".into()));
        i.holder_events.push((900, "barrier-passed".into(), "list · barrier 3".into()));
        let ph = build(&i);
        let why = ph.iter().filter_map(|p| p.waiting_for.clone()).find(|w| w.contains("cap")).expect("a cap wait");
        assert!(why.contains("other · barrier 2 in cpd-3 (finished "), "{why}");
        assert!(why.contains("list · barrier 3 in cpd-4 (running)"), "{why}");
        assert!(why.ends_with("2 more"), "{why}");
        assert!(!why.contains("2 more ("), "{why}");
    }

    #[test]
    fn a_cap_line_naming_the_row_as_a_holder_is_not_its_wait() {
        let mut i = row();
        i.cycle.push((170, "list group 1: r-2 waits — the seat is at its cap of 4: r-1 starting".into()));
        let ph = build(&i);
        let gap = ph.iter().find(|p| p.label.starts_with("gap before leg a")).unwrap();
        assert!(!gap.waiting_for.as_deref().unwrap().contains("cap"), "{gap:?}");
    }

    #[test]
    fn a_check_that_began_cold_says_so_in_its_phase_and_one_without_a_start_reads_as_before() {
        let cold = json!({"id": "r-1", "took": 95, "start": {"target": "cold", "load": 3.24, "cores": 8}, "step_secs": [{"name": "build", "secs": 768, "reused": false}, {"name": "fmt", "secs": 0, "reused": true}]});
        assert_eq!(check_start_note(&cold), " — started on target cold, load 3.2 of 8 cores — per step: build 768s");
        assert_eq!(check_start_note(&json!({"id": "r-1", "took": 95})), "");
        let line = "list group 1: barrier run — 1 step(s), 0 reused, 1 ran, 0 failed, 300s · per step: build 300s · start: pj: target cloned, load 1.0 of 8 cores · scope: full";
        assert_eq!(run_label(1, line.split("barrier run — ").nth(1).unwrap()), "barrier 1 run (1 step(s), 0 reused, 1 ran, 0 failed, 300s) — per step: build 300s — start: pj: target cloned, load 1.0 of 8 cores");
    }

    #[test]
    fn the_row_s_own_check_runs_from_review_to_the_task_checked_end() {
        let mut i = row();
        i.events.push(ev(405, "task-checked", json!({"id": "r-1", "took": 95})));
        i.cycle.retain(|(_, l)| !l.starts_with("spawn r-1 --agent --verify"));
        i.cycle.push((410, "spawn r-1 --agent --verify --kind claude".into()));
        i.cycle.push((415, "list group 1: started r-1 · verifying".into()));
        i.events.retain(|(_, k, _)| k != "task-verified");
        i.events.push(ev(500, "task-verified", json!({"id": "r-1"})));
        let ph = build(&i);
        let check = ph.iter().find(|p| p.label.starts_with("the row's own check")).expect("named");
        assert_eq!((check.start, check.end, check.gap), (305, 405, false));
        let after = ph.iter().find(|p| p.label.starts_with("row's check ended")).unwrap();
        assert_eq!(after.seconds(), 5);
        assert!(after.waiting_for.as_deref().unwrap().contains("hand-off"));
    }

    #[test]
    fn a_check_that_failed_between_two_legs_is_work_not_nothing_the_logs_name() {
        // compounding-506: review at 305 ran the row's check for 299s, it failed,
        // and the rework leg started at 620.
        let mut i = row();
        i.events.retain(|(_, k, _)| k != "task-verified");
        i.events.push(ev(604, "task-checked", json!({"id": "r-1", "took": 299})));
        i.events.push(ev(170, "leg-added", json!({"id": "r-1", "leg": "b"})));
        i.cycle.retain(|(_, l)| !l.starts_with("spawn r-1 --agent --verify"));
        i.cycle.push((620, "list group 1: starting leg b of r-1".into()));
        i.legs.push(LegIn { name: "b".into(), ..Default::default() });
        i.legs[0].name = "a".into();
        let ph = build(&i);
        let check = ph.iter().find(|p| p.label.starts_with("the row's own check")).expect("named");
        assert_eq!((check.start, check.end, check.gap), (305, 604, false), "{ph:?}");
        let after = ph.iter().find(|p| p.label.starts_with("row's check ended")).expect("the rest is a gap");
        assert_eq!((after.start, after.end), (604, 620));
        assert!(!ph.iter().any(|p| p.gap && p.start == 305), "no gap books the check run: {ph:?}");
    }

    #[test]
    fn the_barrier_run_is_a_phase_from_its_start_with_per_step_seconds() {
        let mut i = row();
        i.group = Some(("list".into(), 1, vec!["r-1".into()]));
        i.cycle.push((1000, "list group 1: barrier run — 2 step(s), 0 reused, 2 ran, 0 failed, 600s · per step: build 100s; test 480s".into()));
        i.cycle.push((1010, "tell list: Barrier 1 of list passed".into()));
        let ph = build(&i);
        let run = ph.iter().find(|p| p.label.starts_with("barrier 1 run (")).expect("the run is a phase");
        assert_eq!((run.start, run.end, run.gap), (400, 1000, false));
        assert!(run.label.contains("per step: build 100s; test 480s"), "{}", run.label);
        let wait = ph.iter().find(|p| p.label.starts_with("last verdict → barrier 1 run started")).unwrap();
        assert_eq!((wait.start, wait.end), (380, 400));
        assert!(ph.iter().any(|p| p.label.starts_with("barrier 1 run ended → passed") && p.seconds() == 10), "{ph:?}");
        let unnamed: i64 = ph.iter().filter(|p| p.gap && p.waiting_for.as_deref().is_some_and(|w| w.starts_with("nothing the logs"))).map(|p| p.seconds()).sum();
        assert_eq!(unnamed, 0);
    }

    #[test]
    fn the_first_barrier_run_waits_from_the_last_verdict_not_the_first_while_another_member_is_still_at_work() {
        let mut i = row();
        i.group = Some(("list".into(), 1, vec!["r-1".into(), "r-2".into()]));
        i.events.push(ev(900, "task-verified", json!({"id": "r-2"})));
        i.cycle.push((1000, "list group 1: barrier run — 1 step(s), 0 reused, 1 ran, 0 failed, 60s".into()));
        i.cycle.push((1010, "tell list: Barrier 1 of list passed".into()));
        let ph = build(&i);
        let wait = ph.iter().find(|p| p.label.starts_with("last verdict → barrier 1 run started")).unwrap();
        assert_eq!((wait.start, wait.end), (900, 940), "r-1 was verified at 380; r-2's legs ran to 900, and that is not the barrier's wait: {ph:?}");
    }

    #[test]
    fn a_barrier_held_by_a_member_question_names_the_holder_and_is_waiting_not_the_run() {
        let mut i = row();
        i.group = Some(("list".into(), 1, vec!["r-1".into()]));
        for t in (400..3000).step_by(60) {
            i.cycle.push((t, "list group 1: barrier waits: cpd-9 (r-9) is waiting on the list governor to answer its question m-abc".into()));
        }
        i.cycle.push((3000, "list group 1: started list · barrier 1".into()));
        i.cycle.push((3250, "list group 1: barrier run — 19 step(s), 0 reused, 19 ran, 0 failed, 223s".into()));
        i.cycle.push((3260, "tell list: Barrier 1 of list passed".into()));
        let ph = build(&i);
        let wait = ph.iter().find(|p| p.label.starts_with("last verdict → barrier 1 run started")).expect("the wait is a phase");
        assert!(wait.waiting_for.as_deref().is_some_and(|w| w.starts_with(crate::wallbuckets::BARRIER_HOLD) && w.contains("(r-9)") && w.contains("m-abc")), "{wait:?}");
        let r1 = crate::wallbuckets::MemberIn {
            id: "r-1".into(),
            spans: ph.iter().map(crate::wallbuckets::Span::of).collect(),
            ..Default::default()
        };
        let g = crate::wallbuckets::group_wall(&[r1], None, Some(3260)).unwrap();
        assert_eq!(g.seconds("barrier_run"), 223 + 10, "the run (and the second to the pass), not the hold: {g:?}");
        assert_eq!(g.barrier_run, 223 + 10, "{g:?}");
        assert!(g.waits.iter().any(|((_, c), s)| c.starts_with(crate::wallbuckets::BARRIER_HOLD) && c.contains("m-abc") && *s >= 2500), "{g:?}");
        assert_eq!(g.by.values().sum::<i64>(), g.close - g.open, "the buckets tile the window");
    }

    /// s0-state-core's barrier 3 (`wsp-090`): its check seat held the barrier
    /// for a person's step at 21:10 on 11-17 and a person started it again at
    /// 09:30 on 11-18; effort.json booked the night as the check seat, 741 min
    /// of it, where the seat's own time was 23. Here the seat works 1260 s,
    /// holds for 718 min, and passes 121 s after the `go`.
    #[test]
    fn a_barrier_held_for_a_persons_step_inside_its_check_seat_is_waiting_on_the_hold_not_the_check_seat() {
        let mut i = row();
        i.group = Some(("list".into(), 3, vec!["r-1".into()]));
        let (seat, held, go) = (1010, 1010 + 1260, 1010 + 1260 + 718 * 60);
        i.cycle.push((1000, "list group 3: barrier run — 1 step(s), 0 reused, 1 ran, 0 failed, 60s".into()));
        i.cycle.push((seat, "spawn list --agent --check 3 --kind claude".into()));
        i.cycle.push((go + 121, "tell list: Barrier 3 of list passed".into()));
        let evs = vec![
            (held, "worklist-held".to_string(), json!({"id": "list", "why": "the migration needs a person to run it on the box"})),
            (held + 5, "worklist-held".to_string(), json!({"id": "other", "why": "not this list"})),
            (go, "worklist-go".to_string(), json!({"id": "list", "passed": null, "started": true})),
        ];
        i.holds = holds_of(&evs, "list", 99_999_999);
        assert_eq!(i.holds, vec![(held, go, "the migration needs a person to run it on the box".to_string())]);
        let ph = build(&i);
        let hold = ph.iter().find(|p| p.label == "barrier 3 held → started again").unwrap_or_else(|| panic!("{ph:?}"));
        assert!(hold.gap && hold.shared && (hold.start, hold.end) == (held, go), "{hold:?}");
        let why = hold.waiting_for.as_deref().unwrap_or_default();
        assert!(why.starts_with(crate::wallbuckets::BARRIER_HOLD) && why.contains(HELD_FOR_STEP) && why.contains("barrier 3 of list") && why.contains("the migration"), "{why}");
        let seats: Vec<i64> = ph.iter().filter(|p| p.shared && !p.gap && p.label.contains("check seat")).map(|p| p.seconds()).collect();
        assert_eq!(seats, vec![1260, 121], "the seat before the hold and after the go: {ph:?}");

        let r1 = crate::wallbuckets::MemberIn { id: "r-1".into(), spans: ph.iter().map(crate::wallbuckets::Span::of).collect(), ..Default::default() };
        let g = crate::wallbuckets::group_wall(&[r1], None, Some(go + 121)).unwrap();
        assert_eq!(g.seconds("check_seat"), 10 + 1260 + 121, "the wait for the seat, its work, and the go to the pass: {g:?}");
        let waited: i64 = g.waits.iter().filter(|((_, c), _)| c.contains(HELD_FOR_STEP) && c.contains("barrier 3 of list")).map(|(_, s)| s).sum();
        assert_eq!(waited, 718 * 60, "the hold is its own waiting cause: {g:?}");
        assert_eq!(g.by.values().sum::<i64>(), g.close - g.open, "the buckets tile the window");
    }

    /// s0-state-core's barrier 3 as it ran (`wsp-090`, sent back): the check
    /// seat opened at 21:08:59, wrote its hold 77 s later, and stayed open until
    /// the sweep stopped it at 21:32:03; a person's `go` at 09:30:02 passed it.
    /// The seat is the check seat to its stop, and the hold runs from that stop.
    fn fixture_shaped(by_reading: bool) -> (Vec<Phase>, crate::wallbuckets::GroupWall, i64, i64) {
        let mut i = row();
        i.group = Some(("s0-state-core".into(), 3, vec!["r-1".into()]));
        let (seat, held, stop) = (1010, 1010 + 77, 1010 + 1384);
        let go = stop + 43_079;
        i.cycle.push((1000, "s0-state-core group 3: barrier run — 1 step(s), 0 reused, 1 ran, 0 failed, 60s".into()));
        i.cycle.push((seat, "spawn s0-state-core --agent --check 3 --kind claude".into()));
        i.cycle.push((stop, "stopped in barrier check tree /c/trees/s0-state-core-check-3: TERM 3794".into()));
        i.cycle.push((go, "tell s0-state-core: Barrier 3 of s0-state-core passed".into()));
        if by_reading {
            i.cycle.retain(|(_, l)| !l.starts_with(STOPPED_CHECK));
            i.check_ends = vec![(2, 10, 900), (3, seat, stop)];
        } else {
            i.cycle.insert(0, (held + 10, "stopped in barrier check tree /c/trees/core-check-3: TERM 1".into()));
        }
        let evs = vec![
            (held, "worklist-held".to_string(), json!({"id": "s0-state-core", "why": "a person runs the migration"})),
            (go, "worklist-go".to_string(), json!({"id": "s0-state-core", "passed": 3, "started": true})),
        ];
        i.holds = holds_of(&evs, "s0-state-core", 99_999_999);
        let ph = build(&i);
        let r1 = crate::wallbuckets::MemberIn { id: "r-1".into(), spans: ph.iter().map(crate::wallbuckets::Span::of).collect(), ..Default::default() };
        let g = crate::wallbuckets::group_wall(&[r1], None, Some(go)).unwrap();
        (ph, g, stop, go)
    }

    #[test]
    fn a_check_seat_open_past_the_hold_it_wrote_is_the_check_seat_to_its_stop_and_the_hold_runs_from_there() {
        for by_reading in [false, true] {
            let (ph, g, stop, go) = fixture_shaped(by_reading);
            let hold = ph.iter().find(|p| p.label == "barrier 3 held → started again").unwrap_or_else(|| panic!("{ph:?}"));
            assert_eq!((hold.start, hold.end), (stop, go), "the hold runs from the seat's stop to the go ({by_reading}): {ph:?}");
            assert!(hold.waiting_for.as_deref().is_some_and(|w| w.contains(&format!("held at {}", &util::iso_at(1087)[11..16])) && w.contains("the check seat stopped at")), "{hold:?}");
            assert_eq!(g.seconds("check_seat"), 10 + 1384, "the wait for the seat and the seat open, 21:08:59 → 21:32:03 ({by_reading}): {g:?}");
            let waited: i64 = g.waits.iter().filter(|((_, c), _)| c.contains(HELD_FOR_STEP) && c.contains("barrier 3 of s0-state-core") && c.contains("the migration")).map(|(_, s)| s).sum();
            assert_eq!(waited, 43_079, "about 718 min, its own cause ({by_reading}): {g:?}");
            assert_eq!(g.by.values().sum::<i64>(), g.close - g.open, "the buckets tile the window");
        }
    }

    /// omarchy-env's hold, cut at 200 characters, read "… is not running it
    /// yet. T" in effort.json (`wsp-856`).
    #[test]
    fn a_long_hold_reason_is_cut_between_words_and_ends_in_an_ellipsis() {
        let why = "cause: env — the installed wsp, daemon and manager are 0c38be7, older than all three members, so the barrier ran the code they replace. Hold. The code is right but the machine is not running it yet. The barrier at 8b0aa22 refused";
        let mut i = row();
        i.group = Some(("list".into(), 3, vec!["r-1".into()]));
        i.cycle.push((1000, "list group 3: barrier run — 1 step(s), 0 reused, 1 ran, 0 failed, 60s".into()));
        i.cycle.push((1010, "spawn list --agent --check 3 --kind claude".into()));
        i.cycle.push((9000, "tell list: Barrier 3 of list passed".into()));
        i.holds = vec![(2000, 8000, why.into())];
        let ph = build(&i);
        let cause = ph.iter().find_map(|p| p.waiting_for.as_deref().filter(|w| w.contains(HELD_FOR_STEP))).unwrap_or_else(|| panic!("{ph:?}"));
        assert!(cause.ends_with("is not running it yet.…"), "{cause}");

        assert_eq!(cut_at_word("  a short reason ", 20), "a short reason");
        assert_eq!(cut_at_word("exactly ten", 11), "exactly ten", "at the cap is whole");
        assert_eq!(cut_at_word("one two three", 7), "one two…", "the cap fell between two words");
        assert_eq!(cut_at_word("one two three", 6), "one…");
        assert_eq!(cut_at_word("— é ünbroken", 6), "— é…");
        assert_eq!(cut_at_word("unbrokenword", 4), "…");
    }

    #[test]
    fn a_hold_with_no_go_after_it_runs_to_now_and_a_second_hold_while_held_is_the_same() {
        let evs = vec![
            (10, "worklist-held".to_string(), json!({"id": "l", "why": "a"})),
            (20, "worklist-held".to_string(), json!({"id": "l", "why": "b"})),
            (30, "worklist-resumed".to_string(), json!({"id": "l"})),
            (40, "worklist-go".to_string(), json!({"id": "l"})),
            (50, "worklist-held".to_string(), json!({"id": "l", "why": "c"})),
        ];
        assert_eq!(holds_of(&evs, "l", 90), vec![(10, 30, "a".to_string()), (50, 90, "c".to_string())]);
    }

    #[test]
    fn a_failed_barrier_run_before_a_follow_up_members_verdict_is_still_the_barriers_wall() {
        let mut i = row();
        i.group = Some(("list".into(), 1, vec!["r-1".into(), "r-2".into()]));
        i.events.push(ev(1500, "task-verified", json!({"id": "r-2"})));
        i.cycle.push((1000, "list group 1: barrier run — 2 step(s), 0 reused, 1 ran, 1 failed, 600s · per step: test 600s".into()));
        i.cycle.push((1600, "list group 1: barrier run — 2 step(s), 2 reused, 0 ran, 0 failed, 15s".into()));
        i.cycle.push((1610, "tell list: Barrier 1 of list passed".into()));
        let ph = build(&i);
        let runs: Vec<(i64, i64)> = ph.iter().filter(|p| p.shared && p.label.starts_with("barrier 1 run (")).map(|p| (p.start, p.end)).collect();
        assert_eq!(runs, vec![(400, 1000), (1585, 1600)], "{ph:?}");
        assert!(!ph.iter().any(|p| p.shared && p.start >= 1000 && p.start < 1500), "the stretch to the last verdict is in no barrier phase: {ph:?}");
    }

    /// Signing's group 1 (`cycle.log`, 2026-11-08), line for line: the first
    /// barrier run failed after 1242s (the run line at 19:53:12), its check seat
    /// was spawned at 19:53:13 and settled the barrier at 19:55:13 (follow-up
    /// wsp-531 added, started 19:55:06), wsp-531's verdict came at 20:23:00, the
    /// second run was 9s (20:23:15), its check seat passed at 20:25:20. The
    /// offsets are the log's, the first verdict (19:32:00) at 380 here.
    #[test]
    fn signings_group_one_reads_the_failed_run_and_its_check_seat_and_the_run_again_as_the_barrier() {
        let mut i = row();
        i.group = Some(("list".into(), 1, vec!["r-1".into(), "r-2".into()]));
        let at = |h: i64, m: i64, sec: i64| 380 + (h * 3600 + m * 60 + sec) - (19 * 3600 + 32 * 60);
        i.events.push(ev(at(20, 23, 0), "task-verified", json!({"id": "r-2"})));
        i.cycle.push((at(19, 53, 12), "list group 1: barrier run — 1 step(s), 0 reused, 1 ran, 1 failed, 1242s · per step: full 1241s · scope: full".into()));
        i.cycle.push((at(19, 53, 13), "spawn list --agent --check 1 --kind claude --model sonnet --effort high".into()));
        i.cycle.push((at(19, 54, 31), "tell list: Group 1's barrier in the list run holds, pending follow-ups: r-2. cpd-3834 added them to the group".into()));
        i.cycle.push((at(19, 55, 13), "list: the check in cpd-3834 settled its barrier and its turn is over — ending it".into()));
        i.cycle.push((at(20, 23, 15), "list group 1: barrier run — 2 step(s), 1 reused, 1 ran, 0 failed, 9s · per step: help 8s · scope: full".into()));
        i.cycle.push((at(20, 23, 15), "spawn list --agent --check 1 --kind claude --model sonnet --effort high".into()));
        i.cycle.push((at(20, 25, 20), "tell list: Barrier 1 of list passed".into()));
        let ph = build(&i);
        let runs: Vec<i64> = ph.iter().filter(|p| p.shared && p.label.starts_with("barrier 1 run (")).map(|p| p.seconds()).collect();
        assert_eq!(runs, vec![1242, 9], "{ph:?}");
        let seats: Vec<i64> = ph.iter().filter(|p| p.shared && !p.gap && p.label.contains("check seat")).map(|p| p.seconds()).collect();
        assert_eq!(seats, vec![120, 125], "the failed run's check seat as well as the last: {ph:?}");
        // wsp-531 starts at 19:55:06 and works to its verdict.
        let (s2, v2) = (at(19, 55, 6), at(20, 23, 0));
        let r2 = crate::wallbuckets::MemberIn {
            id: "r-2".into(),
            spans: vec![crate::wallbuckets::Span { start: s2, end: s2 + 10, class: crate::wallbuckets::Class::Start }, crate::wallbuckets::Span { start: s2 + 10, end: v2, class: crate::wallbuckets::Class::Seat }],
            ..Default::default()
        };
        let r1 = crate::wallbuckets::MemberIn { id: "r-1".into(), spans: ph.iter().map(crate::wallbuckets::Span::of).collect(), ..Default::default() };
        let g = crate::wallbuckets::group_wall(&[r1, r2], None, Some(at(20, 25, 20))).unwrap();
        assert_eq!(g.seconds("barrier_run"), 30 + 1242 + 6 + 9, "wait to the run, the failed run, the wait to the run again, the run again: {g:?}");
        assert_eq!(g.seconds("check_seat"), 1 + 120 + 125, "the failed run's seat (and the second before it was up), the last run's seat: {g:?}");
        assert_eq!(g.seconds("serialised"), 0, "wsp-531 had started inside the failed run's check seat; nothing was held: {g:?}");
        assert!(g.seconds("working") >= v2 - (s2 + 120) - 10, "the follow-up's stretch is the members': {g:?}");
        assert_eq!(g.by.values().sum::<i64>(), g.close - g.open, "the buckets sum to the wall");
    }

    #[test]
    fn a_verifier_spawn_waiting_at_the_cap_is_the_cap_and_a_garbled_copied_run_line_reads_as_the_longest_run() {
        let mut i = row();
        // The cap line names the waiting step ("r-1 · verifying") before ` waits`.
        i.events.push(ev(405, "task-checked", json!({"id": "r-1", "took": 95})));
        i.cycle.retain(|(_, l)| !l.starts_with("spawn r-1 --agent --verify"));
        i.cycle.push((450, "list group 1: r-1 · verifying waits — the seat is at its cap of 4: x in cpd-1".into()));
        i.cycle.push((600, "spawn r-1 --agent --verify --kind claude".into()));
        i.cycle.push((605, "list group 1: started r-1 · verifying".into()));
        i.events.retain(|(_, k, _)| k != "task-verified");
        i.events.push(ev(700, "task-verified", json!({"id": "r-1"})));
        i.group = Some(("list".into(), 1, vec!["r-1".into()]));
        // Two copies wrote one line; the longer began first.
        i.cycle.push((1000, "list group 1: barrier run — 1 step(s), 1 reused, 0 ran, 0 failed, 35slist group 1: barrier run — 1 step(s), 0 reused, 1 ran, 0 failed, 300s".into()));
        let ph = build(&i);
        let w = ph.iter().find(|p| p.label.starts_with("row's check ended")).unwrap();
        assert!(w.waiting_for.as_deref().unwrap().contains("cap"), "{w:?}");
        let run = ph.iter().find(|p| p.label.starts_with("barrier 1 run (")).unwrap();
        assert_eq!((run.start, run.end), (700, 1000));
        assert!(run.label.contains("1 ran, 0 failed, 300s)"), "{}", run.label);
    }

    #[test]
    fn an_id_is_matched_whole() {
        assert!(names("starting wsp-366 on x", "wsp-366"));
        assert!(!names("starting wsp-3660 on x", "wsp-366"));
        assert!(!names("starting xwsp-366", "wsp-366"));
    }

    fn line(t: i64, v: Value) -> String {
        let mut v = v;
        v["timestamp"] = json!(util::iso_at(t));
        v.to_string()
    }
    fn asst(t: i64, rid: &str, usage: (u64, u64, u64), tool: Option<(&str, &str)>) -> String {
        let content = match tool {
            Some((id, name)) => json!([{"type": "tool_use", "id": id, "name": name}]),
            None => json!([{"type": "text", "text": "x"}]),
        };
        line(t, json!({"type": "assistant", "requestId": rid, "message": {"content": content,
            "usage": {"input_tokens": usage.0, "cache_creation_input_tokens": 0, "cache_read_input_tokens": usage.1, "output_tokens": usage.2}}}))
    }
    fn bash(t: i64, rid: &str, id: &str, cmd: &str) -> String {
        line(t, json!({"type": "assistant", "requestId": rid, "message": {"content": [{"type": "tool_use", "id": id, "name": "Bash", "input": {"command": cmd}}],
            "usage": {"input_tokens": 1, "output_tokens": 1}}}))
    }

    #[test]
    fn the_governors_window_counts_its_turns_idle_and_the_ones_that_only_looked() {
        let text = [
            bash(1000, "r0", "t0", "wsp say x"),
            bash(1010, "r1", "t1", "wsp watch --once"),
            result(1012, "t1"),
            bash(1030, "r2", "t2", "wsp worklist go y"),
            result(1040, "t2"),
            bash(1200, "r3", "t3", "wsp say z"),
        ]
        .join("\n");
        let w = governor_window_of(&[(text, vec![])], 1005, 1100).unwrap();
        assert_eq!((w.seconds, w.turns, w.looking_turns), (95, 2, 1), "{w:?}");
        assert_eq!(w.tool_secs, 2 + 10);
        assert_eq!(w.looking_secs, 10);
        assert_eq!((w.model_secs, w.idle_secs), (28, 55), "{w:?}");
        assert!(governor_window_of(&[], 1005, 1100).is_none());
    }

    fn result(t: i64, id: &str) -> String {
        line(t, json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": id}]}}))
    }

    fn two_legs() -> Inputs {
        let id = "r-1";
        Inputs {
            id: id.into(),
            events: vec![
                ev(110, "task-claimed", json!({"id": id})),
                ev(112, "session-learned", json!({"id": id, "session": "s"})),
                ev(200, "session-learned", json!({"id": id, "session": "l1"})),
                ev(300, "leg-done", json!({"id": id, "leg": "a"})),
                ev(410, "session-learned", json!({"id": id, "session": "l2"})),
                ev(450, "leg-done", json!({"id": id, "leg": "b"})),
                ev(460, "task-review", json!({"id": id})),
                ev(470, "session-learned", json!({"id": id, "session": "v"})),
            ],
            cycle: vec![
                (100, "starting r-1 on claude (new)".into()),
                (190, "list group 1: starting leg a of r-1".into()),
                (400, "list group 1: starting leg b of r-1".into()),
            ],
            legs: vec![LegIn { name: "a".into(), ..Default::default() }, LegIn { name: "b".into(), ..Default::default() }],
            ..Default::default()
        }
    }

    #[test]
    fn the_seats_tokens_are_summed_once_per_request_and_split_at_the_legs() {
        let i = two_legs();
        let ph = build(&i);
        let text = [
            line(118, json!({"type": "user", "message": {"content": "go"}})),
            asst(120, "q1", (10, 100, 5), None),
            asst(121, "q1", (10, 100, 5), Some(("t1", "Bash"))),
            result(126, "t1"),
            asst(133, "q2", (20, 200, 7), Some(("t2", "Read"))),
            result(134, "t2"),
            asst(250, "q4", (99, 99, 99), None),
            line(305, json!({"type": "user", "message": {"content": "next"}})),
            asst(310, "q3", (1, 2, 3), Some(("t3", "Edit"))),
            result(315, "t3"),
            asst(455, "q5", (4, 0, 8), Some(("t5", "Write"))),
            result(459, "t5"),
        ]
        .join("\n");
        let r = seat_report(&i, &ph, &[], &|s| (s == "s").then(|| text.clone()));
        assert_eq!(r.sessions, vec!["s".to_string()], "a leg's and the verifier's sessions are not the seat's");
        assert!(r.missing.is_empty());
        let labels: Vec<&str> = r.phases.iter().map(|p| p.label.as_str()).collect();
        assert_eq!(labels[..3], ["before leg a", "between leg a and leg b", "after leg b → review typed"]);
        assert!(labels[3].starts_with("while a leg ran"), "the seat's call inside a leg is kept: {labels:?}");
        assert_eq!((r.phases[3].calls, r.phases[3].tokens_out), (1, 99));
        let p = &r.phases[0];
        assert_eq!((p.calls, p.model_secs, p.tokens_in, p.cache_read, p.tokens_out), (2, 3 + 7, 30, 300, 12));
        assert_eq!((p.tools["Bash"], p.tools["Read"]), (5, 1));
        let p = &r.phases[1];
        assert_eq!((p.calls, p.model_secs, p.tokens_in, p.cache_read, p.tokens_out), (1, 5, 1, 2, 3));
        assert_eq!(p.tools["Edit"], 5);
        let p = &r.phases[2];
        assert_eq!((p.calls, p.tokens_in, p.tokens_out, p.tools["Write"]), (1, 4, 8, 4));
    }

    #[test]
    fn the_row_figures_the_effort_report_reads_are_what_timeline_json_prints() {
        let i = two_legs();
        let ph = build(&i);
        let text = [
            line(118, json!({"type": "user", "message": {"content": "go"}})),
            asst(120, "q1", (10, 100, 5), None),
            asst(121, "q1", (10, 100, 5), Some(("t1", "Bash"))),
            result(126, "t1"),
            asst(133, "q2", (20, 200, 7), Some(("t2", "Read"))),
            result(134, "t2"),
            line(305, json!({"type": "user", "message": {"content": "next"}})),
            asst(310, "q3", (1, 2, 3), Some(("t3", "Edit"))),
            result(315, "t3"),
            asst(455, "q5", (4, 0, 8), Some(("t5", "Write"))),
            result(459, "t5"),
        ]
        .join("\n");
        let seat = seat_report(&i, &ph, &[], &|s| (s == "s").then(|| text.clone()));
        let f = figures_of(&ph, &seat);
        let tl = to_json(&i, &ph);
        let sj = seat_json(&seat);
        assert!(f.gap > 0, "the row has a wait in it: {ph:?}");
        assert_eq!(f.gap, tl["gap_seconds"].as_i64().unwrap(), "gap seconds are the timeline's");
        assert_eq!(f.waits.values().sum::<i64>(), f.gap, "every wait is under a cause");
        let first = tl["phases"].as_array().unwrap().iter().map(|p| p["start"].as_str().unwrap().to_string()).min().unwrap();
        assert_eq!(util::iso_at(f.start), first);
        let sum = |k: &str| sj["phases"].as_array().unwrap().iter().map(|p| p[k].as_u64().unwrap()).sum::<u64>();
        assert_eq!(f.model_secs as u64, sum("model_seconds"));
        assert_eq!(f.calls, sum("calls"));
        assert_eq!((f.tokens_in, f.cache_read, f.tokens_out), (sum("tokens_in"), sum("tokens_cache_read"), sum("tokens_out")));
        for k in TOOL_KINDS.iter().chain(["other"].iter()) {
            let want: i64 = sj["phases"].as_array().unwrap().iter().map(|p| p["tool_seconds"][*k].as_i64().unwrap()).sum();
            assert_eq!(f.tools.get(*k).copied().unwrap_or(0), want, "tool kind {k}");
        }
        assert!(f.model_secs > 0 && f.tools.values().sum::<i64>() > 0, "the transcript was read: {f:?}");
    }

    /// `wsp-313`: a store with no `session-learned` read every seat as reading
    /// nothing. A transcript found for the seat (the ledger's, the row's tree's)
    /// is read when it began inside the seat's span; a verifier's in the same
    /// tree, begun after the review, is not.
    #[test]
    fn a_seat_with_no_session_learned_is_read_from_a_transcript_found_for_it() {
        let id = "r-1";
        let mut i = Inputs {
            id: id.into(),
            events: vec![ev(110, "task-claimed", json!({"id": id, "pane": "cpd-1"})), ev(460, "task-review", json!({"id": id}))],
            cycle: vec![(100, "starting r-1 on claude (new)".into())],
            ..Default::default()
        };
        let ph = build(&i);
        let text = [
            line(105, json!({"type": "user", "message": {"content": "go"}})),
            asst(130, "q1", (10, 100, 13060), Some(("t1", "Bash"))),
            result(190, "t1"),
            asst(400, "q2", (1, 2, 3), None),
        ]
        .join("\n");
        let load = |s: &str| (s == "/p/seat.jsonl" || s == "/p/verifier.jsonl").then(|| text.clone());
        let none = seat_report(&i, &ph, &[], &load);
        assert!(none.sessions.is_empty(), "nothing found, nothing read: {none:?}");
        i.found = vec![(105, "/p/seat.jsonl".into()), (470, "/p/verifier.jsonl".into())];
        let r = seat_report(&i, &ph, &[], &load);
        assert_eq!(r.sessions, vec!["/p/seat.jsonl".to_string()], "the verifier began after the review");
        let f = figures_of(&ph, &r);
        assert!(f.measured && f.tokens_out == 13063, "{f:?}");
        assert!(f.model_secs > 0, "a seat with output tokens and a transcript has model time: {f:?}");
        assert_eq!((f.model_secs, f.tools.get("Bash").copied()), (25 + 210, Some(60)));
        assert_eq!(f.untimed, 0);
    }

    #[test]
    fn a_transcript_whose_calls_carry_no_time_is_named_untimed() {
        let i = two_legs();
        let ph = build(&i);
        let text = [
            json!({"type": "assistant", "requestId": "q1", "message": {"content": [], "usage": {"input_tokens": 1, "output_tokens": 500}}}).to_string(),
            json!({"type": "assistant", "requestId": "q2", "message": {"content": [], "usage": {"input_tokens": 1, "output_tokens": 700}}}).to_string(),
        ]
        .join("\n");
        let r = seat_report(&i, &ph, &[], &|s| (s == "s").then(|| text.clone()));
        assert_eq!(r.untimed, vec!["s".to_string()]);
        let f = figures_of(&ph, &r);
        assert_eq!((f.untimed, f.model_secs, f.calls), (1, 0, 0));
        assert!(render_seat(&r).contains("transcript of session s has no timestamps"), "{}", render_seat(&r));
        assert_eq!(seat_json(&r)["untimed"], json!(["s"]));
    }

    #[test]
    fn a_call_runs_from_its_api_request_line_and_not_from_an_attachment_stamped_at_the_reply() {
        // wsp-154's transcript: the reply's attachment line, or a
        // file-history-delta stamped later still, came just before the
        // assistant line and the call read about 0 s.
        let line = |kind: &str, at: &str, extra: Value| {
            let mut v = json!({"type": kind, "timestamp": at});
            v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            v.to_string()
        };
        let reply = |req: &str| json!({"requestId": req, "message": {"content": [], "usage": {"input_tokens": 1, "output_tokens": 10}}});
        let text = [
            line("user", "2026-11-19T22:02:50.546Z", json!({"message": {"content": "go"}})),
            line("attachment", "2026-11-19T22:02:50.550Z", json!({})),
            line("api-request", "2026-11-19T22:02:50.565Z", json!({"querySource": "repl_main_thread"})),
            line("attachment", "2026-11-19T22:06:41.034Z", json!({})),
            line("assistant", "2026-11-19T22:06:41.033Z", reply("r1")),
            line("user", "2026-11-19T22:06:42.483Z", json!({"message": {"content": "next"}})),
            line("api-request", "2026-11-19T22:06:42.528Z", json!({"querySource": "repl_main_thread"})),
            line("file-history-delta", "2026-11-19T22:09:09.178Z", json!({})),
            line("assistant", "2026-11-19T22:06:49.741Z", reply("r2")),
            line("assistant", "2026-11-19T22:09:09.143Z", reply("r2")),
        ]
        .join("\n");
        let (calls, _) = read_transcript(&text);
        let secs: Vec<i64> = calls.iter().map(|c| c.end - c.start).collect();
        assert_eq!(secs, vec![231, 147]);
        // Without api-request lines a call still runs from the line before it.
        let old: String = text.lines().filter(|l| !l.contains("\"api-request\"")).collect::<Vec<_>>().join("\n");
        let (calls, _) = read_transcript(&old);
        assert_eq!(calls.iter().map(|c| c.end - c.start).collect::<Vec<_>>(), vec![0, 0]);
    }

    #[test]
    fn a_missing_transcript_is_named_not_zero() {
        let i = two_legs();
        let ph = build(&i);
        let r = seat_report(&i, &ph, &[], &|_| None);
        assert_eq!(r.missing, vec!["s".to_string()]);
        let out = render_seat(&r);
        assert_eq!(out.lines().count(), 1);
        assert!(out.contains("transcript not found for session s"), "{out}");
        let j = seat_json(&r);
        assert!(j["phases"].as_array().unwrap().is_empty());
        assert!(j["note"].as_str().unwrap().contains("not found"));
    }

    #[test]
    fn a_row_with_no_legs_has_one_seat_phase() {
        let i = row();
        let ph = build(&i);
        let r = seat_report(&i, &ph, &[], &|_| Some(String::new()));
        let legless = Inputs { legs: vec![], ..row() };
        let r2 = seat_report(&legless, &build(&legless), &[], &|_| Some(String::new()));
        assert_eq!(r.phases.len(), 2);
        assert_eq!(r2.phases.len(), 1);
    }

    #[test]
    fn the_last_phase_ends_at_review_typed_not_at_the_verifier_or_the_barrier() {
        let mut i = two_legs();
        i.events.push(ev(520, "task-verified", json!({"id": "r-1"})));
        i.cycle.push((461, "spawn r-1 --agent --verify --kind claude".into()));
        i.cycle.push((470, "list group 1: started r-1 · verifying".into()));
        i.cycle.push((540, "list group 1: barrier-run 1".into()));
        i.cycle.push((600, "tell list: Barrier 1 of list passed".into()));
        i.group = Some(("list".into(), 1, vec!["r-1".into()]));
        let ph = build(&i);
        assert!(ph.iter().any(|p| p.end >= 600), "the timeline runs to the barrier: {ph:?}");
        let text = [
            line(118, json!({"type": "user", "message": {"content": "go"}})),
            asst(455, "q5", (4, 0, 8), None),
            asst(480, "q6", (50, 50, 50), None),
        ]
        .join("\n");
        let r = seat_report(&i, &ph, &[], &|s| (s == "s").then(|| text.clone()));
        let last = r.phases.last().unwrap();
        assert_eq!((last.start, last.end), (450, 460), "{last:?}");
        assert_eq!((last.calls, last.tokens_out), (1, 8), "a call after review typed is not the seat's work");
    }

    #[test]
    fn legs_run_leg_timed_split_the_seat_and_another_rows_legs_are_left_out() {
        let dir = std::env::temp_dir().join(format!("wsp-371-legs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let put = |name: &str, a: f64, b: f64| {
            std::fs::create_dir_all(dir.join(name)).unwrap();
            let v = json!({"component": "board", "leg": name, "events": [{"ev": "start", "at": a}, {"ev": "end", "at": b}]});
            std::fs::write(dir.join(name).join("timeline.json"), v.to_string()).unwrap();
        };
        put("a", 150.4, 280.2);
        put("b", 300.0, 400.0);
        put("other", 9000.0, 9100.0);
        let legs = run_legs(&[dir.clone()], 100, 460);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(legs, vec![("board-a".to_string(), 150, 281), ("board-b".to_string(), 300, 400)]);
        let mut i = two_legs();
        i.cycle.clear();
        i.events.retain(|(_, k, _)| k != "leg-done");
        let text = [
            line(118, json!({"type": "user", "message": {"content": "go"}})),
            asst(149, "q1", (1, 1, 1), Some(("t1", "Bash"))),
            result(281, "t1"),
            asst(283, "q2", (2, 2, 2), None),
            asst(420, "q3", (3, 3, 3), None),
        ]
        .join("\n");
        let ph = build(&i);
        let r = seat_report(&i, &ph, &legs, &|s| (s == "s").then(|| text.clone()));
        assert!(r.source.contains("run-leg.sh"), "{}", r.source);
        let labels: Vec<&str> = r.phases.iter().map(|p| p.label.as_str()).collect();
        assert_eq!(labels, ["before leg board-a", "between leg board-a and leg board-b", "after leg board-b → review typed"]);
        assert_eq!((r.phases[0].start, r.phases[0].end), (110, 150));
        assert_eq!(r.phases[0].tools["Bash"], 1, "only the second before the leg; the wait inside it is the leg's");
        assert_eq!(r.phases[1].tools.get("Bash"), None, "the leg's wait is in no phase");
        assert_eq!(r.phases[1].calls, 1);
        assert_eq!((r.phases[2].start, r.phases[2].end, r.phases[2].calls), (400, 460, 1));
    }

    /// react-chat (`compounding-464`): a person parked the whole list, every seat
    /// was released, and the rows were started again at the resume. The stretch
    /// between is the list's park, by whom and why, not "waiting to be restarted".
    #[test]
    fn a_stretch_inside_a_list_park_is_named_for_the_park_and_not_a_wait_to_be_restarted() {
        let mut i = two_seatings(true);
        i.events.retain(|(_, k, _)| k != "task-after" && k != "task-parked");
        i.group = Some(("l".into(), 1, vec!["r-1".into()]));
        let evs = vec![
            (140, "worklist-parked".to_string(), json!({"id": "l", "why": "stop everything, he needs the machine", "by": "person"})),
            (1980, "worklist-resumed".to_string(), json!({"id": "l", "at": 1, "said": "go", "by": "person"})),
        ];
        i.parks = parks_of(&evs, "l", 9999);
        let ph = build(&i);
        let gap = ph.iter().find(|p| p.gap && p.label.starts_with("parked at")).expect("named");
        let why = gap.waiting_for.as_deref().unwrap();
        assert!(why.starts_with(PARKED_CAUSE) && why.contains("parked by person") && why.contains("he needs the machine"), "{why}");
        assert!(!why.contains("waiting to be restarted"), "{why}");
        assert_eq!(figures_of(&ph, &SeatReport::default()).between_attempts, 1840);
    }

    /// wsp-629 (`wsp-645`): the list resumed at 12:58, and the row's checks timed
    /// out under load, and were rerun, until it was started again at 13:38. The
    /// park ends at the resume; the checks are work, not the park.
    #[test]
    fn check_timeouts_after_a_resume_are_booked_to_the_checks_and_the_park_ends_at_the_resume() {
        let mut i = two_seatings(true);
        i.events.retain(|(_, k, _)| k != "task-after" && k != "task-parked");
        i.group = Some(("l".into(), 1, vec!["r-1".into()]));
        let evs = vec![
            (140, "worklist-parked".to_string(), json!({"id": "l", "why": "he needs the machine", "by": "person"})),
            (1100, "worklist-resumed".to_string(), json!({"id": "l", "at": 1, "said": "go", "by": "person"})),
        ];
        i.parks = parks_of(&evs, "l", 9999);
        for n in 0..5 {
            let state = if n == 3 { "failed" } else { "waiting" };
            i.events.push(ev(1200 + n * 100, "task-checked", json!({"id": "r-1", "state": state, "took": 23, "start": {"load": 40.0 - n as f64}, "load_end": 9.0})));
        }
        let ph = build(&i);
        let park = ph.iter().find(|p| p.gap && p.label.starts_with("parked at")).expect("the park");
        assert_eq!((park.start, park.end), (150, 1100));
        let checks: Vec<&Phase> = ph.iter().filter(|p| !p.gap && p.label.starts_with("the row's own check")).collect();
        assert_eq!(checks.len(), 5, "{ph:?}");
        assert!(checks.iter().all(|p| p.seconds() == 23));
        assert!(checks.iter().any(|p| p.label.starts_with("the row's own check failed")), "the failed one is a check too");
        // Nothing after the first check is the park or a release: the minutes
        // between the checks, and from the last one to the restart, are the
        // checks waiting to rerun.
        let first = 1200 - 23;
        let after: Vec<&Phase> = ph.iter().filter(|p| p.gap && p.start >= first).collect();
        assert!(!after.is_empty());
        assert!(after.iter().all(|p| p.waiting_for.as_deref().unwrap().starts_with(CHECK_WAIT_CAUSE)), "{after:?}");
        let waited: i64 = after.iter().map(|p| p.seconds()).sum();
        assert_eq!(waited, 1990 - first - 5 * 23);
        let why = after[0].waiting_for.as_deref().unwrap();
        assert!(why.contains("5 checks") && why.contains("load 40"), "{why}");
        let booked: i64 = ph.iter().filter(|p| p.gap && p.waiting_for.as_deref().is_some_and(|w| w.starts_with(PARKED_CAUSE))).map(|p| p.seconds()).sum();
        // The park, then the resume to the first check's start.
        assert_eq!(booked, first - 150, "{ph:?}");
        let f = figures_of(&ph, &SeatReport::default());
        assert_eq!(f.between_attempts, booked);
    }

    /// A sent-back row's stretch holds the verifier's failed check (`wsp-645`):
    /// the wait after it is the rework wait, still booked to `between_attempts`.
    /// A failure is booked as a check wait only in a parked stretch, where it is
    /// the row's own check the load made it rerun; after a review it is the seat
    /// fixing the work.
    #[test]
    fn a_failed_check_in_a_sent_back_stretch_stays_under_rework() {
        let mut i = two_seatings(false);
        i.events.push(ev(600, "task-checked", json!({"id": "r-1", "state": "failed", "took": 23, "start": {"load": 4.0}, "load_end": 4.0})));
        i.events.push(ev(900, "task-checked", json!({"id": "r-1", "state": "waiting", "took": 23, "start": {"load": 40.0}, "load_end": 9.0})));
        let ph = build(&i);
        let gaps: Vec<&Phase> = ph.iter().filter(|p| p.gap && p.start >= 200 && p.end <= 1990).collect();
        assert_eq!(gaps.len(), 1, "{ph:?}");
        assert!(gaps[0].waiting_for.as_deref().unwrap().starts_with(REWORK_CAUSE));
        assert_eq!((gaps[0].start, gaps[0].end), (200, 1990));
        assert!(!ph.iter().any(|p| p.label.starts_with("the row's own check")), "{ph:?}");
        let f = figures_of(&ph, &SeatReport::default());
        assert_eq!(f.between_attempts, 1790);
    }

    /// wsp-303's records (`wsp-311`): the seat reviewed at 22:57, a decision
    /// in force "hold until the person detaches" (d5, with d1 and d2 before it
    /// saying hold), the person's step done at 06:04 and the next seat claimed
    /// then. The 25652 s between are the person's step, named with d5, and none
    /// of it rework; it is still the stretch between attempts, and the wall's
    /// waiting_by books it under the person's step and sums to the wall.
    fn wsp_303(decisions: bool) -> Inputs {
        let e = |s: &str| util::epoch_of(s);
        let id = "wsp-303";
        let events = vec![
            ev(e("2026-11-19T22:44:12Z"), "task-claimed", json!({"id": id, "pane": "cpd-1"})),
            ev(e("2026-11-19T22:57:22Z"), "task-review", json!({"id": id})),
            ev(e("2026-11-19T23:00:30Z"), "task-released", json!({"id": id})),
            ev(e("2026-11-20T06:04:54Z"), "task-claimed", json!({"id": id, "pane": "cpd-2"})),
            ev(e("2026-11-20T06:06:46Z"), "task-review", json!({"id": id})),
            ev(e("2026-11-20T06:07:35Z"), "task-verified", json!({"id": id, "verdict": "holds"})),
        ];
        let mut i = Inputs { id: id.into(), events, ..Default::default() };
        if decisions {
            i.decisions = vec![
                (e("2026-11-19T22:46:10Z"), "d1".into(), "answering m-1: wsp mv denied: The person does it. Then hold, don't land.".into()),
                (e("2026-11-19T22:55:39Z"), "d2".into(), "answering m-2: I've asked the person to run the detach. Hold until then.".into()),
                (e("2026-11-19T22:55:56Z"), "d3".into(), "Lead, overnight on Jesper's authority: leave the 86 rows parented for now, then submit wsp-303 for review.".into()),
                (e("2026-11-19T22:59:17Z"), "d5".into(), "Superseded. The lead's later answer is: hold until the person detaches, and don't change the checks.".into()),
                (e("2026-11-20T06:04:54Z"), "d6".into(), "Detached: the person ran wsp mv --parent none for the 86 ids in ids.txt.".into()),
            ];
        }
        i
    }

    #[test]
    fn a_row_held_for_a_persons_step_after_its_review_is_the_persons_step_not_rework() {
        let i = wsp_303(true);
        let ph = build(&i);
        let (rv, next) = (util::epoch_of("2026-11-19T22:57:22Z"), util::epoch_of("2026-11-20T06:04:54Z"));
        assert!(!ph.iter().any(|p| p.waiting_for.as_deref().is_some_and(|w| w.starts_with(REWORK_CAUSE))), "{ph:?}");
        let step: Vec<&Phase> = ph.iter().filter(|p| p.gap && p.waiting_for.as_deref().is_some_and(|w| w.starts_with(PERSON_STEP_CAUSE))).collect();
        assert_eq!(step.len(), 1, "{ph:?}");
        assert_eq!((step[0].start, step[0].end, step[0].seconds()), (rv, next, 25652));
        let why = step[0].waiting_for.as_deref().unwrap();
        assert!(why.contains("decision d5 at 22:59") && why.contains("hold until the person detaches") && why.contains("claimed at 06:04"), "{why}");
        let f = figures_of(&ph, &SeatReport::default());
        assert_eq!(f.between_attempts, 25652);
        let member = crate::wallbuckets::MemberIn { id: i.id.clone(), spans: f.spans.clone(), measured: false, ..Default::default() };
        let w = crate::wallbuckets::group_wall(&[member], None, None).unwrap();
        let wall = w.close - w.open;
        assert_eq!(crate::wallbuckets::BUCKETS.iter().map(|b| w.seconds(b)).sum::<i64>(), wall);
        assert_eq!(w.waits.values().sum::<i64>(), w.seconds("waiting"));
        let person: i64 = w.waits.iter().filter(|((_, c), _)| c.contains(PERSON_STEP_CAUSE)).map(|(_, s)| *s).sum();
        assert_eq!(person, 25652, "{:?}", w.waits);
        assert!(!w.waits.keys().any(|(_, c)| c.contains(REWORK_CAUSE)), "{:?}", w.waits);
    }

    /// The same row with no decision owing the person anything is still rework:
    /// a verdict sent it back and a seat was asked to change something.
    #[test]
    fn a_row_reopened_after_a_verdict_with_no_person_owed_is_still_rework() {
        let ph = build(&wsp_303(false));
        assert!(!ph.iter().any(|p| p.waiting_for.as_deref().is_some_and(|w| w.starts_with(PERSON_STEP_CAUSE))), "{ph:?}");
        let gap = ph.iter().find(|p| p.gap && p.waiting_for.as_deref().is_some_and(|w| w.starts_with(REWORK_CAUSE))).expect("rework named");
        assert_eq!(gap.seconds(), 25652);
        // A decision that names the person but holds nothing does not make it one.
        let mut i = two_seatings(false);
        i.decisions = vec![(150, "d1".into(), "the person may detach them later; until it's run, wsp done needs --force".into())];
        assert!(build(&i).iter().any(|p| p.waiting_for.as_deref().is_some_and(|w| w.starts_with(REWORK_CAUSE))));
        assert!(!holds_for_person("Detached: the person ran wsp mv --parent none"));
    }

    /// A question the seat put to the person, unanswered at the review, is the
    /// person's step from the review to its answer; what follows the answer is
    /// the send-back's own wait again.
    #[test]
    fn a_question_to_the_person_unanswered_at_the_review_is_the_persons_step_until_its_answer() {
        let mut i = two_seatings(false);
        i.events.push(ev(180, "message-raised", json!({"id": "m-p", "about": {"task": "r-1"}, "for": "person", "from": {"pane": "p1"}, "shape": "question"})));
        i.events.push(ev(1500, "message-raised", json!({"id": "m-a", "about": {"task": "r-1"}, "for": null, "from": {"human": true}, "reply_to": "m-p", "shape": "notification"})));
        let ph = build(&i);
        let gaps: Vec<(i64, i64, &str)> = ph.iter().filter(|p| p.gap && p.start >= 200 && p.end <= 1990).map(|p| (p.start, p.end, p.waiting_for.as_deref().unwrap())).collect();
        assert_eq!(gaps.len(), 2, "{gaps:?}");
        assert_eq!((gaps[0].0, gaps[0].1), (200, 1500));
        assert!(gaps[0].2.starts_with(PERSON_STEP_CAUSE) && gaps[0].2.contains("question m-p") && gaps[0].2.contains("answered at"), "{gaps:?}");
        assert_eq!((gaps[1].0, gaps[1].1), (1500, 1990));
        assert!(gaps[1].2.starts_with(REWORK_CAUSE), "{gaps:?}");
        assert_eq!(figures_of(&ph, &SeatReport::default()).between_attempts, 1790);
    }

    /// wsp-635's 65m (`wsp-645`): a stretch no park explains still names its row.
    #[test]
    fn the_stretch_the_logs_do_not_name_names_its_row() {
        let i = two_seatings(true);
        let why = wait_name(&i, 300, 2900, "r-1");
        assert!(why.starts_with("nothing the logs name") && why.contains("r-1"), "{why}");
    }

    /// wsp-635 (`wsp-645`): a stretch a list park covered is named for it, not
    /// as nothing the logs name.
    #[test]
    fn a_stretch_inside_a_list_park_is_named_for_the_park_by_wait_name() {
        let mut i = two_seatings(true);
        let evs = vec![
            (140, "worklist-parked".to_string(), json!({"id": "l", "why": "he needs the machine", "by": "person"})),
            (1900, "worklist-resumed".to_string(), json!({"id": "l", "at": 1, "said": "go", "by": "person"})),
        ];
        i.parks = parks_of(&evs, "l", 9999);
        let why = wait_name(&i, 300, 1950, "r-1");
        assert!(why.starts_with(PARKED_LIST) && why.contains("he needs the machine"), "{why}");
        assert!(wait_name(&i, 1890, 2900, "r-1").starts_with("nothing the logs"), "a park under half of it names nothing");
    }

    /// react-parity-now (`compounding-464`): rows sent back at a rebase sat idle
    /// through a list park; the stretch is the park, not rework.
    #[test]
    fn a_sent_back_row_idle_through_a_list_park_is_the_park_and_not_rework() {
        let mut i = two_seatings(false);
        let evs = vec![
            (210, "worklist-parked".to_string(), json!({"id": "l", "why": "stop", "by": "person"})),
            (1980, "worklist-resumed".to_string(), json!({"id": "l"})),
        ];
        i.parks = parks_of(&evs, "l", 9999);
        let ph = build(&i);
        let why = ph.iter().find(|p| p.gap && p.label.starts_with("parked at")).and_then(|p| p.waiting_for.clone()).expect("named for the park");
        assert!(why.contains("parked by person") && !why.contains("rework"), "{why}");
    }

    #[test]
    fn parks_are_read_from_the_park_to_the_resume_and_a_record_naming_no_one_is_somebody() {
        let evs = vec![
            (10, "worklist-parked".to_string(), json!({"id": "l", "why": "w"})),
            (50, "worklist-resumed".to_string(), json!({"id": "l"})),
            (60, "worklist-parked".to_string(), json!({"id": "l", "why": "again", "by": "primary"})),
            (70, "worklist-parked".to_string(), json!({"id": "other", "why": "x"})),
        ];
        let p = parks_of(&evs, "l", 100);
        assert_eq!(p.len(), 2);
        assert_eq!((p[0].start, p[0].end, p[0].by.as_str(), p[0].open), (10, 50, "somebody", false));
        assert_eq!((p[1].start, p[1].end, p[1].by.as_str(), p[1].open), (60, 100, "primary", true));
        assert!(p[1].words().contains("still parked") && p[0].words().contains("parked by somebody"));
    }

    /// `wsp-334` over wsp-030's five leg boundaries (06:43:38 to 07:08:44 on
    /// 2026-11-20, `leg-done` each), on the timeline. cycle's
    /// `wsp_030s_five_leg_boundaries_each_cost_under_30s` drives the run for real
    /// and finds each next leg started by the poke of the leg done before it,
    /// with no tick taken; so the starts go into the fixture at the done time
    /// plus a hand-off for the poke (and a [`TICK`] for each tick, none). The
    /// gap before each leg is under 30 s. The log as it happened (two ticks a
    /// boundary) is read the same way for comparison: every gap is "nothing the
    /// logs name", and 614 s in all. Split from the cycle test when the engine
    /// left the root crate (wsp-440).
    #[test]
    fn wsp_030s_five_leg_boundaries_on_the_timeline() {
        const NAMES: [&str; 6] = ["wire-inproc", "daemon-tick", "verbs", "tests-docs", "finish", "rework"];
        let at = |hms: &str| util::epoch_of(&format!("2026-11-20T{hms}Z"));
        let done = ["06:43:38", "06:57:40", "07:02:46", "07:05:52", "07:08:44"].map(at);
        let then = ["06:45:26", "06:59:43", "07:04:50", "07:07:55", "07:11:00"].map(at);
        // The ticks the run took at each boundary, as cycle's test finds them.
        let ticks = [0usize; 5];
        let gaps = |starts: &[i64]| {
            let mut i = Inputs { id: "wsp-030".into(), ..Default::default() };
            i.legs = NAMES.iter().map(|n| LegIn { name: n.to_string(), ..Default::default() }).collect();
            i.cycle.push((at("06:35:26"), "m1-agent-manager group 8: starting leg wire-inproc of wsp-030".into()));
            for k in 0..5 {
                i.events.push((done[k], "leg-done".into(), serde_json::json!({ "id": "wsp-030", "leg": NAMES[k] })));
                i.cycle.push((starts[k], format!("m1-agent-manager group 8: starting leg {} of wsp-030", NAMES[k + 1])));
            }
            build(&i).into_iter().filter(|p| p.gap && p.label.starts_with("gap before leg")).collect::<Vec<_>>()
        };
        let fixed: Vec<i64> = (0..5).map(|k| done[k] + HAND_OFF / 2 + ticks[k] as i64 * TICK).collect();
        let now = gaps(&fixed);
        assert_eq!(now.len(), 5, "{now:?}");
        for g in &now {
            assert!(g.seconds() < 30, "{g:?}");
            assert!(!g.waiting_for.as_deref().unwrap_or_default().contains("did not run for longer than a tick"), "{g:?}");
        }
        let was = gaps(&then);
        assert_eq!(was.len(), 5, "{was:?}");
        assert!(was.iter().all(|g| g.waiting_for.as_deref().unwrap_or_default().contains("the step of wsp-030 did not run for longer than a tick")), "{was:?}");
        assert_eq!(was.iter().map(|g| g.seconds()).sum::<i64>(), 614);
    }
}
