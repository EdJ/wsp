//! A row run as a graph of agents: `docs/legs.md`, decided on wsp-287 d3.
//!
//! A row carries its legs under `## Legs`, one entry each, in the order they
//! were added. Modelled on [`crate::verification`], and for the same reason: the
//! record is the entry, and everything that asks about a leg asks the entry.
//!
//! ```text
//! ## Legs
//! - scout · claude sonnet medium · done · pane cpd-31 · ended
//!   order: Read the three call sites of … and say which layer owns …
//!   baton: three layers: store (src/legs.rs new), …
//! - plan · claude opus high · done · pane cpd-34 · ended
//!   why: decides which layer owns the leg record, from the scout's options
//! - build-store · claude sonnet medium · pending
//!   after: plan
//! ```
//!
//! This module is the record: the parse and write, the rules every write is
//! checked against, and `wsp leg add | after | skip | show`. A leg's seat, its
//! tree, the baton it hands on and the run that starts the next leg are later
//! rows (legs-seat, legs-tree, legs-run); the clauses they write are read and
//! kept here so those rows have nowhere to put a second record.
//!
//! # The choices the doc left open (wsp-291, v1)
//!
//! - **`add` appends.** An entry with no `after:` waits on the entry above it,
//!   so inserting above one would change what it waits on. A new entry goes at
//!   the end of the section and every other leg keeps its waits, exactly. The
//!   file order is therefore the order legs were added, and it is what an
//!   entry's implicit wait reads.
//! - **`--before X` with no `--after`** gives the new leg the legs `X` waits
//!   on now, and adds the new leg to `X`. Taken literally, "the current last
//!   leg" would be `X` itself and the write would be a cycle.
//! - **A leg is ready when every leg upstream of it, by any path, is `done` or
//!   `skipped`** — not only the legs it names. With A → B → J, A running in the
//!   row's tree and B skipped, J's `after:` is all satisfied while A still
//!   works in the tree the join would merge into. Readiness waits on A instead
//!   of the write refusing the skip, because a skip of a pending leg is a
//!   reasonable thing for a governor to do while the leg before it runs.
//! - **The default tier is exactly `claude sonnet medium`** ([`DEFAULT_AGENT`]).
//!   `claude sonnet` and `claude sonnet high` are other policies and need a
//!   `why:`: an unwritten effort is whatever the spawn would choose, which is
//!   not a constant a record can print.
//! - **A re-point is judged by the waits it adds.** `finish` waits on `b1` and
//!   `b2`; `b1`, seated, may re-point it and keep `b2`, which it already waited
//!   on. Only a wait the leg did not have must be the seated leg or downstream of
//!   it, so siblings can each re-point the last leg (the doc allows it) and none
//!   can point a leg at a sibling that was not there.
//! - **Every write is judged by what it changes.** A record that was already
//!   wrong in the same way before the write (a hand edit) does not make an
//!   unrelated write refuse.

use std::collections::{BTreeMap, BTreeSet};

use crate::model::{Policy, Task};
use crate::store::Store;
use crate::util::{self, Paint};
use crate::verification::Ending;
use crate::Args;

/// The heading the legs sit under on the row.
pub(crate) const SECTION: &str = "Legs";

/// What a leg with no policy of its own runs on: one constant, so no member's or
/// group's `agent` line ever reaches a leg (`docs/legs.md`, *The record*).
pub(crate) const DEFAULT_AGENT: &str = "claude sonnet medium";

/// Most legs a row carries, rework included. A row that needs more is rows.
pub(crate) const MAX_LEGS: usize = 8;
/// The order and a baton, each, in characters.
pub(crate) const TEXT_CAP: usize = 3_000;
/// The batons that arrive at one leg, together.
pub(crate) const JOIN_CAP: usize = 6_000;
/// A `why:` and a skip's reason, in characters.
pub(crate) const WHY_CAP: usize = 300;
/// A leg's name, in characters.
const NAME_CAP: usize = 40;

/// Where a leg is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    Pending,
    Running,
    Done,
    Skipped,
}

impl State {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            State::Pending => "pending",
            State::Running => "running",
            State::Done => "done",
            State::Skipped => "skipped",
        }
    }

    fn parse(s: &str) -> Option<State> {
        Some(match s {
            "pending" => State::Pending,
            "running" => State::Running,
            "done" => State::Done,
            "skipped" => State::Skipped,
            _ => return None,
        })
    }

    /// Whether a leg waiting on this one may go on.
    fn settled(self) -> bool {
        matches!(self, State::Done | State::Skipped)
    }
}

/// One leg of a row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Leg {
    pub name: String,
    /// What it runs on. Always a policy: an entry that names none reads as
    /// [`default_policy`], and is written back with it so `wsp show` never hides
    /// a tier.
    pub policy: Policy,
    pub state: State,
    /// The seat its agent was started in. Written by legs-seat.
    pub pane: Option<String>,
    /// The tree it stands in, when it is not the row's. Written by legs-tree.
    pub tree: Option<String>,
    pub ending: Ending,
    /// The legs it waits on, as written. `None` is no `after:` line: it waits on
    /// the entry above, and the first entry waits on nothing. `Some(vec![])` is
    /// `after: -`.
    pub after: Option<Vec<String>>,
    /// Why this tier, for a leg that is not on the default.
    pub why: Option<String>,
    pub order: String,
    pub baton: String,
    /// The line a glance reads, written with the baton.
    pub caption: Option<String>,
    /// Why it was skipped.
    pub skipped: Option<String>,
    /// When it was opened and when it finished, RFC 3339. Written by legs-seat.
    pub started: Option<String>,
    pub closed: Option<String>,
    /// What the spawn did to bring this leg's waits together: the branches it
    /// merged, the conflict it aborted, the overlaps its siblings reported.
    /// Written by legs-tree when the leg opens; the leg's brief carries it.
    pub join: Option<String>,
    /// For the leg a blocking verdict appended: the legs the verdict named,
    /// which may be none. `None` is a leg that is not a rework.
    pub rework: Option<Vec<String>>,
}

/// The tier a leg with no policy runs on.
pub(crate) fn default_policy() -> Policy {
    Policy::parse(DEFAULT_AGENT).expect("the default agent reads as a policy")
}

/// Whether a policy is the default tier, and so needs no reason.
pub(crate) fn is_default(p: &Policy) -> bool {
    *p == default_policy()
}

/// A policy as an entry writes it: the words `claude opus high`, which is also
/// what `wsp leg add --agent` takes.
fn policy_line(p: &Policy) -> String {
    let mut out = p.kind.clone();
    for w in [&p.model, &p.effort].into_iter().flatten() {
        out.push(' ');
        out.push_str(w);
    }
    out
}

impl Leg {
    /// A pending leg on the default tier.
    pub(crate) fn pending(name: &str, order: &str) -> Leg {
        Leg {
            name: name.to_string(),
            policy: default_policy(),
            state: State::Pending,
            pane: None,
            tree: None,
            ending: Ending::Standing,
            after: None,
            why: None,
            order: order.trim().to_string(),
            baton: String::new(),
            caption: None,
            skipped: None,
            started: None,
            closed: None,
            join: None,
            rework: None,
        }
    }

    fn head(&self) -> String {
        let mut out = format!("- {} · {} · {}", self.name, policy_line(&self.policy), self.state.as_str());
        let mut clause = |k: &str, v: &Option<String>| {
            if let Some(v) = v.as_deref().filter(|v| !v.is_empty()) {
                out.push_str(&format!(" · {k} {v}"));
            }
        };
        clause("pane", &self.pane);
        clause("tree", &self.tree);
        clause("started", &self.started);
        clause("closed", &self.closed);
        match &self.ending {
            Ending::Standing => {}
            Ending::Ended => out.push_str(" · ended"),
            Ending::Failed(why) => out.push_str(&format!(" · end failed: {}", crate::cmd_task::fold(why))),
        }
        out
    }

    fn lines(&self) -> String {
        let mut out = self.head();
        let mut field = |key: &str, text: &str| {
            let text = text.trim();
            if text.is_empty() {
                return;
            }
            let mut lines = text.lines();
            out.push_str(&format!("\n  {key}: {}", lines.next().unwrap_or_default().trim_end()));
            for l in lines {
                // Four spaces, so a line of somebody's text can never read as
                // a key at two.
                out.push('\n');
                if !l.trim().is_empty() {
                    out.push_str(&format!("    {}", l.trim_end()));
                }
            }
        };
        if let Some(after) = &self.after {
            field("after", &if after.is_empty() { "-".to_string() } else { after.join(", ") });
        }
        field("why", self.why.as_deref().unwrap_or_default());
        field("order", &self.order);
        field("caption", self.caption.as_deref().unwrap_or_default());
        field("baton", &self.baton);
        field("skipped", self.skipped.as_deref().unwrap_or_default());
        field("join", self.join.as_deref().unwrap_or_default());
        if let Some(of) = &self.rework {
            field("rework", &if of.is_empty() { "-".to_string() } else { of.join(", ") });
        }
        out
    }

    fn parse_head(line: &str) -> Option<Leg> {
        let rest = line.strip_prefix("- ")?;
        let mut clauses = rest.split(" · ").map(str::trim);
        let name = clauses.next().filter(|n| !n.is_empty())?.to_string();
        let second = clauses.next()?;
        let (policy, state) = match State::parse(second) {
            Some(s) => (default_policy(), s),
            None => (Policy::of_label(second)?, State::parse(clauses.next()?)?),
        };
        let mut leg = Leg { policy, state, ..Leg::pending(&name, "") };
        for c in clauses {
            if c == "ended" {
                leg.ending = Ending::Ended;
            } else if let Some(why) = c.strip_prefix("end failed:") {
                leg.ending = Ending::Failed(why.trim().to_string());
            } else if let Some((k, v)) = c.split_once(' ') {
                let v = Some(v.trim().to_string());
                match k {
                    "pane" => leg.pane = v,
                    "tree" => leg.tree = v,
                    "started" => leg.started = v,
                    "closed" => leg.closed = v,
                    _ => {}
                }
            }
        }
        Some(leg)
    }
}

/// The legs written on a row, in file order.
pub(crate) fn legs(t: &Task) -> Vec<Leg> {
    let Some(text) = t.section(SECTION) else { return Vec::new() };
    let mut out: Vec<(Leg, BTreeMap<String, String>, Option<String>)> = Vec::new();
    for line in text.lines() {
        if line.starts_with("- ") {
            if let Some(leg) = Leg::parse_head(line) {
                out.push((leg, BTreeMap::new(), None));
            }
            continue;
        }
        let Some((_, fields, key)) = out.last_mut() else { continue };
        const KEYS: [&str; 8] = ["after", "why", "order", "caption", "baton", "skipped", "join", "rework"];
        let two = line.strip_prefix("  ").filter(|r| !r.starts_with(' '));
        if let Some((k, v)) = two.and_then(|r| r.split_once(':')).filter(|(k, _)| KEYS.contains(k)) {
            fields.insert(k.to_string(), v.strip_prefix(' ').unwrap_or(v).to_string());
            *key = Some(k.to_string());
        } else if let Some(k) = key.as_ref() {
            let more = line.strip_prefix("    ").unwrap_or(line.trim_start());
            let f = fields.entry(k.clone()).or_default();
            f.push('\n');
            f.push_str(more);
        }
    }
    out.into_iter()
        .map(|(mut leg, f, _)| {
            let get = |k: &str| f.get(k).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
            leg.after = get("after").map(|a| {
                a.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty() && s != "-").collect()
            });
            leg.why = get("why");
            leg.order = get("order").unwrap_or_default();
            leg.caption = get("caption");
            leg.baton = get("baton").unwrap_or_default();
            leg.skipped = get("skipped");
            leg.join = get("join");
            leg.rework = get("rework").map(|a| {
                a.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty() && s != "-").collect()
            });
            leg
        })
        .collect()
}

/// Write the legs back under the heading, in the order given. Does not touch the
/// row's `updated`, as a verification pass does not: a leg opening is
/// bookkeeping, and a re-verify bought by it would be bought by nobody.
pub(crate) fn write(t: &mut Task, legs: &[Leg]) {
    let text = legs.iter().map(Leg::lines).collect::<Vec<_>>().join("\n");
    crate::model::set_section_in(&mut t.body, SECTION, &text);
}

// ---- the graph ----------------------------------------------------------------

/// What each leg waits on, aligned with `legs`: its `after:`, or the entry
/// above, or nothing for the first.
pub(crate) fn waits(legs: &[Leg]) -> Vec<Vec<String>> {
    legs.iter()
        .enumerate()
        .map(|(i, l)| match &l.after {
            Some(a) => a.clone(),
            None if i == 0 => Vec::new(),
            None => vec![legs[i - 1].name.clone()],
        })
        .collect()
}

fn edges(legs: &[Leg]) -> BTreeMap<String, Vec<String>> {
    legs.iter().map(|l| l.name.clone()).zip(waits(legs)).collect()
}

/// Every leg `name` waits on, by any path. Terminates on a cycle.
pub(crate) fn upstream(legs: &[Leg], name: &str) -> BTreeSet<String> {
    let edges = edges(legs);
    let mut out = BTreeSet::new();
    let mut todo: Vec<&str> = edges.get(name).map(|v| v.iter().map(String::as_str).collect()).unwrap_or_default();
    while let Some(n) = todo.pop() {
        if out.insert(n.to_string()) {
            todo.extend(edges.get(n).into_iter().flatten().map(String::as_str));
        }
    }
    out
}

/// Every leg that waits on `name`, by any path.
pub(crate) fn downstream(legs: &[Leg], name: &str) -> BTreeSet<String> {
    legs.iter().map(|l| l.name.as_str()).filter(|n| upstream(legs, n).contains(name)).map(str::to_string).collect()
}

/// The legs nothing waits on. A record ends in exactly one.
pub(crate) fn lasts(legs: &[Leg]) -> Vec<String> {
    let waited: BTreeSet<String> = waits(legs).into_iter().flatten().collect();
    legs.iter().map(|l| l.name.clone()).filter(|n| !waited.contains(n)).collect()
}

/// The legs a run may open now: pending, with every leg upstream of them, by any
/// path, `done` or `skipped`.
///
/// Not only the legs they name — see the module docs, third bullet.
pub(crate) fn ready(legs: &[Leg]) -> Vec<String> {
    let state: BTreeMap<&str, State> = legs.iter().map(|l| (l.name.as_str(), l.state)).collect();
    legs.iter()
        .filter(|l| l.state == State::Pending)
        .filter(|l| upstream(legs, &l.name).iter().all(|u| state.get(u.as_str()).is_some_and(|s| s.settled())))
        .map(|l| l.name.clone())
        .collect()
}

/// What `wip` reads of a leg standing in a seat (`docs/legs.md`, *Seen*).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Standing {
    pub task: String,
    pub leg: Leg,
    /// What it waits on, as [`waits`] reads it.
    pub after: Vec<String>,
    /// Record order, counting from 1.
    pub index: usize,
    /// How many legs the row has.
    pub of: usize,
    pub running: usize,
    pub done: usize,
}

/// The legs whose seat is still up, by seat: `running`, or `done` and not yet
/// ended by the tick. A leg holds no claim, so this is where `wip` finds it, as
/// [`crate::verification::seated`] finds a verifier.
pub(crate) fn seated(tasks: &[Task]) -> BTreeMap<String, Standing> {
    let mut out = BTreeMap::new();
    for t in tasks.iter().filter(|t| t.body.contains(&format!("## {SECTION}"))) {
        let ls = legs(t);
        let w = waits(&ls);
        let running = ls.iter().filter(|l| l.state == State::Running).count();
        let done = ls.iter().filter(|l| l.state == State::Done).count();
        for (i, (l, after)) in ls.iter().zip(w).enumerate() {
            let up = matches!(l.state, State::Running | State::Done) && l.ending == Ending::Standing;
            if let Some(pane) = l.pane.clone().filter(|_| up) {
                out.insert(
                    pane,
                    Standing { task: t.id.clone(), leg: l.clone(), after, index: i + 1, of: ls.len(), running, done },
                );
            }
        }
    }
    out
}

/// The seat's label and the `task` column's text: `wsp-287 · build-store`, with no
/// position in the list — in a graph it does not say how far along the row is.
pub(crate) fn seat_label(task: &str, leg: &str) -> String {
    format!("{task} · {leg}")
}

/// `legs 2 running, 2 done of 5`: the row's own line.
pub(crate) fn tally(s: &Standing) -> String {
    format!("legs {} running, {} done of {}", s.running, s.done, s.of)
}

/// The newest leg caption written, as `scout: three layers, plan next`. Newest
/// by when the leg closed; with siblings, whichever finished last. Nothing is
/// derived: a leg with no caption says nothing.
pub(crate) fn newest_caption(t: &Task) -> Option<String> {
    let ls = legs(t);
    let l = ls
        .iter()
        .enumerate()
        .filter(|(_, l)| l.closed.is_some() && l.caption.as_deref().is_some_and(|c| !c.trim().is_empty()))
        .max_by_key(|(i, l)| (l.closed.clone(), *i))
        .map(|(_, l)| l)?;
    Some(format!("{}: {}", l.name, crate::cmd_task::fold(l.caption.as_deref()?)))
}

/// The path a cycle goes round, first leg repeated at the end, in the arrow form
/// `wsp after` refuses with: `plan → build-store → plan`.
fn cycle(legs: &[Leg]) -> Option<Vec<String>> {
    let edges = edges(legs);
    fn walk(edges: &BTreeMap<String, Vec<String>>, at: &str, path: &mut Vec<String>, done: &mut BTreeSet<String>) -> Option<Vec<String>> {
        if let Some(i) = path.iter().position(|p| p == at) {
            let mut c = path[i..].to_vec();
            c.push(at.to_string());
            return Some(c);
        }
        if done.contains(at) {
            return None;
        }
        path.push(at.to_string());
        for next in edges.get(at).into_iter().flatten() {
            if let Some(c) = walk(edges, next, path, done) {
                return Some(c);
            }
        }
        path.pop();
        done.insert(at.to_string());
        None
    }
    let mut done = BTreeSet::new();
    legs.iter().find_map(|l| walk(&edges, &l.name, &mut Vec::new(), &mut done))
}

/// The most characters a baton from `name` may be, and the join that set it when
/// it is under the 3,000 a lone baton gets: the batons arriving at one leg are
/// 6,000 together, so each is `min(3,000, 6,000 / n)` for the widest `n` of any
/// leg waiting on it.
pub(crate) fn baton_cap(legs: &[Leg], name: &str) -> (usize, Option<(String, usize)>) {
    let widest = legs
        .iter()
        .zip(waits(legs))
        .filter(|(_, w)| w.iter().any(|x| x == name))
        .map(|(l, w)| (l.name.clone(), w.len()))
        .max_by_key(|(_, n)| *n);
    match widest {
        Some((join, n)) if JOIN_CAP / n < TEXT_CAP => (JOIN_CAP / n, Some((join, n))),
        _ => (TEXT_CAP, None),
    }
}

/// Refuse a baton over its cap, saying which join set it. Never truncated.
pub(crate) fn check_baton(legs: &[Leg], name: &str, text: &str) -> Result<(), String> {
    let (cap, join) = baton_cap(legs, name);
    let n = text.trim().chars().count();
    if n <= cap {
        return Ok(());
    }
    let why = match join {
        Some((j, w)) => format!("{j} waits on {w} legs and their batons are {JOIN_CAP} together"),
        None => "a baton is pointers, not a plan".to_string(),
    };
    Err(format!("the baton from {name} is {n} characters and its cap is {cap} — {why}"))
}

// ---- the rules ----------------------------------------------------------------

fn name_ok(n: &str) -> bool {
    // No `.`: a leg's tree is `<task>.<leg>`, and a task id carries none. No `,`
    // or space: `after:` is a comma list.
    !n.is_empty()
        && n.chars().count() <= NAME_CAP
        && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && n != "-"
}

/// What a record must satisfy, as the first thing that is wrong with it.
pub(crate) fn check(legs: &[Leg]) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for l in legs {
        if !name_ok(&l.name) {
            return Err(format!(
                "`{}` is not a leg name — letters, digits, `-` and `_`, at most {NAME_CAP}",
                l.name
            ));
        }
        if !seen.insert(l.name.as_str()) {
            return Err(format!("two legs are called {}", l.name));
        }
    }
    if legs.len() > MAX_LEGS {
        return Err(format!("{} legs is over the {MAX_LEGS} a row carries, rework included — split the row", legs.len()));
    }
    for (l, w) in legs.iter().zip(waits(legs)) {
        for on in &w {
            if on == &l.name {
                return Err(format!("{} cannot wait on itself", l.name));
            }
            if !seen.contains(on.as_str()) {
                return Err(format!("{} waits on `{on}`, and no leg is called that", l.name));
            }
        }
        let over = |what: &str, text: &str, cap: usize| {
            let n = text.trim().chars().count();
            (n > cap).then(|| format!("the {what} of {} is {n} characters, over the {cap} cap", l.name))
        };
        if let Some(e) = over("order", &l.order, TEXT_CAP)
            .or_else(|| over("why", l.why.as_deref().unwrap_or_default(), WHY_CAP))
            .or_else(|| over("skip reason", l.skipped.as_deref().unwrap_or_default(), WHY_CAP))
        {
            return Err(e);
        }
        check_baton(legs, &l.name, &l.baton)?;
        match (is_default(&l.policy), l.why.is_some()) {
            (false, false) => {
                return Err(format!(
                    "{} is on {}, not the default {DEFAULT_AGENT}, and gives no reason — `--why \"…\"` says what \
                     in that leg's own work needs it, or leave --agent off",
                    l.name,
                    policy_line(&l.policy)
                ))
            }
            (true, true) => {
                return Err(format!(
                    "{} is on the default {DEFAULT_AGENT} and has a `why:`, which would be a reason for nothing — \
                     drop it, or give the leg the tier the reason is for",
                    l.name
                ))
            }
            _ => {}
        }
    }
    if let Some(c) = cycle(legs) {
        return Err(format!("the legs make a cycle — {}", c.join(" → ")));
    }
    let ends = lasts(legs);
    if ends.len() > 1 {
        return Err(format!(
            "{} each have nothing waiting on them, and a record ends in one last leg — \
             `--before` one of them, or `wsp leg after` one onto another",
            ends.join(" and ")
        ));
    }
    Ok(())
}

/// [`check`] for a write: refused only if the record it leaves is wrong in a way
/// the record it started from was not.
fn judged(old: &[Leg], new: &[Leg]) -> Result<(), String> {
    match check(new) {
        Ok(()) => Ok(()),
        Err(e) if check(old).err().as_deref() == Some(e.as_str()) => Ok(()),
        Err(e) => Err(e),
    }
}

/// The leg this pane is the seat of, if it is running one: the actor a write is
/// judged as. A pane that is no leg's — a person, the governor — may change any
/// pending leg.
pub(crate) fn actor_of(legs: &[Leg], pane: Option<&str>) -> Option<String> {
    let pane = pane?;
    legs.iter().find(|l| l.state == State::Running && l.pane.as_deref() == Some(pane)).map(|l| l.name.clone())
}

/// A seated leg may change only pending legs downstream of itself, and point
/// them only at itself or legs downstream of it.
fn authorize(old: &[Leg], actor: Option<&str>, changing: &[&str], pointing_at: &[String]) -> Result<(), String> {
    let Some(me) = actor else { return Ok(()) };
    let mine = downstream(old, me);
    for c in changing {
        if !mine.contains(*c) {
            return Err(format!("{me} may change only legs downstream of itself, and {c} is not one of them"));
        }
    }
    for p in pointing_at {
        if p != me && !mine.contains(p) {
            return Err(format!("{me} may point a leg only at itself or a leg downstream of it, and {p} is neither"));
        }
    }
    Ok(())
}

fn pending<'a>(legs: &'a mut [Leg], name: &str) -> Result<&'a mut Leg, String> {
    match legs.iter_mut().find(|l| l.name == name) {
        None => Err(format!("no leg called {name}")),
        Some(l) if l.state != State::Pending => {
            Err(format!("{name} is {}, and only a pending leg can be changed", l.state.as_str()))
        }
        Some(l) => Ok(l),
    }
}

/// A leg to add.
#[derive(Clone, Debug, Default)]
pub(crate) struct New {
    pub name: String,
    pub agent: Option<String>,
    pub why: Option<String>,
    pub order: String,
    pub after: Option<Vec<String>>,
    pub before: Option<String>,
}

/// The record `new` leaves when added, or why it may not be.
pub(crate) fn add(old: &[Leg], new: New, actor: Option<&str>) -> Result<Vec<Leg>, String> {
    let mut legs = old.to_vec();
    let policy = match new.agent.as_deref() {
        None => default_policy(),
        Some(a) => Policy::parse(a).ok_or_else(|| format!("`{a}` is not a policy — a kind, then a model and an effort, as `claude opus high`"))?,
    };
    if new.order.trim().is_empty() {
        return Err("a leg needs an order: what it is to do".into());
    }
    if let Some(before) = &new.before {
        pending(&mut legs, before).map_err(|e| format!("--before: {e}"))?;
    }
    let all = waits(old);
    let waits_of = |name: &str| old.iter().position(|l| l.name == name).map(|i| all[i].clone());
    let after = match (&new.after, &new.before) {
        (Some(a), _) => a.clone(),
        (None, Some(b)) => waits_of(b).unwrap_or_default(),
        (None, None) => match lasts(old).as_slice() {
            [] => Vec::new(),
            [one] => vec![one.clone()],
            many => return Err(format!("{} each end the record, so there is no one last leg to follow — say --after", many.join(" and "))),
        },
    };
    let mut changing: Vec<&str> = Vec::new();
    if let Some(b) = &new.before {
        changing.push(b);
    }
    authorize(old, actor, &changing, &after)?;

    if let Some(b) = &new.before {
        let mut w = waits_of(b).unwrap_or_default();
        w.push(new.name.clone());
        pending(&mut legs, b)?.after = Some(w);
    }
    let mut leg = Leg::pending(&new.name, &new.order);
    leg.policy = policy;
    leg.why = new.why.map(|w| w.trim().to_string()).filter(|w| !w.is_empty());
    // Written only when it differs from the entry above, so a chain stays a
    // chain with no `after:` lines.
    let implicit: Vec<String> = old.last().map(|l| vec![l.name.clone()]).unwrap_or_default();
    leg.after = (after != implicit).then_some(after);
    legs.push(leg);
    judged(old, &legs)?;
    Ok(legs)
}

/// The record with `name` skipped, for a reason.
pub(crate) fn skip(old: &[Leg], name: &str, why: &str, actor: Option<&str>) -> Result<Vec<Leg>, String> {
    let why = why.trim();
    if why.is_empty() {
        return Err("a skip says why".into());
    }
    let mut legs = old.to_vec();
    pending(&mut legs, name)?;
    authorize(old, actor, &[name], &[])?;
    let leg = pending(&mut legs, name)?;
    leg.state = State::Skipped;
    leg.skipped = Some(why.to_string());
    judged(old, &legs)?;
    Ok(legs)
}

/// The record with a pending leg's `after:` replaced. An empty `on` is `after: -`.
pub(crate) fn set_after(old: &[Leg], name: &str, on: Vec<String>, actor: Option<&str>) -> Result<Vec<Leg>, String> {
    let mut legs = old.to_vec();
    pending(&mut legs, name)?;
    // Only the waits this adds are "pointed at": a sibling the leg already waits
    // on may stay, or two siblings could never both re-point the last leg.
    let held = waits(old).into_iter().zip(old).find(|(_, l)| l.name == name).map(|(w, _)| w).unwrap_or_default();
    let added: Vec<String> = on.iter().filter(|o| !held.contains(o)).cloned().collect();
    authorize(old, actor, &[name], &added)?;
    let mut on_deduped: Vec<String> = Vec::new();
    for o in on {
        if !on_deduped.contains(&o) {
            on_deduped.push(o);
        }
    }
    pending(&mut legs, name)?.after = Some(on_deduped);
    judged(old, &legs)?;
    Ok(legs)
}

// ---- a leg's seat (legs-seat) ----------------------------------------------------

fn find<'a>(legs: &'a mut [Leg], name: &str) -> Result<&'a mut Leg, String> {
    legs.iter_mut().find(|l| l.name == name).ok_or_else(|| format!("no leg called {name}"))
}

/// The record with `name` opened: `running`, stamped, no seat yet. Idempotent for
/// a leg already running with no seat, which is the key a retried spawn finds.
pub(crate) fn open(old: &[Leg], name: &str, at: &str) -> Result<Vec<Leg>, String> {
    let mut legs = old.to_vec();
    let leg = find(&mut legs, name)?;
    match leg.state {
        State::Running if leg.pane.is_none() => return Ok(legs),
        State::Pending => {}
        other => return Err(format!("{name} is {}, and only a pending leg can be opened", other.as_str())),
    }
    if !ready(old).iter().any(|r| r == name) {
        let open: Vec<String> = upstream(old, name)
            .into_iter()
            .filter(|u| old.iter().any(|l| &l.name == u && !l.state.settled()))
            .collect();
        return Err(format!("{name} is not ready: still open upstream of it — {}", open.join(", ")));
    }
    let leg = find(&mut legs, name)?;
    leg.state = State::Running;
    leg.started = Some(at.to_string());
    Ok(legs)
}

/// An opening taken back: a spawn that never got a seat leaves the leg pending.
pub(crate) fn unopen(old: &[Leg], name: &str) -> Vec<Leg> {
    let mut legs = old.to_vec();
    if let Ok(l) = find(&mut legs, name) {
        if l.state == State::Running && l.pane.is_none() {
            l.state = State::Pending;
            l.started = None;
            // Made for this opening (`leg_tree::record`); the next one makes its own.
            l.tree = None;
            l.join = None;
        }
    }
    legs
}

/// The seat written onto the entry, as a verifier's is onto its pass.
pub(crate) fn attach(old: &[Leg], name: &str, seat: &str) -> Result<Vec<Leg>, String> {
    let mut legs = old.to_vec();
    let leg = find(&mut legs, name)?;
    if leg.state != State::Running {
        return Err(format!("{name} is {}, not running", leg.state.as_str()));
    }
    leg.pane = Some(seat.to_string());
    Ok(legs)
}

/// The record with a running leg `done`, its baton and caption written. The baton
/// is held to the cap the record sets for it.
pub(crate) fn finish(old: &[Leg], name: &str, baton: &str, caption: &str, at: &str) -> Result<Vec<Leg>, String> {
    let caption = caption.trim();
    if caption.is_empty() {
        return Err("a leg's end takes a caption: the line a glance reads, --caption \"…\"".into());
    }
    let mut legs = old.to_vec();
    let leg = find(&mut legs, name)?;
    if leg.state != State::Running {
        return Err(format!("{name} is {}, and only a running leg can be ended", leg.state.as_str()));
    }
    leg.state = State::Done;
    leg.baton = baton.trim().to_string();
    leg.caption = Some(caption.to_string());
    leg.closed = Some(at.to_string());
    judged(old, &legs)?;
    Ok(legs)
}

/// What `review` from a leg does to the record: refused while another leg is
/// running, otherwise this leg is done and every pending leg is skipped.
pub(crate) fn end_by_review(old: &[Leg], name: &str, account: &str, at: &str) -> Result<Vec<Leg>, String> {
    if let Some(other) = old.iter().find(|l| l.name != name && l.state == State::Running) {
        return Err(format!(
            "{} is still running — the row is not finished while work on it is open. \
             `wsp leg done` ends your leg; `review` is typed by the last leg, once nothing else runs",
            other.name
        ));
    }
    let mut legs = old.to_vec();
    let leg = find(&mut legs, name)?;
    if leg.state == State::Running {
        leg.state = State::Done;
        leg.closed = Some(at.to_string());
        leg.caption.get_or_insert_with(|| util::truncate(&crate::cmd_task::fold(account), 100));
    }
    for l in legs.iter_mut().filter(|l| l.state == State::Pending) {
        l.state = State::Skipped;
        l.skipped = Some("ended by review".to_string());
    }
    Ok(legs)
}

// ---- the run ---------------------------------------------------------------------

/// How a leg is named where an agent is counted or a start is held: `wsp-287 · build-store`,
/// as its seat's label is.
pub(crate) fn label(task: &str, leg: &str) -> String {
    format!("{task} · {leg}")
}

/// Whether a row is run as legs: it has the section. The test the run's repairs
/// leave a row alone by (`wedged`, `sent_back_unseated`), and not "has a running
/// leg" — a staged row with no claim is the ordinary state of one.
pub(crate) fn staged(t: &Task) -> bool {
    t.section(SECTION).is_some_and(|s| s.lines().any(|l| l.starts_with("- ")))
}

/// The legs the run may start now, in record order: those ready, and those
/// opened with no seat long enough ago that the start never arrived.
pub(crate) fn startable(legs: &[Leg], lost: &dyn Fn(&str) -> bool) -> Vec<(String, bool)> {
    let ready = ready(legs);
    legs.iter()
        .filter_map(|l| match l.state {
            State::Pending if ready.contains(&l.name) => Some((l.name.clone(), false)),
            State::Running if l.pane.is_none() && l.started.as_deref().is_some_and(lost) => Some((l.name.clone(), true)),
            _ => None,
        })
        .collect()
}

/// Whether a word of a verdict is the leg's name: the name with nothing that
/// could continue it on either side.
fn names(text: &str, name: &str) -> bool {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    text.match_indices(name).any(|(i, m)| {
        !text[..i].chars().next_back().is_some_and(word) && !text[i + m.len()..].chars().next().is_some_and(word)
    })
}

/// How dear a policy is, to take the dearest of several: the model by name, then
/// the effort. A model the table does not know is not cheap.
fn dearness(p: &Policy) -> (u8, u8) {
    let m = p.model.as_deref().unwrap_or_default();
    let model = match () {
        _ if m.contains("opus") => 3,
        _ if m.contains("haiku") => 1,
        _ => 2,
    };
    let effort = match p.effort.as_deref() {
        Some("low") => 1,
        Some("high") => 3,
        Some("xhigh") => 4,
        Some("max") => 5,
        _ => 2,
    };
    (model, effort)
}

/// Whether the record ends in a rework leg that has not finished: a blocked row
/// that has a leg to go back to. False at the leg cap, where none could be added.
pub(crate) fn rework_open(legs: &[Leg]) -> bool {
    legs.last().is_some_and(|l| l.rework.is_some() && matches!(l.state, State::Pending | State::Running))
}

/// The record a blocking verdict leaves: one `rework` leg appended, waiting on
/// the last leg and becoming it, whose order is the verdict.
///
/// It runs on the default tier, unless a leg the verdict names was on another:
/// then it takes the dearest of those tiers, and that leg's `why:` prefixed
/// `rework of <leg>:`. A verdict that names no leg gives no reason for anything
/// but the default. `Ok(None)` when a rework is already outstanding at the end of
/// the record — a verdict recorded twice is one send-back, not two.
pub(crate) fn rework(old: &[Leg], verdict: &str) -> Result<Option<Vec<Leg>>, String> {
    if rework_open(old) {
        return Ok(None);
    }
    if old.len() >= MAX_LEGS {
        return Err(format!(
            "{} legs is the {MAX_LEGS} a row carries, rework included — no rework leg was added, and the row's governor decides",
            old.len()
        ));
    }
    let named: Vec<&Leg> = old.iter().filter(|l| names(verdict, &l.name)).collect();
    let dearest = named.iter().filter(|l| !is_default(&l.policy)).max_by_key(|l| dearness(&l.policy));
    let name = (1..).map(|n| if n == 1 { "rework".to_string() } else { format!("rework-{n}") })
        .find(|n| !old.iter().any(|l| &l.name == n))
        .unwrap_or_default();
    let verdict = verdict.trim();
    let head = "The verifier blocked this row. Its verdict, which is what is owed:\n\n";
    let mut leg = Leg::pending(&name, &format!("{head}{}", util::truncate(verdict, TEXT_CAP - head.chars().count())));
    if let Some(d) = dearest {
        leg.policy = d.policy.clone();
        leg.why = Some(format!("rework of {}: {}", d.name, d.why.as_deref().unwrap_or_default()));
    }
    leg.rework = Some(named.iter().map(|l| l.name.clone()).collect());
    let after = lasts(old);
    let implicit: Vec<String> = old.last().map(|l| vec![l.name.clone()]).unwrap_or_default();
    leg.after = (after != implicit).then_some(after);
    let mut legs = old.to_vec();
    legs.push(leg);
    judged(old, &legs)?;
    Ok(Some(legs))
}

// ---- the record, read ----------------------------------------------------------

/// The legs as `wsp show --json` gives them.
pub(crate) fn json(t: &Task) -> serde_json::Value {
    let ls = legs(t);
    let w = waits(&ls);
    let ready = ready(&ls);
    serde_json::json!(ls
        .iter()
        .zip(w)
        .map(|(l, w)| serde_json::json!({
            "name": l.name,
            "agent": policy_line(&l.policy),
            "default": is_default(&l.policy),
            "why": l.why,
            "state": l.state.as_str(),
            "ready": ready.contains(&l.name),
            "after": w,
            "pane": l.pane,
            "tree": l.tree,
            "caption": l.caption,
            "skipped": l.skipped,
            "started": l.started,
            "ended": l.closed,
            "order": l.order,
            "baton": l.baton,
            "rework_of": l.rework,
        }))
        .collect::<Vec<_>>())
}

/// One line a leg, for `wsp show` and `wsp leg show`: every tier printed, the
/// default's too, and what each waits on spelled out.
pub(crate) fn table(legs: &[Leg]) -> String {
    let w = waits(legs);
    let ready = ready(legs);
    let mut out = Vec::new();
    for (l, w) in legs.iter().zip(w) {
        let state = if ready.contains(&l.name) { "ready" } else { l.state.as_str() };
        let after = if w.is_empty() { "-".to_string() } else { w.join(", ") };
        let mut line = format!("- {} · {} · {state} · after {after}", l.name, policy_line(&l.policy));
        if let Some(why) = &l.why {
            line.push_str(&format!(" · why {}", util::truncate(&crate::cmd_task::fold(why), 80)));
        }
        if let Some(c) = l.caption.as_deref().or(l.skipped.as_deref()) {
            line.push_str(&format!(" · {}", util::truncate(&crate::cmd_task::fold(c), 80)));
        }
        out.push(line);
    }
    out.join("\n")
}

/// `wsp show`'s `## Legs`, with the tier and the waits printed in full.
pub(crate) fn section_for_show(t: &Task) -> Option<String> {
    let ls = legs(t);
    (!ls.is_empty()).then(|| table(&ls))
}

// ---- the verbs ------------------------------------------------------------------

const USAGE: &str = "usage:
  wsp leg add <task> <name> --order FILE|- [--agent \"claude opus high\" --why \"…\"] [--after <leg>,…|--after=-] [--before <leg>]
  wsp leg after <task> <name> <leg>…|-     replace a pending leg's waits
  wsp leg skip <task> <name> \"why\"         a pending leg, never run
  wsp leg show <task> [<name>]             the table, or one leg whole
  wsp leg done <task> --from FILE|- --caption \"…\"   a leg's seat ends it, at a commit";

pub fn dispatch(store: &Store, args: &Args) -> i32 {
    let me = crate::cmd_agent::my_pane();
    run(store, args, me.as_deref())
}

/// [`dispatch`] with the pane given, so a test is not asked where it is standing.
pub(crate) fn run(store: &Store, args: &Args, pane: Option<&str>) -> i32 {
    match args.rest.first().map(String::as_str) {
        Some("add") => cmd_add(store, args, pane),
        Some("after") => cmd_after(store, args, pane),
        Some("skip") => cmd_skip(store, args, pane),
        Some("done") => crate::leg_seat::cmd_done(store, args, pane),
        Some("show") | Some("ls") | None => cmd_show(store, args),
        Some(other) => {
            eprintln!("wsp leg: unknown subcommand `{other}`\n{USAGE}");
            2
        }
    }
}

/// Apply one edit to a row's legs under the store lock, so two seats writing at
/// once each see the other's result (`docs/legs.md`, *Who may change it*).
fn edit(
    store: &Store,
    needle: &str,
    pane: Option<&str>,
    f: impl FnOnce(&[Leg], Option<&str>) -> Result<Vec<Leg>, String>,
) -> Result<(Task, Vec<Leg>), String> {
    let id = store.task_or_why(needle)?.id;
    store.locked(|| {
        let mut t = store.find_task(&id).ok_or_else(|| format!("no task {id}"))?;
        let old = legs(&t);
        let actor = actor_of(&old, pane);
        let new = f(&old, actor.as_deref())?;
        write(&mut t, &new);
        store.save_task(&t).map_err(|e| format!("write failed: {e}"))?;
        Ok((t, new))
    })
}

fn refuse(why: String) -> i32 {
    eprintln!("wsp: {}", why.trim_start_matches("wsp: "));
    1
}

/// The order: `--order FILE`, or `-` for stdin. `-` after a long flag is a
/// positional (a `-`-led word is never a value), so the flag reads as bare.
fn order_text(args: &Args) -> Result<String, String> {
    let src = match args.get("order") {
        Some(v) if v == "true" && args.rest.iter().skip(3).any(|r| r == "-") => "-".to_string(),
        Some(v) if v != "true" => v,
        _ => return Err("a leg needs an order: --order FILE, or --order - to read it from stdin".into()),
    };
    if src == "-" && util::stdin_is_tty() {
        return Err("nothing is piped in — `--order -` reads the order from a stream".into());
    }
    crate::cmd_task::read_source(&src).map_err(|e| format!("cannot read {src}: {e}"))
}

fn cmd_add(store: &Store, args: &Args, pane: Option<&str>) -> i32 {
    let (Some(task), Some(name)) = (args.rest.get(1), args.rest.get(2)) else {
        eprintln!("{USAGE}");
        return 2;
    };
    let order = match order_text(args) {
        Ok(o) => o,
        Err(why) => return refuse(why),
    };
    let after = args.get("after").map(|_| args.all("after").into_iter().filter(|a| a != "-").collect::<Vec<_>>());
    let new = New {
        name: name.clone(),
        agent: args.get("agent"),
        why: args.get("why"),
        order,
        after,
        before: args.get("before"),
    };
    let receipt = format!(
        "{name}{}{}",
        new.after.as_ref().map(|a| format!(" · after {}", if a.is_empty() { "-".into() } else { a.join(", ") })).unwrap_or_default(),
        new.before.as_ref().map(|b| format!(" · before {b}")).unwrap_or_default()
    );
    match edit(store, task, pane, |old, actor| add(old, new, actor)) {
        Ok((t, legs)) => {
            store.log_event("leg-added", serde_json::json!({ "id": t.id, "leg": name }));
            store.git_commit(&format!("wsp: {} leg {name} added", t.id));
            println!("{} {}  leg {receipt}  ({} of {MAX_LEGS})", Paint::new().green("✓"), t.id, legs.len());
            0
        }
        Err(why) => refuse(why),
    }
}

fn cmd_after(store: &Store, args: &Args, pane: Option<&str>) -> i32 {
    let (Some(task), Some(name)) = (args.rest.get(1), args.rest.get(2)) else {
        eprintln!("{USAGE}");
        return 2;
    };
    let on: Vec<String> =
        args.rest.iter().skip(3).flat_map(|s| s.split(',')).map(str::trim).filter(|s| !s.is_empty() && *s != "-").map(str::to_string).collect();
    if args.rest.len() < 4 {
        eprintln!("{USAGE}");
        return 2;
    }
    match edit(store, task, pane, |old, actor| set_after(old, name, on, actor)) {
        Ok((t, legs)) => {
            store.git_commit(&format!("wsp: {} leg {name} re-pointed", t.id));
            let w = waits(&legs);
            let at = legs.iter().position(|l| &l.name == name).unwrap_or(0);
            println!("{} {}  leg {name} · after {}", Paint::new().green("✓"), t.id, if w[at].is_empty() { "-".into() } else { w[at].join(", ") });
            0
        }
        Err(why) => refuse(why),
    }
}

fn cmd_skip(store: &Store, args: &Args, pane: Option<&str>) -> i32 {
    let (Some(task), Some(name)) = (args.rest.get(1), args.rest.get(2)) else {
        eprintln!("{USAGE}");
        return 2;
    };
    let why = args.text(3);
    match edit(store, task, pane, |old, actor| skip(old, name, &why, actor)) {
        Ok((t, _)) => {
            store.git_commit(&format!("wsp: {} leg {name} skipped", t.id));
            println!("{} {}  leg {name} skipped", Paint::new().green("✓"), t.id);
            0
        }
        Err(why) => refuse(why),
    }
}

fn cmd_show(store: &Store, args: &Args) -> i32 {
    let Some(task) = args.rest.get(1) else {
        eprintln!("{USAGE}");
        return 2;
    };
    let t = match store.task_or_why(task) {
        Ok(t) => t,
        Err(why) => return refuse(why),
    };
    let ls = legs(&t);
    if args.json() {
        println!("{}", serde_json::to_string_pretty(&json(&t)).unwrap_or_default());
        return 0;
    }
    if ls.is_empty() {
        println!("{} has no legs", t.id);
        return 0;
    }
    let Some(name) = args.rest.get(2) else {
        println!("{}", table(&ls));
        return 0;
    };
    let Some(leg) = ls.iter().find(|l| &l.name == name) else {
        return refuse(format!("{} has no leg called {name}", t.id));
    };
    println!("{}", leg.lines());
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leg(name: &str) -> Leg {
        Leg::pending(name, &format!("do {name}"))
    }

    fn with(name: &str, after: &[&str]) -> Leg {
        Leg { after: Some(after.iter().map(|s| s.to_string()).collect()), ..leg(name) }
    }

    fn opus(name: &str) -> Leg {
        Leg { policy: Policy::parse("claude opus high").unwrap(), why: Some("decides which layer owns it".into()), ..leg(name) }
    }

    fn names(v: &[String]) -> Vec<&str> {
        v.iter().map(String::as_str).collect()
    }

    fn row(body: &str) -> Task {
        let mut t = Task::new("a row", "m-1");
        t.body = format!("## Overview\nthe work\n\n{body}\n## Log\n- 2026-10-18 claimed\n");
        t
    }

    /// Every field of an entry is read back as written, a multi-line order keeps
    /// its lines and blank ones, and a line of the order that looks like a key
    /// stays text.
    #[test]
    fn an_entry_reads_back_as_it_was_written() {
        let mut t = row("");
        let full = Leg {
            name: "plan".into(),
            policy: Policy::parse("claude opus high").unwrap(),
            state: State::Running,
            pane: Some("cpd-34".into()),
            tree: Some("m-1.plan".into()),
            ending: Ending::Failed("still standing".into()),
            after: Some(vec!["scout".into(), "probe".into()]),
            why: Some("decides which layer owns it".into()),
            order: "Read the call sites.\n\nafter: this is text\n  order: so is this\n- and so is a dash".into(),
            baton: "three layers: store, spawn".into(),
            caption: Some("three layers · one owner".into()),
            skipped: None,
            started: Some("2026-10-18T10:00:00Z".into()),
            closed: Some("2026-10-18T10:30:00Z".into()),
            join: Some("merged a (m-1.a) into the row's branch; it is at abc123".into()),
            rework: Some(vec!["scout".into(), "probe".into()]),
        };
        let bare_rework = Leg { rework: Some(Vec::new()), ..with("redo", &["plan"]) };
        let skipped = Leg { skipped: Some("the scout found nothing".into()), state: State::Skipped, ..with("probe", &[]) };
        write(&mut t, &[leg("scout"), skipped.clone(), full.clone(), bare_rework.clone()]);
        assert_eq!(legs(&t), vec![leg("scout"), skipped, full, bare_rework], "{}", t.body);
        assert!(t.body.find("## Legs").unwrap() < t.body.find("## Log").unwrap(), "and the log stays last");
    }

    /// And through the store, whose file parser has its own opinions about blank
    /// lines and headings.
    #[test]
    fn the_record_survives_the_store() {
        let env = util::isolated("legs-store");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let mut t = Task::new("a row", "m-1");
        t.body = "## Overview\nthe work\n\n## Log\n- 2026-10-18 claimed\n".into();
        let l = Leg { order: "one\n\n## two\n\nthree".into(), ..leg("scout") };
        write(&mut t, &[l.clone()]);
        store.save_task(&t).unwrap();
        assert_eq!(legs(&store.find_task("m-1").unwrap()), vec![l]);
    }

    /// A d1 record: no `after:` anywhere, and no policy on one entry. Each leg
    /// waits on the entry above, the first on nothing, and the entry with no
    /// policy reads as the default.
    #[test]
    fn a_d1_record_with_no_after_reads_as_a_chain() {
        let t = row(
            "## Legs\n- scout · claude sonnet medium · done · pane cpd-31 · ended\n  order: look\n  baton: three layers\n\
             - plan · claude opus high · done\n  why: chooses\n\
             - build · pending\n\
             - finish · claude sonnet medium · pending\n",
        );
        let ls = legs(&t);
        assert_eq!(ls.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["scout", "plan", "build", "finish"]);
        let w = waits(&ls);
        assert_eq!(w, vec![vec![], vec!["scout".to_string()], vec!["plan".to_string()], vec!["build".to_string()]]);
        assert_eq!(lasts(&ls), vec!["finish"]);
        assert!(is_default(&ls[2].policy), "no policy reads as the default");
        assert_eq!(names(&ready(&ls)), ["build"], "and only the next one in the chain is ready");
        assert!(check(&ls).is_ok(), "{:?}", check(&ls));
    }

    #[test]
    fn after_dash_waits_on_nothing_and_a_listed_after_waits_on_what_it_names() {
        let ls = vec![leg("a"), with("b", &[]), with("c", &["a", "b"])];
        assert_eq!(waits(&ls), vec![vec![], vec![], vec!["a".to_string(), "b".to_string()]]);
        assert_eq!(names(&ready(&ls)), ["a", "b"], "two legs with nothing to wait on are both ready");
        let t = row("## Legs\n- a · pending\n- b · pending\n  after: -\n- c · pending\n  after: a, b\n");
        assert_eq!(waits(&legs(&t)), waits(&ls));
    }

    /// A leg is ready when everything upstream of it is done or skipped, and a
    /// skipped leg counts.
    #[test]
    fn a_leg_is_ready_when_what_it_waits_on_is_done_or_skipped() {
        let mut ls = vec![leg("a"), leg("b"), leg("c")];
        assert_eq!(names(&ready(&ls)), ["a"]);
        ls[0].state = State::Running;
        assert!(ready(&ls).is_empty(), "a running leg is not done");
        ls[0].state = State::Done;
        assert_eq!(names(&ready(&ls)), ["b"]);
        ls[1].state = State::Skipped;
        assert_eq!(names(&ready(&ls)), ["c"], "a skipped leg satisfies the wait");
    }

    /// The case wsp-296's verifier found: A → B → J with B skipped. J names only
    /// B, and A is still working in the tree J would merge into. J must not be
    /// ready while A runs — or while A is yet to run.
    #[test]
    fn a_skip_does_not_make_a_leg_ready_while_anything_upstream_of_it_is_unfinished() {
        let mut ls = vec![leg("a"), leg("b"), leg("j")];
        ls[0].state = State::Running;
        ls[1].state = State::Skipped;
        assert!(ready(&ls).is_empty(), "J waits for A, which B's skip does not release");
        ls[0].state = State::Pending;
        assert_eq!(names(&ready(&ls)), ["a"], "and with A pending, only A is ready");
        ls[0].state = State::Done;
        assert_eq!(names(&ready(&ls)), ["j"]);
        // The skip itself is accepted while upstream runs: readiness is the guard.
        let mut ls = vec![leg("a"), leg("b"), leg("j")];
        ls[0].state = State::Running;
        let after = skip(&ls, "b", "not needed", None).unwrap();
        assert_eq!(after[1].state, State::Skipped);
        assert!(ready(&after).is_empty());
    }

    #[test]
    fn an_order_and_a_baton_are_refused_over_three_thousand_and_never_truncated() {
        let big = |n: usize| "x".repeat(n);
        let ok = vec![Leg { order: big(3_000), baton: big(3_000), ..leg("a") }];
        assert!(check(&ok).is_ok());
        let long_order = vec![Leg { order: big(3_001), ..leg("a") }];
        assert!(check(&long_order).unwrap_err().contains("order of a is 3001"), "{:?}", check(&long_order));
        let long_baton = vec![Leg { baton: big(3_001), ..leg("a") }];
        assert!(check(&long_baton).unwrap_err().contains("baton from a is 3001"));
        // Through add, which is what a person types.
        let refused = add(&[], New { name: "a".into(), order: big(3_001), ..Default::default() }, None);
        assert!(refused.unwrap_err().contains("over the 3000 cap"));
    }

    /// min(3,000, 6,000 / n): one waiter or two is 3,000, three is 2,000, four
    /// 1,500, and the widest join that waits on a leg is the one that counts.
    #[test]
    fn a_baton_is_capped_at_the_smaller_of_3000_and_6000_over_the_joins_width() {
        let chain = vec![leg("a"), leg("b")];
        assert_eq!(baton_cap(&chain, "a"), (3_000, None));
        assert_eq!(baton_cap(&chain, "b"), (3_000, None), "the last leg has nothing waiting on it");
        let two = vec![leg("a"), with("b", &["a"]), with("j", &["a", "b"])];
        assert_eq!(baton_cap(&two, "a"), (3_000, None), "6000 / 2 is the 3000 a lone baton gets");
        let three = vec![leg("a"), with("b", &["a"]), with("c", &["a"]), with("j", &["a", "b", "c"])];
        assert_eq!(baton_cap(&three, "b"), (2_000, Some(("j".into(), 3))));
        let four = vec![leg("a"), with("b", &[]), with("c", &[]), with("d", &[]), with("j", &["a", "b", "c", "d"])];
        assert_eq!(baton_cap(&four, "d").0, 1_500);
        // The widest of several waiters.
        let mixed = vec![leg("a"), with("b", &["a"]), with("c", &[]), with("d", &[]), with("j", &["a", "b", "c", "d"])];
        assert_eq!(baton_cap(&mixed, "a").0, 1_500);

        let text = |n: usize| "x".repeat(n);
        assert!(check_baton(&three, "b", &text(2_000)).is_ok());
        let e = check_baton(&three, "b", &text(2_001)).unwrap_err();
        assert!(e.contains("cap is 2000") && e.contains("j waits on 3 legs"), "{e}");
        assert!(check_baton(&chain, "a", &text(3_001)).is_err());
        // And a record whose baton is over its join's cap is refused whole.
        let mut over = three.clone();
        over[1].baton = text(2_001);
        assert!(check(&over).unwrap_err().contains("cap is 2000"));
    }

    #[test]
    fn a_why_is_capped_at_three_hundred() {
        let mut l = opus("a");
        l.why = Some("x".repeat(300));
        assert!(check(&[l.clone()]).is_ok());
        l.why = Some("x".repeat(301));
        assert!(check(&[l]).unwrap_err().contains("why of a is 301"));
    }

    #[test]
    fn an_after_naming_no_leg_or_the_leg_itself_is_refused() {
        let e = check(&[leg("a"), with("b", &["ghost"])]).unwrap_err();
        assert!(e.contains("b waits on `ghost`"), "{e}");
        let e = check(&[leg("a"), with("b", &["a", "b"])]).unwrap_err();
        assert!(e.contains("b cannot wait on itself"), "{e}");
        // Through the verbs.
        let base = vec![leg("a"), leg("b")];
        assert!(add(&base, New { name: "c".into(), order: "x".into(), after: Some(vec!["ghost".into()]), ..Default::default() }, None)
            .unwrap_err()
            .contains("no leg is called that"));
        assert!(set_after(&base, "b", vec!["b".into()], None).unwrap_err().contains("cannot wait on itself"));
    }

    /// The cycle is named in the arrow form `wsp after` uses.
    #[test]
    fn a_cycle_is_refused_and_named() {
        let e = check(&[with("plan", &["build-store"]), with("build-store", &["plan"])]).unwrap_err();
        assert!(e.contains("plan → build-store → plan"), "{e}");
        let base = vec![leg("plan"), leg("build"), leg("finish")];
        let e = set_after(&base, "plan", vec!["finish".into()], None).unwrap_err();
        assert!(e.contains("cycle") && e.contains("plan → finish → build → plan"), "{e}");
    }

    #[test]
    fn a_record_ends_in_one_last_leg() {
        let e = check(&[leg("a"), with("b", &["a"]), with("c", &["a"])]).unwrap_err();
        assert!(e.contains("b and c each have nothing waiting on them"), "{e}");
        assert!(check(&[leg("a"), with("b", &["a"]), with("c", &["a"]), with("d", &["b", "c"])]).is_ok());
        // An add that leaves two ends is refused, and the one that names its join is not.
        let plan = vec![leg("plan"), leg("finish")];
        let e = add(&plan, New { name: "x".into(), order: "o".into(), after: Some(vec!["plan".into()]), ..Default::default() }, None).unwrap_err();
        assert!(e.contains("finish and x each have nothing waiting on them"), "{e}");
        let ok = add(&plan, New { name: "x".into(), order: "o".into(), after: Some(vec!["plan".into()]), before: Some("finish".into()), ..Default::default() }, None);
        assert!(ok.is_ok(), "{ok:?}");
    }

    #[test]
    fn a_ninth_leg_is_refused_and_an_eighth_is_not() {
        let mut ls: Vec<Leg> = (0..7).map(|i| leg(&format!("l{i}"))).collect();
        let next = |ls: &[Leg], n: &str| add(ls, New { name: n.into(), order: "o".into(), ..Default::default() }, None);
        ls = next(&ls, "l7").unwrap();
        assert_eq!(ls.len(), 8);
        let e = next(&ls, "l8").unwrap_err();
        assert!(e.contains("9 legs is over the 8"), "{e}");
    }

    /// Ed, through compound's custodian, 2026-10-18: a leg is Sonnet unless it
    /// says why not, and the member's `agent` line never reaches it.
    #[test]
    fn a_leg_with_no_policy_is_sonnet_whatever_the_members_agent_line_says() {
        let groups = crate::model::parse_groups("- 1  m-1\n  agent: claude opus high\n  agent m-1: claude opus high\n");
        let members = groups[0].policy_for("m-1").unwrap();
        assert_eq!(members.model.as_deref(), Some("opus"), "the member is on Opus unstaged");
        let t = row("## Legs\n- scout · pending\n  order: look\n");
        let l = &legs(&t)[0];
        assert_eq!(policy_line(&l.policy), "claude sonnet medium");
        assert!(is_default(&l.policy) && l.why.is_none());
        // And a leg added with no --agent is written with the tier printed.
        let added = add(&[], New { name: "scout".into(), order: "o".into(), ..Default::default() }, None).unwrap();
        let mut t2 = row("");
        write(&mut t2, &added);
        assert!(t2.body.contains("- scout · claude sonnet medium · pending"), "{}", t2.body);
    }

    #[test]
    fn another_tier_needs_a_why_and_a_why_on_the_default_is_refused() {
        let new = |agent: Option<&str>, why: Option<&str>| New {
            name: "plan".into(),
            agent: agent.map(str::to_string),
            why: why.map(str::to_string),
            order: "o".into(),
            ..Default::default()
        };
        let e = add(&[], new(Some("claude opus high"), None), None).unwrap_err();
        assert!(e.contains("plan is on claude opus high") && e.contains("gives no reason"), "{e}");
        let ok = add(&[], new(Some("claude opus high"), Some("decides which layer owns it")), None).unwrap();
        assert_eq!(ok[0].why.as_deref(), Some("decides which layer owns it"));
        // Another effort, another model and another kind each need one.
        for agent in ["claude sonnet high", "claude sonnet", "claude haiku medium", "opencode opencode/x-free"] {
            assert!(add(&[], new(Some(agent), None), None).is_err(), "{agent}");
        }
        // The default, spelled out, is the default: no why, and a why is refused.
        assert!(add(&[], new(Some("claude sonnet medium"), None), None).is_ok());
        let e = add(&[], new(None, Some("because")), None).unwrap_err();
        assert!(e.contains("reason for nothing"), "{e}");
        let e = add(&[], new(Some("claude sonnet medium"), Some("because")), None).unwrap_err();
        assert!(e.contains("reason for nothing"), "{e}");
        assert!(add(&[], new(Some("manual"), None), None).unwrap_err().contains("not a policy"));
    }

    /// Adding a leg changes no other leg's waits — the d3 doc left open where it
    /// goes, and above an entry with no `after:` would have re-pointed it.
    #[test]
    fn adding_a_leg_leaves_every_other_legs_waits_unchanged() {
        let chain = vec![leg("scout"), leg("plan"), leg("finish")];
        let before = waits(&chain);
        // At the end, as a chain.
        let more = add(&chain, New { name: "tail".into(), order: "o".into(), ..Default::default() }, None).unwrap();
        assert_eq!(&waits(&more)[..3], &before[..]);
        assert_eq!(more.last().unwrap().after, None, "a chain gains no `after:` line");
        // Fanning out: two builds between plan and finish. finish had no
        // `after:` and waited on plan; it now waits on plan and both builds, and
        // every leg before it is as it was.
        let one = add(&chain, New { name: "b1".into(), order: "o".into(), after: Some(vec!["plan".into()]), before: Some("finish".into()), ..Default::default() }, None).unwrap();
        let two = add(&one, New { name: "b2".into(), order: "o".into(), after: Some(vec!["plan".into()]), before: Some("finish".into()), ..Default::default() }, None).unwrap();
        let w = waits(&two);
        assert_eq!(&w[..2], &before[..2], "scout and plan are untouched");
        let at = |n: &str| two.iter().position(|l| l.name == n).unwrap();
        assert_eq!(w[at("finish")], vec!["plan", "b1", "b2"]);
        assert_eq!(w[at("b1")], vec!["plan"]);
        assert_eq!(w[at("b2")], vec!["plan"]);
        assert_eq!(lasts(&two), vec!["finish"]);
        assert_eq!(two.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["scout", "plan", "finish", "b1", "b2"], "appended, never inserted");
        // The doc's two commands, in the order it types them, through the file.
        let mut t = row("");
        write(&mut t, &two);
        assert_eq!(waits(&legs(&t)), w, "and the file reads back to the same waits");
    }

    /// `--before X` alone: the new leg takes X's waits, so it sits in front of X.
    #[test]
    fn before_with_no_after_inserts_the_leg_in_front_of_that_leg() {
        let chain = vec![leg("scout"), leg("plan"), leg("finish")];
        let ls = add(&chain, New { name: "check".into(), order: "o".into(), before: Some("finish".into()), ..Default::default() }, None).unwrap();
        let w = waits(&ls);
        assert_eq!(w[3], vec!["plan"]);
        assert_eq!(w[2], vec!["plan", "check"]);
        assert!(check(&ls).is_ok());
        // Only a pending leg can be put before.
        let mut running = chain.clone();
        running[2].state = State::Running;
        let e = add(&running, New { name: "x".into(), order: "o".into(), before: Some("finish".into()), ..Default::default() }, None).unwrap_err();
        assert!(e.contains("only a pending leg can be changed"), "{e}");
    }

    #[test]
    fn a_leg_may_wait_on_a_done_one_and_is_ready_at_once() {
        let mut ls = vec![leg("a"), leg("b")];
        ls[0].state = State::Done;
        ls[1].state = State::Done;
        let ls = add(&ls, New { name: "rework".into(), order: "o".into(), after: Some(vec!["b".into()]), ..Default::default() }, None).unwrap();
        assert_eq!(names(&ready(&ls)), ["rework"]);
    }

    /// Who may change what.
    #[test]
    fn a_person_changes_any_pending_leg_a_seated_one_only_those_downstream_and_nobody_a_running_or_done_one() {
        // plan → (b1, b2) → finish, with plan running in cpd-34.
        let mut ls = vec![leg("plan"), with("b1", &["plan"]), with("b2", &["plan"]), with("finish", &["b1", "b2"])];
        ls[0].state = State::Running;
        ls[0].pane = Some("cpd-34".into());
        assert_eq!(actor_of(&ls, Some("cpd-34")).as_deref(), Some("plan"));
        assert_eq!(actor_of(&ls, Some("cpd-99")), None, "a pane that is no leg's is a person or the governor");
        assert_eq!(actor_of(&ls, None), None);

        // A person re-points any pending leg.
        assert!(set_after(&ls, "b1", vec!["plan".into(), "b2".into()], None).is_ok());
        // The running leg re-points a leg downstream of itself, at itself.
        assert!(set_after(&ls, "finish", vec!["plan".into(), "b1".into(), "b2".into()], Some("plan")).is_ok());
        // Nobody edits the running leg, and nobody a done one.
        for who in [None, Some("plan")] {
            let e = set_after(&ls, "plan", vec![], who).unwrap_err();
            assert!(e.contains("plan is running"), "{e}");
            assert!(skip(&ls, "plan", "no", who).is_err());
        }
        let mut done = ls.clone();
        done[1].state = State::Done;
        assert!(skip(&done, "b1", "no", None).unwrap_err().contains("b1 is done"));
        assert!(add(&done, New { name: "x".into(), order: "o".into(), before: Some("b1".into()), ..Default::default() }, None).is_err());

        // A seated leg adds legs after itself, before a leg downstream of itself.
        let ok = add(&ls, New { name: "decide".into(), order: "o".into(), agent: Some("claude opus high".into()), why: Some("chooses".into()), after: Some(vec!["plan".into()]), before: Some("finish".into()) }, Some("plan"));
        assert!(ok.is_ok(), "{ok:?}");
    }

    #[test]
    fn a_seated_leg_cannot_touch_a_sibling_or_point_a_leg_at_one() {
        // scout → (b1, b2) → finish, b1 running in cpd-1: b2 is its sibling.
        let mut ls = vec![leg("scout"), with("b1", &["scout"]), with("b2", &["scout"]), with("finish", &["b1", "b2"])];
        ls[0].state = State::Done;
        ls[1].state = State::Running;
        ls[1].pane = Some("cpd-1".into());
        let me = Some("b1");
        let e = skip(&ls, "b2", "mine to drop", me).unwrap_err();
        assert!(e.contains("b1 may change only legs downstream of itself, and b2 is not one"), "{e}");
        assert!(set_after(&ls, "b2", vec!["scout".into()], me).is_err());
        // Both siblings re-pointing the last leg is each changing a leg downstream of itself.
        assert!(set_after(&ls, "finish", vec!["b1".into(), "b2".into()], me).is_ok());
        // A new leg must wait on itself or what is downstream of it.
        let new = |after: &[&str], before: Option<&str>| New {
            name: "x".into(),
            order: "o".into(),
            after: Some(after.iter().map(|s| s.to_string()).collect()),
            before: before.map(str::to_string),
            ..Default::default()
        };
        assert!(add(&ls, new(&["b1"], Some("finish")), me).is_ok());
        let e = add(&ls, new(&["b2"], Some("finish")), me).unwrap_err();
        assert!(e.contains("may point a leg only at itself or a leg downstream of it, and b2 is neither"), "{e}");
        let e = add(&ls, new(&["scout"], Some("finish")), me).unwrap_err();
        assert!(e.contains("scout is neither"), "a done leg upstream is not downstream: {e}");
        // --before names a leg downstream of the seated one too.
        let ls2 = vec![leg("a"), leg("b"), leg("c")];
        let mut ls2 = ls2;
        ls2[0].state = State::Running;
        ls2[0].pane = Some("p".into());
        ls2.push(with("side", &[]));
        let e = add(&ls2, New { name: "n".into(), order: "o".into(), after: Some(vec!["a".into()]), before: Some("side".into()), ..Default::default() }, Some("a")).unwrap_err();
        assert!(e.contains("side is not one of them"), "{e}");
    }

    /// A record wrong before the write, in the way it is wrong after, does not
    /// make an unrelated write refuse; a write that makes a new fault does.
    #[test]
    fn a_write_is_judged_by_what_it_changes() {
        let mut ls = vec![leg("a"), with("b", &["a"]), with("c", &["a"])];
        assert!(check(&ls).is_err(), "two ends, from a hand edit");
        ls = skip(&ls, "c", "not needed", None).unwrap();
        assert_eq!(ls[2].state, State::Skipped);
        assert!(add(&ls, New { name: "d".into(), order: "o".into(), after: Some(vec!["ghost".into()]), ..Default::default() }, None).is_err());
    }

    #[test]
    fn leg_names_are_words() {
        for bad in ["", "a b", "a,b", "a.b", "-", "x".repeat(41).as_str()] {
            assert!(check(&[leg(bad)]).is_err(), "{bad:?}");
        }
        assert!(check(&[leg("build_store-2")]).is_ok());
        assert!(check(&[leg("a"), leg("a")]).unwrap_err().contains("two legs are called a"));
    }

    #[test]
    fn show_prints_the_tier_the_default_included_and_what_each_leg_waits_on() {
        let ls = vec![leg("scout"), opus("plan"), with("b", &[])];
        let t = table(&ls);
        assert!(t.contains("- scout · claude sonnet medium · ready · after -"), "{t}");
        assert!(t.contains("- plan · claude opus high · pending · after scout · why decides which layer owns it"), "{t}");
        let mut row = row("");
        write(&mut row, &ls);
        let j = json(&row);
        assert_eq!(j[1]["agent"], "claude opus high");
        assert_eq!(j[1]["default"], false);
        assert_eq!(j[0]["default"], true);
        assert_eq!(j[2]["after"], serde_json::json!([]));
    }

    // ---- the verbs, against a store ----

    fn scratch(tag: &str) -> (util::Isolated, Store) {
        let env = util::isolated(&format!("legs-{tag}"));
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let mut t = Task::new("a row", "m-1");
        t.body = "## Overview\nthe work\n\n## Log\n- 2026-10-18 claimed\n".into();
        store.save_task(&t).unwrap();
        (env, store)
    }

    fn parse(line: &[&str]) -> Args {
        Args::parse(line.iter().map(|s| (*s).to_string()).collect())
    }

    /// The doc's fan-out typed as commands, each against the store, and the row's
    /// section after it.
    #[test]
    fn the_verbs_build_a_fan_out_and_refuse_what_the_rules_refuse() {
        let (env, store) = scratch("verbs");
        let file = |name: &str, text: &str| {
            let p = env.home().join(name);
            std::fs::write(&p, text).unwrap();
            p.to_string_lossy().to_string()
        };
        let order = file("order.md", "Read the call sites.");
        let go = |line: &[&str]| run(&store, &parse(line), None);
        assert_eq!(go(&["leg", "add", "m-1", "scout", "--order", &order]), 0);
        assert_eq!(go(&["leg", "add", "m-1", "plan", "--order", &order, "--agent", "claude opus high", "--why", "chooses between layers"]), 0);
        assert_eq!(go(&["leg", "add", "m-1", "finish", "--order", &order]), 0);
        assert_eq!(go(&["leg", "add", "m-1", "b1", "--order", &order, "--after", "plan", "--before", "finish"]), 0);
        assert_eq!(go(&["leg", "add", "m-1", "b2", "--order", &order, "--after=plan", "--before", "finish"]), 0);
        let ls = legs(&store.find_task("m-1").unwrap());
        assert_eq!(ls.len(), 5);
        assert_eq!(policy_line(&ls[1].policy), "claude opus high", "--agent with a value reached the leg");
        assert_eq!(waits(&ls)[2], vec!["plan", "b1", "b2"]);
        // Refused: no why, a dangling end, a missing order.
        assert_eq!(go(&["leg", "add", "m-1", "x", "--order", &order, "--agent", "claude opus high"]), 1);
        assert_eq!(go(&["leg", "add", "m-1", "x", "--order", &order, "--after", "plan"]), 1);
        assert_eq!(go(&["leg", "add", "m-1", "x"]), 1);
        assert_eq!(go(&["leg", "after", "m-1", "plan", "finish"]), 1, "a cycle");
        assert_eq!(go(&["leg", "skip", "m-1", "b2", "the", "split", "was", "wrong"]), 0);
        assert_eq!(go(&["leg", "after", "m-1", "b1", "scout"]), 0);
        assert_eq!(go(&["leg", "after", "m-1", "b1", "-"]), 0);
        assert_eq!(go(&["leg", "show", "m-1"]), 0);
        assert_eq!(go(&["leg", "show", "m-1", "plan"]), 0);
        assert_eq!(go(&["leg", "show", "m-1", "ghost"]), 1);
        let ls = legs(&store.find_task("m-1").unwrap());
        assert_eq!(ls[4].state, State::Skipped);
        assert_eq!(ls[4].skipped.as_deref(), Some("the split was wrong"));
        assert_eq!(ls[3].after, Some(vec![]));
        assert_eq!(go(&["leg", "bogus"]), 2);
    }

    // ---- legs-seat (wsp-292) ----

    /// A leg opens only when everything upstream is settled, a retried open of
    /// an open leg with no seat is quiet, and an open one with a seat is not.
    #[test]
    fn a_leg_opens_when_ready_and_an_open_one_is_not_opened_twice() {
        let ls = vec![leg("a"), leg("b")];
        assert!(open(&ls, "b", "t").unwrap_err().contains("still open upstream of it — a"));
        let one = open(&ls, "a", "2026-10-18T10:00:00Z").unwrap();
        assert_eq!(one[0].state, State::Running);
        assert_eq!(one[0].started.as_deref(), Some("2026-10-18T10:00:00Z"));
        assert_eq!(open(&one, "a", "later").unwrap()[0].started.as_deref(), Some("2026-10-18T10:00:00Z"));
        let seated = attach(&one, "a", "cpd-1").unwrap();
        assert!(open(&seated, "a", "t").is_err(), "a seated leg is not opened again");
        assert_eq!(unopen(&seated, "a")[0].state, State::Running, "and a seated one is not given back");
        assert_eq!(unopen(&one, "a")[0].state, State::Pending);
    }

    /// The baton is held to the cap the record sets, a caption is required, and
    /// `review` is refused with a sibling running.
    #[test]
    fn finishing_checks_the_caption_and_the_baton_and_review_checks_the_siblings() {
        let ls = vec![
            Leg { state: State::Done, ..leg("plan") },
            Leg { state: State::Running, after: Some(vec!["plan".into()]), ..leg("a") },
            Leg { state: State::Running, after: Some(vec!["plan".into()]), ..leg("b") },
            Leg { after: Some(vec!["a".into(), "b".into()]), ..leg("finish") },
        ];
        assert!(finish(&ls, "a", "ok", "  ", "t").is_err(), "no caption");
        let cap = baton_cap(&ls, "a").0;
        assert_eq!(cap, 3_000, "two batons meeting fit 3,000 each");
        assert!(finish(&ls, "a", &"x".repeat(cap + 1), "c", "t").unwrap_err().contains("its cap is 3000"));
        let done = finish(&ls, "a", "pointers", "c", "t").unwrap();
        assert_eq!((done[1].state, done[1].caption.as_deref()), (State::Done, Some("c")));
        assert!(finish(&done, "a", "again", "c", "t").is_err(), "a done leg is not ended twice");
        assert!(end_by_review(&ls, "a", "x", "t").unwrap_err().contains("b is still running"));
        let ended = end_by_review(&done, "b", "all fine", "t").unwrap();
        assert_eq!(ended[2].state, State::Done);
        assert_eq!(ended[3].state, State::Skipped);
    }

    /// `show --json` carries the leg's times and whole order and baton.
    /// compound marks a leg `↺ rework` from `rework_of`: null on a leg that is
    /// not one, a list of the legs the verdict named on the one that is, and an
    /// empty list when it named none.
    #[test]
    fn the_json_gives_rework_of_as_null_a_list_or_an_empty_list() {
        let old = vec![leg("build")];
        let named = rework(&old, "build is thin").unwrap().unwrap();
        let none = rework(&old, "it does not hold").unwrap().unwrap();
        let mut t = row("");
        write(&mut t, &named);
        let j = json(&t);
        assert_eq!(j[0]["rework_of"], serde_json::Value::Null);
        assert_eq!(j[1]["rework_of"], serde_json::json!(["build"]));
        write(&mut t, &none);
        assert_eq!(json(&t)[1]["rework_of"], serde_json::json!([]));
        assert_eq!(legs(&t)[1].rework, Some(Vec::new()), "and it survives the text");
    }

    #[test]
    fn the_json_carries_started_ended_order_and_baton() {
        let mut t = row("");
        let l = Leg {
            state: State::Done,
            started: Some("2026-10-18T10:00:00Z".into()),
            closed: Some("2026-10-18T10:30:00Z".into()),
            baton: "pointers".into(),
            ..leg("plan")
        };
        write(&mut t, &[l]);
        let j = json(&t);
        assert_eq!(j[0]["started"], "2026-10-18T10:00:00Z");
        assert_eq!(j[0]["ended"], "2026-10-18T10:30:00Z");
        assert_eq!((j[0]["order"].as_str(), j[0]["baton"].as_str()), (Some("do plan"), Some("pointers")));
        write(&mut t, &[leg("fresh")]);
        assert!(json(&t)[0]["started"].is_null() && json(&t)[0]["ended"].is_null(), "null until set");
    }
}
