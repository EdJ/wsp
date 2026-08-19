//! `wsp watch` — the monitor every governor rebuilds by hand, and gets wrong.
//!
//! Two seats built one of these on 2026-08-19, separately, in a shell. The
//! `wsp` seat built it three times and shipped the third; the `worklist` seat
//! ran four at once, one per group. Between them they wrote the same loop six
//! times and produced three distinct classes of bug:
//!
//! 1. **A `while true` loop that only notified on exit.** It watched all night
//!    and said nothing, because the thing reporting was the process ending.
//! 2. **`comm` on input that was not globally sorted.** It reported every
//!    blocked task as new on the first tick.
//! 3. **Four monitors that re-derived `needs_a_person` by hand** — joining
//!    `wip --json`'s `turning` and the task's status themselves — against a
//!    JSON document that was already carrying `needs_you` computed correctly,
//!    seat exception and all, in the same object they were parsing.
//!
//! The third is the interesting one and it is the real argument for this file.
//! `robustness-051` describes a signal wsp computes and never delivers. This is
//! worse and more useful: wsp computes it, delivers it, publishes it in the
//! JSON, and a careful consumer standing directly in front of the field still
//! did not use it. **That is not a plumbing failure, it is a discoverability
//! failure, and a subscription removes it completely — you name the thing you
//! care about instead of knowing which field already answers it.** A correct
//! hand-written loop is still the wrong shape.
//!
//! # The unit of subscription is a named predicate over joined state
//!
//! Not a record selector, and `wsp-095` proved that from the tree rather than
//! arguing it. [`crate::place::Event`] already exists — `Opened | Started |
//! Moved | Stopped | Closed`, five variants all keyed on a seat, a change feed
//! over one record type — and the single most valuable thing any of those six
//! hand-built monitors reported cannot be expressed in it:
//!
//!     worklist-004    task status = doing      agent = STOPPED-blocked
//!
//! `Moved(seat, Blocked)` is one half. `task.status == doing` is the other and
//! lives in the store, which no seat event carries. A watch on the task's
//! status sees `doing` and says nothing, because nothing changed; a watch on
//! the agent sees `blocked` and cannot tell it from a seat idling between the
//! agents it is sequencing. **Only the conjunction is a signal.**
//!
//! So what you subscribe to is a *signal kind* — `wsp watch needs-a-person` —
//! and never a row. "About X" survives as a filter on a signal's subject
//! (`--about`, `-p`), which is a different thing from being what is subscribed
//! to.
//!
//! And the conjunction itself is not re-derived here. [`Kind::NeedsAPerson`]
//! reads `needs_you` off the rows [`crate::cmd_agent::wip_rows`] builds — the
//! same rows `wsp wip --json` serialises — so there is one definition of the
//! predicate ([`cmd_govern::needs_a_person`]) and this is its second caller.
//! Three definitions of it is how the exception for seats gets quietly lost,
//! and it has already been lost once: `agent_status == "idle"` was written out
//! four times in four censuses and three of them were wrong.
//!
//! # Subscribe to the level; stream edges only for liveness
//!
//! `robustness-075`'s constraint, and it is the one that outranks every taste
//! question here: *a poll is self-healing and an event stream is not.* One
//! dropped event leaves a surface silently wrong until something unrelated
//! moves, and on a stream silence cannot distinguish *nothing happened* from *I
//! am disconnected* from *the broker died*.
//!
//! Therefore the primitive is a **full level read** — [`Source::sample`]
//! answers *everything that is up right now* — and edges are computed from two
//! consecutive reads by [`Ledger::advance`]. `wsp watch --now` is that read on
//! its own, and it is correct after any disconnection, restart or missed tick.
//! **A level read answers "nothing is up" positively**, which is the whole
//! difference between a watcher that is quiet and a watcher that is broken.
//! That was the fault in the first `while true` loop, in a form no amount of
//! care about `comm` and `sort` could have removed.
//!
//! Levels are also why nothing is ever re-emitted on a timer. `robustness-088`
//! named the failure — *"a panel full of flags nobody reads, which is worse
//! than the silence it replaces"* — and it is what you get from modelling a
//! level as an edge.
//!
//! # Where `wsp-095` lands
//!
//! [`Source`] is the seam and it has one implementation, [`Poll`]. A push
//! source implements the same trait: it keeps the predicate set current from
//! whatever transport arrives, and answers `sample()` out of what it is
//! holding. That is deliberately *not* a change feed — a source that could only
//! answer deltas would put `robustness-075`'s failure back — and it is why the
//! trait's one method asks for the level set rather than for the news.
//!
//! Nothing about the transport is decided here; `robustness-075` owns that and
//! is parked. What is decided is that the caller does not change when it lands:
//! `wsp watch` stops polling and starts listening, and the lines are the same.
//!
//! The payload is `wsp-095`'s Part 4 envelope rather than a second one — see
//! [`Signal::envelope`] — because *what crosses the wire* is `robustness-075`'s
//! open question, and answering it independently here is how a transport gets
//! chosen twice.
//!
//! # Silence must not look like success, in five places
//!
//! The failure this file exists to prevent has five separate causes and each
//! one needs its own answer. Listing them because a partial answer here reads
//! exactly like a complete one:
//!
//! 1. **The watcher never says anything at all.** It opens by naming its scope,
//!    its interval and how many facts are already standing, so a stream that
//!    begins with nothing is a watcher that never started.
//! 2. **It stops mid-run.** It heartbeats — one line, by default every thirty
//!    minutes, carrying the standing count. Absence of the heartbeat is legible
//!    where absence of an event is not.
//! 3. **The process is gone and nobody is looking at the stream.** Every tick
//!    writes the register in [`crate::store::Store::watches`]. `wsp doctor`
//!    reads it and reports a watch whose pid has died or whose last tick is
//!    stale — a *reporter* that has stopped is exactly the fact no other
//!    surface in wsp could state.
//! 4. **It is running and blind.** herdr unreachable means half the predicates
//!    cannot be evaluated at all, so [`Kind::Blind`] goes up, and it is the one
//!    signal exempt from priming: a watch that is blind from its first tick
//!    must say so, because its whole output is otherwise indistinguishable from
//!    a quiet fleet.
//! 5. **It ended and the reason was not said.** Every exit prints a line naming
//!    why, and a fault exits non-zero. There is no silent return.
//!
//! # What it costs when nothing is happening
//!
//! One tick is one store sweep plus three herdr calls. Measured on this machine
//! on 2026-08-19 against the live store — see the task — and the number is in
//! `wsp watch --now`'s own timing rather than assumed. At the default sixty
//! seconds it is free; the reason to say the number rather than assume it is
//! that a governor runs this for a night.
//!
//! # The name
//!
//! `watch` is already the port's word: [`crate::place::Place::watch`] is the
//! backend event stream, *"block, calling `f` for each event"*. The two sit in
//! one grep at different altitudes, and that is accepted deliberately rather
//! than discovered — because if push ever lands, `Place::watch` is precisely
//! what feeds this, and the shared word will be describing one pipeline instead
//! of two things.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use crate::cmd_agent::{self, Bound, Probe, Wip};
use crate::cmd_govern;
use crate::herdr;
use crate::model::{Status, Task};
use crate::place::State;
use crate::place_herdr;
use crate::resolve::Index;
use crate::store::Store;
use crate::util::{self, Paint};
use crate::worklist;
use crate::Args;

/// How often a tick runs, when nobody says.
///
/// Sixty seconds because the sweep is milliseconds and the thing being watched
/// moves in minutes. Nothing here is tuned to it: every threshold below is a
/// duration compared against a clock, so an interval of ten seconds or ten
/// minutes changes how soon news arrives and changes nothing about what counts
/// as news.
const EVERY: i64 = 60;

/// How long a stall must hold before it is worth a line.
///
/// **The watcher's one advantage over a snapshot, and the reason this number is
/// not `quiet_note`'s hour.** `wsp doctor` has no memory, so it asks a proxy —
/// nothing written to the task for an hour — and needs the long threshold to
/// stop an agent between turns reading as a stall. A watch sees the same
/// predicate on every tick and can therefore measure how long it has actually
/// been true, which is the fact everybody wanted and nobody had.
///
/// Five minutes: an agent between turns is stopped for seconds, and a turn in
/// flight is not stopped at all — [`crate::place::State::turn_in_flight`] is
/// the reading, so a long turn never enters this at all. Five minutes of
/// genuinely no turn on a task that is still `doing` is not a gap between
/// turns, because nothing in this system starts the next one on its own.
const SETTLE: i64 = 5 * 60;

/// How often the stream says it is still there. Thirty minutes, and it carries
/// the standing count so it is a level read rather than a pulse.
const HEARTBEAT: i64 = 30 * 60;

/// How stale a register entry may be before `doctor` calls it dead: three ticks
/// plus slack. Three rather than one because a tick that ran long is not a
/// watcher that died, and a watch is a diagnostic — a diagnostic that cries
/// wolf is one nobody reads.
pub(crate) const STALE_TICKS: i64 = 3;

// ---------------------------------------------------------------------------
// the vocabulary
// ---------------------------------------------------------------------------

/// The predicates a governor may subscribe to.
///
/// Each variant is a *named predicate over joined state*, which is the unit
/// `wsp-095` Part 13 settled on. None of them is a record changing: `Review` is
/// "at review", not "reached review", because a level self-clears when the
/// condition goes away and an edge has to be caught at the instant it happens
/// or it is lost.
///
/// `core-003`'s five requirements and `worklist-007`'s five moments are the
/// starting vocabulary and adding to it later is a new variant rather than a
/// new interface. Four of `core-003`'s five are below. The fifth — *something
/// landed on the trunk touching a file my lane is in* — is deliberately absent;
/// see the module-level note on scope at the foot of this file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Kind {
    /// The join. `stopped && doing && !seat`, read off the row
    /// [`cmd_agent::wip_rows`] publishes rather than recomputed here.
    NeedsAPerson,
    /// A task in scope is sitting at `review`. The single most-polled fact in
    /// the fleet, and the agent's terminal verb — only a person says `done`.
    Review,
    /// A task in scope is `blocked`: an agent stopped and addressed a question
    /// to whoever answers for it, which under a seat is the seat.
    Blocked,
    /// A hand is up on a task this seat is the addressee for.
    Flag,
    /// A binding whose pane herdr no longer lists, or whose pane is alive with
    /// the agent gone. Covers *an agent died* and *an agent never started*,
    /// which look identical from here and want the same first move.
    AgentGone,
    /// wsp cannot see the agents at all. Never subscribed to and never filtered
    /// out: it is liveness, not news, and it is the difference between a quiet
    /// fleet and a watcher reporting on half a world.
    Blind,
}

impl Kind {
    /// What you type to subscribe to it, and what is printed in the first
    /// column. One word, because both readers are counting characters.
    pub(crate) fn word(&self) -> &'static str {
        match self {
            Kind::NeedsAPerson => "needs-a-person",
            Kind::Review => "review",
            Kind::Blocked => "blocked",
            Kind::Flag => "flag",
            Kind::AgentGone => "agent-gone",
            Kind::Blind => "blind",
        }
    }

    /// Everything a bare `wsp watch` subscribes to.
    pub(crate) fn every() -> Vec<Kind> {
        vec![Kind::NeedsAPerson, Kind::Review, Kind::Blocked, Kind::Flag, Kind::AgentGone]
    }

    fn parse(word: &str) -> Option<Kind> {
        Kind::every()
            .into_iter()
            .chain(std::iter::once(Kind::Blind))
            .find(|k| k.word() == word)
    }

    /// Whether news of it going *away* is worth a line.
    ///
    /// Asked per kind rather than emitted for everything, because a level going
    /// down is only news when somebody else took it down. A stall that resolved
    /// itself changes what the governor does next — do not go and prod that
    /// agent — and so does a hand lowered by another seat. A task leaving
    /// `review` is nearly always the reader having just closed it, and a line
    /// saying so is the surface telling you what you did.
    fn clears(&self) -> bool {
        matches!(self, Kind::NeedsAPerson | Kind::Flag | Kind::AgentGone | Kind::Blind)
    }

    /// Whether evaluating this predicate needs herdr to answer.
    ///
    /// **Read by [`Ledger::advance`], and it is the difference between a level
    /// that went away and one nobody could look at.** [`Poll::sample`] derives
    /// the agent half from a census, so when herdr is unreachable that half of
    /// every tick is simply absent — no rows, no bindings, nothing. A diff
    /// taken against that reads every stall on the machine as having resolved,
    /// and the next tick after herdr comes back reads them all as new.
    ///
    /// One herdr restart is therefore two edges per standing signal, neither of
    /// which happened. On a stream a person discounts it, having just seen
    /// `blind` go up; through a hook it is a phone buzzing twice for every
    /// stalled agent on the box, at whatever hour herdr was restarted. That is
    /// `robustness-088`'s named failure — *noise is worse than the silence it
    /// replaces* — arriving by the one route that has nobody reading it.
    ///
    /// So: silence is not evidence, which is the judgement `sync` already makes
    /// before it reaps. A level of a kind that could not be read this tick is
    /// **held**, not cleared, and it goes down on the first tick that can
    /// actually see it.
    fn needs_herdr(&self) -> bool {
        match self {
            Kind::NeedsAPerson | Kind::AgentGone => true,
            // The store answers these whether or not anything else does, which
            // is what makes saying `blind` worth anything at all.
            Kind::Review | Kind::Blocked | Kind::Flag => false,
            // Its own evidence.
            Kind::Blind => false,
        }
    }

    /// How much of the receiver's time this may take, as
    /// [`crate::message::Kind`].
    ///
    /// *"A derived signal → `note`, except herdr's `blocked` — a modal holding
    /// the keyboard, fixed by one keypress — which `quiet_note` already
    /// promotes past the hour and which is the one derived signal that earns
    /// `direction`."* That promotion is carried on the signal rather than on
    /// the kind, because it is a fact about the reading and not about the
    /// predicate; see [`Signal::loud`].
    ///
    /// The enum is `message.rs`'s and not a word written out here. It was a
    /// literal `"note"` until the record landed, which is two vocabularies
    /// agreeing by hand — and the one thing the fleet has paid for repeatedly
    /// is a word spelled out in a second place drifting from the first.
    fn loudness(&self) -> crate::message::Kind {
        crate::message::Kind::Note
    }
}

/// One fact that is true now, and worth acting on.
///
/// The *key* — kind plus subject — is what the ledger diffs on, and `detail` is
/// deliberately outside it. A stall whose duration grows every tick must be one
/// unchanged fact rather than sixty of them, which is the whole of "emit on
/// change, never on state" written as a type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Signal {
    pub(crate) kind: Kind,
    /// A task id, mostly. A pane for a binding whose task is gone, and the
    /// literal `herdr` for [`Kind::Blind`].
    pub(crate) subject: String,
    /// The clause after the id: what to do, or why. Never the task's title —
    /// `wsp-095` Part 13 holds this to `wsp worklist next`'s bar, *ids not
    /// titles*, because a governor knows its own ids and pays for every line on
    /// every request of every session.
    pub(crate) detail: String,
    /// This reading is loud enough to be `direction` rather than `note`: a
    /// modal has the keyboard and one keypress fixes it.
    pub(crate) loud: bool,
    /// This one may be emitted the instant it is seen. False for the states
    /// that flap — an agent is briefly "stopped" between every pair of turns —
    /// and true for the ones that do not.
    pub(crate) at_once: bool,
}

impl Signal {
    pub(crate) fn new(kind: Kind, subject: &str, detail: &str) -> Signal {
        Signal {
            kind,
            subject: subject.to_string(),
            detail: detail.to_string(),
            loud: false,
            at_once: true,
        }
    }

    fn settling(mut self) -> Signal {
        self.at_once = false;
        self
    }

    pub(crate) fn loud(mut self) -> Signal {
        self.loud = true;
        self
    }

    pub(crate) fn key(&self) -> String {
        format!("{}:{}", self.kind.word(), self.subject)
    }

    /// The signal as `wsp-095` Part 4's envelope.
    ///
    /// Part 4 wrote one envelope for the two *edge* shapes and Part 3 named the
    /// third; `shape: "signal"` is that third, and it is what decides which
    /// fields are here. A signal is a **level nobody wrote**: it has no sender
    /// to reply to, no disposition to record and no escalation history, so
    /// `from` is `wsp`, and `state`, `via`, `waiting` and `reply_to` are absent
    /// rather than null — a field that can never be filled is noise on a wire.
    ///
    /// `role` is the one place this is knowingly short of Part 12. That part
    /// splits a seat's inbox into what it is the **addressee** for and what it
    /// merely **observes**, and the split needs `seats_for` — the untruncated
    /// routing walk — which is `wsp-095`'s to write. Until it exists this watch
    /// subscribes only to what [`cmd_govern::seat_for`] addresses here, so the
    /// field is a constant, and it is present so the second value is a value
    /// rather than a schema change.
    pub(crate) fn envelope(&self, edge: Edge, to: &str, at: &str) -> Value {
        json!({
            "shape": crate::message::Shape::Signal.as_str(),
            "signal": self.kind.word(),
            "kind": match self.loud {
                true => crate::message::Kind::Direction,
                false => self.kind.loudness(),
            }
            .as_str(),
            "edge": edge.word(),
            "from": "wsp",
            "to": to,
            "role": "addressee",
            "about": self.subject,
            "at": at,
            "text": self.detail,
        })
    }
}

/// Which way a level moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Edge {
    Up,
    Down,
}

impl Edge {
    pub(crate) fn word(&self) -> &'static str {
        match self {
            Edge::Up => "up",
            Edge::Down => "down",
        }
    }
}

// ---------------------------------------------------------------------------
// the diff
// ---------------------------------------------------------------------------

/// One level that is up, and since when.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Held {
    signal: Signal,
    /// Epoch seconds. Not an ISO stamp: this is arithmetic, and every reader of
    /// it is comparing rather than printing.
    since: i64,
    /// Whether it has been said out loud. False for a level that is up and
    /// still settling, which is a state a snapshot has no name for.
    told: bool,
}

/// What was up at the last read, so this one can be a diff.
///
/// **The diffing is inside the verb, and that is an argument for the verb
/// existing rather than a convenience.** The monitors that got this right did
/// it by polling `--json` and diffing in python, which sidesteps the
/// `comm`-on-unsorted-input class of bug entirely; the ones that got it wrong
/// used `comm` on input that was not globally sorted and reported the whole
/// backlog as new. A general verb cannot assume its caller will pick the first.
/// A `BTreeMap` keyed on [`Signal::key`] makes the sorted-input bug
/// unrepresentable rather than avoided.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Ledger {
    up: BTreeMap<String, Held>,
}

/// One line's worth of news.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Emit {
    pub(crate) edge: Edge,
    pub(crate) signal: Signal,
    /// How long the level had been up when this was said. Zero on the way up
    /// for anything that did not settle.
    pub(crate) held: i64,
}

impl Ledger {
    /// Take a level read and say what is news.
    ///
    /// Pure, and `now` is a parameter for that reason: the settle rule is the
    /// one piece of behaviour here that depends on the clock, and it is the one
    /// worth being able to test without one.
    ///
    /// Three things happen, in this order, and the order is load-bearing:
    ///
    /// 1. every level in `now` that is new is recorded with its first-seen
    ///    time, and **not** said;
    /// 2. every level that has gone is dropped, and said if it had been said —
    ///    a level that went away while still settling was never news, so its
    ///    disappearance is not news either;
    /// 3. every recorded level that has now held long enough is said.
    ///
    /// Doing (3) last is what makes a signal that appears and settles inside
    /// one tick emit exactly once rather than not at all.
    pub(crate) fn advance(&mut self, now: &[Signal], at: i64, settle: i64) -> Vec<Emit> {
        let mut out = Vec::new();
        let seen: BTreeSet<String> = now.iter().map(Signal::key).collect();
        // Half the predicates could not be evaluated at all this tick, so their
        // absence from `now` says nothing. See [`Kind::needs_herdr`] for what
        // treating it as news costs.
        let blind = now.iter().any(|s| s.kind == Kind::Blind);

        for s in now {
            let e = self.up.entry(s.key()).or_insert_with(|| Held {
                signal: s.clone(),
                since: at,
                told: false,
            });
            // The detail is refreshed and the key is not: a stall whose pane
            // changed its wording is the same stall, and re-keying it would
            // emit it twice.
            e.signal = s.clone();
        }

        let gone: Vec<String> = self
            .up
            .iter()
            .filter(|(k, h)| !seen.contains(*k) && !(blind && h.signal.kind.needs_herdr()))
            .map(|(k, _)| k.clone())
            .collect();
        for k in gone {
            if let Some(h) = self.up.remove(&k) {
                if h.told && h.signal.kind.clears() {
                    out.push(Emit { edge: Edge::Down, signal: h.signal, held: at - h.since });
                }
            }
        }

        for h in self.up.values_mut() {
            if h.told {
                continue;
            }
            let ready = h.signal.at_once || at - h.since >= settle;
            if ready {
                h.told = true;
                out.push(Emit { edge: Edge::Up, signal: h.signal.clone(), held: at - h.since });
            }
        }
        out
    }

    /// Absorb a read without saying any of it.
    ///
    /// The fix for the second hand-built monitor, which dumped every blocked
    /// task as new on its first tick. A watch is a subscription to *changes
    /// from now*; what is already true is reported once, by the opening line,
    /// as a standing count rather than as sixty events.
    ///
    /// [`Kind::Blind`] is the exception and it is the whole of item 4 in the
    /// module docs: a watch that cannot see the agents from its first tick must
    /// say so, because everything else it does not print is then meaningless.
    pub(crate) fn prime(&mut self, now: &[Signal], at: i64) -> Vec<Emit> {
        let mut out = Vec::new();
        for s in now {
            let blind = s.kind == Kind::Blind;
            self.up.insert(s.key(), Held { signal: s.clone(), since: at, told: true });
            if blind {
                out.push(Emit { edge: Edge::Up, signal: s.clone(), held: 0 });
            }
        }
        out
    }

    /// How many levels are up, said or still settling. The number the opening
    /// line and every heartbeat carry, and the reason silence is legible: it is
    /// a positive answer to *what is up right now*.
    pub(crate) fn standing(&self) -> usize {
        self.up.len()
    }

    pub(crate) fn json(&self) -> Value {
        Value::Object(
            self.up
                .iter()
                .map(|(k, h)| {
                    (
                        k.clone(),
                        json!({
                            "signal": h.signal.kind.word(),
                            "subject": h.signal.subject,
                            "detail": h.signal.detail,
                            "loud": h.signal.loud,
                            "at_once": h.signal.at_once,
                            "since": h.since,
                            "told": h.told,
                        }),
                    )
                })
                .collect(),
        )
    }

    /// Read a ledger back.
    ///
    /// **The whole signal is stored, and the first version of this stored only
    /// the timing.** That version rebuilt each signal from the read it was
    /// being compared against, which is fine for everything still up and wrong
    /// for the one case a pull-mode subscription exists to catch: a level that
    /// has *gone*. It is absent from the read by definition, so there was
    /// nothing to rebuild it from, so it silently vanished from the ledger and
    /// its clearing was never reported. Caught by running it — a flag raised
    /// and then lowered between two `--once` calls printed the raise and
    /// nothing else, and *a hand in my scope lowered by somebody else* is one
    /// of the five things a governor asked to be told.
    pub(crate) fn of_json(v: &Value) -> Ledger {
        let mut up = BTreeMap::new();
        for (key, rec) in v.as_object().into_iter().flatten() {
            let word = rec.get("signal").and_then(Value::as_str).unwrap_or_default();
            let Some(kind) = Kind::parse(word) else { continue };
            up.insert(
                key.clone(),
                Held {
                    signal: Signal {
                        kind,
                        subject: rec.get("subject").and_then(Value::as_str).unwrap_or_default().to_string(),
                        detail: rec.get("detail").and_then(Value::as_str).unwrap_or_default().to_string(),
                        loud: rec.get("loud").and_then(Value::as_bool).unwrap_or(false),
                        at_once: rec.get("at_once").and_then(Value::as_bool).unwrap_or(true),
                    },
                    since: rec.get("since").and_then(Value::as_i64).unwrap_or(0),
                    told: rec.get("told").and_then(Value::as_bool).unwrap_or(false),
                },
            );
        }
        Ledger { up }
    }
}

// ---------------------------------------------------------------------------
// the source — where the levels come from
// ---------------------------------------------------------------------------

/// Where a watch gets its level set.
///
/// **One method, and it asks for the level rather than for the news.** That is
/// the seam `wsp-095` lands on and the shape it has to land in: a push source
/// keeps its own predicate set current from whatever transport arrives and
/// answers this out of what it is holding, so a dropped message costs a stale
/// answer for one tick rather than a surface that is silently wrong until
/// something unrelated moves. A trait that could only answer deltas would put
/// `robustness-075`'s failure back and hide it behind an interface.
///
/// The caller does not change when that happens. [`run`] reads a level set,
/// diffs it, prints the edges, and knows nothing about how the set was
/// obtained.
pub(crate) trait Source {
    /// Everything up right now, in whatever order. The ledger sorts.
    fn sample(&mut self) -> Vec<Signal>;
}

/// The scope a watch answers for: a project id or a worklist slug.
///
/// `seated` is the difference between the two ways a scope is decided, and it
/// picks which membership rule applies. A **seat** answers for exactly what the
/// routing walk sends it — the same rule `wsp flag` addresses by, so what a
/// watch reports and what a raised hand reaches are one definition. An
/// **asked-for** scope has no seat to route to, so it is plain membership: the
/// project and its sub-tree, or a worklist's members. Two rules because there
/// are two questions, and using the routing rule for an unseated `-p` would
/// silently drop every task some other seat happens to answer for.
///
/// There is a third way, and it has no name to type: [`Scope::machine`]. Every
/// task on the box, which is not a thing a *governor* ever wants — a seat that
/// answered for everything would answer for nothing — and is exactly what the
/// unattended pass in [`crate::attention`] is for. Nobody asked it to look, so
/// there is nobody whose scope it could take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Scope {
    pub(crate) name: String,
    pub(crate) seated: bool,
    /// The workspace holding the seat, when there is one. The watch ends when
    /// this workspace is no longer in the slot — an exit condition that costs
    /// nothing and is the natural end of a seat's subscription.
    pub(crate) workspace: String,
    /// Every task, with no membership test at all. See [`Scope::machine`].
    pub(crate) all: bool,
}

impl Scope {
    /// The whole machine, for a reader that is nobody's seat.
    ///
    /// Deliberately not reachable from argv. `wsp watch` is a *subscription*
    /// and a subscription is to a scope somebody answers for; the daemon is not
    /// a subscriber, it is the process that runs anyway, and what it derives is
    /// addressed per signal by [`cmd_govern::seat_for`] rather than by having
    /// been asked for. Putting a `--all` on the verb would give a governor a
    /// way to subscribe to work it cannot act on, which is the noise
    /// `wsp-095` Part 9 asks to be kept out.
    pub(crate) fn machine() -> Scope {
        Scope { name: "this machine".into(), seated: false, workspace: String::new(), all: true }
    }
}

/// Whether a task is this scope's to report on.
pub(crate) fn in_scope(
    scope: &Scope,
    index: &Index,
    governors: &BTreeMap<String, Value>,
    lists: &worklist::Running,
    task: &Task,
) -> bool {
    if scope.all {
        return true;
    }
    if scope.seated {
        return cmd_govern::seat_for(governors, index, lists.list_of(&task.id), task.project.as_deref())
            .is_some_and(|s| s.scope == scope.name);
    }
    if lists.list_of(&task.id) == Some(scope.name.as_str()) {
        return true;
    }
    task.project.as_deref().is_some_and(|p| {
        p == scope.name || index.ancestors(p).iter().any(|a| a == &scope.name)
    })
}

/// The polling implementation: one store sweep and three herdr calls per tick.
pub(crate) struct Poll<'a> {
    store: &'a Store,
    scope: Scope,
    want: BTreeSet<Kind>,
    /// A subject filter, and it is a filter rather than a subscription — see
    /// the module docs on why "about X" is not the unit.
    about: Option<String>,
}

impl<'a> Poll<'a> {
    pub(crate) fn new(store: &'a Store, scope: Scope, want: BTreeSet<Kind>, about: Option<String>) -> Poll<'a> {
        Poll { store, scope, want, about }
    }
}

impl Source for Poll<'_> {
    fn sample(&mut self) -> Vec<Signal> {
        let probe = Probe::live();
        // Constructed here rather than through `Wip::live` so the pane listing
        // and the agent listing come from one probe. `Wip::live` would ask
        // herdr for the agents a second time, and two listings taken a
        // round-trip apart is how a pane comes to be in one and not the other.
        let (agents, panes, blind) = match &probe {
            Probe::Up { agents, panes } => (agents.clone(), panes.clone(), None),
            Probe::Unreachable(e) => (Vec::new(), Vec::new(), Some(format!("herdr unreachable: {e}"))),
            Probe::Down => (Vec::new(), Vec::new(), Some("no herdr socket on this machine".to_string())),
        };
        let up = blind.is_none();
        let wip = Wip {
            tasks: self.store.tasks(),
            index: Index::new(self.store.projects()),
            bindings: self.store.bindings(),
            claims: self.store.claims(),
            pins: self.store.pins(),
            governors: self.store.governors(),
            agents,
            workspaces: match up {
                true => herdr::workspaces().unwrap_or_default(),
                false => Vec::new(),
            },
        };
        let lists = worklist::Running::read(self.store);
        let mine = |t: &Task| in_scope(&self.scope, &wip.index, &wip.governors, &lists, t);
        let task_of = |id: &str| wip.tasks.iter().find(|t| t.id == id);

        let mut out: Vec<Signal> = Vec::new();

        // (a) The store half: two statuses, both levels, both about work rather
        // than about an agent. Neither needs herdr, so both keep answering
        // while the watch is blind — which is the point of saying blind at all.
        for t in wip.tasks.iter().filter(|t| mine(t)) {
            match t.status() {
                Status::Review => out.push(Signal::new(
                    Kind::Review,
                    &t.id,
                    "finished and waiting on you — wsp done <id> · wsp reopen <id>",
                )),
                Status::Blocked => out.push(Signal::new(
                    Kind::Blocked,
                    &t.id,
                    "stopped on a question — wsp show <id> has it",
                )),
                _ => {}
            }
        }

        // (b) Raised hands addressed here. `addressed` is the routing walk and
        // it is asked rather than reimplemented, so a hand this watch reports
        // is exactly a hand `wsp flag --seat` would list.
        for m in crate::message::raised(self.store).iter().filter(|m| !m.is_reply()) {
            let Some(t) = m.about.task().and_then(task_of) else { continue };
            if !mine(t) {
                continue;
            }
            let said = m.title();
            let ask = m.ask().unwrap_or(crate::message::Ask::Nothing).as_str();
            // The sentence, not the title. A flag with no words is one you have
            // to go and read either way, and one with words is carrying the
            // only thing on this line that is not derivable.
            let detail = match (said.is_empty(), ask.is_empty()) {
                (true, true) => "a hand is up — wsp flag lists it".to_string(),
                (true, false) => format!("asking to {ask}"),
                (false, true) => util::truncate(said, 60),
                (false, false) => format!("{} · asking to {ask}", util::truncate(said, 48)),
            };
            out.push(Signal::new(Kind::Flag, &t.id, &detail));
        }

        // (c) The join, read rather than recomputed. Every row here carries
        // `needs_you` — `stopped && doing && !seat` — computed once by
        // `cmd_govern::needs_a_person` and published by `wsp wip --json`.
        for r in cmd_agent::wip_rows(&wip) {
            if !r.needs_you || r.task_id.is_empty() {
                continue;
            }
            let Some(t) = task_of(&r.task_id) else { continue };
            if !mine(t) {
                continue;
            }
            // The one reading that changes the answer's urgency, and it is read
            // off the published row's own `state` word rather than by asking
            // herdr again. A modal has the keyboard: the repair is a keypress,
            // and waiting five minutes to mention it would be waiting five
            // minutes to say a word — `quiet_note` makes the same exception for
            // the same reason.
            let s = match place_herdr::of_word(&r.state) {
                State::Blocked => Signal::new(
                    Kind::NeedsAPerson,
                    &t.id,
                    &format!("{} · stopped on a prompt only a person can answer — wsp peek <id>", r.pane),
                )
                .loud(),
                _ => Signal::new(
                    Kind::NeedsAPerson,
                    &t.id,
                    &format!("{} · no turn running — wsp tell <id> reaches it without ending it", r.pane),
                )
                .settling(),
            };
            out.push(s);
        }

        // (d) Bindings with nobody in them. `bound_state` is `doctor`'s reading
        // and is asked here for the same reason `needs_you` is: one definition,
        // two callers. `Quiet` is deliberately not handled — a pane with an
        // agent in it and no turn running is (c)'s, and reporting it twice is
        // how a governor learns to skim.
        if up {
            let answered = cmd_agent::answered_by_machine(panes.iter().map(|p| p.pane_id.as_str()));
            for (pane, b) in &wip.bindings {
                let id = b.get("task_id").and_then(Value::as_str).unwrap_or("");
                let Some(t) = task_of(id) else { continue };
                if !mine(t) {
                    continue;
                }
                let detail = match cmd_agent::bound_state(pane, &wip.agents, &panes, &answered) {
                    Bound::Emptied => format!("{pane} · pane alive, agent gone — the claim and the tree are still held"),
                    Bound::Gone => format!("{pane} · the pane is gone — wsp sync reaps the binding"),
                    _ => continue,
                };
                out.push(Signal::new(Kind::AgentGone, &t.id, &detail).settling());
            }
        }

        if let Some(why) = blind {
            out.push(Signal::new(
                Kind::Blind,
                "herdr",
                &format!("{why} — the agent half of this watch is not being read"),
            ));
        }

        out.retain(|s| {
            s.kind == Kind::Blind
                || (self.want.contains(&s.kind)
                    && self.about.as_deref().is_none_or(|a| s.subject == a))
        });
        out
    }
}

// ---------------------------------------------------------------------------
// the register — what makes a dead watcher a fact rather than an absence
// ---------------------------------------------------------------------------

/// A watch, as another surface finds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Registered {
    pub(crate) key: String,
    pub(crate) scope: String,
    pub(crate) pid: u32,
    pub(crate) host: String,
    pub(crate) tick: String,
    pub(crate) every: i64,
    pub(crate) standing: usize,
    /// The daemon's unattended pass rather than somebody's subscription.
    ///
    /// It is here for one reason and it is the repair line. Everything else
    /// about the two is identical — a process that claimed to keep reporting, a
    /// ledger, a last tick — and the advice is not: a watch that has stopped is
    /// forgotten and started again, and a daemon that has stopped is neither.
    /// Advice that does not work is worse than none, and this check exists
    /// precisely to be believed.
    pub(crate) daemon: bool,
}

impl Registered {
    /// A process that said it would keep reporting.
    ///
    /// `--once` registers with no pid, because there is nothing left running to
    /// have died: its loop is its caller's, and wsp cannot tell a cron that
    /// stopped from one that is between runs. Only a watch that claimed a
    /// process may be reported as having lost one — a check that called every
    /// pull-mode ledger a dead watcher would be the false alarm that teaches
    /// people to skip this section of `doctor`.
    pub(crate) fn watching(&self) -> bool {
        self.pid > 0
    }

    /// Long enough since the last tick that something is wrong. Three ticks,
    /// with a floor so a very short interval does not make this hair-trigger.
    pub(crate) fn stale(&self) -> bool {
        self.watching() && util::since(&self.tick) > (self.every * STALE_TICKS).max(90)
    }
}

/// Every watch registered on this machine.
///
/// Read by `wsp doctor` and by `wsp watch --status`, and by nothing that runs
/// on a loop. A register is a fact about a *reporter*, which is the one thing
/// no other record in wsp describes: a claim, a flag and a seat all describe
/// work, and none of them can tell a fleet with nothing to say from a watcher
/// that stopped saying it.
pub(crate) fn registered(store: &Store) -> Vec<Registered> {
    let host = util::hostname();
    store
        .watches()
        .into_iter()
        .filter(|(_, v)| v.get("host").and_then(Value::as_str).unwrap_or_default() == host)
        .map(|(key, v)| Registered {
            key,
            scope: v.get("scope").and_then(Value::as_str).unwrap_or_default().to_string(),
            pid: v.get("pid").and_then(Value::as_u64).unwrap_or(0) as u32,
            host: v.get("host").and_then(Value::as_str).unwrap_or_default().to_string(),
            tick: v.get("tick").and_then(Value::as_str).unwrap_or_default().to_string(),
            every: v.get("every").and_then(Value::as_i64).unwrap_or(EVERY),
            standing: v.get("standing").and_then(Value::as_u64).unwrap_or(0) as usize,
            daemon: v.get("daemon").and_then(Value::as_bool).unwrap_or(false),
        })
        .collect()
}

/// What `doctor` says about the watches, and it is the only check in this tree
/// whose subject is a thing that reports rather than a thing that works.
///
/// A problem rather than a note when the process is gone: a governor that
/// started a watch is *relying* on it, and every minute after it died looks
/// exactly like a quiet night. A wedged watch — pid alive, ticks stale — is the
/// same fault with a different repair, so it is the same line with a different
/// clause.
pub(crate) fn health(store: &Store, problems: &mut Vec<String>) {
    let watches = registered(store);
    if watches.is_empty() {
        return;
    }
    let live = crate::place_super::alive(&watches.iter().filter(|w| w.watching()).map(|w| w.pid).collect::<Vec<_>>());
    for w in &watches {
        let dead = w.watching() && !live.contains(&w.pid);
        if !dead && !w.stale() {
            continue;
        }
        let ago = util::duration_human(util::since(&w.tick));
        let why = match dead {
            true => "the process is gone",
            false => "the process is alive and has stopped ticking",
        };
        // Two subjects, one fault, two repairs. The daemon's pass is not a
        // subscription somebody started and it is not one they can restart with
        // `wsp watch`; `daemon::health` says whether the process is there at
        // all, and this says whether the half of it that watches is running —
        // which is a distinct failure, because `Store::run_hook` waits for its
        // child and a hook that blocks wedges the loop without killing it.
        let repair = match w.daemon {
            true => "`wsp doctor` names the daemon below; `kill` it and herdr restarts it, or run `wsp daemon` yourself".to_string(),
            false => format!("`wsp watch --forget {}` clears the record; `wsp watch` starts one again", w.key),
        };
        let what = match w.daemon {
            true => "the daemon's unattended pass".to_string(),
            false => format!("the watch on {} ({})", w.scope, w.key),
        };
        problems.push(format!(
            "{what} last ticked {ago} ago — {why}, so silence from it means nothing. {repair}"
        ));
    }
}

// ---------------------------------------------------------------------------
// what a person sees
// ---------------------------------------------------------------------------

/// `14:32` in the reader's own clock.
///
/// A watch's output is read hours after it was written, in a scrollback with no
/// times in it. Five characters, and without them a governor cannot tell a
/// stall reported three minutes ago from one reported at four in the morning —
/// which is the difference between going to look and not.
fn clock(at: i64) -> String {
    util::local_hm(at)
}

/// One line of news, in the shape both hand-built monitors converged on.
fn line(e: &Emit, at: i64, p: &Paint) -> String {
    let word = util::pad(e.signal.kind.word(), 14);
    let head = match e.edge {
        Edge::Up => match e.signal.loud {
            true => p.red(&word),
            false => p.yellow(&word),
        },
        Edge::Down => p.dim(&util::pad("cleared", 14)),
    };
    let detail = match e.edge {
        Edge::Up if !e.signal.at_once => format!("{} · held {}", e.signal.detail, util::duration_human(e.held)),
        Edge::Up => e.signal.detail.clone(),
        Edge::Down => format!("{} · was up {}", e.signal.kind.word(), util::duration_human(e.held)),
    };
    format!("{} {head} {}  {}", p.dim(&clock(at)), p.bold(&e.signal.subject), p.dim(&detail))
}

/// A line that is not news: the opening, the heartbeat, the ending.
///
/// Marked with a `·` in the column the signal name would occupy, so a stream
/// can be read down its second column and the events picked out of it without
/// reading the words.
fn aside(at: i64, said: &str, p: &Paint) -> String {
    format!("{} {} {}", p.dim(&clock(at)), p.dim(&util::pad("·", 14)), p.dim(said))
}

// ---------------------------------------------------------------------------
// the verb
// ---------------------------------------------------------------------------

/// `30s`, `5m`, `2h`, or a bare number of seconds.
///
/// No unit means seconds, because every number typed at this verb is one and a
/// wrong guess is a watch that ticks once an hour while somebody watches the
/// screen.
fn duration(s: &str) -> Option<i64> {
    let s = s.trim();
    let last = *s.as_bytes().last()?;
    let (digits, mul) = match last {
        b's' | b'S' => (&s[..s.len() - 1], 1),
        b'm' | b'M' => (&s[..s.len() - 1], 60),
        b'h' | b'H' => (&s[..s.len() - 1], 3600),
        _ => (s, 1),
    };
    digits.trim().parse::<i64>().ok().filter(|v| *v > 0).map(|v| v * mul)
}

/// Everything a run of the verb was asked for.
struct Spec {
    scope: Scope,
    want: BTreeSet<Kind>,
    about: Option<String>,
    every: i64,
    settle: i64,
    heartbeat: i64,
    /// Stop after this many seconds, or never.
    stop_after: Option<i64>,
    /// Stop when this task, project or worklist is out of open work.
    until: Option<String>,
    json: bool,
}

/// argv, split into the scope it names and the signals it subscribes to.
///
/// One function because the two readings depend on each other: a bare
/// `wsp watch robustness` means the *project*, and `wsp watch -p wsp review`
/// means the *signal*, so neither positional can be classified without knowing
/// whether the other flag was given.
///
/// **A word that is neither is refused rather than ignored.** That is this
/// tree's own lesson, and the dated instance is `wsp tag <id> +dsp -ui`, which
/// read `-ui` as a flag, added `dsp` and exited 0 having silently dropped the
/// removal. A watch typed with a signal name that does not exist would
/// otherwise subscribe to everything and look like it had done what was asked.
fn split(args: &Args) -> Result<(Option<String>, BTreeSet<Kind>), String> {
    let named = args.get("project");
    let mut rest: Vec<String> = args.rest.clone();
    let scope = match &named {
        Some(p) => Some(p.clone()),
        // Only the first, and only when it is not a signal: `wsp watch review`
        // subscribes, `wsp watch robustness` scopes.
        None => match rest.first().filter(|w| Kind::parse(w).is_none()).cloned() {
            Some(p) => {
                rest.remove(0);
                Some(p)
            }
            None => None,
        },
    };
    let mut want = BTreeSet::new();
    for word in &rest {
        match Kind::parse(word) {
            Some(k) => {
                want.insert(k);
            }
            None => {
                return Err(format!(
                    "wsp: `{word}` is not a signal. Try: {}",
                    Kind::every().iter().map(|k| k.word()).collect::<Vec<_>>().join(", ")
                ))
            }
        }
    }
    // Nothing named is everything, which is what a governor wants and what
    // costs it no arguments. `blind` is never in the set either way — it is
    // liveness, not news, and it is never filtered out of the stream.
    if want.is_empty() {
        want.extend(Kind::every());
    }
    want.remove(&Kind::Blind);
    Ok((scope, want))
}

/// Which scope this watch answers for, and how it came to be that one.
///
/// A seat needs no argument, which is the case that matters: anything a
/// governor runs on repeat is paid for in context on every request, and a verb
/// that needs its own scope typed is a verb whose invocation is longer than
/// most of its output.
fn scope_of(store: &Store, named: Option<String>) -> Result<Scope, String> {
    let governors = store.governors();
    if let Some(p) = named {
        let index = Index::new(store.projects());
        let name = index.find(&p).map(|x| x.id.clone()).unwrap_or(p);
        // A seat on the scope somebody named is still a seat, so the routing
        // rule applies and a governor naming its own project gets the same
        // answer it would have got by naming nothing.
        let workspace = governors
            .get(&name)
            .and_then(|r| r.get("workspace"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let here = herdr::Env::read().workspace_id.unwrap_or_default();
        return Ok(Scope { seated: !workspace.is_empty() && workspace == here, name, workspace, all: false });
    }
    let Some(ws) = herdr::Env::read().workspace_id else {
        return Err("wsp: no scope. Run this in the seat's workspace, or say `wsp watch -p <project>`".into());
    };
    match cmd_govern::governs(&governors, &ws) {
        Some(name) => Ok(Scope { name, seated: true, workspace: ws, all: false }),
        None => Err("wsp: this workspace is nobody's seat, so there is no scope to watch.\n\
                     \x20    `wsp watch <project>` watches one, `wsp govern <project>` takes the seat"
            .into()),
    }
}

fn spec(store: &Store, args: &Args) -> Result<Spec, String> {
    let (named, want) = split(args)?;
    let dur = |name: &str, default: i64| -> Result<i64, String> {
        match args.get(name) {
            None => Ok(default),
            Some(v) if v == "off" || v == "never" => Ok(i64::MAX),
            Some(v) => duration(&v).ok_or_else(|| format!("wsp: --{name} {v} is not a duration — try 30s, 5m, 2h")),
        }
    };
    Ok(Spec {
        scope: scope_of(store, named)?,
        want,
        about: args.get("about"),
        every: dur("every", EVERY)?,
        settle: dur("settle", SETTLE)?,
        heartbeat: dur("heartbeat", HEARTBEAT)?,
        stop_after: match args.get("for") {
            None => None,
            Some(v) => Some(duration(&v).ok_or_else(|| format!("wsp: --for {v} is not a duration"))?),
        },
        until: args.get("until"),
        json: args.json(),
    })
}

/// Whether the thing this watch was told to wait for is out.
///
/// **An exit condition is part of the subscription.** Every one of the four
/// monitors the worklist seat ran ended when its group was out, rather than
/// staying armed after being answered — a subscription with no natural end is
/// one somebody has to remember to cancel, and nobody does.
///
/// One flag and three subjects, because "is it out" is one question: a *task*
/// is out when it leaves the open statuses, and a *project or list* is out when
/// nothing in it is open and no agent is holding one of its tasks. The second
/// clause matters — a group whose last task reached `review` is out of work and
/// not out of agents, and ending the watch there would end it exactly when the
/// review is about to be handed back.
fn is_out(store: &Store, scope: &Scope, needle: &str) -> bool {
    let tasks = store.tasks();
    if let Some(t) = tasks.iter().find(|t| t.id == needle) {
        return !t.status().is_open();
    }
    let index = Index::new(store.projects());
    let lists = worklist::Running::read(store);
    let name = index.find(needle).map(|p| p.id.clone()).unwrap_or_else(|| needle.to_string());
    let asked = Scope { name, seated: false, workspace: scope.workspace.clone(), all: false };
    let members: Vec<&Task> = tasks
        .iter()
        .filter(|t| in_scope(&asked, &index, &store.governors(), &lists, t))
        .collect();
    if members.iter().any(|t| t.status().is_open()) {
        return false;
    }
    let bound: BTreeSet<String> = store
        .bindings()
        .values()
        .filter_map(|b| b.get("task_id").and_then(Value::as_str).map(str::to_string))
        .collect();
    !members.iter().any(|t| bound.contains(&t.id))
}

/// Why a run ended. Every one of them is printed; none of them is silent.
enum Over {
    SeatVacated,
    Elapsed,
    Out(String),
    StoreGone,
}

impl Over {
    fn said(&self) -> String {
        match self {
            Over::SeatVacated => "the seat was vacated — nothing left to watch for".into(),
            Over::Elapsed => "the time asked for is up".into(),
            Over::Out(what) => format!("{what} is out — no open work and nobody holding it"),
            Over::StoreGone => "the store is gone".into(),
        }
    }

    fn code(&self) -> i32 {
        match self {
            Over::StoreGone => 1,
            _ => 0,
        }
    }
}

pub fn watch(store: &Store, args: &Args) -> i32 {
    if args.has("status") {
        return status(store, args);
    }
    if let Some(key) = args.get("forget") {
        let gone = store.clear_watch(&key);
        println!("{}", match gone {
            true => format!("forgot the watch record for {key}"),
            false => format!("no watch registered as {key}"),
        });
        return 0;
    }
    let spec = match spec(store, args) {
        Ok(s) => s,
        Err(why) => {
            eprintln!("{why}");
            return 2;
        }
    };
    let mut poll = Poll::new(store, spec.scope.clone(), spec.want.clone(), spec.about.clone());
    if args.has("now") {
        return level_read(&mut poll, &spec);
    }
    if args.has("once") {
        return once(store, &mut poll, &spec);
    }
    run(store, &mut poll, &spec)
}

/// `wsp watch --now` — the full level read, on its own.
///
/// **The primitive, and the answer to `robustness-075`.** It is correct after
/// any disconnection, any missed tick and any restart, because it derives
/// everything from state rather than from what it was told. It is also the one
/// call that answers *nothing is up* **positively**, which is the whole
/// difference between a watcher that is quiet and a watcher that is broken.
fn level_read(poll: &mut Poll, spec: &Spec) -> i32 {
    let now = poll.sample();
    let at = util::epoch_secs();
    let stamp = util::now_iso();
    if spec.json {
        for s in &now {
            println!("{}", s.envelope(Edge::Up, &spec.scope.name, &stamp));
        }
        return 0;
    }
    let p = Paint::new();
    if now.is_empty() {
        println!("{}", aside(at, &format!("nothing is up on {} — read just now", spec.scope.name), &p));
        return 0;
    }
    for s in &now {
        println!("{}", line(&Emit { edge: Edge::Up, signal: s.clone(), held: 0 }, at, &p));
    }
    0
}

/// `wsp watch --once` — one tick against the ledger the last one left.
///
/// The pull half of the same subscription, for a caller that does not want to
/// hold a process: a hook, a cron, a governor between turns. Same source, same
/// diff, same vocabulary; the only difference is that the loop is somebody
/// else's.
///
/// **Its own ledger, keyed apart from the loop's.** A pull caller and a running
/// watch in one pane are two subscribers, and a shared ledger would hand each
/// piece of news to whichever asked first — so a governor reading `--once`
/// between turns would silently lose whatever its background watch had already
/// swallowed. Two subscriptions, two ledgers, each told everything.
fn once(store: &Store, poll: &mut Poll, spec: &Spec) -> i32 {
    let key = format!("once:{}", watch_key());
    let at = util::epoch_secs();
    let now = poll.sample();
    let rec = store.watches().get(&key).cloned().unwrap_or(Value::Null);
    let known = rec.get("ledger").is_some();
    let mut ledger = match rec.get("ledger") {
        Some(v) => Ledger::of_json(v),
        None => Ledger::default(),
    };
    let emits = match known {
        true => ledger.advance(&now, at, spec.settle),
        false => ledger.prime(&now, at),
    };
    say(&emits, at, spec);
    if !known && !spec.json {
        println!(
            "{}",
            aside(
                at,
                &format!("primed on {} · {} standing · from here, only what changes", spec.scope.name, ledger.standing()),
                &Paint::new()
            )
        );
    }
    // pid 0, deliberately: this process is already over. A pull ledger is not a
    // reporter that can die, so it must never read as one — see
    // [`Registered::watching`].
    register_as(store, &key, spec, &ledger, 1, 0);
    0
}

/// The loop.
fn run(store: &Store, poll: &mut Poll, spec: &Spec) -> i32 {
    let p = Paint::new();
    let key = watch_key();
    let started = util::epoch_secs();
    let mut ledger = Ledger::default();
    let mut ticks: u64 = 0;
    let mut last_beat = started;

    // The opening line, and it is item 1 of the five: a watcher that says
    // nothing at all from the first second is indistinguishable from one that
    // never started. It names the scope, the interval and the standing count,
    // which between them make the next hour of silence mean something.
    let first = poll.sample();
    let opening = ledger.prime(&first, started);
    if !spec.json {
        println!(
            "{}",
            aside(
                started,
                &format!(
                    "watching {} · every {} · {} standing · {}",
                    spec.scope.name,
                    util::duration_human(spec.every),
                    ledger.standing(),
                    spec.want.iter().map(|k| k.word()).collect::<Vec<_>>().join(" ")
                ),
                &p
            )
        );
    }
    say(&opening, started, spec);
    register(store, &key, spec, &ledger, ticks);

    let over = loop {
        std::thread::sleep(std::time::Duration::from_secs(spec.every.max(1) as u64));
        let at = util::epoch_secs();
        ticks += 1;

        if !store.exists() {
            break Over::StoreGone;
        }
        let emits = ledger.advance(&poll.sample(), at, spec.settle);
        say(&emits, at, spec);
        register(store, &key, spec, &ledger, ticks);

        // Item 2 of the five. One line, and it carries the standing count
        // rather than a pulse, so it is a level read in miniature: `0 standing`
        // is a positive statement that nothing is wrong.
        if spec.heartbeat != i64::MAX && at - last_beat >= spec.heartbeat && !spec.json {
            last_beat = at;
            println!(
                "{}",
                aside(
                    at,
                    &format!(
                        "watching {} · {} · {} standing",
                        spec.scope.name,
                        util::duration_human(at - started),
                        ledger.standing()
                    ),
                    &p
                )
            );
        }

        if spec.stop_after.is_some_and(|s| at - started >= s) {
            break Over::Elapsed;
        }
        if let Some(what) = &spec.until {
            if is_out(store, &spec.scope, what) {
                break Over::Out(what.clone());
            }
        }
        // A seat's subscription ends when the seat does. Free, and it is the
        // natural end the flag asks callers to supply.
        if spec.scope.seated && cmd_govern::governs(&store.governors(), &spec.scope.workspace).as_deref()
            != Some(spec.scope.name.as_str())
        {
            break Over::SeatVacated;
        }
    };

    store.clear_watch(&key);
    let at = util::epoch_secs();
    // Item 5. There is no silent return from this verb: an ending that is not
    // said reads exactly like the process that died without saying anything,
    // which is the fault the whole file is against.
    if spec.json {
        println!("{}", json!({ "shape": "signal", "signal": "watch-over", "to": spec.scope.name, "text": over.said(), "at": util::now_iso() }));
    } else {
        println!("{}", aside(at, &format!("stopped watching {} — {}", spec.scope.name, over.said()), &p));
    }
    over.code()
}

fn say(emits: &[Emit], at: i64, spec: &Spec) {
    let p = Paint::new();
    let stamp = util::now_iso();
    for e in emits {
        match spec.json {
            true => println!("{}", e.signal.envelope(e.edge, &spec.scope.name, &stamp)),
            false => println!("{}", line(e, at, &p)),
        }
    }
    // A watch is read by an agent whose stdout is a pipe, and a pipe buffers.
    // News held in a buffer until the next line arrives is news that is late by
    // however long the fleet stays quiet, which on a quiet night is the whole
    // night.
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

/// How a watch is named in the register.
///
/// The pane, because that is what a person reading `--status` would go and look
/// at. A watch started outside a pane has only its pid, which is still enough
/// to answer the one question the register exists for.
fn watch_key() -> String {
    match herdr::Env::read().pane_id {
        Some(p) if !p.is_empty() => p,
        _ => format!("pid:{}", std::process::id()),
    }
}

fn register(store: &Store, key: &str, spec: &Spec, ledger: &Ledger, ticks: u64) {
    register_as(store, key, spec, ledger, ticks, std::process::id())
}

fn register_as(store: &Store, key: &str, spec: &Spec, ledger: &Ledger, ticks: u64, pid: u32) {
    store.set_watch(
        key,
        json!({
            "scope": spec.scope.name,
            "seated": spec.scope.seated,
            "pid": pid,
            "host": util::hostname(),
            "tick": util::now_iso(),
            "every": spec.every,
            "ticks": ticks,
            "standing": ledger.standing(),
            "watching": spec.want.iter().map(|k| k.word()).collect::<Vec<_>>(),
            "ledger": ledger.json(),
        }),
    );
}

/// `wsp watch --status` — who is watching what, and whether they still are.
fn status(store: &Store, args: &Args) -> i32 {
    let watches = registered(store);
    if args.json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&watches
                .iter()
                .map(|w| json!({ "key": w.key, "scope": w.scope, "pid": w.pid, "tick": w.tick, "stale": w.stale(), "watching": w.watching() }))
                .collect::<Vec<_>>())
            .unwrap_or_default()
        );
        return 0;
    }
    let p = Paint::new();
    if watches.is_empty() {
        println!("{}", p.dim("nobody is watching · `wsp watch` starts one"));
        return 0;
    }
    let live = crate::place_super::alive(&watches.iter().filter(|w| w.watching()).map(|w| w.pid).collect::<Vec<_>>());
    for w in &watches {
        let dead = w.watching() && !live.contains(&w.pid);
        let note = match (dead, w.stale(), w.watching()) {
            (true, _, _) => p.red("the process is gone"),
            (false, true, _) => p.red("has stopped ticking"),
            // Said rather than left blank: a pull ledger looks exactly like a
            // running watch in this list, and the reader is here to find out
            // which of their watches is still going.
            (false, false, false) => p.dim(&format!("{} standing · read on demand", w.standing)),
            (false, false, true) => p.dim(&format!("{} standing", w.standing)),
        };
        println!(
            "{}  {}  {}  {}",
            p.bold(&util::pad(&w.scope, 12)),
            p.dim(&util::pad(&w.key, 10)),
            p.dim(&util::pad(&format!("ticked {} ago", util::duration_human(util::since(&w.tick))), 22)),
            note
        );
    }
    0
}

// ---------------------------------------------------------------------------
// what is deliberately not here
// ---------------------------------------------------------------------------
//
// `core-003`'s fifth requirement — *something landed on the trunk touching a
// file my lane is in* — is not a signal this file computes, and that is a
// decision rather than an omission.
//
// It is the only one of the five that is not a predicate over wsp's own state.
// Both halves of it are git: what my lane is holding is `git status --short`
// per live worktree, and what landed is a diff against the trunk. `wsp-095`
// Part 12 costs it honestly — git-priced, so it belongs on the daemon's
// fifteen-minute refresh or on a `task-landed` event, and **that event does not
// exist**: `src/cmd_checkout.rs` contains zero `log_event` calls, so a branch
// reaching the trunk is invisible to everything in wsp. Putting a `git status`
// per worktree inside a sixty-second tick would make this verb the most
// expensive thing on the machine to answer a question it cannot answer well.
//
// So it waits for the one line `wsp-095` Part 12 asks for, and lands here as
// one more [`Kind`] with no change to anything above. That is the point of a
// vocabulary of named predicates: adding to it later is a new variant, not a
// new interface.

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(kind: Kind, subject: &str) -> Signal {
        Signal::new(kind, subject, "because")
    }

    /// The bug the second hand-built monitor shipped: `comm` against input that
    /// was not globally sorted reported the whole backlog as new on the first
    /// tick. Priming is the fix, and this asserts the *behaviour* rather than
    /// the sort — the ledger is a map, so there is no sorted input to get wrong.
    #[test]
    fn a_watch_says_nothing_about_what_was_already_true_when_it_started() {
        let now = vec![sig(Kind::Review, "a-1"), sig(Kind::Blocked, "a-2")];
        let mut l = Ledger::default();
        assert!(l.prime(&now, 0).is_empty());
        assert_eq!(l.standing(), 2, "primed levels are still up, they are just not news");
        assert!(l.advance(&now, 60, SETTLE).is_empty(), "nothing changed, so nothing is said");
    }

    /// Emit on change, never on state. The difference between a watcher and one
    /// event per poll for ever.
    #[test]
    fn a_level_that_stays_up_is_said_once_and_not_again() {
        let now = vec![sig(Kind::Review, "a-1")];
        let mut l = Ledger::default();
        l.prime(&[], 0);
        assert_eq!(l.advance(&now, 60, SETTLE).len(), 1);
        for tick in 2..10 {
            assert!(l.advance(&now, tick * 60, SETTLE).is_empty(), "tick {tick} said it again");
        }
    }

    /// A stall's wording carries a duration and the key does not. Without that
    /// split every tick is a new fact and the watcher is a change feed over its
    /// own output.
    #[test]
    fn the_wording_of_a_signal_may_change_without_it_becoming_news_again() {
        let mut l = Ledger::default();
        l.prime(&[], 0);
        let first = vec![Signal::new(Kind::NeedsAPerson, "a-1", "w1:p1 · no turn for 5m")];
        let later = vec![Signal::new(Kind::NeedsAPerson, "a-1", "w1:p1 · no turn for 40m")];
        assert_eq!(l.advance(&first, 60, 0).len(), 1);
        assert!(l.advance(&later, 3000, 0).is_empty());
    }

    /// **Silence is not evidence.** A tick that could not reach herdr has no
    /// agent half at all, so the levels derived from it are absent — and a diff
    /// that read absence as resolution would report every stall on the machine
    /// as cleared, then report them all as new when herdr came back.
    ///
    /// One herdr restart, two edges per standing signal, none of which
    /// happened. Through `wsp watch` a person discounts it, having just seen
    /// `blind` go up on the line above; through `attention`'s hook it is a
    /// phone buzzing twice per stalled agent at whatever hour herdr restarted.
    #[test]
    fn going_blind_holds_the_levels_it_can_no_longer_read_rather_than_clearing_them() {
        let mut l = Ledger::default();
        l.prime(&[], 0);
        let seeing = vec![sig(Kind::NeedsAPerson, "a-1"), sig(Kind::Review, "a-2")];
        assert_eq!(l.advance(&seeing, 60, 0).len(), 2);

        // herdr goes away. The store half still answers, so `review` is a real
        // reading and stays; the agent half is unreadable, so it is held.
        let blind = vec![sig(Kind::Review, "a-2"), sig(Kind::Blind, "herdr")];
        let out = l.advance(&blind, 120, 0);
        assert_eq!(out.len(), 1, "only `blind` itself is news");
        assert_eq!(out[0].signal.kind, Kind::Blind);
        assert_eq!(l.standing(), 3, "the stall is still up, it is just not being looked at");

        // And back. Nothing is re-announced, because nothing changed.
        let out = l.advance(&seeing, 180, 0);
        assert_eq!(out.len(), 1, "the stall arrived again");
        assert_eq!(out[0].signal.kind, Kind::Blind, "and it is `blind` clearing");
        assert_eq!(out[0].edge, Edge::Down);
    }

    /// The other half of the same rule: a stall that genuinely resolved while
    /// wsp was blind is reported on the first tick that can see it, one tick
    /// late. Holding is a deferral, never a floor.
    #[test]
    fn a_level_that_went_away_while_blind_is_reported_once_the_view_comes_back() {
        let mut l = Ledger::default();
        l.prime(&[], 0);
        l.advance(&[sig(Kind::NeedsAPerson, "a-1")], 60, 0);
        l.advance(&[sig(Kind::Blind, "herdr")], 120, 0);

        let out = l.advance(&[], 180, 0);
        let cleared: Vec<Kind> = out.iter().map(|e| e.signal.kind).collect();
        assert!(cleared.contains(&Kind::NeedsAPerson), "the stall's clearing was owed and paid");
        assert_eq!(l.standing(), 0);
    }

    /// An agent is "stopped" for a few seconds between every pair of turns, so
    /// the join alone would fire on nearly every agent on nearly every tick.
    /// The settle is the watcher's one advantage over a snapshot: it can
    /// measure how long the predicate has actually held.
    #[test]
    fn a_stall_must_hold_before_it_is_worth_a_line() {
        let mut l = Ledger::default();
        let stalled = vec![sig(Kind::NeedsAPerson, "a-1").settling()];
        l.prime(&[], 0);
        assert!(l.advance(&stalled, 60, 300).is_empty(), "one minute in, this is a gap between turns");
        assert!(l.advance(&stalled, 240, 300).is_empty());
        assert_eq!(l.advance(&stalled, 360, 300).len(), 1, "six minutes in, nothing is going to restart it");
    }

    /// The worklist-004 case: a modal has the keyboard and one keypress fixes
    /// it. Waiting five minutes to mention that would be waiting five minutes
    /// to say a word — `quiet_note` makes the same exception for the same
    /// reason.
    #[test]
    fn a_prompt_only_a_person_can_answer_is_said_at_once() {
        let mut l = Ledger::default();
        l.prime(&[], 0);
        let prompt = vec![sig(Kind::NeedsAPerson, "a-1").loud()];
        let out = l.advance(&prompt, 1, 300);
        assert_eq!(out.len(), 1);
        assert!(out[0].signal.loud, "and it is the loud reading, which the envelope sends as `direction`");
    }

    /// A stall that never settled was never said, so its going away is not news
    /// either. Reporting it would tell a governor about an agent it was never
    /// told about.
    #[test]
    fn a_level_that_goes_away_before_it_settled_is_never_mentioned_in_either_direction() {
        let mut l = Ledger::default();
        l.prime(&[], 0);
        let stalled = vec![sig(Kind::NeedsAPerson, "a-1").settling()];
        assert!(l.advance(&stalled, 60, 300).is_empty());
        assert!(l.advance(&[], 120, 300).is_empty());
        assert_eq!(l.standing(), 0);
    }

    /// A stall that resolved itself changes what a governor does next — do not
    /// go and prod that agent — and a task leaving `review` is nearly always
    /// the reader having just closed it.
    #[test]
    fn only_the_levels_somebody_else_may_have_taken_down_are_reported_when_they_clear() {
        let mut l = Ledger::default();
        l.prime(&[], 0);
        let up = vec![sig(Kind::NeedsAPerson, "a-1"), sig(Kind::Review, "a-2")];
        assert_eq!(l.advance(&up, 60, 0).len(), 2);
        let out = l.advance(&[], 120, 0);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].signal.kind, Kind::NeedsAPerson);
        assert_eq!(out[0].edge, Edge::Down);
    }

    /// Item 4 of the five ways silence lies. Everything else a blind watch does
    /// not print is meaningless, so this one is exempt from the rule that a
    /// starting watch is quiet.
    #[test]
    fn a_watch_that_is_blind_from_its_first_tick_says_so_even_while_priming() {
        let mut l = Ledger::default();
        let out = l.prime(&[sig(Kind::Review, "a-1"), sig(Kind::Blind, "herdr")], 0);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].signal.kind, Kind::Blind);
    }

    /// `--once` is the same subscription without a process, so its ledger has
    /// to survive the exit — and what has already been said must not be said
    /// again to the caller that comes back for it.
    #[test]
    fn a_ledger_read_back_from_state_remembers_what_has_already_been_said() {
        let mut l = Ledger::default();
        l.prime(&[], 0);
        let up = vec![Signal::new(Kind::Review, "a-1", "waiting on you")];
        assert_eq!(l.advance(&up, 60, 0).len(), 1);

        let mut back = Ledger::of_json(&l.json());
        assert!(back.advance(&up, 120, 0).is_empty(), "it has already been said");
    }

    /// The defect running it found. A level that has *gone* is absent from the
    /// read by definition, so a ledger that rebuilt its signals from the read
    /// had nothing to rebuild it from and dropped it silently — and a flag
    /// raised and lowered between two `--once` calls printed the raise and
    /// never the lowering. *A hand in my scope lowered by somebody else* is one
    /// of the five things a governor asked to be told.
    #[test]
    fn a_level_that_went_away_between_two_pull_reads_is_still_reported_as_cleared() {
        let mut l = Ledger::default();
        l.prime(&[], 0);
        let up = vec![Signal::new(Kind::Flag, "a-1", "can I take this?")];
        assert_eq!(l.advance(&up, 60, 0).len(), 1);

        let mut back = Ledger::of_json(&l.json());
        let out = back.advance(&[], 120, 0);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].edge, Edge::Down);
        assert_eq!(out[0].signal.kind, Kind::Flag);
    }

    /// A pull-mode ledger has no process to lose, so nothing may report it as
    /// a watcher that died — that false alarm is how a `doctor` section comes
    /// to be skipped.
    #[test]
    fn a_ledger_read_on_demand_is_never_a_dead_watcher() {
        let pull = Registered {
            key: "once:w1:p1".into(),
            scope: "wsp".into(),
            pid: 0,
            host: String::new(),
            tick: util::iso_at(util::epoch_secs() - 86_400),
            every: 60,
            standing: 0,
            daemon: false,
        };
        assert!(!pull.watching());
        assert!(!pull.stale(), "a day old and still not a fault: nobody promised to tick");
    }

    /// The vocabulary is what you subscribe to, so every word in it has to be
    /// typeable and every word typed has to be in it.
    #[test]
    fn every_signal_a_bare_watch_subscribes_to_can_be_named_on_the_command_line() {
        for k in Kind::every() {
            assert_eq!(Kind::parse(k.word()), Some(k), "{}", k.word());
        }
        assert_eq!(Kind::parse("no-such-signal"), None);
    }

    /// `blind` is liveness rather than news: it is never in the subscribed set
    /// and never filtered out of the stream.
    #[test]
    fn blind_is_not_one_of_the_signals_a_governor_subscribes_to() {
        assert!(!Kind::every().contains(&Kind::Blind));
        assert!(Kind::parse("blind").is_some(), "it is still a word, so `wsp watch blind` is not a silent no-op");
    }

    /// `wsp-095` Part 4's envelope, not a second one — and a level carries no
    /// sender, no disposition and no escalation history, so those fields are
    /// absent rather than null.
    #[test]
    fn a_signal_travels_as_part_4s_envelope_with_the_fields_a_level_cannot_have_left_out() {
        let e = sig(Kind::Review, "a-1").envelope(Edge::Up, "wsp", "2026-08-19T12:00:00Z");
        assert_eq!(e["shape"], "signal");
        assert_eq!(e["signal"], "review");
        assert_eq!(e["kind"], "note");
        assert_eq!(e["from"], "wsp");
        assert_eq!(e["to"], "wsp");
        assert_eq!(e["about"], "a-1");
        for absent in ["state", "via", "waiting", "reply_to"] {
            assert!(e.get(absent).is_none(), "{absent} has no meaning on a level");
        }
    }

    /// The one derived signal `wsp-095` Part 4 promotes: a modal holding the
    /// keyboard is `direction`, everything else derived is `note`.
    #[test]
    fn the_only_derived_signal_loud_enough_to_be_direction_is_the_one_a_keypress_fixes() {
        let quiet = sig(Kind::NeedsAPerson, "a-1").envelope(Edge::Up, "wsp", "");
        let loud = sig(Kind::NeedsAPerson, "a-1").loud().envelope(Edge::Up, "wsp", "");
        assert_eq!(quiet["kind"], "note");
        assert_eq!(loud["kind"], "direction");
    }

    fn argv(line: &[&str]) -> crate::Args {
        crate::Args::parse(line.iter().map(|s| (*s).to_string()).collect())
    }

    /// The one positional this verb has is ambiguous by design — a bare word is
    /// a project, and a bare word that names a signal is a signal — so the
    /// split is asserted rather than assumed.
    #[test]
    fn a_bare_word_is_the_scope_unless_it_is_the_name_of_a_signal() {
        let (scope, want) = split(&argv(&["watch", "robustness"])).unwrap();
        assert_eq!(scope.as_deref(), Some("robustness"));
        assert_eq!(want.len(), Kind::every().len(), "no signal named is every signal");

        let (scope, want) = split(&argv(&["watch", "review"])).unwrap();
        assert_eq!(scope, None);
        assert_eq!(want, [Kind::Review].into_iter().collect::<BTreeSet<_>>());

        let (scope, want) = split(&argv(&["watch", "robustness", "review", "flag"])).unwrap();
        assert_eq!(scope.as_deref(), Some("robustness"));
        assert_eq!(want, [Kind::Review, Kind::Flag].into_iter().collect::<BTreeSet<_>>());
    }

    /// `wsp tag <id> +dsp -ui` read `-ui` as a flag, added `dsp`, and exited 0
    /// having silently dropped the removal. A signal name that does not exist
    /// must not subscribe to everything and report success.
    #[test]
    fn a_word_that_is_neither_a_scope_nor_a_signal_is_refused_rather_than_ignored() {
        let e = split(&argv(&["watch", "-p", "wsp", "nonsense"])).unwrap_err();
        assert!(e.contains("nonsense"), "{e}");
        assert!(e.contains("needs-a-person"), "and it says what the words are: {e}");
    }

    #[test]
    fn a_duration_may_be_typed_in_seconds_minutes_or_hours_and_a_bare_number_is_seconds() {
        assert_eq!(duration("30"), Some(30));
        assert_eq!(duration("30s"), Some(30));
        assert_eq!(duration("5m"), Some(300));
        assert_eq!(duration("2h"), Some(7200));
        assert_eq!(duration("0m"), None, "a watch that ticks every no seconds is a spin");
        assert_eq!(duration("soon"), None);
    }

    /// Three ticks and a floor. One slow tick is not a dead watcher, and a
    /// diagnostic that cries wolf is one nobody reads.
    #[test]
    fn a_watch_is_only_called_stale_after_several_missed_ticks() {
        let at = |ago: i64| {
            let mut r = Registered {
                key: "w1:p1".into(),
                scope: "wsp".into(),
                pid: 1,
                host: String::new(),
                tick: String::new(),
                every: 60,
                standing: 0,
                daemon: false,
            };
            r.tick = util::iso_at(util::epoch_secs() - ago);
            r
        };
        assert!(!at(60).stale());
        assert!(!at(120).stale());
        assert!(at(600).stale());
    }
}
