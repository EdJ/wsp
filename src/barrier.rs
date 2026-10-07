//! A barrier check is an entry on the list's group: `wsp-230`.
//!
//! Every barrier used to be filed as a row of its own, "Barrier: <list> group
//! N", whose overview was the check's work order. Ed, 2026-10-07: each row then
//! sat at `review` waiting for a `done` nobody owed it, and the backlog grew by
//! one per group — compound-365, -367, -369 and -371 on `gpui-migration` alone.
//! `wsp-188` stopped doing this for verifiers, and the barrier is the same kind
//! of step.
//!
//! So the check is recorded where its verdict is read: on the group, under a
//! `checks:` block beside the `verdict:` it writes. One entry per check, oldest
//! first, and a re-check after a blocking follow-up adds one:
//!
//! ```text
//!   checks: 2026-10-07T12:04:11Z passes read:55c4839abcde pane:cpd-484
//!           on:claude+opus/high ended · 2026-10-07T13:00:00Z running
//! ```
//!
//! **Tokens, like `landed:`**, because every value is whitespace-free — an
//! instant, a word, a hash, a seat id, a tier with its spaces turned to `+` —
//! so a wrapped block is the same walk run again and nothing a writer does can
//! split an entry. An entry starts at an instant; every token after it until
//! the next instant is one of its clauses, keyed by the word before the colon.
//! The prose stays where it already was: a pass's sentence is the group's
//! `verdict:`, a hold's and a follow-up's are on the list's `## Log`.
//!
//! # The entry is the record, and the only one
//!
//! A check has no row and holds no claim. The `running` entry is written under
//! the store lock *before* its agent is spawned — the idempotence key the row's
//! title used to be — and `wsp spawn <list> --check N` writes the seat onto it.
//! Everything that asks about a check asks the entry: the step that opens one
//! ([`open`]), the verbs that settle one ([`settle`]), the ending
//! ([`crate::cycle::end_checks_when_idle`] and the tick), `wsp wip` naming the
//! seat against its list ([`seats`]), and the brief it is given.
//!
//! # The rows that came before
//!
//! A Barrier row wsp filed is still read by the run while it is open — its
//! agent may be mid-check when this binary is installed, and the verbs it was
//! told to finish with still take it — and [`migrate`] moves it onto its group
//! once it has settled.

use std::collections::BTreeMap;

use crate::model::{Group, Status, Task, Worklist};
use crate::store::Store;
use crate::util::{self, Paint};
use crate::Args;

/// Where a check is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    /// Opened, and no verdict yet. Its seat may not exist yet either.
    Running,
    /// `go`: the barrier is passed.
    Passes,
    /// `hold`: the run stops here, and the governor is told.
    Holds,
    /// `followup --next`: it passes, and the rows it found are the next group.
    Next,
    /// `followup --blocking`: it holds until the rows it found land, and a
    /// fresh check reads the group again.
    Blocking,
    /// Its agent went away before it said anything, and the step opened a
    /// fresh check rather than wait on a seat with nobody in it.
    Abandoned,
}

impl State {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            State::Running => "running",
            State::Passes => "passes",
            State::Holds => "holds",
            State::Next => "next",
            State::Blocking => "blocking",
            State::Abandoned => "abandoned",
        }
    }

    fn parse(s: &str) -> Option<State> {
        Some(match s {
            "running" => State::Running,
            "passes" => State::Passes,
            "holds" => State::Holds,
            "next" => State::Next,
            "blocking" => State::Blocking,
            "abandoned" => State::Abandoned,
            _ => return None,
        })
    }

    /// What a reader is told it said.
    pub(crate) fn said(self) -> &'static str {
        match self {
            State::Running => "checking",
            State::Passes => "passed",
            State::Holds => "held",
            State::Next => "passed, with follow-ups next",
            State::Blocking => "held for follow-ups",
            State::Abandoned => "abandoned",
        }
    }
}

/// Whether the check's seat has been ended. **Failed is terminal**, for
/// `verification::Ending`'s reason: tried once, said once in `cycle.log`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum Ending {
    #[default]
    Standing,
    Ended,
    Failed,
}

/// One check on one barrier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Check {
    /// When it was opened, as `util::now_iso` writes it.
    pub at: String,
    pub state: State,
    /// The trunk commit it was opened against. `None` where there is no
    /// repository to read one from.
    pub read: Option<String>,
    /// The seat its agent was started in.
    pub pane: Option<String>,
    /// What it ran on: the policy's label, its spaces written as `+` — a
    /// model's own name has `/` in it (`openrouter/…`), so that cannot be it.
    pub on: Option<String>,
    pub ending: Ending,
    /// The Barrier row this entry was migrated from.
    pub was: Option<String>,
}

impl Check {
    pub(crate) fn opened(read: Option<String>, on: Option<String>) -> Check {
        Check { at: util::now_iso(), state: State::Running, read, pane: None, on, ending: Ending::Standing, was: None }
    }

    /// Still somebody's: opened and not yet settled.
    pub(crate) fn open(&self) -> bool {
        self.state == State::Running
    }

    /// A seat that has not been ended.
    pub(crate) fn standing(&self) -> Option<&str> {
        self.pane.as_deref().filter(|_| self.ending == Ending::Standing)
    }
}

/// Whether a token starts an entry: an instant as `now_iso` writes one.
fn instant(tok: &str) -> bool {
    let b = tok.as_bytes();
    b.len() == 20 && b[4] == b'-' && b[10] == b'T' && b[19] == b'Z' && b[0].is_ascii_digit()
}

/// One token stream of a `checks:` block, folded into the entries already read
/// — the walk `model::landed_push` does, for the same reason: a continuation
/// line is the same walk run again, so wrapping is free.
pub(crate) fn push(acc: &mut Vec<Check>, text: &str) {
    for tok in text.split_whitespace() {
        if tok == "·" {
            continue;
        }
        if instant(tok) {
            acc.push(Check {
                at: tok.to_string(),
                state: State::Running,
                read: None,
                pane: None,
                on: None,
                ending: Ending::Standing,
                was: None,
            });
            continue;
        }
        let Some(c) = acc.last_mut() else { continue };
        match tok.split_once(':') {
            Some(("read", v)) => c.read = Some(v.to_string()),
            Some(("pane", v)) => c.pane = Some(v.to_string()),
            Some(("on", v)) => c.on = Some(v.replace('+', " ")),
            Some(("was", v)) => c.was = Some(v.to_string()),
            Some(_) => {}
            None => match tok {
                "ended" => c.ending = Ending::Ended,
                "end-failed" => c.ending = Ending::Failed,
                w => {
                    if let Some(s) = State::parse(w) {
                        c.state = s;
                    }
                }
            },
        }
    }
}

/// A `checks:` block as the file holds it: the inverse of [`push`].
pub(crate) fn text(checks: &[Check]) -> String {
    let mut out: Vec<String> = Vec::new();
    for c in checks {
        let mut e = vec![c.at.clone(), c.state.as_str().to_string()];
        if let Some(r) = &c.read {
            e.push(format!("read:{r}"));
        }
        if let Some(p) = &c.pane {
            e.push(format!("pane:{p}"));
        }
        if let Some(o) = &c.on {
            e.push(format!("on:{}", o.split_whitespace().collect::<Vec<_>>().join("+")));
        }
        if let Some(w) = &c.was {
            e.push(format!("was:{w}"));
        }
        match c.ending {
            Ending::Standing => {}
            Ending::Ended => e.push("ended".into()),
            Ending::Failed => e.push("end-failed".into()),
        }
        out.push(e.join(" "));
    }
    out.join(" · ")
}

/// What `wsp wip` and the seat's own label call a check: against its list,
/// since there is no row to name it by.
pub(crate) fn seat_label(list: &str, at: usize) -> String {
    format!("{list} · barrier {at}")
}

/// The name a check's brief file and tree are kept under, as a verifier's is
/// `<member>-verify`.
pub(crate) fn subject(list: &str, at: usize) -> String {
    format!("{list}-check-{at}")
}

/// The list and group a [`subject`] names, if it is one.
pub(crate) fn parse_subject(name: &str) -> Option<(&str, usize)> {
    let (list, at) = name.rsplit_once("-check-")?;
    Some((list, at.parse().ok()?))
}

/// Every standing check seat, by pane: the list and the group it is reading.
///
/// Every list, and not only running ones: a check that held its list is still
/// seated until the tick ends it, and `wip` must not draw it as holding
/// nothing in the meantime.
pub(crate) fn seats(lists: &[Worklist]) -> BTreeMap<String, (String, usize)> {
    let mut out = BTreeMap::new();
    for w in lists {
        for (i, g) in w.groups().iter().enumerate() {
            for c in &g.checks {
                if let Some(p) = c.standing() {
                    out.insert(p.to_string(), (w.id.clone(), i + 1));
                }
            }
        }
    }
    out
}

/// The list and group a seat is checking, if it is one.
pub(crate) fn seat_of(store: &Store, seat: &str) -> Option<(String, usize)> {
    seats(&store.worklists()).remove(seat)
}

/// The open check on a group, if there is one.
pub(crate) fn running(g: &Group) -> Option<&Check> {
    g.checks.iter().rev().find(|c| c.open())
}

/// Write a change to group `at` of `list` under the lock, reading the list
/// afresh, and say whether it was made. `f` answers whether it changed anything.
fn change(store: &Store, list: &str, at: usize, f: impl FnOnce(&mut Group) -> bool) -> bool {
    store.locked(|| {
        let Some(mut w) = store.worklist(list) else { return false };
        let mut groups = w.groups();
        let Some(g) = groups.get_mut(at - 1) else { return false };
        if !f(g) {
            return false;
        }
        w.set_groups(&groups);
        store.save_worklist(&w).is_ok()
    })
}

/// A check opened on group `at`, or none owed.
pub(crate) struct Opened {
    /// It took over an open entry nobody's agent ever reached.
    pub retaken: bool,
}

/// How long an open entry with no seat is left before it is taken again: the
/// start is assumed to have failed. The same ten minutes as a row's.
const WEDGED_AFTER: i64 = 10 * 60;

/// Open a check on group `at`, if one is owed, under the lock. `gone` answers
/// whether a seat has nobody in it any more.
///
/// **One open entry is the key**, as a row's title used to be. An open entry
/// with a seat is a check in flight and nothing is owed — unless that seat has
/// gone, when it is marked abandoned and a fresh one opens, rather than the run
/// waiting on an agent that will never answer. An open entry with no seat after
/// [`WEDGED_AFTER`] is a start that never arrived, and is taken again.
pub(crate) fn open(store: &Store, w: &Worklist, at: usize, read: Option<String>, on: Option<String>, gone: &dyn Fn(&str) -> bool) -> Option<Opened> {
    let mut opened = None;
    let changed = change(store, &w.id, at, |g| {
        if let Some(c) = g.checks.iter_mut().rev().find(|c| c.open()) {
            match c.pane.as_deref() {
                Some(p) if gone(p) => {
                    c.state = State::Abandoned;
                    c.ending = Ending::Ended;
                }
                Some(_) => return false,
                None if util::epoch_secs() - util::epoch_of(&c.at) > WEDGED_AFTER => {
                    c.at = util::now_iso();
                    opened = Some(Opened { retaken: true });
                    return true;
                }
                None => return false,
            }
        }
        g.checks.push(Check::opened(read, on));
        opened = Some(Opened { retaken: false });
        true
    });
    changed.then_some(()).and(opened)
}

/// Name the seat of the open check on group `at`. `wsp spawn --check` calls it
/// once the seat exists and before the agent starts, because the brief it
/// reads at start-up finds it by this seat.
pub(crate) fn name_seat(store: &Store, list: &str, at: usize, seat: &str) -> bool {
    change(store, list, at, |g| match g.checks.iter_mut().rev().find(|c| c.open()) {
        Some(c) => {
            c.pane = Some(seat.to_string());
            true
        }
        None => false,
    })
}

/// Settle the open check on group `at`. Returns the seat it ran in, for the
/// ending, when there was one to settle.
///
/// **Whoever settles it**: the check itself, nearly always, but a governor's
/// `go` over a check still reading also ends that check's job, and the entry
/// says what the barrier came to either way.
pub(crate) fn settle(store: &Store, list: &str, at: usize, state: State) -> Option<String> {
    let mut seat = None;
    change(store, list, at, |g| match g.checks.iter_mut().rev().find(|c| c.open()) {
        Some(c) => {
            c.state = state;
            seat = c.pane.clone();
            true
        }
        None => false,
    });
    seat
}

/// Record the ending of the check that ran in `seat`, wherever it is.
/// `despawn` calls it for every seat it ends, so the record follows the
/// ending however it came about.
pub(crate) fn mark_ended(store: &Store, seat: &str, ok: bool) -> bool {
    let Some((list, at)) = seat_of(store, seat) else { return false };
    change(store, &list, at, |g| {
        let mut any = false;
        for c in g.checks.iter_mut().filter(|c| c.standing() == Some(seat)) {
            c.ending = if ok { Ending::Ended } else { Ending::Failed };
            any = true;
        }
        any
    })
}

/// Group `at`'s standing check seats, settled or not.
pub(crate) fn standing_on(g: &Group) -> Vec<String> {
    g.checks.iter().filter_map(|c| c.standing().map(str::to_string)).collect()
}

/// The one-line readings of a group's checks, for `worklist show --verdicts`:
/// what each said, the commit it read and the seat it ran in.
pub(crate) fn lines(p: &Paint, g: &Group) -> Vec<String> {
    g.checks
        .iter()
        .map(|c| {
            let mut parts = vec![format!("check {}", c.at), c.state.said().to_string()];
            if let Some(r) = &c.read {
                parts.push(format!("read {}", &r[..r.len().min(12)]));
            }
            if let Some(s) = &c.pane {
                parts.push(format!("in {s}"));
            }
            if let Some(o) = &c.on {
                parts.push(format!("on {o}"));
            }
            if let Some(w) = &c.was {
                parts.push(format!("was {w}"));
            }
            p.dim(&parts.join(" · "))
        })
        .collect()
}

// ---- the rows that came before ----------------------------------------

/// How the barrier work order wsp filed on every row began. What the migration
/// keys on, with the tag: a row somebody wrote by hand about a barrier
/// (compound-291, compound-330) carries neither.
const ORDER_OPENS: &str = "Check the barrier after group ";

/// A Barrier row wsp filed: the tag, and wsp's own work order.
pub(crate) fn filed_by_wsp(t: &Task) -> bool {
    t.tags.iter().any(|g| g == crate::cycle::BARRIER_TAG)
        && t.section("Overview").is_some_and(|o| o.trim_start().starts_with(ORDER_OPENS))
}

/// The list and group a wsp-filed row checked, from its title.
fn placed(t: &Task) -> Option<(String, usize)> {
    let list = crate::cycle::barrier_list(t)?.to_string();
    let rest = t.title.strip_prefix(&format!("{}{list} group ", crate::cycle::BARRIER_TITLE))?;
    let at: usize = rest.split_whitespace().next()?.parse().ok()?;
    crate::cycle::is_barrier(t, &list, at).then_some((list, at))
}

/// The entry a settled row becomes. The state is read off what the run
/// recorded rather than the row's status, which only ever said `review`: a
/// follow-up names the row in the list's log, and otherwise a verdict on the
/// group is a pass — unless a later check on the same barrier exists, in which
/// case this one held.
fn legacy(row: &Task, w: &Worklist, g: &Group, later: bool) -> Check {
    let log = w.section("Log").unwrap_or_default();
    let mark = |how: &str| log.lines().any(|l| l.contains(&format!("follow-up by {} {how}", row.id)));
    let state = if mark("passes") {
        State::Next
    } else if mark("holds") {
        State::Blocking
    } else if !g.verdict.trim().is_empty() && !later {
        State::Passes
    } else {
        State::Holds
    };
    let rlog = row.section("Log").unwrap_or_default();
    let pane = rlog
        .lines()
        .find_map(|l| l.split("claimed by pane ").nth(1))
        .and_then(|r| r.split_whitespace().next())
        .map(str::to_string);
    let on = rlog
        .lines()
        .find_map(|l| l.split("spawned at ").nth(1))
        .and_then(|r| r.split_whitespace().next())
        .map(str::to_string);
    Check { at: row.created.clone(), state, read: None, pane, on, ending: Ending::Ended, was: Some(row.id.clone()) }
}

/// What [`migrate`] would do.
pub(crate) struct Plan {
    /// (list, group) -> the rows whose checks go onto it, oldest first.
    pub moves: BTreeMap<(String, usize), Vec<Task>>,
    /// Still open or still holding a claim: an agent may be reading.
    pub open: Vec<String>,
    /// Filed by wsp, but naming a list or group that is not there.
    pub orphans: Vec<String>,
    /// Tagged `barrier`, or titled like one, without wsp's work order.
    pub by_hand: Vec<String>,
}

pub(crate) fn plan(store: &Store) -> Plan {
    let tasks = store.tasks();
    let claims = store.claims();
    let mut plan = Plan { moves: BTreeMap::new(), open: Vec::new(), orphans: Vec::new(), by_hand: Vec::new() };
    for t in &tasks {
        if !filed_by_wsp(t) {
            let looks = t.tags.iter().any(|g| g == crate::cycle::BARRIER_TAG) || t.title.starts_with("Barrier");
            if looks {
                plan.by_hand.push(t.id.clone());
            }
            continue;
        }
        if claims.contains_key(&t.id) || !matches!(t.status(), Status::Review | Status::Done) {
            plan.open.push(t.id.clone());
            continue;
        }
        let Some((list, at)) = placed(t) else {
            plan.orphans.push(t.id.clone());
            continue;
        };
        if store.worklist(&list).map_or(true, |w| w.groups().len() < at) {
            plan.orphans.push(t.id.clone());
            continue;
        }
        plan.moves.entry((list, at)).or_default().push(t.clone());
    }
    for rows in plan.moves.values_mut() {
        rows.sort_by(|a, b| a.created.cmp(&b.created).then_with(|| a.id.cmp(&b.id)));
    }
    plan
}

/// `wsp migrate --barrier-rows [-n]` — every settled Barrier row wsp filed,
/// moved onto its list's group as a check and archived.
///
/// **Old ids keep resolving.** The row is archived, not deleted, and `wsp show`
/// reads the archive, so a log or a verdict that cites compound-369 still finds
/// it, and the entry on the group names it with `was:`.
pub(crate) fn migrate(store: &Store, args: &Args) -> i32 {
    let dry = args.has("n") || args.has("dry-run");
    let p = Paint::new();
    let plan = plan(store);
    for ((list, at), rows) in &plan.moves {
        println!(
            "{}  {} check(s) from {}",
            p.bold(&format!("{list} group {at}")),
            rows.len(),
            rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>().join(" ")
        );
    }
    if !plan.open.is_empty() {
        println!("{}", p.yellow(&format!("left open: {} — an agent may still be reading; run this again once it settles", plan.open.join(" "))));
    }
    if !plan.orphans.is_empty() {
        println!("{}", p.yellow(&format!("left alone: {} — no such list or group", plan.orphans.join(" "))));
    }
    if !plan.by_hand.is_empty() {
        println!("{}", p.dim(&format!("made by hand, left alone: {}", plan.by_hand.join(" "))));
    }
    let n: usize = plan.moves.values().map(Vec::len).sum();
    println!("\n{n} Barrier row(s) onto {} group(s)", plan.moves.len());
    if n == 0 {
        return 0;
    }
    if dry {
        println!("{}", p.dim("nothing written — drop -n to apply"));
        return 0;
    }
    match apply(store, &plan) {
        Ok(moved) => {
            store.git_commit(&format!("wsp: {moved} Barrier row(s) moved onto their groups' checks"));
            println!("moved {moved}");
            0
        }
        Err(e) => {
            eprintln!("wsp: {e}");
            1
        }
    }
}

fn apply(store: &Store, plan: &Plan) -> Result<usize, String> {
    let mut moved = 0;
    for ((list, at), rows) in &plan.moves {
        let wrote = store.locked(|| -> Result<(), String> {
            let mut w = store.worklist(list).ok_or_else(|| format!("{list} went away"))?;
            let mut groups = w.groups();
            let g = groups.get_mut(at - 1).ok_or_else(|| format!("{list} has no group {at}"))?;
            for (i, row) in rows.iter().enumerate() {
                if g.checks.iter().any(|c| c.was.as_deref() == Some(row.id.as_str())) {
                    continue;
                }
                let entry = legacy(row, &w, g, i + 1 < rows.len());
                g.checks.push(entry);
            }
            g.checks.sort_by(|a, b| a.at.cmp(&b.at));
            w.set_groups(&groups);
            store.save_worklist(&w).map_err(|e| format!("{list}: {e}"))
        });
        wrote?;
        for row in rows {
            store.archive_task(row).map_err(|e| format!("{}: {e}", row.id))?;
            moved += 1;
        }
    }
    Ok(moved)
}
