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
//! # The one predicate whose subject wrote itself down
//!
//! [`Kind::Unanswered`] is `robustness-051`'s own half and it is the odd one
//! here. Every other variant is wsp *inferring* from joined state that a person
//! is probably wanted; this one is an agent having said so, in words, through
//! `wsp ask` — with [`crate::message::Waiting`] naming the pane that is sitting
//! still until it is answered. So the line carries the question rather than the
//! task's title, and the id `wsp answer` takes.
//!
//! It also settles the question `worklist-013` cost 2h14m: the answer and the
//! record were two acts and only one of them was on the path of getting the
//! work moving, so the hand stayed up on finished work while that seat's own
//! watch went on reporting it. A level cannot have that failure — nothing
//! lowers it, it stops being true — and `cmd_message`'s module docs make the
//! same argument from the other end. [`crate::message::open_questions`] wrote
//! the join and named this task in its docstring; this is the caller it wanted,
//! and the one thing added to it is the census, which is what says whether the
//! asker is still turning.
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
//! # Silence must not look like success, in six places
//!
//! The failure this file exists to prevent has six separate causes and each
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
//! 6. **The subject moved out from under it.** A correct reporter, ticking, on
//!    the current build, watching a scope it answers for exactly — and the
//!    routing changed, so a standing level is now somebody else's. That is
//!    [`Edge::Left`] and it is `worklist-039`: it was found on `review`, where
//!    the level left one seat's scope in silence, and driving it turned up the
//!    louder half on `flag`, where the diff had a word for a level going away
//!    and said `cleared` for a hand that is still up. The five above are all
//!    faults of the *reporter*; this one is a fault of the **scope**, which is
//!    why none of them caught it and why it needed a sixth entry rather than a
//!    better answer to one of theirs.
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

/// How long a spooled fact may wait before it is a wake by itself.
///
/// **Four hours, and the number is measured rather than picked.** Replayed
/// against the 134 wakes one seat actually took on 2026-08-19/20 — see
/// [`the_measured_session_is_replayed_and_the_wake_count_is_counted`] — the
/// table alone wakes 70 times. Adding escalation costs, over that same corpus:
///
/// | `--defer-max` | wakes | over no escalation at all |
/// |---|---|---|
/// | 1h | 78 | +8 |
/// | 2h | 74 | +4 |
/// | 4h | 71 | +1 |
///
/// So four hours buys the guarantee for one extra context read in two days,
/// where an hour buys the same guarantee for eight. The other end of it is a
/// fleet with nothing to say at all, where this is the *whole* wake budget: six
/// a day, each carrying everything held.
///
/// **And four hours of latency is affordable precisely because of what is in
/// here.** The spool holds what [`Line::disposition`] argued is not worth
/// acting on now — a heartbeat, a level that went away, a level somebody else
/// answers for, and a `flag`, which `hooks/on-attention-raised` has already
/// delivered to a person for free. None of it is time-critical by construction.
/// Escalation is not a delivery deadline; it is a bound on how long this can be
/// silent, which is the one thing `core-014` will not allow to be unbounded.
const DEFER_MAX: i64 = 4 * 60 * 60;

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
/// new interface — as [`Kind::Unanswered`] was, once the message record existed
/// to be read. Four of `core-003`'s five are below. The fifth — *something
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
    /// A hand is up on a task this seat is the addressee for, and **nothing is
    /// owed back**: somebody wrote it, and reading it is the whole of the
    /// disposition. [`crate::message::Shape::Notification`].
    Flag,
    /// The other half of the same population: a hand that is **waiting for a
    /// sentence**. [`crate::message::Shape::Question`], and any record beside
    /// it this build cannot read.
    ///
    /// **The one predicate here whose subject wrote itself down.** Every other
    /// variant is wsp inferring from state that somebody is probably wanted;
    /// this one is an agent having said so, in words, with
    /// [`crate::message::Waiting`] naming the pane that is sitting still until
    /// it is answered. `worklist-013`'s question was answered in minutes and
    /// its raised hand stood for 2h14m, because the answer and the record were
    /// two acts and only one of them was on the path of getting the work
    /// moving; a level cannot have that failure, because it goes down when the
    /// question closes and there is nothing to remember to lower.
    ///
    /// **Two words rather than one, because `wsp-095` Part 3 is that a
    /// notification and a question are different animals.** Collapsing them
    /// would be lossy in the direction that matters: a governor reading `flag`
    /// could not tell whether an agent was sitting still behind it. So
    /// `wsp flag "…"` arrives as [`Kind::Flag`] and `wsp ask` and
    /// `wsp flag --ask claim` arrive here, and the word itself says what is
    /// owed.
    Unanswered,
    /// A binding whose pane herdr no longer lists, or whose pane is alive with
    /// the agent gone. Covers *an agent died* and *an agent never started*,
    /// which look identical from here and want the same first move.
    AgentGone,
    /// **A seat that has stopped answering** — the one failure the fleet had no
    /// way to state, because [`cmd_govern::needs_a_person`] exempts a seat by
    /// construction and every other variant here takes a *task* as its subject.
    ///
    /// The subject is the seat's scope, and this is the only [`Signal`] in the
    /// vocabulary that is about the reporter rather than about the work.
    /// `worklist-041`; the predicate, and the live evidence that the one the row
    /// specified was false, are on [`stalled_seats`].
    ///
    /// **Produced only by a reader with no boundary**, which is
    /// [`Scope::machine`] and therefore [`crate::attention`]'s pass alone. That
    /// is not a rule about who may subscribe — it is what the predicate means. A
    /// seated scope *is* "what is addressed to me", so a seat asking this of
    /// another seat would be asking about a population it cannot see, and a seat
    /// asking it of itself is the one agent whose answer is worthless. The
    /// daemon is the only thing in wsp that looks when nobody asked, and this is
    /// the level that matters when nobody is.
    SeatStalled,
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
            Kind::Unanswered => "unanswered",
            Kind::AgentGone => "agent-gone",
            Kind::SeatStalled => "seat-stalled",
            Kind::Blind => "blind",
        }
    }

    /// Everything a bare `wsp watch` subscribes to.
    pub(crate) fn every() -> Vec<Kind> {
        vec![
            Kind::NeedsAPerson,
            Kind::Review,
            Kind::Blocked,
            Kind::Flag,
            Kind::Unanswered,
            Kind::AgentGone,
            Kind::SeatStalled,
        ]
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
        matches!(
            self,
            Kind::NeedsAPerson
                | Kind::Flag
                | Kind::Unanswered
                | Kind::AgentGone
                // *The run is moving again* is the whole reason a reader was
                // told it had stopped: the repair is to go and prod a seat, and
                // one that started turning on its own must not be prodded.
                | Kind::SeatStalled
                | Kind::Blind
        )
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
            Kind::NeedsAPerson | Kind::AgentGone | Kind::SeatStalled => true,
            // The store answers these whether or not anything else does, which
            // is what makes saying `blind` worth anything at all.
            Kind::Review | Kind::Blocked | Kind::Flag | Kind::Unanswered => false,
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
    ///
    /// [`Kind::SeatStalled`] is the second exception and the first that is a
    /// fact about the *predicate*. Every other level here says a piece of work
    /// wants somebody; that one says **nothing in a run is moving and the thing
    /// responsible for moving it is not turning**, so no member-level line
    /// beneath it will arrive later to say the same thing and nothing under it
    /// will resolve it. `note` is *act when convenient*, and this state does not
    /// improve while nobody is convenient — it is what `robustness-083` cost
    /// three agents in one day, unread, with every other signal healthy.
    fn loudness(&self) -> crate::message::Kind {
        match self {
            Kind::SeatStalled => crate::message::Kind::Direction,
            _ => crate::message::Kind::Note,
        }
    }
}

/// What class of line this is — the closed vocabulary a consumer keys on
/// instead of guessing from prose.
///
/// **`worklist-033` is why this is a field and not a shape you can recognise.**
/// Five distinct kinds of line leave this verb and four of them were written by
/// [`aside`], which marks all four with one `·`. So a monitor that had to drop
/// the heartbeat could only key on the heartbeat's *wording*, and the `grep -v`
/// it wrote ate [`REPLACED`] — a line of the same shape, carrying the one thing
/// that watch could not afford to lose. The watch then ran eight hours and four
/// installs on stale logic. The consumer was guessing a class from words, and
/// the fix is not a better regex at the consumer: the emitter already knew the
/// answer and never said it.
///
/// One word each, spelled the way [`Kind::word`] is and for the same reason —
/// both readers are counting characters. One of them is a machine matching a
/// field; the other is a person reading this stream down its second column.
///
/// **Closed, and in one place.** These five words exist here and nowhere else.
/// A class spelled out at a call site would reproduce the defect one layer
/// down: a sixth kind of line added next year would print a word no consumer's
/// table has, and nothing here would fail until it mattered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Class {
    /// The stream starting: scope, interval, standing count. Also what a
    /// `--now` read and a `--once` prime are — both say where things stand
    /// before any diff exists, which is what this word means. It is not a
    /// sixth word for them, because a reader acts on the two identically:
    /// this is the baseline, and what follows is change against it.
    Open,
    /// One edge of news. The only class whose line is a [`Signal`], and so the
    /// only one whose JSON form is [`Signal::envelope`] rather than
    /// [`Class::envelope`].
    News,
    /// The heartbeat: nothing changed, and this watch is still looking.
    Beat,
    /// A new build landed under this watch — see [`REPLACED`]. Its own word
    /// rather than a loud [`Class::Beat`], because that collision *is*
    /// `worklist-033`.
    Replaced,
    /// The stream ending, and why.
    Over,
}

impl Class {
    /// The word, in the JSON field and in the column a reader scans.
    pub(crate) fn word(&self) -> &'static str {
        match self {
            Class::Open => "open",
            Class::News => "news",
            Class::Beat => "beat",
            Class::Replaced => "replaced",
            Class::Over => "over",
        }
    }

    /// Back from the word, for a spool written by another build.
    fn parse(word: &str) -> Option<Class> {
        Class::every().into_iter().find(|c| c.word() == word)
    }

    /// The whole vocabulary, so a reader can be handed it and a test can assert
    /// it is closed.
    pub(crate) fn every() -> [Class; 5] {
        [Class::Open, Class::News, Class::Beat, Class::Replaced, Class::Over]
    }

    /// The JSON form of a line that is **not** news.
    ///
    /// Four fields, asked from here rather than written out at each of the four
    /// call sites, for the reason [`Signal::envelope`] gives about hooks and
    /// `--json` being one document: a shape written in four places is four
    /// things to keep in step, and what this file has paid for repeatedly is a
    /// word spelled out in a second place drifting from the first.
    ///
    /// **No `shape`.** [`crate::message::Shape`] names three things that exist
    /// in the world — a level, a notification, a question. These four lines are
    /// none of them; they are the stream talking about itself. The
    /// `shape: "signal", signal: "watch-over"` the ending used to print was
    /// this row's own defect wearing the `signal` field: a word that is not in
    /// [`Kind::word`]'s vocabulary, sitting in the field a consumer reads that
    /// vocabulary out of. `class` is where it belongs, and a consumer keying on
    /// `signal == "watch-over"` was keying on prose with extra steps.
    fn envelope(&self, at: i64, to: &str, text: &str) -> Value {
        json!({ "class": self.word(), "at": util::iso_at(at), "to": to, "text": text })
    }
}

/// What is to be done with a line, under a mode whose reader is asleep.
///
/// **Two, and the absence of a third is the whole row.** `wsp watch`'s stdout
/// *is* the wake: a line printed re-invokes an agent and its whole conversation
/// is re-read, measured at 208k tokens on the seat that filed `core-014` —
/// the same price for a heartbeat as for a question from Ed, because the
/// context is the cost and not the payload. So printing is not free and the
/// only lever wsp has is which lines it prints.
///
/// The tempting third is *drop*, and it is the one thing this must never have.
/// A filter that drops has exactly one failure mode and it is silence, which is
/// indistinguishable from health — the fault this whole file is written
/// against, with six named guards against it already. Triage here decides
/// **when**, never **whether**.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Disposition {
    /// Print it now, and carry the spool out under it. A claim that this line
    /// was worth a context read.
    Wake,
    /// Hold it. It rides the next wake at no extra cost, because that wake was
    /// already being paid for, and it is never lost — see [`Spool`].
    Spool,
}

/// One line, before it is written and before it is known whether it will be
/// written now.
///
/// **The type is what makes the table total.** A class and its payload cannot
/// be separated, so there is no way to ask [`Line::disposition`] about a class
/// without the thing it is a class *of*, and no way for a caller to reach
/// stdout carrying something the table has never seen. See [`Stream`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Line {
    /// One edge of news — the only class that carries a level.
    News(Emit),
    /// Everything else: the class, and the sentence it says.
    Note(Class, String),
}

impl Line {
    pub(crate) fn class(&self) -> Class {
        match self {
            Line::News(_) => Class::News,
            Line::Note(c, _) => *c,
        }
    }

    /// **The table.** Keyed on the class and the level, never on words — which
    /// is what `core-016` is for, and what `worklist-033` cost when a consumer
    /// keyed a `grep -v` on the heartbeat's prose and ate the notice that the
    /// binary underneath it had been replaced.
    ///
    /// A `match` with no wildcard on [`Kind`], deliberately: a signal kind
    /// added later must state its disposition here or fail to compile. The
    /// alternative — a chain of conditionals with a default — is how a new kind
    /// silently inherits *spool* and is never delivered, which is the drop
    /// arriving by the back door.
    ///
    /// Where a reading is genuinely unforeseeable the answer is [`Wake`].
    /// A wake that was not needed costs one context read; a silence that was
    /// wrong costs everything the row is trying to protect, and the two are not
    /// symmetric.
    ///
    /// [`Wake`]: Disposition::Wake
    pub(crate) fn disposition(&self) -> Disposition {
        use Disposition::{Spool, Wake};
        match self {
            // The governor started this watch and is awake at the moment it
            // says so. Item 1 of the six ways silence lies: a watcher that says
            // nothing from its first second is indistinguishable from one that
            // never started, and that is as true through a pipe as on a screen.
            Line::Note(Class::Open, _) => Wake,
            // `core-014` d2, and the one place the six-ways list is *traded*
            // rather than extended. Absence of a heartbeat is only legible to a
            // reader who is still reading, and the reader here is asleep by
            // design; the register is written every tick and a dead process
            // cannot fake it, which is strictly stronger.
            Line::Note(Class::Beat, _) => Spool,
            // `worklist-033` itself. The watch is now running logic that has
            // been fixed, it cannot say so twice, and only the reader can
            // repair it.
            Line::Note(Class::Replaced, _) => Wake,
            // Item 5. There is no silent ending, and under a mode built out of
            // silence least of all — it is also the line the spool rides out
            // on, so a watch that ends holding something still delivers it.
            Line::Note(Class::Over, _) => Wake,
            // Unconstructible: `News` is carried by the other variant, which is
            // what the type is for. Answered rather than `unreachable!` because
            // a panic in the disposition path would be the seventh way to be
            // silent, and this is the safe half of an asymmetric choice.
            Line::Note(Class::News, _) => Wake,
            Line::News(e) => match e.edge {
                // It went away, or somebody else answers for it now. There is
                // nothing for this reader to do about either, and `worklist-039`
                // is why the second is a line at all rather than a silence.
                Edge::Down | Edge::Left => Spool,
                Edge::Up => match e.signal.kind {
                    // Somebody is sitting still.
                    Kind::NeedsAPerson => Wake,
                    // An agent wrote a question and is stopped behind it.
                    Kind::Unanswered => Wake,
                    // A modal holding the keyboard; one keypress fixes it.
                    Kind::Blocked => Wake,
                    // The reporter's own failure, and the only level here whose
                    // subject is a governor rather than a piece of work.
                    Kind::SeatStalled => Wake,
                    // It died or it never started. Both want a first move now.
                    Kind::AgentGone => Wake,
                    // The agent's terminal verb, and the governor's own work.
                    Kind::Review => Wake,
                    // Liveness rather than news, and never filtered — the
                    // existing rule, and the difference between a quiet fleet
                    // and a watcher reporting on half a world.
                    Kind::Blind => Wake,
                    // `core-014` §4: a notification with nothing owed back, so
                    // reading it *is* the whole disposition and it can be read
                    // on the next wake. `hooks/on-attention-raised` already
                    // reaches a person for free, and a governor should be woken
                    // only for what a governor must **act** on.
                    Kind::Flag => Spool,
                },
            },
        }
    }
}

/// One line being held, and what is needed to say it later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Spooled {
    /// When it happened, not when it is delivered. A fact that waited four
    /// hours says four hours ago, because a reader acting on the clock in the
    /// line would otherwise go looking for something that moved long since.
    pub(crate) at: i64,
    /// What it was, when this build has the vocabulary to read it back.
    line: Option<Line>,
    /// What it said, always — the sentence for a note, and the whole rendered
    /// line for news, which has no sentence of its own.
    ///
    /// **The reason nothing is ever dropped on the way back in.** A spool
    /// survives an install, so it is routinely read by a *different* binary,
    /// and a downgrade meets a class or a signal word it has no enum for. The
    /// structured form is then `None` and this is what is printed: a reader can
    /// read words that this build's code cannot parse, and the alternative —
    /// `continue`, the way [`Ledger::of_json`] may safely skip a level it will
    /// re-derive next tick — would be a drop with nothing behind it to notice.
    ///
    /// The asymmetry between the two is not arbitrary. A note's class is drawn
    /// in the column from the stored *word*, so its sentence is all that is
    /// needed; a signal's line is built from a subject, an edge and a duration
    /// this build may no longer know how to read, so the only honest fallback
    /// is the line as it looked when it was written.
    text: String,
    /// The class word as it was written, for the same reason.
    class: String,
}

/// What is being held for the next wake.
///
/// **Beside the register rather than in a file of its own**, which is
/// `core-017`'s "a file per watch key, beside the register" answered the
/// cheaper way: it goes into the watch record under the same key, so it is one
/// atomic write per tick instead of two records to keep in step, `--status` and
/// `doctor` can read its depth for nothing, and it survives exactly what the
/// ledger beside it survives. See [`register_as`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Spool {
    held: Vec<Spooled>,
}

impl Spooled {
    /// A line, ready to be held.
    ///
    /// The words are rendered here rather than at flush time and stored beside
    /// the structured form — see [`Spooled::text`] for why the duplication is
    /// the point rather than an oversight.
    fn of(at: i64, line: Line) -> Spooled {
        Spooled {
            at,
            text: match &line {
                Line::Note(_, said) => said.clone(),
                Line::News(_) => render(&line, at, &util::Paint::plain()),
            },
            class: line.class().word().to_string(),
            line: Some(line),
        }
    }

    /// The line, in whichever document is being written.
    ///
    /// The stored words are the fallback and not the normal path: a build that
    /// can still read the structured form re-renders it, so the reader gets
    /// this terminal's colour and this build's wording. See [`Spooled::text`].
    fn say(&self, spec: &Spec, p: &Paint) -> String {
        match &self.line {
            Some(l) => render_for(l, self.at, spec, p),
            None if spec.json => {
                json!({ "class": self.class, "at": util::iso_at(self.at), "to": spec.scope.name, "text": self.text })
                    .to_string()
            }
            // A class this build knows, carrying a signal it does not: the
            // stored line is already whole, so it is printed as it was.
            None if self.class == Class::News.word() => self.text.clone(),
            None => column(&self.class, self.at, &self.text, &util::Paint::new()),
        }
    }
}

impl Spool {
    pub(crate) fn depth(&self) -> usize {
        self.held.len()
    }

    /// When the oldest thing here happened, which is what escalation reads.
    pub(crate) fn oldest(&self) -> Option<i64> {
        self.held.first().map(|h| h.at)
    }

    /// Everything held, in the order it happened — **taken, not copied**.
    ///
    /// The caller is expected to fail to deliver it and put it back. See
    /// [`Stream::write`]: an entry clears when something arrived, never when a
    /// send was attempted, which is the shape `core-021` needs when the thing
    /// on the other end is a pane whose turn is in flight rather than a pipe.
    fn take(&mut self) -> Vec<Spooled> {
        std::mem::take(&mut self.held)
    }

    fn put_back(&mut self, mut held: Vec<Spooled>) {
        held.extend(self.held.drain(..));
        self.held = held;
    }

    pub(crate) fn json(&self) -> Value {
        Value::Array(
            self.held
                .iter()
                .map(|h| {
                    let mut v = json!({ "at": h.at, "class": h.class, "text": h.text });
                    if let (Some(Line::News(e)), Some(o)) = (&h.line, v.as_object_mut()) {
                        o.insert("edge".into(), json!(e.edge.word()));
                        o.insert("held".into(), json!(e.held));
                        o.insert("to".into(), json!(e.to));
                        o.insert("signal".into(), e.signal.json());
                    }
                    v
                })
                .collect(),
        )
    }

    pub(crate) fn of_json(v: &Value) -> Spool {
        let mut held = Vec::new();
        for rec in v.as_array().into_iter().flatten() {
            let at = rec.get("at").and_then(Value::as_i64).unwrap_or(0);
            let class = rec.get("class").and_then(Value::as_str).unwrap_or_default().to_string();
            let text = rec.get("text").and_then(Value::as_str).unwrap_or_default().to_string();
            // `None` here is not a drop. The entry keeps its words and is
            // printed as them — see [`Spooled::text`].
            let line = match Class::parse(&class) {
                Some(Class::News) => rec
                    .get("signal")
                    .and_then(Signal::of_json)
                    .zip(rec.get("edge").and_then(Value::as_str).and_then(Edge::parse))
                    .map(|(signal, edge)| {
                        Line::News(Emit {
                            edge,
                            to: rec.get("to").and_then(Value::as_str).unwrap_or(EVERYONE).to_string(),
                            held: rec.get("held").and_then(Value::as_i64).unwrap_or(0),
                            signal,
                        })
                    }),
                Some(c) => Some(Line::Note(c, text.clone())),
                None => None,
            };
            held.push(Spooled { at, line, text, class });
        }
        Spool { held }
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
    /// A task id, mostly. A pane for a binding whose task is gone, the literal
    /// `herdr` for [`Kind::Blind`], and **a seat's scope** for
    /// [`Kind::SeatStalled`] — a project id or a worklist slug, which share one
    /// key space with each other and with nothing else here.
    ///
    /// *Mostly* is load-bearing and was nearly a defect. [`Poll::sample`]
    /// addresses on the way out from a map keyed by task id, so a subject that
    /// is not a task silently keeps the [`EVERYONE`] it was built with. That is
    /// right for `herdr` and for a record wsp cannot place — nobody in
    /// particular owns either — and it is exactly wrong for a stalled seat,
    /// whose whole point is that it must reach somebody *other than itself*. So
    /// [`stalled_seats`] addresses its levels where it builds them, after that
    /// map has been applied rather than before.
    pub(crate) subject: String,
    /// The clause after the id: what to do, or why. Never the task's title —
    /// `wsp-095` Part 13 holds this to `wsp worklist next`'s bar, *ids not
    /// titles*, because a governor knows its own ids and pays for every line on
    /// every request of every session.
    pub(crate) detail: String,
    /// The record this level is a reading of, when it is a reading of one.
    ///
    /// A message id, and it is here for two reasons a task id cannot serve.
    /// **It is part of [`Signal::key`]**, so two questions about one task are
    /// two facts rather than one that silently replaces the other — which is
    /// `worklist-017`'s finding about the flag record, kept out of this one by
    /// construction. And it is what the reader needs *in hand* to act: the
    /// subject says which work, the record says which question, and
    /// `wsp answer` takes the second.
    pub(crate) record: Option<String>,
    /// How much of the reader's time this may take, when it is not the kind's
    /// own default.
    ///
    /// `None` is [`Kind::loudness`] — `note`, because a derived level is
    /// nobody's request. It is overridden for exactly two readings and both are
    /// facts about the *reading* rather than about the predicate: herdr's
    /// `blocked`, a modal on the keyboard that one keypress fixes, which
    /// `quiet_note` already promotes past the hour for the same reason; and a
    /// question, which carries the asker's own [`crate::message::Kind`] because
    /// **the sender already said how loud it was** and deriving a second answer
    /// beside it is the two-vocabularies fault this file keeps refusing.
    pub(crate) loud: Option<crate::message::Kind>,
    /// This one may be emitted the instant it is seen. False for the states
    /// that flap — an agent is briefly "stopped" between every pair of turns —
    /// and true for the ones that do not.
    pub(crate) at_once: bool,
    /// **Who answers for this level right now** — a seat's scope, or
    /// [`EVERYONE`].
    ///
    /// Read off the level rather than re-derived at delivery, and that is the
    /// whole of `worklist-039`'s daemon half. [`crate::attention::deliver`]
    /// used to walk [`cmd_govern::seat_for`] per *edge*, at the moment the edge
    /// was sent, which addresses a raise and its clearing by two different
    /// readings of a routing that moves under both. Driven on 2026-08-20
    /// against a fake herdr: one flag, one seat swap in between, and the log
    /// reads
    ///
    ///     attention-raised   to=phase-four  flag demo-001  held=0
    ///     attention-cleared  to=demo        flag demo-001  held=181
    ///
    /// A hook can filter on nothing but `to`, so the seat that was told the
    /// hand went up was never told it came down, and the seat that was told it
    /// came down had never been told it went up. Neither line is wrong on its
    /// own; the pair is, and no reader holding one of them could tell.
    ///
    /// Carrying it on the level fixes that by construction — the address is
    /// read once, with the predicate, and the ledger remembers it — and it is
    /// what makes a *change* of address a fact the diff can see at all. See
    /// [`Routing`].
    ///
    /// It also leaves **one** derivation of the routing walk where there were
    /// two. [`in_scope`] asked it for membership and the daemon asked it again
    /// on the way out; both now go through [`addressed_to`], which is this
    /// file's own standing argument — three definitions of `needs_a_person` is
    /// how the exception for seats got quietly lost, and a fourth would have
    /// been the same shape.
    pub(crate) to: String,
}

/// The address of a level nobody in particular answers for.
///
/// A word rather than an absent field, because the reader is a shell script:
/// `everyone` is a thing to test for and a null is a thing to forget to test
/// for. It is the panel's own last resort said out loud — `wsp flag`'s
/// *"raised on every panel"* — and it is the common case on a machine with no
/// seats, which is why `wsp-095` Part 9 asks that it be visible rather than
/// silently universal.
///
/// It lives beside [`Signal::to`] rather than in [`crate::attention`], where it
/// was written, because the address is now a field on the level and a word
/// spelled out in a second place is what this file keeps refusing.
pub(crate) const EVERYONE: &str = "everyone";

impl Signal {
    pub(crate) fn new(kind: Kind, subject: &str, detail: &str) -> Signal {
        Signal {
            kind,
            subject: subject.to_string(),
            detail: detail.to_string(),
            record: None,
            loud: None,
            at_once: true,
            // Nobody in particular until a reader that knows the routing says
            // otherwise. A signal built by hand — a test, or any caller with no
            // store behind it — is honestly everybody's.
            to: EVERYONE.to_string(),
        }
    }

    /// Addressed, by the one reader that knows the routing. See [`Signal::to`].
    pub(crate) fn to(mut self, seat: &str) -> Signal {
        self.to = seat.to_string();
        self
    }

    pub(crate) fn settling(mut self) -> Signal {
        self.at_once = false;
        self
    }

    pub(crate) fn loud(mut self) -> Signal {
        self.loud = Some(crate::message::Kind::Direction);
        self
    }

    /// Said as loud as whoever wrote the record asked for.
    fn as_loud_as(mut self, k: crate::message::Kind) -> Signal {
        self.loud = Some(k);
        self
    }

    /// The message this is a reading of. See [`Signal::record`].
    pub(crate) fn of(mut self, record: &str) -> Signal {
        self.record = Some(record.to_string());
        self
    }

    /// How loud this reading is, which is the kind's default unless the
    /// reading overrode it.
    pub(crate) fn loudness(&self) -> crate::message::Kind {
        self.loud.unwrap_or_else(|| self.kind.loudness())
    }

    /// Whether this is worth the red column rather than the yellow one.
    fn shouts(&self) -> bool {
        self.loudness() < crate::message::Kind::Note
    }

    /// What the ledger diffs on: the predicate, what it is about, and — when
    /// there is one — the record it read. See [`Signal::record`].
    pub(crate) fn key(&self) -> String {
        match &self.record {
            Some(r) => format!("{}:{}:{}", self.kind.word(), self.subject, r),
            None => format!("{}:{}", self.kind.word(), self.subject),
        }
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
    /// The signal as the two persisted records hold it.
    ///
    /// **Asked from both rather than written out in each.** There are now two
    /// things on disk that carry a level — the ledger a watch resumes from, and
    /// the spool a wake flushes — and this file has paid repeatedly for a field
    /// spelled out in a second place drifting from the first. Distinct from
    /// [`Signal::envelope`], which is the *wire* shape and answers to
    /// `wsp-095` Part 4; this one answers only to itself and may change
    /// whenever both readers do. See [`Ledger::json`] and [`Spool::json`].
    pub(crate) fn json(&self) -> Value {
        json!({
            "signal": self.kind.word(),
            "subject": self.subject,
            "detail": self.detail,
            "record": self.record,
            "loud": self.loud.map(|k| k.as_str()),
            "at_once": self.at_once,
            // Who was answering for it when it was last read. The record is the
            // only thing that remembers, and remembering is what makes a change
            // of address a fact rather than a silence — see [`Signal::to`].
            "to": self.to,
        })
    }

    /// And back, or `None` for a record this build has no vocabulary for.
    ///
    /// The caller decides what `None` means, and the two callers mean different
    /// things by it: a ledger entry that will not parse is a level that is
    /// simply re-read on the next tick, and a **spool** entry that will not
    /// parse is a fact somebody is owed — see [`Spooled::text`], which is why
    /// nothing is dropped on this path.
    pub(crate) fn of_json(rec: &Value) -> Option<Signal> {
        let word = rec.get("signal").and_then(Value::as_str).unwrap_or_default();
        Some(Signal {
            kind: Kind::parse(word)?,
            subject: rec.get("subject").and_then(Value::as_str).unwrap_or_default().to_string(),
            detail: rec.get("detail").and_then(Value::as_str).unwrap_or_default().to_string(),
            record: rec.get("record").and_then(Value::as_str).map(str::to_string),
            // The word, and a `true` from a build that wrote a bool still reads
            // as `direction`. These records survive an `exec` in the middle of
            // an install, so the file this reads is routinely the previous
            // binary's.
            loud: rec.get("loud").and_then(|v| match v {
                Value::Bool(b) => b.then_some(crate::message::Kind::Direction),
                _ => v.as_str().and_then(crate::message::Kind::parse),
            }),
            at_once: rec.get("at_once").and_then(Value::as_bool).unwrap_or(true),
            // A record written before the address was on the level reads as
            // everybody's. It cannot produce a false `left` on the next tick,
            // because a ledger this build did not key is primed rather than
            // diffed and [`Ledger::prime`] takes the whole signal from the read
            // — see [`resume`].
            to: rec.get("to").and_then(Value::as_str).unwrap_or(EVERYONE).to_string(),
        })
    }

    pub(crate) fn envelope(&self, edge: Edge, to: &str, at: &str) -> Value {
        let mut v = json!({
            // The class field every line of this stream carries, and the one
            // value of it that is a signal at all — see [`Class`]. It is here
            // rather than added by the caller so that the document a hook
            // receives and the document `--json` prints stay one document,
            // which is the whole argument of this function.
            "class": Class::News.word(),
            "shape": crate::message::Shape::Signal.as_str(),
            "signal": self.kind.word(),
            "kind": self.loudness().as_str(),
            "edge": edge.word(),
            "from": "wsp",
            "to": to,
            "role": "addressee",
            "about": self.subject,
            "at": at,
            "text": self.detail,
        });
        // Part 4's `id`, and the one field a level normally has no business
        // carrying — *"stable, so a resend is idempotent and an answer can find
        // its way home"*. A level has no id of its own and still does not; this
        // is the id of the **record it is a reading of**, present only when
        // there is one, and it is what turns a hook from a thing that says
        // somebody is waiting into a thing that says what to type. Absent
        // rather than null for the reason every other optional here is: the
        // reader is a shell script, and a field that can never be filled is
        // noise on a wire.
        if let (Some(r), Some(o)) = (&self.record, v.as_object_mut()) {
            o.insert("id".into(), json!(r));
        }
        // Who answers for it now, on the one edge where that is not `to`.
        // Present only there, for the reason every other optional here is
        // absent rather than null: the reader is a shell script, and a field
        // that can never be filled is noise on a wire. It is what lets a hook
        // say *the hand you were told about is now demo's* without parsing the
        // sentence, which is the thing this file refuses to make anybody do.
        if let (Edge::Left, Some(o)) = (edge, v.as_object_mut()) {
            o.insert("moved_to".into(), json!(self.to));
        }
        v
    }
}

/// Which way a level moved.
///
/// Two of these are the level itself changing and the third is the level
/// standing still while the routing changes under it — see [`Edge::Left`],
/// which is `worklist-039`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Edge {
    Up,
    Down,
    /// **The level did not go anywhere; the reader did.** Somebody else
    /// answers for it now.
    ///
    /// `worklist-039`, and it is a third word rather than a `down` because a
    /// seat reading `cleared` for a hand somebody else is now holding is being
    /// told something false. Driven on 2026-08-20, two seats and one worklist
    /// finishing under them: the seat that lost a `review` was told nothing at
    /// all — [`Kind::clears`] is false for it — and the seat that lost a `flag`
    /// was told `cleared · was up 0s` while `wsp flag` went on listing it.
    ///
    /// So `clears()` was never the fault line. It decides only **which** of the
    /// two wrong things happens, and the quiet one is the kinder: a missed fact
    /// costs a reader a look, and a false `cleared` costs them the look they
    /// would otherwise have taken. Both are the fail-silent family with the
    /// cause this row named — a correct reporter whose subject moved out from
    /// under it — and neither is repairable by moving a kind from one side of
    /// `clears()` to the other, because the predicate is still true and there
    /// is nothing here to clear.
    ///
    /// It is emitted for **every** kind for that reason. `clears()` answers
    /// *is this level going away worth a line*; this answers *is it still
    /// mine*, and they are different questions about different events.
    Left,
}

impl Edge {
    pub(crate) fn word(&self) -> &'static str {
        match self {
            Edge::Up => "up",
            Edge::Down => "down",
            Edge::Left => "left",
        }
    }

    /// Back from the word, for a spool written by another build.
    fn parse(word: &str) -> Option<Edge> {
        [Edge::Up, Edge::Down, Edge::Left].into_iter().find(|e| e.word() == word)
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
    /// **Who this line is for**, which is [`Signal::to`] for everything except
    /// an [`Edge::Left`] — where the level has already moved on and the line is
    /// for the seat it left.
    ///
    /// Carried on the emit rather than worked out where it is sent, because
    /// *where it is sent* is a tick later than *what happened* and the routing
    /// moves in between: see [`Signal::to`] for the pair of log lines that cost.
    pub(crate) to: String,
}

// ---------------------------------------------------------------------------
// what a level read cannot say
// ---------------------------------------------------------------------------

/// Where the subjects a reader was holding stand **now**.
///
/// [`Source::sample`] answers *what is up in my scope*, and that is the whole
/// primitive — but a key that has gone from it has two possible histories, and
/// they are opposite pieces of news:
///
/// - the predicate went false, which is the level clearing;
/// - the subject moved out from under the reader, which is somebody else's
///   level now and is still standing.
///
/// Nothing in two consecutive reads distinguishes them, so a diff that has only
/// the reads must guess, and before `worklist-039` it guessed *cleared* — for
/// the kinds that clear, out loud and falsely, and for the rest by saying
/// nothing.
///
/// This is the fact that removes the guess, and it is deliberately **beside**
/// the read rather than inside it: a level set stays the answer to *what is
/// up*, which is what makes it self-healing after a disconnection
/// (`robustness-075`), and a source that cannot answer this at all degrades to
/// exactly the behaviour above rather than to nothing.
///
/// Three variants because there are three readers and their boundaries are
/// three different shapes — see [`Scope`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum Routing {
    /// A reader with no store behind it, which cannot say. Every departure is
    /// read as the predicate going false — the behaviour this file had before
    /// a move was a thing it could see, kept as the default so a new
    /// [`Source`] is wrong in the old quiet way and never in a new loud one.
    #[default]
    Unknown,
    /// A scope with a boundary: which subjects are still inside it, and who
    /// answers for the ones that are not.
    ///
    /// This is both the seated case and the named-project case, and one
    /// variant serves both because the question the diff asks is *is it still
    /// mine* — which [`in_scope`] answers under either membership rule.
    Scoped {
        inside: BTreeSet<String>,
        to: BTreeMap<String, String>,
    },
    /// The whole machine, where nothing is ever outside.
    ///
    /// The daemon's pass, and the reason it needs its own variant: a move
    /// cannot show up here as a departure, because [`Scope::machine`] has no
    /// boundary to depart. It shows up as [`Signal::to`] changing on a level
    /// that never moved — which is invisible to a `Scoped` reader, where an
    /// address change *is* the departure, and which must not be read as one on
    /// a named project, where the seat above a scope can change without a
    /// single task leaving it.
    Machine,
}

impl Routing {
    /// Whether the subject is still this reader's, and who has it if not.
    ///
    /// `None` means *still mine, or I cannot say* — both of which leave the
    /// diff on its original path. `Some(seat)` is the departure, and it names
    /// where the level went so the line can say it.
    ///
    /// A subject that is not in `to` at all has genuinely gone — a task
    /// deleted, or an id a renumbering did not reach — and that is a level
    /// clearing rather than a level moving. `wsp flag` cannot list it and no
    /// seat can be pointed at it, so the honest word is the one already there.
    fn left(&self, subject: &str) -> Option<&str> {
        match self {
            Routing::Unknown | Routing::Machine => None,
            Routing::Scoped { inside, to } => match inside.contains(subject) {
                true => None,
                false => to.get(subject).map(String::as_str),
            },
        }
    }

    /// Whether a level standing still, with a new address on it, is news.
    ///
    /// Only for the machine reader. On a seat an address change **is** the
    /// departure and is caught by [`Routing::left`]; on a named project the
    /// address changes every time somebody takes the seat above it, with
    /// nothing having moved and nothing to say.
    fn readdresses(&self) -> bool {
        matches!(self, Routing::Machine)
    }
}

impl Ledger {
    /// Take a level read and say what is news.
    ///
    /// Pure, and `now` is a parameter for that reason: the settle rule is the
    /// one piece of behaviour here that depends on the clock, and it is the one
    /// worth being able to test without one.
    ///
    /// Four things happen, in this order, and the order is load-bearing:
    ///
    /// 1. every level in `now` that is new is recorded with its first-seen
    ///    time, and **not** said; every level that was already there and has
    ///    been **readdressed** is said to have left, and re-armed for the seat
    ///    that has it now;
    /// 2. every level that has gone is dropped — and said, if it had been said,
    ///    either as having left this scope or as having cleared, which
    ///    `routing` is what tells them apart. A level that went away while
    ///    still settling was never news, so its going is not news either;
    /// 3. every recorded level that has now held long enough is said.
    ///
    /// Doing (3) last is what makes a signal that appears and settles inside
    /// one tick emit exactly once rather than not at all — and it is also what
    /// carries a readdressed level's arrival, so the seat that gains one hears
    /// about it in the same words, and at the same settle, as one raised there.
    ///
    /// # `routing`, and why the read alone is not enough
    ///
    /// See [`Routing`]. A key's absence from `now` is two different pieces of
    /// news and this is what separates them; a [`Routing::Unknown`] leaves
    /// every branch below on the path it took before `worklist-039`, which is
    /// what a test with no store behind it wants and what a new [`Source`] gets
    /// for free.
    pub(crate) fn advance(&mut self, now: &[Signal], routing: &Routing, at: i64, settle: i64) -> Vec<Emit> {
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
            // The level never moved and the seat answering for it did. Said to
            // the seat that had it, and then re-armed — `told` back to false —
            // so (3) below announces it to the seat that has it now, in the
            // same words and after the same settle as a level raised there.
            //
            // **`since` is deliberately kept.** The new seat is told how long
            // the hand has actually been up, which is the fact it needs and the
            // one a fresh entry would have thrown away: a nine-hour stall
            // handed over is not a stall that started at the hand-over.
            let moved = routing.readdresses() && e.told && e.signal.to != s.to;
            if moved {
                out.push(Emit {
                    edge: Edge::Left,
                    signal: s.clone(),
                    held: at - e.since,
                    to: e.signal.to.clone(),
                });
                e.told = false;
            }
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
            let Some(h) = self.up.remove(&k) else { continue };
            if !h.told {
                continue;
            }
            match routing.left(&h.signal.subject) {
                // It is out of this scope and still standing. Said whatever
                // `clears()` answers, because that answers a different question
                // — see [`Edge::Left`] — and the line carries who has it now,
                // which is the only thing the reader can act on.
                Some(now_to) => out.push(Emit {
                    edge: Edge::Left,
                    to: h.signal.to.clone(),
                    signal: h.signal.clone().to(now_to),
                    held: at - h.since,
                }),
                None if h.signal.kind.clears() => {
                    out.push(Emit { edge: Edge::Down, to: h.signal.to.clone(), signal: h.signal, held: at - h.since })
                }
                None => {}
            }
        }

        for h in self.up.values_mut() {
            if h.told {
                continue;
            }
            let ready = h.signal.at_once || at - h.since >= settle;
            if ready {
                h.told = true;
                out.push(Emit {
                    edge: Edge::Up,
                    to: h.signal.to.clone(),
                    signal: h.signal.clone(),
                    held: at - h.since,
                });
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
    ///
    /// # The second caller, and why this keeps what it recognises
    ///
    /// The first version of this replaced the map outright, which is right for
    /// a ledger that is empty and wrong for [`resume`]'s other caller: a
    /// ledger written by a build that spelled [`Signal::key`] differently. That
    /// one is not empty, and most of what is in it keys the same either side —
    /// so wiping it would reset `since` on every standing level and mark every
    /// one still inside its settle window as told. Ed installs several times a
    /// day; `held` on the eventual clearing would have measured the time since
    /// the last install rather than how long the hand was up, and a level four
    /// minutes into a five-minute settle would have been swallowed.
    ///
    /// So: what the read still has keeps the time it went up and whether it has
    /// been said, what the read no longer has is **dropped and not cleared**,
    /// and what is new is recorded as already told. Nothing is announced and
    /// nothing is unsaid. On the first tick the map is empty and all three
    /// clauses collapse to the original behaviour.
    pub(crate) fn prime(&mut self, now: &[Signal], at: i64) -> Vec<Emit> {
        let seen: BTreeSet<String> = now.iter().map(Signal::key).collect();
        // Dropped, not cleared. A level whose key this build spells another way
        // has not gone anywhere, and saying it has is the false edge the whole
        // of `worklist-034` is about — three hands `wsp flag` was listing
        // throughout, reported lowered by an install.
        self.up.retain(|k, _| seen.contains(k));
        let mut out = Vec::new();
        for s in now {
            let key = s.key();
            let said = self.up.get(&key).is_some_and(|h| h.told);
            let e = self.up.entry(key).or_insert_with(|| Held {
                signal: s.clone(),
                since: at,
                told: true,
            });
            e.signal = s.clone();
            // Only when it is news. A re-prime over a ledger that already said
            // this would be saying it twice, which is the shape of defect this
            // path exists to remove rather than one to add on the way.
            if s.kind == Kind::Blind && !said {
                out.push(Emit { edge: Edge::Up, to: s.to.clone(), signal: s.clone(), held: at - e.since });
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

    /// Every level that has actually been said, for a surface that draws the
    /// set rather than the news.
    ///
    /// **`told` and not `up`**, and that is the whole of the method. A level
    /// still inside its settle window is an agent between turns as far as
    /// anybody knows; drawing it would put a word on the sidebar for four
    /// minutes and take it off again, which is the flicker the settle exists to
    /// prevent and which reads worse on a permanently visible surface than in a
    /// stream nobody has scrolled back to.
    pub(crate) fn told(&self) -> impl Iterator<Item = &Signal> {
        self.up.values().filter(|h| h.told).map(|h| &h.signal)
    }

    pub(crate) fn json(&self) -> Value {
        Value::Object(
            self.up
                .iter()
                .map(|(k, h)| {
                    let mut v = h.signal.json();
                    if let Some(o) = v.as_object_mut() {
                        // The two facts the *ledger* adds to a level: how long
                        // it has been up, and whether it has been said out loud.
                        // Neither belongs on the signal — a level does not know
                        // its own age, only a diff does.
                        o.insert("since".into(), json!(h.since));
                        o.insert("told".into(), json!(h.told));
                    }
                    (k.clone(), v)
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
            let Some(signal) = Signal::of_json(rec) else { continue };
            up.insert(
                key.clone(),
                Held {
                    signal,
                    since: rec.get("since").and_then(Value::as_i64).unwrap_or(0),
                    told: rec.get("told").and_then(Value::as_bool).unwrap_or(false),
                },
            );
        }
        Ledger { up }
    }
}

/// A ledger read back out of a watch record, and whether it may be **diffed
/// against** or only carried forward.
///
/// One call, because there are two persisted ledgers and they had the same
/// fault: the daemon's unattended pass ([`crate::attention::tick`]) and the
/// pull ledger `wsp watch --once` leaves for the next caller. Both survive the
/// process on purpose, and surviving the process means routinely being read by
/// a **different build** — the daemon `exec`s itself the moment an install
/// lands on its path, and a `--once` caller is a cron whose binary was replaced
/// between two runs.
///
/// # What went wrong, and it is not the vocabulary
///
/// [`Ledger::of_json`] validates the signal *word* — `Kind::parse` drops what
/// it cannot read — and never the *key shape*. `Signal::key` gained a `record`
/// component in `34ae8a3` for the two predicates that read a message record,
/// so a ledger written either side of that commit is keyed by rules the other
/// one does not use. Loaded and advanced against, every old-shaped key is
/// absent from the new read and reported **gone**, and every new-shaped key is
/// absent from the old ledger and reported **raised**: one false `cleared` and
/// one false `raised` per standing hand, per install. Driven on 2026-08-19
/// across `5a1f71d`↔`796c2d2` — which is exactly the pair `34ae8a3` sits
/// between — with three hands `wsp flag` listed throughout reported lowered.
///
/// `needs-a-person` carries no record and keys the same either side, which is
/// the only reason Ed's hook filter stayed quiet through it. That is luck, and
/// the next predicate to gain a key component takes the phone with it.
///
/// # Why the build and not the key shape
///
/// A key shape cannot be checked by re-deriving it. Both directions of the
/// change are invisible to that: the pre-`34ae8a3` file has no record in it to
/// re-derive *from*, so its entries round-trip to themselves and the mismatch
/// only appears against the new read. Only something written down beside the
/// ledger can answer *whose rules is this keyed by*, and the build is the one
/// such thing that cannot be forgotten — a hand-bumped schema number is exactly
/// the discipline whose absence caused this.
///
/// The cost is a [`Ledger::prime`] on every install rather than only on the
/// installs that moved a key, and that is what makes `prime` keep what it
/// recognises: a prime that preserved nothing would have been a worse trade
/// than the bug, several times a day.
///
/// A record with no `build` in it was written before this existed, so it is a
/// ledger we cannot place — primed, once, and stamped on the way out.
pub(crate) fn resume(rec: &Value) -> (Ledger, bool) {
    let Some(v) = rec.get("ledger") else { return (Ledger::default(), false) };
    let keyed_by = rec.get("build").and_then(Value::as_str).unwrap_or_default();
    (Ledger::of_json(v), keyed_by == crate::build_stamp())
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

    /// Where the subjects of the **last** sample stand now — see [`Routing`].
    ///
    /// A second method rather than a second return value, and a defaulted one,
    /// because it is the answer to a question the level read does not ask: a
    /// source is still a thing that says what is up, and one that cannot say
    /// where anything is addressed degrades to the diff this file made before
    /// `worklist-039` rather than to no diff at all.
    fn routing(&self) -> Routing {
        Routing::Unknown
    }
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
    /// And the pane in it, so that end condition is read exactly. A workspace
    /// holds more than one agent, and a subscription that survived because a
    /// *neighbour* was still in the room would be a seat's watch going on after
    /// the seat did — see [`cmd_govern::governs`]. Empty for a scope that was
    /// named rather than sat in, where there is no pane to speak of.
    pub(crate) pane: String,
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
        Scope { name: "this machine".into(), seated: false, workspace: String::new(), pane: String::new(), all: true }
    }
}

/// Who answers for a task: the seat the routing walk reaches, or [`EVERYONE`].
///
/// **One walk, and this is the only one.** It is the same rule `wsp flag`
/// addresses by, so what a watch reports, what a raised hand reaches and what
/// the daemon's hook is told are one definition — which was the standing claim
/// in this file's module docs and was not quite true: [`in_scope`] walked it
/// for membership and `attention::addressee` walked it again at delivery, a
/// tick later and against a routing that had moved in between. See
/// [`Signal::to`] for the pair of log lines that produced.
///
/// The task is passed rather than an id on purpose. A subject arrives at the
/// diff off a *persisted* ledger, which looks like the raw-read fault this tree
/// has fixed three times — resolving a recorded id against a renumbering — and
/// is not: `watches.json` is in [`crate::store::Store::state_files_with_ids`],
/// so a renumbering rewrites the ledger's subjects, and an id that no longer
/// names a task is one that has genuinely gone. A caller with only an id can
/// therefore look it up plainly, and a miss means nobody rather than *ask
/// again*.
pub(crate) fn addressed_to(
    index: &Index,
    governors: &BTreeMap<String, Value>,
    lists: &worklist::Running,
    task: &Task,
) -> String {
    cmd_govern::seat_for(governors, index, lists.list_of(&task.id), task.project.as_deref())
        .map(|s| s.scope)
        .unwrap_or_else(|| EVERYONE.to_string())
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
        return addressed_to(index, governors, lists, task) == scope.name;
    }
    if lists.list_of(&task.id) == Some(scope.name.as_str()) {
        return true;
    }
    task.project.as_deref().is_some_and(|p| {
        p == scope.name || index.ancestors(p).iter().any(|a| a == &scope.name)
    })
}

/// One message record, as a level — or nothing, when this build has no reason
/// to think anybody is waiting on it.
///
/// Pure, and separate from [`Poll::sample`] for the reason every other
/// predicate in this file is not: this one has no herdr in it at all. The
/// record answers whether a question is open and the census answers whether its
/// asker is still turning, and both are in hand, so the whole of what a
/// governor is told about an unanswered question can be driven by a test on a
/// machine with no socket.
///
/// **`needs_attention`, never `is_open`.** A state this build cannot parse is
/// not *not open*; it is *somebody should look at this*. The installed binary
/// is routinely older than the tree, and one answering `is_open` here would
/// report a quiet fleet at the exact moment a newer build had written a
/// question it could not read — `robustness-051` arriving through the module
/// built to remove it, which is the argument `Message::needs_attention`'s own
/// docstring makes and this is the first caller to owe it.
///
/// A record this cannot place — an unreadable shape, or one whose state is a
/// word from a newer build — is reported **loudly and in its own sentence**,
/// rather than being drawn as whichever kind it happens to parse closest to. It
/// is the one case where the words on the record are the half wsp could not
/// read, so the line says that instead of quoting them.
fn asking(m: &crate::message::Message, subject: &str, rows: &[cmd_agent::WipRow]) -> Option<Signal> {
    use crate::message::Kind as Loud;
    // `Message::wants_answering` and not a filter written out here: it is the
    // same predicate `wsp ask` lists by, asked once, so the verb and the watch
    // cannot come to disagree about whether the fleet is quiet. Answering
    // `None` is (b)'s signal to draw the record as a [`Kind::Flag`] instead —
    // the split is here, in one call, rather than spelled out at the call site.
    if !m.wants_answering() {
        return None;
    }
    let unreadable = m.shape().is_none() || m.state().is_none();

    // The join the record could not make on its own. `Waiting` names the pane;
    // the census says whether it is still turning, and an asker mid-turn is not
    // sitting still — it asked and carried on. Said either way, because the
    // question is unanswered either way: this changes how urgent it reads, not
    // whether it is true.
    let pane = m.waiting.as_ref().map(|w| w.pane.as_str()).unwrap_or_default();
    let who = match rows.iter().find(|r| r.pane == pane) {
        Some(r) if r.turning => format!("{pane} asked and is still working"),
        Some(r) => format!("{} waiting", r.pane),
        None if pane.is_empty() => "nobody is sitting on it".to_string(),
        None => format!("{pane} asked and is gone"),
    };

    let s = match unreadable {
        // Its own sentence rather than the question's, because the words are
        // the half this build could not read, and `direction` because the
        // repair is a person's and is one command.
        true => Signal::new(
            Kind::Unanswered,
            subject,
            &format!(
                "{who} · this wsp cannot read the record — wsp ask lists it, \
                 and wsp install may be what it wants"
            ),
        )
        .loud(),
        false => Signal::new(
            Kind::Unanswered,
            subject,
            // `ask` is named where there is one, because it is a **capability
            // of the surface** and not a synonym for the words: it says one
            // keypress answers this, which changes what the reader does with
            // the line. `wsp flag --ask claim` would otherwise arrive reading
            // like any other question.
            &match m.ask() {
                Some(crate::message::Ask::Claim) => format!(
                    "{who} · asking to claim · {} — wsp answer {} \"…\"",
                    util::truncate(m.title(), 44),
                    m.id
                ),
                _ => format!(
                    "{who} · {} — wsp answer {} \"…\"",
                    util::truncate(m.title(), 60),
                    m.id
                ),
            },
        )
        // The asker's own judgement, read off the record rather than derived
        // beside it. See [`Signal::loud`]: wsp does not get a second opinion
        // about how much of somebody's time somebody else's question may take.
        .as_loud_as(m.kind().unwrap_or(Loud::Note)),
    };
    Some(s.of(&m.id))
}

/// [`Poll::sample`]'s arm (e): **a seat that has stopped answering** — the one
/// agent whose failure ends a run, and the one that cannot report it.
///
/// Pure, and separate from [`Poll::sample`] for [`asking`]'s reason: every input
/// is a reading somebody else already took, so the predicate, its exemptions and
/// the escalating address can all be driven by a test on a machine with no herdr
/// socket — which is where `wsp verify` runs.
///
/// `worklist-041`. [`cmd_govern::needs_a_person`] is `stopped && doing &&
/// !seat`, so a seat is exempt however stopped it is, and the exemption is
/// correct: a governor is idle *between* the agents it is sequencing, which is
/// most of the time, and reading that as a stall marked the busiest agent on the
/// machine as stuck. A seat needs a different predicate, not none, and this is
/// it:
///
///     the seat is running no turn
///  && nothing it answers for is running one either
///  && it answers for an unsettled member of a running worklist
///  && no agent is bound to any of them
///
/// # The third clause is the whole of it, and the row as filed had another one
///
/// The row specified `stopped && standing > 0 && seat` — *something is addressed
/// to it*, standing in for the `doing` a custodian deliberately has not got.
/// **Driven against the live store on 2026-08-20, that fires on every healthy
/// seat on this machine.** Both were running no turn, and the daemon's own
/// ledger held 42 levels addressed to one of them and 10 to the other; 51 of
/// those 52 were `review`.
///
/// Which is the reason and not a coincidence. `review` is where worklist work
/// stops — `done` is Ed's, on every task — so a `review` addressed to a seat is
/// a level that seat **cannot take down**, and they accumulate for the life of a
/// project. A seat sitting under forty-two of them is not stalled; it is doing
/// exactly what a seat is for. `standing` counts what is *pointed at* a seat,
/// and what makes a stopped agent a person's problem is what it *owes*. Those
/// are one number for a worker, whose task is both, and they are nothing like
/// one number for a seat.
///
/// So the obligation is named rather than inferred: **a running worklist is a
/// seat's `doing`**. Somebody committed to driving that queue, its members are
/// written down, and [`worklist::Settlement::settled`] already says which of
/// them are still somebody's to finish. A seat with no run under it owes nothing
/// and stays silent however long it sits — which is the exemption clause the row
/// wanted `standing == 0` to be, in the one shape that survives this store.
///
/// # The second clause, which is `robustness-083` read backwards
///
/// Absence of movement is not evidence: a seat is *supposed* to be still. What
/// is evidence is that the seat is still **and so is everything it answers
/// for**, with a queue under it that is not finished. One turning agent anywhere
/// in the scope exempts the seat — the broadest exemption available, taken
/// deliberately, because a seat with work moving under it is being driven by
/// something whether or not this predicate can see what.
///
/// # The fourth clause: this is the stoppage with no agent to be its subject
///
/// Without it the signal doubles every other one in the vocabulary. A member
/// that stops while somebody is on it is reported *as that member* —
/// [`Kind::NeedsAPerson`] if its pane is stopped, [`Kind::AgentGone`] if its
/// pane has died, [`Kind::Blocked`] if it asked a question — and adding *and
/// the seat above it is idle* to each of those is two lines about one stoppage,
/// which is how a governor learns to skim. It would also be close to false: a
/// governor is prompt-driven and does not poll, so five minutes of not having
/// reacted to a member's stall is not a governor that has died.
///
/// So the run has to be **unattended**: no agent bound to any unsettled member.
/// That is exactly `robustness-083`'s shape and it is the one stoppage with no
/// agent to be its subject — the members landed cleanly and were despawned,
/// the next group was never started, and there is nothing left in the fleet to
/// raise a hand about except the seat that did not advance the barrier.
///
/// Bindings and not the census, because the two disagree in a useful direction:
/// a binding whose pane has gone is [`Kind::AgentGone`]'s to report, and it
/// keeps this quiet until `wsp sync` reaps it — at which point the member is
/// genuinely unattended and this takes over. One line per stoppage, the whole
/// way through.
///
/// # Read by the machine reader alone, which is a fact about the predicate
///
/// See [`Kind::SeatStalled`]. A seated scope *is* "what is addressed to me", so
/// no seat can honestly evaluate this about another one, and the seat it is
/// about is the last agent whose answer is worth having. [`Scope::machine`] is
/// unreachable from argv, so this is [`crate::attention`]'s pass and nothing
/// else — which is where the row asked for it, for the same reason.
///
/// # It says, it does not act
///
/// `robustness-090` d1. Nothing here vacates a slot, ends a pane or advances a
/// barrier. A seat that looks stalled may be one Ed is about to talk to.
/// The run a task is an outstanding member of, if it is one of those.
///
/// **One sentence, two readers**, which is the discipline this file keeps: a
/// seat's obligation is asked from the stall side by [`stalled_seats`] and from
/// the stand-down side by [`Poll::owes_a_run`], and those are one predicate read
/// from its two ends — `worklist-037` said so before either existed. Written out
/// twice, they would be one install away from disagreeing about whether a
/// governor may go home.
///
/// `Settlement::of` and not a status test spelled out here, for the same reason
/// one level up: `review` being the end of the line is `worklist.rs`'s sentence.
fn outstanding<'a>(t: &Task, lists: &'a worklist::Running) -> Option<&'a str> {
    lists.list_of(&t.id).filter(|_| !worklist::Settlement::of(t).settled())
}

fn stalled_seats(
    tasks: &[Task],
    rows: &[cmd_agent::WipRow],
    bindings: &BTreeMap<String, Value>,
    to: &BTreeMap<String, String>,
    lists: &worklist::Running,
    governors: &BTreeMap<String, Value>,
    index: &Index,
) -> Vec<Signal> {
    // Which seats have something moving under them, read off the same map the
    // levels above were addressed from — so *my scope* means one thing in both.
    let moving: BTreeSet<&str> = rows
        .iter()
        .filter(|r| r.turning)
        .filter_map(|r| to.get(&r.task_id).map(String::as_str))
        .collect();
    // Who is standing on something. See the fourth clause above: a member with
    // an agent bound to it has an agent to be the subject of its own stoppage,
    // and this signal is the one for the stoppage that has none.
    let attended: BTreeSet<&str> = bindings
        .values()
        .filter_map(|b| b.get("task_id").and_then(Value::as_str))
        .collect();
    // What each seat still owes a run for. `Settlement::of` rather than a status
    // test written out here: `review` being the end of the line is
    // `worklist.rs`'s sentence, and this is its second reader.
    let mut owed: BTreeMap<&str, (BTreeSet<&str>, Vec<&str>)> = BTreeMap::new();
    let mut held: BTreeSet<&str> = BTreeSet::new();
    for t in tasks.iter() {
        let Some(list) = outstanding(t, lists) else { continue };
        // A member nobody answers for is nobody's stall. That a running list
        // with no seat anywhere above it is invisible here is `worklist-037`'s
        // question 3 and its documented no — absence of a decision to have a
        // seat is not a vacancy — and this arm reports occupants, not posts.
        let Some(seat) = to.get(&t.id).filter(|s| s.as_str() != EVERYONE) else { continue };
        if attended.contains(t.id.as_str()) {
            held.insert(seat.as_str());
            continue;
        }
        let e = owed.entry(seat.as_str()).or_default();
        e.0.insert(list);
        e.1.push(t.id.as_str());
    }
    // Per seat and not per row, because [`cmd_govern::governs`] falls back to the
    // *room* for a record naming no pane — `wsp govern -w` writes one — and a
    // workspace holds more than one agent. Asked row by row, a seat with a busy
    // pane and an idle pane in the same room would report itself stalled out of
    // the idle one.
    let mut seats: BTreeMap<&str, Vec<&cmd_agent::WipRow>> = BTreeMap::new();
    for r in rows.iter() {
        if let Some(scope) = r.seat.as_deref() {
            seats.entry(scope).or_default().push(r);
        }
    }
    let mut out: Vec<Signal> = Vec::new();
    for (scope, panes) in seats {
        if panes.iter().any(|r| r.turning) || moving.contains(scope) || held.contains(scope) {
            continue;
        }
        let Some((in_lists, ids)) = owed.get(scope) else { continue };
        let where_ = in_lists.iter().copied().collect::<Vec<_>>().join(", ");
        // Ids and not titles, and a bounded number of them: this line is read on
        // a phone at 3am by somebody who knows their own ids.
        let waiting = match ids.len() {
            0..=3 => ids.join(" "),
            n => format!("{} +{}", ids[..3].join(" "), n - 3),
        };
        // **Never to the seat this is about.** [`cmd_govern::seat_for`] stops at
        // the first seat it finds, which here is the one that failed;
        // [`cmd_govern::seat_above`] is the same walk started one step past it,
        // terminating at [`EVERYONE`].
        let above = cmd_govern::seat_above(governors, index, scope)
            .map(|s| s.scope)
            .unwrap_or_else(|| EVERYONE.to_string());
        let detail = format!(
            "{} · no turn here and none under it — {where_} still waiting on {waiting} \
             · wsp govern {scope} --tell -",
            panes[0].pane
        );
        // Settling, because a seat between two turns is stopped for seconds and
        // a barrier crossing is a seat turning. Five minutes of neither, with a
        // queue open under it, is not a gap between turns.
        out.push(Signal::new(Kind::SeatStalled, scope, &detail).settling().to(&above));
    }
    out
}

/// The polling implementation: one store sweep and three herdr calls per tick.
pub(crate) struct Poll<'a> {
    store: &'a Store,
    scope: Scope,
    want: BTreeSet<Kind>,
    /// A subject filter, and it is a filter rather than a subscription — see
    /// the module docs on why "about X" is not the unit.
    about: Option<String>,
    /// What the last [`Poll::sample`] saw of the routing, for [`Ledger`] to ask
    /// about the levels that have gone from it.
    ///
    /// Held on the source rather than returned beside the read because the two
    /// have different lifetimes: the read is consumed once, and this is
    /// consulted for keys the read does not contain. Rebuilt every tick, so it
    /// is never staler than the sample it belongs to.
    routing: Routing,
}

impl<'a> Poll<'a> {
    pub(crate) fn new(store: &'a Store, scope: Scope, want: BTreeSet<Kind>, about: Option<String>) -> Poll<'a> {
        Poll { store, scope, want, about, routing: Routing::Unknown }
    }

    /// Whether this scope still answers for a run that is not finished.
    ///
    /// **The other side of [`stalled_seats`]'s third clause, and what closes
    /// the ambiguity `worklist-037` had to leave open.** That row could offer a
    /// seat the stand-down at `0 standing` and no more, because nothing in wsp
    /// could tell *nothing left to answer for* from *nobody is answering*. Half
    /// of that is now answerable: a seat at zero with an unstarted group under
    /// it is not finished, it has not begun — and the members of a group nobody
    /// has spawned raise no level at all, so the count is zero and the sentence
    /// underneath it said *stand down*.
    ///
    /// Asked of the store and not of the level read, for exactly that reason: a
    /// `todo` member with no agent on it is invisible to every predicate in
    /// this file and is the whole of what makes the zero misleading.
    ///
    /// The unseated answer is `false` before anything is read, which is the
    /// same guard [`nothing_addressed`] applies and is here so it costs
    /// nothing: a watch on a named project asks this question, gets the answer
    /// it always had, and does not pay a store sweep for it.
    fn owes_a_run(&self) -> bool {
        if !self.scope.seated {
            return false;
        }
        let index = Index::new(self.store.projects());
        let governors = self.store.governors();
        let lists = worklist::Running::read(self.store);
        self.store.tasks().iter().any(|t| {
            outstanding(t, &lists).is_some()
                && addressed_to(&index, &governors, &lists, t) == self.scope.name
        })
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
        // The routing, taken once for every task in the store and before any
        // predicate is evaluated — so the address a level carries and the
        // membership test that kept it are the same reading, rather than two
        // walks a tick apart. See [`Signal::to`].
        let to: BTreeMap<String, String> = wip
            .tasks
            .iter()
            .map(|t| (t.id.clone(), addressed_to(&wip.index, &wip.governors, &lists, t)))
            .collect();
        let mine = |t: &Task| in_scope(&self.scope, &wip.index, &wip.governors, &lists, t);
        let task_of = |id: &str| wip.tasks.iter().find(|t| t.id == id);

        // Read once and above everything, because (b) and (c) both want it: (c)
        // is the census itself, and (b) asks it whether the party a question is
        // waiting on is still turning.
        let rows = cmd_agent::wip_rows(&wip);

        let mut out: Vec<Signal> = Vec::new();
        // The panes that have said why they stopped. Filled by (b), read by
        // (c), and the reason the two are in this order.
        let mut spoken_for: BTreeSet<String> = BTreeSet::new();

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

        // (b) Everything somebody wrote down and nobody has dealt with —
        // `message::raised`, which is the routing population `wsp flag --seat`
        // lists, asked rather than reimplemented.
        //
        // **Split by shape, into the two words that say what is owed back.**
        // `wsp-095` Part 3 is that a notification and a question are different
        // animals — *one is over when it is sent, the other has a lifecycle and
        // an agent sitting still while it is open* — and a vocabulary that gave
        // them one word would be lossy in the direction that matters: a
        // governor cannot tell, from `flag`, whether anybody is waiting.
        //
        // So `flag` is now exactly *a hand is up and nothing is owed*, and
        // `unanswered` is *somebody is sitting still until you write a
        // sentence*. `wsp flag --ask claim` mints a question and therefore
        // arrives as the second, which is the one behaviour change here worth
        // knowing about: a seat that names `flag` on the command line rather
        // than taking the default set no longer sees them.
        //
        // Both carry the record's id, which is what makes two hands on one task
        // two facts in the ledger — `worklist-017` from the reading side. The
        // record fixed the write; keying the level on the task alone would have
        // put the same loss straight back at the surface.
        for m in crate::message::raised(self.store).iter().filter(|m| !m.is_reply()) {
            // A task subject is what routing and `--about` are made of, and
            // `wsp ask` and `wsp flag` both require one — so this is every
            // record in practice. A record whose subject this build cannot read
            // has no chain to walk and is only the machine pass's, which is
            // right and is better than dropping it: `Scope::machine` is the
            // reader that answers for everything, and a record wsp cannot place
            // is exactly the kind it must not go quiet about.
            let subject = match m.about.task().and_then(task_of) {
                Some(t) if mine(t) => t.id.clone(),
                Some(_) => continue,
                None if self.scope.all => m.id.clone(),
                None => continue,
            };
            if let Some(sig) = asking(m, &subject, &rows) {
                if let Some(w) = &m.waiting {
                    spoken_for.insert(w.pane.clone());
                }
                out.push(sig);
                continue;
            }
            let said = m.title();
            let ask = m.ask().unwrap_or(crate::message::Ask::Nothing).as_str();
            // The sentence, not the title. A flag with no words is one you
            // have to go and read either way, and one with words is carrying
            // the only thing on this line that is not derivable.
            let detail = match (said.is_empty(), ask.is_empty()) {
                (true, true) => "a hand is up — wsp flag lists it".to_string(),
                (true, false) => format!("asking to {ask}"),
                (false, true) => util::truncate(said, 60),
                (false, false) => format!("{} · asking to {ask}", util::truncate(said, 48)),
            };
            out.push(Signal::new(Kind::Flag, &subject, &detail).of(&m.id));
        }

        // (c) The join, read rather than recomputed. Every row here carries
        // `needs_you` — `stopped && doing && !seat` — computed once by
        // `cmd_govern::needs_a_person` and published by `wsp wip --json`.
        for r in rows.iter() {
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
                // An agent that has said why it stopped is not also *stopped
                // for a reason wsp cannot see*, which is all this reading ever
                // was. Two lines about one agent is how a governor learns to
                // skim — the judgement (d) already makes about `Quiet` — and
                // here the weaker line is worse than redundant: its repair is
                // `wsp tell`, and telling an agent that is waiting on
                // `wsp answer` is `worklist-013` exactly, the answer going down
                // the channel with no memory while the question stays open.
                //
                // Only this arm. A modal holding the keyboard is a different
                // repair — one keypress, not a sentence — and an agent can be
                // in one while a question of its own is standing.
                _ if spoken_for.contains(&r.pane) => continue,
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

        // Addressed on the way out, in one place, so no branch above can build
        // a level that does not know who answers for it. A subject that is not
        // a task — `herdr`, for [`Kind::Blind`], and a message record the
        // machine pass could not place — has no chain to walk and is
        // everybody's by construction, which is right: wsp being unable to see
        // the agents is not one seat's problem.
        for s in out.iter_mut() {
            if let Some(seat) = to.get(&s.subject) {
                s.to = seat.clone();
            }
        }

        // (e) A seat that has stopped answering — [`stalled_seats`], which holds
        // the predicate and the argument for it. **After** the addressing above
        // and not before: its subject is a seat, its address is the walk started
        // one step past that seat, and a map keyed by task id must not get the
        // chance to say otherwise. See [`Signal::subject`].
        if self.scope.all {
            out.extend(stalled_seats(
                &wip.tasks,
                &rows,
                &wip.bindings,
                &to,
                &lists,
                &wip.governors,
                &wip.index,
            ));
        }

        out.retain(|s| {
            s.kind == Kind::Blind
                || (self.want.contains(&s.kind)
                    && self.about.as_deref().is_none_or(|a| s.subject == a))
        });
        self.routing = match self.scope.all {
            true => Routing::Machine,
            false => Routing::Scoped {
                inside: wip.tasks.iter().filter(|t| mine(t)).map(|t| t.id.clone()).collect(),
                to,
            },
        };
        out
    }

    /// See [`Routing`]. Rebuilt by every [`Poll::sample`], and
    /// [`Routing::Unknown`] before the first one — a diff against a read that
    /// never happened has nothing to be right about.
    fn routing(&self) -> Routing {
        self.routing.clone()
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
    /// What the process holding this register was built from, and empty for a
    /// record written before the field existed.
    ///
    /// **`worklist-033`.** An install replaces a file; it has no view of the
    /// processes already holding the old one. `wsp panel`, `wsp view` and the
    /// daemon all `exec` into the new binary within a tick, and a `wsp watch`
    /// does not — so a governor's watch goes on reporting in the vocabulary it
    /// was born with, indefinitely. On 2026-08-19 two seats did exactly that,
    /// four times between them, and every detection was a person remembering.
    ///
    /// It is worse than an ordinary staleness because **the stale process here
    /// is the thing that reports**. A stale `wsp verify` gives a wrong number
    /// and somebody re-runs it; a stale watch says *nothing is up* out of logic
    /// that has been fixed, and there is nothing behind it to disagree. Driven
    /// side by side, the older watch was not merely wrong, it was blind to a
    /// whole signal kind — five signals against six, with `unanswered` missing,
    /// and a question raised in between appearing on one stream and never on
    /// the other.
    ///
    /// So the register carries it, and [`stale_build`] is the comparison. The
    /// alternative considered was `stat`ing the binary each watcher `exec`'d
    /// from; this is cheaper, it travels to another machine's records, and it
    /// is the same field [`resume`] already needed.
    pub(crate) build: String,
    /// How many facts this watch is holding for its reader — see [`Spool`].
    ///
    /// A watcher that has decided not to wake somebody is a watcher with
    /// something to answer for, and a triage nobody can inspect would be the
    /// seventh way silence lies in a file that already carries six. `--drain`
    /// is how a reader collects it.
    pub(crate) spooled: usize,
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

    /// Reporting out of a build that is not the one asking.
    ///
    /// Only for a watcher that claimed a process, because that is the only kind
    /// that can go on holding an old image: a `--once` ledger is written by
    /// whatever binary the caller last ran and describes nothing still running.
    /// The daemon is excluded for the opposite reason — it `exec`s itself
    /// within a tick of an install landing on its path, so its register is
    /// briefly behind on purpose and naming it would be the false alarm that
    /// teaches people to skip this line.
    ///
    /// **Against the build asking, not against what is installed.** Reading
    /// the installed binary means running it, and the honest question a reader
    /// has is *is this watcher the same wsp as the one in my hands* — which is
    /// the right question when the wsp in your hands is the one you just
    /// installed, and is still a true statement when it is not.
    ///
    /// A record with no stamp is not reported. It was written before the field
    /// existed, which makes it old, and one restart of anything makes it
    /// answerable — an unanswerable question is not a fault to put in red.
    pub(crate) fn stale_build(&self) -> bool {
        self.watching() && !self.daemon && !self.build.is_empty() && self.build != crate::build_stamp()
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
            build: v.get("build").and_then(Value::as_str).unwrap_or_default().to_string(),
            spooled: v.get("spool").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
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
        // Said before the two liveness faults and separately from them, because
        // it is the one that looks like nothing at all: this watcher is
        // ticking, on time, answering `--status` green, and reporting out of
        // code somebody has since fixed. See [`Registered::build`].
        if !dead && w.stale_build() {
            problems.push(format!(
                "the watch on {} ({}) is running {}, and the wsp asking is {} — an install replaces a file and a process already holding the old one keeps it, so what this reports is that build's logic, in that build's vocabulary. Stop it and run `wsp watch` again",
                w.scope,
                w.key,
                w.build,
                match crate::build_stamp().is_empty() {
                    true => "a binary that cannot say".to_string(),
                    false => crate::build_stamp(),
                }
            ));
        }
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
        Edge::Up => match e.signal.shouts() {
            true => p.red(&word),
            false => p.yellow(&word),
        },
        Edge::Down => p.dim(&util::pad("cleared", 14)),
        // Its own word in the column a reader scans, and dim like `cleared`
        // because the level has left this stream either way. What separates
        // them is the clause: one says it is over and the other says where it
        // went, and a reader who acts on the wrong one either stops looking at
        // a live hand or goes looking for one that is gone.
        Edge::Left => p.dim(&util::pad("moved", 14)),
    };
    let detail = match e.edge {
        Edge::Up if !e.signal.at_once => format!("{} · held {}", e.signal.detail, util::duration_human(e.held)),
        Edge::Up => e.signal.detail.clone(),
        Edge::Down => format!("{} · was up {}", e.signal.kind.word(), util::duration_human(e.held)),
        // **Still up** said out loud, because the whole risk of this line is
        // being read as the other one. Then who has it, which is the only thing
        // the reader can act on.
        Edge::Left => format!(
            "{} · still up, and {} answers for it now · was here {}",
            e.signal.kind.word(),
            e.signal.to,
            util::duration_human(e.held)
        ),
    };
    format!("{} {head} {}  {}", p.dim(&clock(at)), p.bold(&e.signal.subject), p.dim(&detail))
}

/// A line that is not news: the opening, the heartbeat, [`REPLACED`], the end.
///
/// **The `·` that used to sit here has become the class word.** The column the
/// signal name occupies was already how this stream is read down its second
/// column, and the `·` was already saying something there — *not news*. It was
/// just saying it about four different lines at once, which is the whole of
/// [`Class`]. The word keeps that reading and adds the distinction the `·` was
/// hiding, and nothing else moves: same clock, same fourteen-wide column, same
/// dim. Two seats read this stream by eye and one of them reads it down that
/// column, so a shifted field costs more than the field is worth.
fn aside(class: Class, at: i64, said: &str, p: &Paint) -> String {
    column(class.word(), at, said, p)
}

/// The same line, from a word rather than a [`Class`].
///
/// One caller and it is not a loosening of the vocabulary: [`Spooled`] may hold
/// an entry written by a build that had a class this one does not, and printing
/// its words in the column it was written for is the alternative to dropping it.
/// Everything that *emits* still goes through [`aside`].
fn column(word: &str, at: i64, said: &str, p: &Paint) -> String {
    format!("{} {} {}", p.dim(&clock(at)), p.dim(&util::pad(word, 14)), p.dim(said))
}

/// One line, as a person reads it.
fn render(l: &Line, at: i64, p: &Paint) -> String {
    match l {
        Line::News(e) => line(e, at, p),
        Line::Note(c, said) => aside(*c, at, said, p),
    }
}

/// …and as a machine does.
fn render_json(l: &Line, at: i64, to: &str) -> Value {
    match l {
        Line::News(e) => e.signal.envelope(e.edge, to, &util::iso_at(at)),
        Line::Note(c, said) => c.envelope(at, to, said),
    }
}

/// One line in whichever of the two documents this watch is writing.
fn render_for(l: &Line, at: i64, spec: &Spec, p: &Paint) -> String {
    match spec.json {
        true => render_json(l, at, &spec.scope.name).to_string(),
        false => render(l, at, p),
    }
}

/// One line that is not news, in whichever of the two documents is being
/// written.
///
/// **Both of them, always, and that is the second half of this row.** The
/// opening, the heartbeat and [`REPLACED`] used to be printed under
/// `if !spec.json`, so a `--json` watcher emitted nothing at all until
/// something moved — items 1 and 2 of the six ways silence lies, absent for
/// exactly the reader most likely to be a machine and least able to ask a
/// person whether its watch is still alive. The gate was never a decision; it
/// was three call sites that each only knew how to write text.
///
/// So there is one emitter and it is unconditional. Leaving a class out of one
/// of the two documents is now something you would have to do on purpose,
/// rather than something you get by adding a `println!` next to the others.
fn note(class: Class, at: i64, spec: &Spec, said: &str, p: &Paint) -> String {
    render_for(&Line::Note(class, said.to_string()), at, spec, p)
}

/// The opening line: what is being watched, how often, and how much is already
/// up.
///
/// Item 1 of the six. A watcher that says nothing at all from the first second
/// is indistinguishable from one that never started, and the three facts here
/// are what make the next hour of silence mean something.
///
/// A function rather than a `format!` in the loop because it is the line whose
/// absence under `--json` was failure 1, and a line nothing can construct is a
/// line nothing can assert on. See
/// [`a_json_watcher_says_it_started_before_anything_has_happened`].
fn opening(spec: &Spec, standing: usize, owes: bool) -> Line {
    Line::Note(
        Class::Open,
        format!(
            "watching {} · every {} · {} standing · {}{}",
            spec.scope.name,
            util::duration_human(spec.every),
            standing,
            spec.want.iter().map(|k| k.word()).collect::<Vec<_>>().join(" "),
            // Only when it is a seat and only at zero — see
            // [`nothing_addressed`]. On every other line this is the empty
            // string, so a watch on a project is byte-for-byte what it was.
            nothing_addressed(&spec.scope, standing, owes).map(|note| format!(" · {note}")).unwrap_or_default()
        ),
    )
}

/// The heartbeat.
///
/// Item 2 of the six. One line, and it carries the standing count rather than a
/// pulse, so it is a level read in miniature: `0 standing` is a positive
/// statement that nothing is wrong, where a bare tick would only be a statement
/// that the process is alive.
fn heartbeat(spec: &Spec, standing: usize, owes: bool, since: i64) -> Line {
    Line::Note(
        Class::Beat,
        format!(
            "watching {} · {} · {} standing{}",
            spec.scope.name,
            util::duration_human(since),
            standing,
            nothing_addressed(&spec.scope, standing, owes).map(|note| format!(" · {note}")).unwrap_or_default()
        ),
    )
}

// ---------------------------------------------------------------------------
// the one way out
// ---------------------------------------------------------------------------

/// **Everything this verb prints goes through here, and there is no other way
/// to stdout from the loop.**
///
/// That is the guarantee, not a tidiness. `core-014`'s argument is that a
/// filter which *drops* fails as silence and silence is indistinguishable from
/// health; this file already carries six named ways that has happened. So the
/// thing to be able to check by reading is that there is no early return, no
/// `continue` and no second `println!` — one function takes a [`Line`], and
/// the only two things it can do with one are print it now and hold it.
///
/// A **bare** `wsp watch` sets [`Spec::wake`] false and every line is a wake,
/// so its output is byte-for-byte what it was before this existed: a person in
/// front of a stream wants the heartbeat, and `core-014` d2 traded that away
/// only for the reader that is asleep.
struct Stream<'a> {
    spec: &'a Spec,
    p: Paint,
    /// Judged worth a context read this tick, in the order it happened.
    hot: Vec<Spooled>,
    /// Judged not worth one *on its own*, this tick. Merged into the durable
    /// spool by [`Stream::tick`].
    cold: Vec<Spooled>,
}

impl<'a> Stream<'a> {
    fn new(spec: &'a Spec) -> Stream<'a> {
        Stream { spec, p: Paint::new(), hot: Vec::new(), cold: Vec::new() }
    }

    /// Offer a line. Print now, or hold — there is no third outcome, and no
    /// argument you can pass that reaches neither.
    fn put(&mut self, at: i64, l: Line) {
        // A bare watch has one disposition. The table is only consulted for a
        // reader who is not there to read.
        let d = match self.spec.wake {
            true => l.disposition(),
            false => Disposition::Wake,
        };
        let held = Spooled::of(at, l);
        match d {
            Disposition::Wake => self.hot.push(held),
            Disposition::Spool => self.cold.push(held),
        }
    }

    /// End of tick: decide, write, and hand back what is still held.
    ///
    /// `spool` is passed in rather than owned because **the register is the
    /// truth**, not this process's memory: `wsp watch --drain` may have emptied
    /// it from another process since the last tick, and a spool kept only here
    /// would deliver those entries a second time. See [`run`], which re-reads
    /// it every tick.
    /// Returns what was written, which is empty on a tick that earned no
    /// wake. The count of non-empty ticks is this row's whole claim — see
    /// [`the_measured_session_is_replayed_and_the_wake_count_is_counted`] —
    /// and a claim nothing can count is an estimate.
    fn tick(&mut self, at: i64, spool: &mut Spool) -> Vec<Spooled> {
        spool.held.append(&mut self.cold);
        // Escalation. Without it a fleet that never earns a wake never delivers
        // its backlog, and the drop is back in through the door — so a spooled
        // fact nobody has collected becomes justification by itself and carries
        // everything else out with it. See [`DEFER_MAX`].
        let overdue = spool.oldest().is_some_and(|o| at - o >= self.spec.defer_max);
        if self.hot.is_empty() && !overdue {
            return Vec::new();
        }
        // The wake first, then the backlog under it. The backlog costs nothing:
        // the context read was already being paid for by the line above it.
        //
        // **Except `over`, which goes last, and that is not tidiness.** A
        // consumer that reads until the stream says it is over is the obvious
        // one to write, and with the ending sitting on top of its own backlog
        // that consumer drops every line the wake was carrying — a drop this
        // row would have created by ordering, in the flush most likely to be
        // carrying something, since `over` wakes unconditionally. Found by
        // running it: a watch ended holding five heartbeats and printed them
        // underneath the line saying it had stopped.
        let mut out = std::mem::take(&mut self.hot);
        let (ending, rest): (Vec<_>, Vec<_>) = out.into_iter().partition(|h| h.class == Class::Over.word());
        out = rest;
        let carried = spool.take();
        out.extend(carried.iter().cloned());
        out.extend(ending);
        if !self.write(&out) {
            // Nothing arrived, so nothing clears. `core-021` is the case this
            // is shaped for — a wake that cannot be typed at a pane whose turn
            // is in flight — and the rule is the same either way: an entry
            // clears when something arrived, never when a send was attempted.
            spool.put_back(carried);
            return Vec::new();
        }
        out
    }

    /// Write, and say whether it arrived.
    ///
    /// A watch is read by an agent whose stdout is a pipe, and a pipe buffers.
    /// News held in a buffer until the next line arrives is news that is late
    /// by however long the fleet stays quiet, which on a quiet night is the
    /// whole night — so the flush is not housekeeping, it is the delivery, and
    /// its result is what the spool clears on.
    fn write(&self, out: &[Spooled]) -> bool {
        for h in out {
            println!("{}", h.say(self.spec, &self.p));
        }
        use std::io::Write;
        std::io::stdout().flush().is_ok()
    }
}

/// What `0 standing` means when the scope being watched is a **seat**, said
/// where the count is already being printed.
///
/// worklist-037, and the whole of what that row asked to be built. Two seats
/// agreed a hand-over by calendar — *"take `worklist` back when phase two is
/// done"* — and what actually fired was this number: `phase-two` went `done`,
/// the seat governed a finished list, and the next tick reported **0
/// standing**. A calendar rule needs somebody to remember it and to judge that
/// the moment has come; a level nobody has to author cannot be forgotten and is
/// already computed. It is `wsp-095`'s argument for SIGNAL over NOTIFICATION,
/// applied to the position rather than to the work.
///
/// **It says and it never acts, and that is not timidity.** `robustness-090`
/// d1 is that destroying a record is asked for and never automatic, and a slot
/// is a position — vacating one is the same class of act. A seat with nothing
/// standing is not necessarily a seat that should go: the next group's members
/// have not started yet, and work arriving one tick later would arrive at
/// nobody. So this hands a person the sentence at the moment they are already
/// looking, and `wsp govern --clear` stays something somebody types.
///
/// # Why it must stay a statement, which is the half the row did not know
///
/// **`0 standing` on a seat does not distinguish *nothing left to answer for*
/// from *nobody is answering*, and this file cannot tell them apart.** Driven
/// on a fake herdr: a seated pane alive with its turn abandoned, its member
/// working normally, `wsp watch --now` printing *nothing is up* and `wsp
/// doctor* printing *no problems*. The healthy seat and the dead one produce
/// the identical reading, and the difference between them is whether the seat
/// is turning — which nothing computes, because
/// [`cmd_govern::needs_a_person`] exempts a seat by construction and every
/// [`Kind`] above takes a task as its subject.
///
/// So anything that wired this number to an act would stand down a seat for
/// having died. The exemption is the joint: `standing == 0` is what makes an
/// idle seat *correct*, and `standing > 0` held past [`SETTLE`] is what would
/// make it a stall.
///
/// # `owes`, which is worklist-041 arriving on this line
///
/// The other half turned out not to be a predicate over `standing` at all — see
/// [`stalled_seats`], where the row's `stopped && standing > 0 && seat` is
/// driven and found false. What it produced instead is a seat's *obligation*,
/// and that fixes a second thing this line got wrong on its own terms.
///
/// **A seat at zero with a group nobody has started is not finished; it has not
/// begun.** The members of an unspawned group are `todo` with no agent on them,
/// which is invisible to every predicate in this file, so the count is honestly
/// zero — and the sentence under it said *stand down*, to a governor with a run
/// in front of it. [`Poll::owes_a_run`] is the fact that was missing, and the
/// advice is now offered only where it is safe to take.
fn nothing_addressed(scope: &Scope, standing: usize, owes: bool) -> Option<&'static str> {
    (scope.seated && standing == 0 && !owes)
        .then_some("nothing is addressed to this seat — wsp govern --clear stands it down")
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
    /// `--wake`: the reader is asleep, so a line printed has been judged worth
    /// a context read. Opt-in, and a bare watch is byte-for-byte what it was —
    /// a person in front of a stream wants the heartbeat, and `core-014` d2
    /// traded that away only for the reader that is not there.
    wake: bool,
    /// How old a spooled fact may get before it is a wake on its own. See
    /// [`DEFER_MAX`].
    defer_max: i64,
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
        let env = herdr::Env::read();
        let here = env.workspace_id.unwrap_or_default();
        let pane = env.pane_id.unwrap_or_default();
        let seated = !workspace.is_empty()
            && workspace == here
            && cmd_govern::governs(&governors, &here, Some(&pane)).as_deref() == Some(name.as_str());
        return Ok(Scope { seated, name, workspace, pane, all: false });
    }
    let env = herdr::Env::read();
    let Some(ws) = env.workspace_id else {
        return Err("wsp: no scope. Run this in the seat's workspace, or say `wsp watch -p <project>`".into());
    };
    let pane = env.pane_id.unwrap_or_default();
    match cmd_govern::governs(&governors, &ws, Some(&pane)) {
        Some(name) => Ok(Scope { name, seated: true, workspace: ws, pane, all: false }),
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
        wake: args.has("wake"),
        defer_max: dur("defer-max", DEFER_MAX)?,
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
    let asked =
        Scope { name, seated: false, workspace: scope.workspace.clone(), pane: scope.pane.clone(), all: false };
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
    if args.has("drain") {
        return drain(store, &spec);
    }
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
    let stamp = util::iso_at(at);
    let p = Paint::new();
    if now.is_empty() {
        // **Under `--json` too**, which it was not. The one call whose whole
        // point is that it answers *nothing is up* positively answered it with
        // zero bytes for the reader that cannot tell zero bytes from a binary
        // that failed to start. [`Class::Open`] is the word because that is
        // what this read is: the baseline, with no diff behind it.
        let said = match nothing_addressed(&spec.scope, 0, poll.owes_a_run()) {
            Some(note) => format!("nothing is up on {} — read just now · {note}", spec.scope.name),
            None => format!("nothing is up on {} — read just now", spec.scope.name),
        };
        println!("{}", note(Class::Open, at, spec, &said, &p));
        return 0;
    }
    for s in &now {
        match spec.json {
            true => println!("{}", s.envelope(Edge::Up, &spec.scope.name, &stamp)),
            false => println!("{}", line(&Emit { edge: Edge::Up, to: s.to.clone(), signal: s.clone(), held: 0 }, at, &p)),
        }
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
    // A cron's binary is replaced between two of its runs, which is the same
    // event the daemon meets as an `exec`. See [`resume`].
    let (mut ledger, known) = resume(&rec);
    let emits = match known {
        true => ledger.advance(&now, &poll.routing(), at, spec.settle),
        false => ledger.prime(&now, at),
    };
    say(&emits, at, spec);
    if !known {
        // Two reasons to have said nothing, and the reader wants to know which:
        // a first call is a subscription starting, and a re-prime is a tick
        // this caller has genuinely lost.
        let said = match rec.get("ledger").is_some() {
            true => format!(
                "re-primed on {} · {} standing · the last read was another build's, so this tick is a baseline and not a diff",
                spec.scope.name,
                ledger.standing()
            ),
            false => format!(
                "primed on {} · {} standing · from here, only what changes",
                spec.scope.name,
                ledger.standing()
            ),
        };
        println!("{}", note(Class::Open, at, spec, &said, &Paint::new()));
    }
    // pid 0, deliberately: this process is already over. A pull ledger is not a
    // reporter that can die, so it must never read as one — see
    // [`Registered::watching`].
    register_as(store, &key, spec, &ledger, 1, 0, &Spool::default());
    0
}

/// What the stream says when the binary underneath it is replaced.
///
/// **`worklist-033`, and it is a line rather than an `exec`.** `wsp panel`,
/// `wsp view` and the daemon all replace themselves when an install lands on
/// their path; this loop does not, and going on quietly is the failure — a
/// stale watch reports *nothing is up* out of logic that has since been fixed,
/// and there is nothing behind it to disagree. Driven side by side on
/// 2026-08-19, the older of two watches on one store listed five signals where
/// the newer listed six, and a question raised between them appeared on one
/// and never on the other. Not late. Absent, with no way to know the category
/// existed.
///
/// Re-`exec`ing here was weighed and is not done. A watch that replaces itself
/// mid-stream has to carry its ledger across, and the only ledger it has is in
/// this variable — so it would have to resume from the register, which changes
/// what a *freshly started* `wsp watch` means: today that is a subscription
/// beginning, and it would become one resuming whatever the last watch in this
/// pane had swallowed. That is a separate decision with its own failure, and it
/// is not worth taking to remove a line the reader can act on in one keystroke.
///
/// Once per install, not once per tick: the reading is adopted when it is
/// said, so a machine somebody is installing on all evening costs one line per
/// install and the warning never becomes wallpaper.
const REPLACED: &str = "wsp was replaced under this watch — it keeps the build it started with, \
                        so what follows is that build's logic and vocabulary. ^C and `wsp watch` again";

/// The loop.
fn run(store: &Store, poll: &mut Poll, spec: &Spec) -> i32 {
    let key = watch_key();
    let started = util::epoch_secs();
    let mut ledger = Ledger::default();
    let mut ticks: u64 = 0;
    let mut last_beat = started;
    // What we are executing, so the stream can say when an install lands under
    // it. The same reading `daemon::run` takes for the same event, and the
    // difference between the two is the whole of `worklist-033`: the daemon
    // `exec`s on it, and this cannot. See [`said_replaced`].
    let mut running = util::exe_stamp();

    let mut stream = Stream::new(spec);
    // What the last watch under this key was holding, which is routinely
    // something: a spool that dies with the process that wrote it is a drop
    // wearing the words *the process ended*. See [`spool_of`].
    let mut spool = spool_of(store, &key);

    let first = poll.sample();
    let primed = ledger.prime(&first, started);
    stream.put(started, opening(spec, ledger.standing(), poll.owes_a_run()));
    for e in primed {
        stream.put(started, Line::News(e));
    }
    stream.tick(started, &mut spool);
    register(store, &key, spec, &ledger, ticks, &spool);

    let over = loop {
        std::thread::sleep(std::time::Duration::from_secs(spec.every.max(1) as u64));
        let at = util::epoch_secs();
        ticks += 1;

        if !store.exists() {
            break Over::StoreGone;
        }
        // Re-read rather than carried, because `--drain` may have emptied it
        // from another process since the last tick — see [`spool_of`].
        spool = spool_of(store, &key);
        // Before the read, so a reader scrolling back finds the warning above
        // the first line it should not trust.
        if let Some(now) = util::exe_stamp().filter(|now| running.is_some_and(|was| was != *now)) {
            running = Some(now);
            stream.put(at, Line::Note(Class::Replaced, REPLACED.to_string()));
        }

        // Sampled first and asked for the routing after, because the routing
        // is a fact about the read that has just happened. See [`Routing`].
        let now = poll.sample();
        for e in ledger.advance(&now, &poll.routing(), at, spec.settle) {
            stream.put(at, Line::News(e));
        }

        if spec.heartbeat != i64::MAX && at - last_beat >= spec.heartbeat {
            last_beat = at;
            stream.put(at, heartbeat(spec, ledger.standing(), poll.owes_a_run(), at - started));
        }

        // One decision per tick, after everything this tick produced has been
        // offered. A wake carries the backlog out with it, so the order the
        // lines were put in is the order they are read in.
        stream.tick(at, &mut spool);
        register(store, &key, spec, &ledger, ticks, &spool);

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
        if spec.scope.seated
            && cmd_govern::governs(&store.governors(), &spec.scope.workspace, Some(&spec.scope.pane)).as_deref()
            != Some(spec.scope.name.as_str())
        {
            break Over::SeatVacated;
        }
    };

    let at = util::epoch_secs();
    // Item 5. There is no silent return from this verb: an ending that is not
    // said reads exactly like the process that died without saying anything,
    // which is the fault the whole file is against. It is also the line the
    // spool rides out on — `over` is a wake, so a watch that ends holding
    // something still delivers it, and a spool that died with its process
    // would be a drop with a different word on it.
    //
    // One sentence in both documents, where it used to be two. The JSON form
    // said `signal: "watch-over"` — a word outside [`Kind::word`]'s vocabulary
    // in the field that vocabulary is read from — and now says `class: "over"`,
    // which is the same fact in the field every other line of this stream also
    // carries. See [`Class::envelope`].
    stream.put(at, Line::Note(Class::Over, format!("stopped watching {} — {}", spec.scope.name, over.said())));
    stream.tick(at, &mut spool);
    match spool.depth() {
        // Nothing held, so the record goes and `doctor` stays quiet.
        0 => {
            store.clear_watch(&key);
        }
        // The ending could not be written — a closed pipe, most likely, which
        // is the reader having gone away. The record stays, so the next watch
        // in this pane resumes what this one was holding and `doctor` reports a
        // watcher that stopped with a backlog rather than nothing at all.
        _ => register(store, &key, spec, &ledger, ticks, &spool),
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

/// What the last watch under this key was holding.
///
/// **The register is the truth about the spool, not this process's memory.**
/// `wsp watch --drain` empties it from another process, and a watch that kept
/// its own copy would deliver those entries a second time; a watch that never
/// read it at start-up would leave behind everything the previous process in
/// this pane was holding when it died, which is a drop wearing the words *the
/// process ended*.
fn spool_of(store: &Store, key: &str) -> Spool {
    Spool::of_json(store.watches().get(key).and_then(|v| v.get("spool")).unwrap_or(&Value::Null))
}

/// `wsp watch --drain` — print what is held and clear it, for a governor that
/// is already awake and chooses to look.
///
/// Free by definition: the reader is running this, so the context read is
/// already being paid for. It is also the escape hatch that makes the rest of
/// this safe to trust — a design that decides when somebody is told is only
/// tolerable if the somebody can always ask.
fn drain(store: &Store, spec: &Spec) -> i32 {
    let key = watch_key();
    let at = util::epoch_secs();
    let mut spool = spool_of(store, &key);
    let stream = Stream::new(spec);
    if spool.depth() == 0 {
        // Said, and said positively. Nothing held and no watch at all are
        // different facts and a reader draining a spool wants to know which.
        let said = match store.watches().contains_key(&key) {
            true => format!("nothing held on {} — the watch has told you everything it has", spec.scope.name),
            false => format!("nothing held on {} — no watch is registered as {key}", spec.scope.name),
        };
        println!("{}", note(Class::Open, at, spec, &said, &Paint::new()));
        return 0;
    }
    let held = spool.take();
    if !stream.write(&held) {
        spool.put_back(held);
        return 1;
    }
    // Only now, and only this field: the record belongs to a watch that is
    // probably still running, and rewriting the rest of it from here would
    // stamp this process's pid over a live reporter's.
    if let Some(mut rec) = store.watches().get(&key).cloned() {
        if let Some(o) = rec.as_object_mut() {
            o.insert("spool".into(), spool.json());
        }
        store.set_watch(&key, rec);
    }
    0
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

fn register(store: &Store, key: &str, spec: &Spec, ledger: &Ledger, ticks: u64, spool: &Spool) {
    register_as(store, key, spec, ledger, ticks, std::process::id(), spool)
}

fn register_as(store: &Store, key: &str, spec: &Spec, ledger: &Ledger, ticks: u64, pid: u32, spool: &Spool) {
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
            // Two readers, one field. [`resume`] asks it whose key rules the
            // ledger below was written by; `--status`, `doctor` and
            // `wsp install` ask it whether the process that wrote this record
            // is running the binary that is now live — which is the same
            // question about the same file, and `worklist-033` is the half of
            // it that has no `exec` behind it.
            "build": crate::build_stamp(),
            "ledger": ledger.json(),
            // Beside the ledger, under the same key, written in the same atomic
            // update — so it survives exactly what the ledger survives and
            // `--status` and `doctor` can read its depth for nothing. See
            // [`Spool`].
            "spool": spool.json(),
        }),
    );
}

/// `wsp watch --status` — who is watching what, and whether they still are.
/// What a watch is holding, said where the standing count already is.
///
/// Empty on everything that is holding nothing, so a watch without `--wake` —
/// which can never hold anything — is byte-for-byte the line it always was.
fn held(w: &Registered) -> String {
    match w.spooled {
        0 => String::new(),
        n => format!(" · holding {n} · wsp watch --drain"),
    }
}

fn status(store: &Store, args: &Args) -> i32 {
    let watches = registered(store);
    if args.json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&watches
                .iter()
                .map(|w| json!({ "key": w.key, "scope": w.scope, "pid": w.pid, "tick": w.tick, "stale": w.stale(), "watching": w.watching(),
                                 "build": w.build, "stale_build": w.stale_build(), "spooled": w.spooled }))
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
            // Ahead of the standing count, because a count from an old build is
            // the thing this line exists to stop being read as a reassurance —
            // `worklist-033`: five signals where the fixed binary read six.
            (false, false, true) if w.stale_build() => {
                p.red(&format!("on {} — restart it to pick up this build", w.build))
            }
            // Said rather than left blank: a pull ledger looks exactly like a
            // running watch in this list, and the reader is here to find out
            // which of their watches is still going.
            (false, false, false) => p.dim(&format!("{} standing · read on demand", w.standing)),
            (false, false, true) => p.dim(&format!("{} standing{}", w.standing, held(w))),
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

    // ---- what a seat is told about itself ----------------------------------

    fn scope(name: &str, seated: bool) -> Scope {
        Scope { name: name.into(), seated, workspace: "w1".into(), pane: "w1:p1".into(), all: false }
    }

    /// The row worklist-037 was filed on: two seats agreed a hand-over by
    /// calendar and what fired was this number. It is said at the moment
    /// somebody is already reading the count, and it names the verb rather
    /// than running it.
    #[test]
    fn a_seat_with_nothing_standing_is_told_what_that_means() {
        let note =
            nothing_addressed(&scope("phase-two", true), 0, false).expect("a seat at zero is told");
        assert!(note.contains("nothing is addressed to this seat"), "{note}");
        assert!(note.contains("wsp govern --clear"), "it names the stand-down: {note}");
    }

    /// And never at the moment it would be wrong. A seat with something up is
    /// a seat with work to do, and telling it how to vacate is the opposite of
    /// what the level says.
    #[test]
    fn a_seat_with_something_standing_is_not_offered_the_stand_down() {
        assert_eq!(nothing_addressed(&scope("phase-two", true), 1, false), None);
    }

    /// A watch on a project this pane does not sit in gets nothing added, and
    /// that is not tidiness: `wsp govern --clear` stands *this workspace*
    /// down, so offering it to a reader who holds no seat would name a verb
    /// that does nothing — and every existing line stays byte-for-byte what it
    /// was, which is the bar anything added to a stream a governor runs all
    /// night is held to.
    #[test]
    fn a_watch_on_a_scope_it_does_not_sit_in_is_told_nothing_about_standing_down() {
        assert_eq!(nothing_addressed(&scope("robustness", false), 0, false), None);
        assert_eq!(
            nothing_addressed(&Scope::machine(), 0, false),
            None,
            "and neither is the daemon's pass"
        );
    }

    /// `worklist-041` on `worklist-037`'s line. A seat at zero with a group
    /// nobody has started is not finished, it has not begun — the members are
    /// `todo` with no agent on them, which no predicate in this file can see, so
    /// the count is honestly zero and the advice under it was *go home*.
    #[test]
    fn a_seat_with_a_run_it_has_not_started_is_not_offered_the_stand_down() {
        assert_eq!(nothing_addressed(&scope("phase-two", true), 0, true), None);
    }

    /// And the fact itself, read off the store rather than off the level set,
    /// because the members that make the zero misleading are exactly the ones
    /// no level is derived from.
    #[test]
    fn a_seat_owes_a_run_until_every_member_of_it_is_settled() {
        let night = Night::new("owes")
            .projects(&[("nightly", None)])
            .task("nightly-1", "nightly", Status::Todo)
            .running("tonight", &["nightly-1"])
            .seat("nightly", "w1:p1", false);
        let seated = Scope {
            name: "nightly".into(),
            seated: true,
            workspace: "w1".into(),
            pane: "w1:p1".into(),
            all: false,
        };
        let poll = |store| Poll::new(store, seated.clone(), BTreeSet::new(), None);
        assert!(poll(&night.store).owes_a_run(), "a member nobody has spawned is still owed");

        let mut t = night.store.task("nightly-1").unwrap();
        t.set_status(Status::Review);
        night.store.save_task(&t).unwrap();
        assert!(!poll(&night.store).owes_a_run(), "and review is where a run's work stops");
    }

    // ---- the questions somebody wrote down ---------------------------------

    use crate::message::{About, Message, Party, Shape, Waiting};

    /// A question asked from `w1:p1`, about `a-1`, by an agent working `a-1`.
    fn question(text: &str) -> Message {
        Message::question(
            Party::pane("w1:p1", "w1"),
            crate::message::Kind::Note,
            text,
            Waiting::new("w1:p1", "a-1"),
        )
        .about(About::Task("a-1".into()))
    }

    /// One census row, with only the fields [`asking`] reads.
    fn row(pane: &str, turning: bool) -> cmd_agent::WipRow {
        cmd_agent::WipRow {
            project: String::new(),
            task: String::new(),
            task_id: "a-1".into(),
            pane: pane.into(),
            workspace: "w1".into(),
            state: String::new(),
            turning,
            needs_you: false,
            seat: None,
        }
    }

    /// **The thing this task is about.** An agent stopped and asked something;
    /// until now that reached whoever ran `wsp ask`, and the record's own
    /// `open_questions` had no caller at all. It is a level, so it needs no
    /// verb to raise it and none to lower it.
    #[test]
    fn a_question_nobody_has_answered_is_a_level_with_the_words_in_it() {
        let q = question("can I land this?\n\nthe branch is green");
        let s = asking(&q, "a-1", &[row("w1:p1", false)]).expect("an open question is a level");

        assert_eq!(s.kind, Kind::Unanswered);
        assert_eq!(s.subject, "a-1", "addressed on the work, so `seat_for` can route it");
        assert!(s.detail.contains("can I land this?"), "the words, not the task's title: {}", s.detail);
        assert!(s.detail.contains("w1:p1 waiting"), "and who is sitting still: {}", s.detail);
        assert!(
            s.detail.contains(&format!("wsp answer {}", q.id)),
            "and what to type, which needs the message id and not the task's: {}",
            s.detail
        );
        assert!(s.at_once, "a question does not flap: nobody asks one twice a second");
    }

    /// `worklist-017`'s finding, kept out of this record by construction. A
    /// flag is keyed by task id and a second one silently replaces the first;
    /// a level about a message is keyed by the message, so two questions on one
    /// task are two facts and answering one leaves the other standing.
    #[test]
    fn two_questions_about_one_task_are_two_facts_and_neither_eats_the_other() {
        let rows = [row("w1:p1", false)];
        let first = asking(&question("may I land?"), "a-1", &rows).unwrap();
        let second = asking(&question("and which branch?"), "a-1", &rows).unwrap();
        assert_ne!(first.key(), second.key(), "the second question overwrote the first");

        let mut l = Ledger::default();
        l.prime(&[], 0);
        assert_eq!(l.advance(&[first.clone(), second.clone()], &Routing::Unknown, 60, 0).len(), 2);
        assert_eq!(l.standing(), 2);

        // The first is answered. The second is untouched and still up.
        let out = l.advance(&[second], &Routing::Unknown, 120, 0);
        assert_eq!(out.len(), 1, "one clearing, not two");
        assert_eq!(out[0].edge, Edge::Down);
        assert_eq!(out[0].signal.record, first.record, "and it is the one that was answered");
        assert_eq!(l.standing(), 1, "the other question is still waiting on somebody");
    }

    /// `worklist-013`: the question was answered in minutes and the raised hand
    /// stood for 2h14m, because the answer and the record were two acts and
    /// only one of them was on the path of getting the work moving. A level
    /// cannot have that failure — nothing lowers it, it stops being true.
    #[test]
    fn answering_a_question_takes_its_level_down_with_nobody_lowering_anything() {
        let q = question("may I land?");
        let up = asking(&q, "a-1", &[row("w1:p1", false)]).unwrap();
        let mut l = Ledger::default();
        l.prime(&[], 0);
        assert_eq!(l.advance(&[up], &Routing::Unknown, 60, 0).len(), 1);

        // `wsp answer` closed it, so it is simply absent from the next read.
        let mut answered = q.clone();
        answered.state_raw = "answered".into();
        assert!(asking(&answered, "a-1", &[]).is_none(), "a closed question is not a level");

        let out = l.advance(&[], &Routing::Unknown, 120, 0);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].edge, Edge::Down, "the governor is told it no longer needs them");
    }

    /// The seat's instruction, as an assertion: `needs_attention`, never
    /// `is_open`. An older binary meeting a state a newer one wrote must not
    /// answer *nothing is up* — "I cannot read this" is a first-class reason to
    /// fetch a person, and reporting it as quiet is this task's own fault
    /// arriving through the module built to remove it.
    #[test]
    fn a_record_this_build_cannot_read_still_fetches_a_person() {
        let mut q = question("may I land?");
        q.state_raw = "deferred-until-tuesday".into();
        assert!(!q.is_open(), "the premise: an unknown state is not open");

        let s = asking(&q, "a-1", &[row("w1:p1", false)]).expect("and it is still somebody's");
        assert!(s.detail.contains("cannot read"), "and it says which half it could not read");
        assert_eq!(s.loudness(), crate::message::Kind::Direction, "wsp being confused is a person's job");
    }

    /// **The split, from the side that decides it.** `asking` answering `None`
    /// is what sends a record to (b)'s `flag` line instead, so a notification
    /// — open or acknowledged — is never an `unanswered` level. `wsp-095`
    /// Part 3: one is over when it is sent, the other has an agent sitting
    /// still while it is open, and a governor reading one word for both could
    /// not tell which it had.
    #[test]
    fn a_notification_is_a_raised_hand_and_never_an_unanswered_one() {
        let mut m = Message::new(Party::pane("w1:p1", "w1"), crate::message::Kind::Note, "landed");
        m.about = About::Task("a-1".into());
        assert_eq!(m.shape(), Some(Shape::Notification));
        assert!(m.needs_attention(), "the premise: it is open and wants a disposition");
        assert!(asking(&m, "a-1", &[]).is_none(), "and it is still not a level");

        m.state_raw = "acknowledged".into();
        assert!(asking(&m, "a-1", &[]).is_none());
    }

    /// The sender already said how much of your time this may take, so wsp does
    /// not get a second opinion. A `stop` question reaching a hook as `note`
    /// is the difference between a phone that rings and one that does not.
    #[test]
    fn a_question_is_as_loud_as_whoever_asked_it_said_it_was() {
        let mut q = question("do NOT run git stash pop");
        q.kind_raw = "stop".into();
        let s = asking(&q, "a-1", &[row("w1:p1", false)]).unwrap();
        assert_eq!(s.loudness(), crate::message::Kind::Stop);
        assert_eq!(s.envelope(Edge::Up, "wsp", "")["kind"], "stop");

        let quiet = asking(&question("which of these two?"), "a-1", &[]).unwrap();
        assert_eq!(quiet.envelope(Edge::Up, "wsp", "")["kind"], "note");
    }

    /// A hook is handed the envelope and nothing else. Without the record's id
    /// it can say somebody is waiting and cannot say what to answer, which is
    /// `worklist-018` — a message stored, listed, reported delivered, and never
    /// seen by its reader.
    #[test]
    fn the_envelope_names_the_question_so_its_reader_can_answer_it() {
        let q = question("may I land?");
        let e = asking(&q, "a-1", &[row("w1:p1", false)]).unwrap().envelope(Edge::Up, "wsp", "now");
        assert_eq!(e["id"], q.id, "the record, so `wsp answer` has an argument");
        assert_eq!(e["about"], "a-1", "the work, so a person knows where to look");
        assert_eq!(e["shape"], "signal", "a level: there is no disposition owed on this");

        // And a level with no record behind it carries no id at all, rather
        // than a null for a shell script to forget to test for.
        let stall = sig(Kind::NeedsAPerson, "a-1").envelope(Edge::Up, "wsp", "now");
        assert!(stall.get("id").is_none());
    }

    /// An agent that asked and carried on is not sitting still, and saying it
    /// is would train a governor to discount the line. The question is still
    /// unanswered, so the level is still up — this changes the wording and not
    /// the fact.
    #[test]
    fn an_asker_that_is_still_turning_is_not_reported_as_waiting() {
        let q = question("when you get a moment, which of these two?");
        let s = asking(&q, "a-1", &[row("w1:p1", true)]).expect("still unanswered, so still up");
        assert!(s.detail.contains("still working"), "{}", s.detail);

        let gone = asking(&q, "a-1", &[]).unwrap();
        assert!(gone.detail.contains("gone"), "an asker that has been despawned: {}", gone.detail);
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
        assert!(l.advance(&now, &Routing::Unknown, 60, SETTLE).is_empty(), "nothing changed, so nothing is said");
    }

    /// Emit on change, never on state. The difference between a watcher and one
    /// event per poll for ever.
    #[test]
    fn a_level_that_stays_up_is_said_once_and_not_again() {
        let now = vec![sig(Kind::Review, "a-1")];
        let mut l = Ledger::default();
        l.prime(&[], 0);
        assert_eq!(l.advance(&now, &Routing::Unknown, 60, SETTLE).len(), 1);
        for tick in 2..10 {
            assert!(l.advance(&now, &Routing::Unknown, tick * 60, SETTLE).is_empty(), "tick {tick} said it again");
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
        assert_eq!(l.advance(&first, &Routing::Unknown, 60, 0).len(), 1);
        assert!(l.advance(&later, &Routing::Unknown, 3000, 0).is_empty());
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
        assert_eq!(l.advance(&seeing, &Routing::Unknown, 60, 0).len(), 2);

        // herdr goes away. The store half still answers, so `review` is a real
        // reading and stays; the agent half is unreadable, so it is held.
        let blind = vec![sig(Kind::Review, "a-2"), sig(Kind::Blind, "herdr")];
        let out = l.advance(&blind, &Routing::Unknown, 120, 0);
        assert_eq!(out.len(), 1, "only `blind` itself is news");
        assert_eq!(out[0].signal.kind, Kind::Blind);
        assert_eq!(l.standing(), 3, "the stall is still up, it is just not being looked at");

        // And back. Nothing is re-announced, because nothing changed.
        let out = l.advance(&seeing, &Routing::Unknown, 180, 0);
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
        l.advance(&[sig(Kind::NeedsAPerson, "a-1")], &Routing::Unknown, 60, 0);
        l.advance(&[sig(Kind::Blind, "herdr")], &Routing::Unknown, 120, 0);

        let out = l.advance(&[], &Routing::Unknown, 180, 0);
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
        assert!(l.advance(&stalled, &Routing::Unknown, 60, 300).is_empty(), "one minute in, this is a gap between turns");
        assert!(l.advance(&stalled, &Routing::Unknown, 240, 300).is_empty());
        assert_eq!(l.advance(&stalled, &Routing::Unknown, 360, 300).len(), 1, "six minutes in, nothing is going to restart it");
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
        let out = l.advance(&prompt, &Routing::Unknown, 1, 300);
        assert_eq!(out.len(), 1);
        assert!(
            out[0].signal.shouts(),
            "and it is the loud reading, which the envelope sends as `direction`"
        );
    }

    /// A stall that never settled was never said, so its going away is not news
    /// either. Reporting it would tell a governor about an agent it was never
    /// told about.
    #[test]
    fn a_level_that_goes_away_before_it_settled_is_never_mentioned_in_either_direction() {
        let mut l = Ledger::default();
        l.prime(&[], 0);
        let stalled = vec![sig(Kind::NeedsAPerson, "a-1").settling()];
        assert!(l.advance(&stalled, &Routing::Unknown, 60, 300).is_empty());
        assert!(l.advance(&[], &Routing::Unknown, 120, 300).is_empty());
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
        assert_eq!(l.advance(&up, &Routing::Unknown, 60, 0).len(), 2);
        let out = l.advance(&[], &Routing::Unknown, 120, 0);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].signal.kind, Kind::NeedsAPerson);
        assert_eq!(out[0].edge, Edge::Down);
    }

    /// Item 4 of the six ways silence lies. Everything else a blind watch does
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
        assert_eq!(l.advance(&up, &Routing::Unknown, 60, 0).len(), 1);

        let mut back = Ledger::of_json(&l.json());
        assert!(back.advance(&up, &Routing::Unknown, 120, 0).is_empty(), "it has already been said");
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
        assert_eq!(l.advance(&up, &Routing::Unknown, 60, 0).len(), 1);

        let mut back = Ledger::of_json(&l.json());
        let out = back.advance(&[], &Routing::Unknown, 120, 0);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].edge, Edge::Down);
        assert_eq!(out[0].signal.kind, Kind::Flag);
    }

    // ---- a ledger the previous build wrote ---------------------------------

    /// One watch record as a build writes it: the ledger, and the stamp saying
    /// whose key rules it is in.
    fn record(l: &Ledger, build: &str) -> Value {
        json!({ "build": build, "ledger": l.json() })
    }

    /// **`worklist-034`, from the side that produced it.** A hand that was up
    /// before an install and is up after it, keyed `flag:a-1` by the build that
    /// wrote the ledger and `flag:a-1:m-1` by the build reading it — the
    /// `record` component `34ae8a3` added.
    ///
    /// Diffed, that is one clearing that never happened and one raising for a
    /// level that never went down, and `wsp flag` lists the hand throughout
    /// both. The fix is upstream of the diff: a ledger another build keyed is
    /// not something to diff against.
    #[test]
    fn a_hand_that_never_moved_is_not_lowered_and_raised_by_an_install() {
        let mut before = Ledger::default();
        before.prime(&[], 0);
        assert_eq!(before.advance(&[Signal::new(Kind::Flag, "a-1", "can I take this?")], &Routing::Unknown, 60, 0).len(), 1);

        // The same hand, read by a build that puts the record in the key.
        let after = vec![Signal::new(Kind::Flag, "a-1", "can I take this?").of("m-1")];

        let mut diffed = Ledger::of_json(&before.json());
        let wrong = diffed.advance(&after, &Routing::Unknown, 120, 0);
        assert_eq!(wrong.len(), 2, "the defect itself, so this test fails if the keying stops moving: {wrong:?}");

        let (mut ledger, known) = resume(&record(&before, "another-build"));
        assert!(!known, "the stamp is what says this may not be diffed against");
        assert!(ledger.prime(&after, 120).is_empty(), "nothing moved, so nothing is said");
        assert_eq!(ledger.standing(), 1, "and the hand is still up, under this build's key");
    }

    /// The other half of the same guard: an install that did not move a key
    /// still primes, and a prime that threw the ledger away would reset every
    /// standing level's clock. Ed installs several times a day, so `held` on
    /// the eventual clearing would have measured the time since the last
    /// install rather than how long the hand was up.
    #[test]
    fn re_priming_keeps_the_time_a_standing_level_went_up() {
        let up = vec![Signal::new(Kind::Flag, "a-1", "can I take this?")];
        let mut before = Ledger::default();
        before.prime(&[], 0);
        assert_eq!(before.advance(&up, &Routing::Unknown, 60, 0).len(), 1);

        let (mut ledger, _) = resume(&record(&before, "another-build"));
        assert!(ledger.prime(&up, 600).is_empty());
        let out = ledger.advance(&[], &Routing::Unknown, 660, 0);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].held, 600, "measured from when the hand went up, not from the install");
    }

    /// A level that genuinely went away while the daemon was replacing itself
    /// is dropped and **not** reported cleared. That is the cost of the prime
    /// and it is the whole of it: one tick of edges lost, in exchange for never
    /// inventing one. The level is still readable in `wsp wip`, `doctor` and
    /// the panel, which is where a lost push is recovered from.
    #[test]
    fn a_level_that_went_away_across_a_re_prime_is_dropped_and_not_cleared() {
        let mut before = Ledger::default();
        before.prime(&[], 0);
        assert_eq!(before.advance(&[Signal::new(Kind::Flag, "a-1", "up")], &Routing::Unknown, 60, 0).len(), 1);

        let (mut ledger, _) = resume(&record(&before, "another-build"));
        assert!(ledger.prime(&[], 120).is_empty());
        assert_eq!(ledger.standing(), 0);
    }

    /// The stamp is the *build*, so a ledger this build wrote is diffed exactly
    /// as it always was — the guard costs a tick only where a tick was already
    /// going to be wrong.
    #[test]
    fn a_ledger_this_build_wrote_is_still_diffed_against() {
        let mut l = Ledger::default();
        l.prime(&[], 0);
        let up = vec![Signal::new(Kind::Flag, "a-1", "up")];
        assert_eq!(l.advance(&up, &Routing::Unknown, 60, 0).len(), 1);

        let (mut back, known) = resume(&record(&l, &crate::build_stamp()));
        assert!(known);
        let out = back.advance(&[], &Routing::Unknown, 120, 0);
        assert_eq!(out.len(), 1, "and the clearing still arrives: {out:?}");
        assert_eq!(out[0].edge, Edge::Down);
    }

    /// Two ways to have nothing to diff against, and they are one branch. A
    /// record from before the stamp existed cannot be placed, so it is treated
    /// as another build's — which is what it is.
    #[test]
    fn a_ledger_with_no_stamp_and_no_ledger_at_all_are_both_a_baseline() {
        let mut l = Ledger::default();
        l.prime(&[sig(Kind::Flag, "a-1")], 0);
        assert!(!resume(&json!({ "ledger": l.json() })).1, "written before there was a stamp to write");
        assert!(!resume(&Value::Null).1, "nobody has ever watched here");
        assert_eq!(resume(&Value::Null).0.standing(), 0);
    }

    /// Item 4 of the six ways silence lies survives the re-prime in both
    /// directions: a watch that comes up blind says so, and one that was
    /// already saying so does not say it twice because its binary changed.
    #[test]
    fn a_re_prime_says_blind_only_when_the_reader_has_not_been_told() {
        let blind = vec![sig(Kind::Blind, "herdr")];
        let mut first = Ledger::default();
        assert_eq!(first.prime(&blind, 0).len(), 1, "a watch blind from its first tick says so");

        let (mut again, _) = resume(&record(&first, "another-build"));
        assert!(again.prime(&blind, 60).is_empty(), "and an install is not a second reason to say it");
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
            build: crate::build_stamp(),
            spooled: 0,
        };
        assert!(!pull.watching());
        assert!(!pull.stale(), "a day old and still not a fault: nobody promised to tick");
    }

    /// One register, as a surface finds it.
    fn reg(pid: u32, daemon: bool, build: &str) -> Registered {
        Registered {
            key: "w1:p1".into(),
            scope: "wsp".into(),
            pid,
            host: String::new(),
            tick: util::now_iso(),
            every: 60,
            standing: 0,
            daemon,
            build: build.into(),
            spooled: 0,
        }
    }

    /// **`worklist-033`.** A watch does not re-`exec` when an install lands, so
    /// it goes on reporting in the vocabulary it was born with. Four instances
    /// in one day across two seats and every detection was a person
    /// remembering; this is the comparison that makes it a fact.
    ///
    /// Ticking, on time, and wrong — which is why it is its own reading and not
    /// a clause on [`Registered::stale`].
    #[test]
    fn a_watch_still_running_an_older_build_is_a_fact_rather_than_something_to_remember() {
        let old = reg(1, false, "5a1f71d");
        assert!(!old.stale(), "it is ticking, which is the whole difficulty");
        assert!(old.stale_build());
        assert!(!reg(1, false, &crate::build_stamp()).stale_build(), "this build is not a fault");
    }

    /// The daemon is the one long-lived process that *does* replace itself —
    /// `daemon::reload` `exec`s within a tick of a build landing on its path.
    /// Naming it would put a line on every install that is wrong by the time
    /// anybody reads it, and a check that cries wolf is one people skip along
    /// with the ones that mean something.
    #[test]
    fn the_daemon_is_not_named_for_a_build_it_execs_into_within_the_tick() {
        assert!(!reg(1, true, "5a1f71d").stale_build());
    }

    /// Two records that describe no running process, for two different reasons,
    /// and neither is a stale reporter: a pull ledger has no process at all,
    /// and a register written before the stamp existed cannot answer the
    /// question. An unanswerable question is not a fault to print in red.
    #[test]
    fn nothing_that_cannot_be_holding_an_old_binary_is_reported_as_holding_one() {
        assert!(!reg(0, false, "5a1f71d").stale_build(), "a `--once` ledger is not a process");
        assert!(!reg(1, false, "").stale_build(), "written before there was a stamp to write");
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
                build: crate::build_stamp(),
                spooled: 0,
            };
            r.tick = util::iso_at(util::epoch_secs() - ago);
            r
        };
        assert!(!at(60).stale());
        assert!(!at(120).stale());
        assert!(at(600).stale());
    }

    // ---- a subject that moved out from under the reader --------------------
    //
    // `worklist-039`. Every one of these is a level that is **still true**, so
    // anything the reader is told about it going away is false; the question is
    // only whether the file can tell that from a level that genuinely went
    // down. See [`Routing`].

    /// A scope that has lost a subject to another seat, as the diff sees it.
    fn moved_to(subject: &str, seat: &str) -> Routing {
        Routing::Scoped {
            inside: BTreeSet::new(),
            to: [(subject.to_string(), seat.to_string())].into_iter().collect(),
        }
    }

    /// **The half the row did not name, and the worse one.** `flag` clears, so
    /// a seat swap did not merely lose the hand quietly — it said out loud that
    /// it had been lowered. Driven on 2026-08-20 against a fake herdr, two
    /// seats and a worklist finishing under them: `cleared  demo-001  flag ·
    /// was up 0s`, while `wsp flag` went on listing it and the other seat now
    /// held it.
    #[test]
    fn a_hand_that_another_seat_now_holds_is_not_reported_as_lowered() {
        let up = [sig(Kind::Flag, "a-1")];
        let mut l = Ledger::default();
        l.prime(&[], 0);
        assert_eq!(l.advance(&up, &Routing::Unknown, 60, 0).len(), 1);

        let out = l.advance(&[], &moved_to("a-1", "demo"), 120, 0);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].edge, Edge::Left, "it did not clear — somebody else answers for it");
        assert_eq!(out[0].to, EVERYONE, "and the line is for the seat that lost it");
        assert_eq!(out[0].signal.to, "demo", "which needs to know where it went");
        assert_eq!(out[0].held, 60, "and how long it had been up here");
    }

    /// The row's own finding, from the other side of `clears()`. A `review`
    /// left one scope and went nowhere at all, because a review is not a thing
    /// that goes away by itself and the diff had no other word for it.
    #[test]
    fn a_level_that_never_clears_still_says_something_when_it_leaves_the_scope() {
        let up = [sig(Kind::Review, "a-1")];
        assert!(!Kind::Review.clears(), "the premise: nothing about it going away is news");
        let mut l = Ledger::default();
        l.prime(&[], 0);
        assert_eq!(l.advance(&up, &Routing::Unknown, 60, 0).len(), 1);

        let out = l.advance(&[], &moved_to("a-1", "demo"), 120, 0);
        assert_eq!(out.len(), 1, "a hand one seat stops seeing and the other never started seeing");
        assert_eq!(out[0].edge, Edge::Left);
    }

    /// And the thing that must not change with it: a predicate that genuinely
    /// went false is still a clearing, and is still silent for the kinds that
    /// do not clear.
    #[test]
    fn a_predicate_that_went_false_is_a_clearing_and_not_a_move() {
        let still_mine = |subject: &str| Routing::Scoped {
            inside: [subject.to_string()].into_iter().collect(),
            to: [(subject.to_string(), "demo".to_string())].into_iter().collect(),
        };
        let mut l = Ledger::default();
        l.prime(&[], 0);
        l.advance(&[sig(Kind::Flag, "a-1")], &Routing::Unknown, 60, 0);
        let out = l.advance(&[], &still_mine("a-1"), 120, 0);
        assert_eq!(out[0].edge, Edge::Down, "the subject never left; the hand came down");

        let mut l = Ledger::default();
        l.prime(&[], 0);
        l.advance(&[sig(Kind::Review, "a-1")], &Routing::Unknown, 60, 0);
        assert!(
            l.advance(&[], &still_mine("a-1"), 120, 0).is_empty(),
            "and a review leaving review is nearly always the reader having just closed it"
        );
    }

    /// A subject the routing cannot place at all has genuinely gone — a task
    /// deleted, or an id a renumbering did not reach. Nobody can be pointed at
    /// it, so `moved` would be a line with nowhere to send the reader.
    #[test]
    fn a_subject_that_is_no_longer_anywhere_is_a_clearing_rather_than_a_move() {
        let mut l = Ledger::default();
        l.prime(&[], 0);
        l.advance(&[sig(Kind::Flag, "a-1")], &Routing::Unknown, 60, 0);
        let empty = Routing::Scoped { inside: BTreeSet::new(), to: BTreeMap::new() };
        let out = l.advance(&[], &empty, 120, 0);
        assert_eq!(out[0].edge, Edge::Down);
    }

    /// **The named-project case, which must not gain this.** A seat taken over
    /// `wsp` readdresses every level under it and moves not one task out of the
    /// scope somebody typed, so a watch on that project has nothing to say —
    /// and saying it would be `robustness-088`'s named failure, a line per
    /// standing level every time a governor sits down.
    #[test]
    fn a_watch_on_a_named_project_is_not_told_a_level_moved_because_the_seat_above_it_changed() {
        let inside = |seat: &str| Routing::Scoped {
            inside: ["a-1".to_string()].into_iter().collect(),
            to: [("a-1".to_string(), seat.to_string())].into_iter().collect(),
        };
        let mut l = Ledger::default();
        l.prime(&[], 0);
        assert_eq!(l.advance(&[sig(Kind::Flag, "a-1").to("phase-two")], &inside("phase-two"), 60, 0).len(), 1);
        assert!(
            l.advance(&[sig(Kind::Flag, "a-1").to("wsp")], &inside("wsp"), 120, 0).is_empty(),
            "nothing left the project, so nothing happened to this reader"
        );
    }

    /// **The daemon's half, and it is not a departure at all.**
    /// [`Scope::machine`] has no boundary to leave, so a move shows up as the
    /// address on a standing level changing under it — which is exactly what
    /// nothing was watching on 2026-08-20, when a raise addressed to
    /// `phase-four` was followed three minutes later by a clearing addressed to
    /// `demo` and neither seat could see the pair.
    #[test]
    fn the_machine_pass_says_a_level_moved_when_the_seat_answering_for_it_changes() {
        let mut l = Ledger::default();
        l.prime(&[], 0);
        assert_eq!(l.advance(&[sig(Kind::Flag, "a-1").to("phase-four")], &Routing::Machine, 60, 0).len(), 1);

        let out = l.advance(&[sig(Kind::Flag, "a-1").to("demo")], &Routing::Machine, 660, 0);
        assert_eq!(out.len(), 2, "one seat lost it and another gained it");
        assert_eq!(out[0].edge, Edge::Left);
        assert_eq!(out[0].to, "phase-four", "the seat that was told the hand went up hears it went somewhere");
        assert_eq!(out[1].edge, Edge::Up);
        assert_eq!(out[1].to, "demo", "and the seat that has it now is told, in the words it would have had anyway");
        assert_eq!(out[1].held, 600, "carrying how long the hand has actually been up, not how long it has been theirs");
    }

    /// A level still inside its settle window was never news here, so its
    /// moving is not news either — the same judgement the clearing path makes,
    /// and for the same reason: the flicker the settle exists to prevent must
    /// not come back through the routing.
    #[test]
    fn a_level_that_had_not_been_said_yet_moves_without_a_word() {
        let stalled = [Signal::new(Kind::NeedsAPerson, "a-1", "w1:p1 · no turn").settling()];
        let moved = [Signal::new(Kind::NeedsAPerson, "a-1", "w1:p1 · no turn").settling().to("demo")];
        let mut l = Ledger::default();
        l.prime(&[], 0);
        assert!(l.advance(&stalled, &Routing::Machine, 60, 300).is_empty(), "one minute in, this is a gap between turns");
        assert!(l.advance(&moved, &Routing::Machine, 120, 300).is_empty(), "and it has not been said, so it cannot have left");
        let out = l.advance(&moved, &Routing::Machine, 400, 300);
        assert_eq!(out.len(), 1, "it settles once, where it now is");
        assert_eq!(out[0].edge, Edge::Up);
        assert_eq!(out[0].to, "demo");
    }

    /// The `left` envelope names who has it now, as a field. A hook filters on
    /// `to` and nothing else, so *the hand you were told about is now demo's*
    /// has to be readable without parsing the sentence.
    #[test]
    fn a_move_says_on_the_wire_where_the_level_went() {
        let v = sig(Kind::Flag, "a-1").to("demo").envelope(Edge::Left, "phase-four", "now");
        assert_eq!(v["edge"], "left");
        assert_eq!(v["to"], "phase-four", "the seat that lost it — this is the line addressed to them");
        assert_eq!(v["moved_to"], "demo");
        let up = sig(Kind::Flag, "a-1").envelope(Edge::Up, "demo", "now");
        assert!(up.get("moved_to").is_none(), "a field that can never be filled is noise on a wire");
    }

    /// A source that cannot say where anything went is left on the path this
    /// file took before it could see a move — quietly wrong in the old way,
    /// rather than loudly wrong in a new one.
    #[test]
    fn a_reader_with_no_routing_behind_it_diffs_exactly_as_it_did_before() {
        let mut l = Ledger::default();
        l.prime(&[], 0);
        l.advance(&[sig(Kind::Flag, "a-1")], &Routing::Unknown, 60, 0);
        let out = l.advance(&[], &Routing::Unknown, 120, 0);
        assert_eq!(out[0].edge, Edge::Down);
    }

    /// The address survives the file the ledger is written to, which is what
    /// makes a move visible across the `exec` an install puts in the middle of
    /// the daemon's loop.
    #[test]
    fn the_ledger_remembers_who_was_answering_for_a_level() {
        let mut l = Ledger::default();
        l.prime(&[sig(Kind::Flag, "a-1").to("phase-four")], 0);
        let back = Ledger::of_json(&l.json());
        assert_eq!(back.told().next().map(|s| s.to.clone()).as_deref(), Some("phase-four"));
    }

    // ---- a seat that has stopped answering ---------------------------------
    //
    // `worklist-041`. Every input to [`stalled_seats`] is a reading somebody
    // else took, so the whole predicate is driven here with no herdr socket in
    // the room — which is the point of it being a function rather than an arm.

    use crate::model::{Group, Worklist, WorklistStatus};

    /// The fleet on a given night, as the five readings the predicate joins.
    struct Night {
        _env: util::Isolated,
        store: Store,
        governors: BTreeMap<String, Value>,
        bindings: BTreeMap<String, Value>,
        index: Index,
        rows: Vec<cmd_agent::WipRow>,
    }

    impl Night {
        fn new(tag: &str) -> Night {
            let env = util::isolated(&format!("stalled-{tag}"));
            let store = Store::at(env.home(), env.state());
            store.ensure_dirs().unwrap();
            Night {
                _env: env,
                store,
                governors: BTreeMap::new(),
                bindings: BTreeMap::new(),
                index: Index::new(Vec::new()),
                rows: Vec::new(),
            }
        }

        /// A project, and its parent where it has one — the chain
        /// [`cmd_govern::seat_above`] walks.
        fn projects(mut self, of: &[(&str, Option<&str>)]) -> Night {
            self.index = Index::new(
                of.iter()
                    .map(|(id, parent)| {
                        let mut p = crate::model::Project::new(id);
                        p.parent = parent.map(str::to_string);
                        p
                    })
                    .collect(),
            );
            self
        }

        fn task(self, id: &str, project: &str, status: Status) -> Night {
            let mut t = Task::new("a task", id);
            t.project = Some(project.into());
            t.set_status(status);
            self.store.save_task(&t).unwrap();
            self
        }

        /// A running list over those members. `Running::read` only ever sees a
        /// running one, which is the half of the obligation that is a decision
        /// somebody took rather than a state wsp inferred.
        fn running(self, slug: &str, members: &[&str]) -> Night {
            let mut w = Worklist::new(slug, slug);
            w.set_status(WorklistStatus::Running);
            w.set_groups(&[Group {
                members: members.iter().map(|m| m.to_string()).collect(),
                ..Default::default()
            }]);
            self.store.save_worklist(&w).unwrap();
            self
        }

        /// A custodian in a pane: `seat` is what `governs` answers for it.
        ///
        /// Written to the store as well as held here, because the two readers
        /// under test take it from different places — [`stalled_seats`] is
        /// handed the map and [`Poll::owes_a_run`] reads its own.
        fn seat(mut self, scope: &str, pane: &str, turning: bool) -> Night {
            let rec = json!({ "workspace": pane.split(':').next().unwrap_or(pane), "pane": pane });
            self.governors.insert(scope.to_string(), rec.clone());
            self.store.set_governor(scope, rec);
            self.rows.push(row_for(pane, "", Some(scope), turning));
            self
        }

        /// A worker in a pane, on a task — which is a census row and the
        /// binding that put it there.
        fn worker(mut self, pane: &str, task: &str, turning: bool) -> Night {
            self.rows.push(row_for(pane, task, None, turning));
            self.bindings.insert(pane.to_string(), json!({ "task_id": task }));
            self
        }

        /// A binding with nobody in it: the pane has gone and `wsp sync` has
        /// not reaped it yet. Somebody is still recorded as holding the work.
        fn bound(mut self, pane: &str, task: &str) -> Night {
            self.bindings.insert(pane.to_string(), json!({ "task_id": task }));
            self
        }

        fn read(&self) -> Vec<Signal> {
            let tasks = self.store.tasks();
            let lists = worklist::Running::read(&self.store);
            let to: BTreeMap<String, String> = tasks
                .iter()
                .map(|t| (t.id.clone(), addressed_to(&self.index, &self.governors, &lists, t)))
                .collect();
            stalled_seats(
                &tasks,
                &self.rows,
                &self.bindings,
                &to,
                &lists,
                &self.governors,
                &self.index,
            )
        }
    }

    fn row_for(pane: &str, task: &str, seat: Option<&str>, turning: bool) -> cmd_agent::WipRow {
        cmd_agent::WipRow {
            project: String::new(),
            task: String::new(),
            task_id: task.into(),
            pane: pane.into(),
            workspace: pane.split(':').next().unwrap_or(pane).into(),
            state: String::new(),
            turning,
            needs_you: false,
            seat: seat.map(str::to_string),
        }
    }

    /// **The failure the row was filed on**, and the one nothing in wsp could
    /// state: `robustness-083`, which hit three agents in one day. The seat's
    /// pane is alive and its turn is abandoned, its member landed cleanly and
    /// went quiet, and the barrier never advances — with every other signal
    /// healthy, because `needs_a_person` exempts a seat by construction.
    #[test]
    fn a_run_that_has_stopped_moving_raises_a_hand_about_the_seat_that_stopped_it() {
        let night = Night::new("stopped")
            .projects(&[("nightly", None)])
            .task("nightly-1", "nightly", Status::Review)
            .task("nightly-2", "nightly", Status::Todo)
            .running("tonight", &["nightly-1", "nightly-2"])
            .seat("nightly", "w1:p1", false);

        let up = night.read();
        assert_eq!(up.len(), 1, "one hand, about the seat: {up:?}");
        assert_eq!(up[0].kind, Kind::SeatStalled);
        assert_eq!(up[0].subject, "nightly", "the subject is a seat, not a task");
        assert!(up[0].detail.contains("nightly-2"), "and names what is waiting: {}", up[0].detail);
        assert!(up[0].detail.contains("tonight"), "and which run: {}", up[0].detail);
        assert!(
            !up[0].at_once,
            "a seat between two turns is stopped for seconds — this must settle first"
        );
    }

    /// **The half of the address that makes this more than an arm.** The fact
    /// must never reach the seat it is about: that seat is what failed.
    /// `seat_for` stops at the first seat it finds, which here is the dead one,
    /// so the walk starts one step past it — and `robustness` stalling reaches
    /// `wsp` exactly as a hand raised in `robustness` would while that seat is
    /// away.
    #[test]
    fn a_stalled_seats_hand_goes_to_the_seat_above_it_and_never_to_itself() {
        let night = Night::new("above")
            .projects(&[("robustness", Some("wsp")), ("wsp", None)])
            .task("r-1", "robustness", Status::Doing)
            .running("tonight", &["r-1"])
            .seat("robustness", "w1:p1", false)
            .seat("wsp", "w2:p1", true);

        let up = night.read();
        assert_eq!(up.len(), 1, "the seat above is turning, so only one is stalled: {up:?}");
        assert_eq!(up[0].subject, "robustness");
        assert_eq!(up[0].to, "wsp", "addressed above its own level, never to itself");
    }

    /// And it terminates at *everyone*, which is not a fallback: `seat_for`'s
    /// chain is `list, project, ancestors`, so the list step is the front of it
    /// and what lies past a list is the project chain of one member. A list
    /// cutting across projects — which is what a list is for — has as many of
    /// those as it has members, so there is no single scope above a run.
    #[test]
    fn a_stalled_worklist_seat_escalates_to_everyone_because_a_list_has_nothing_above_it() {
        let night = Night::new("everyone")
            .projects(&[("nightly", None)])
            .task("nightly-1", "nightly", Status::Doing)
            .running("tonight", &["nightly-1"])
            .seat("tonight", "w1:p1", false);

        let up = night.read();
        assert_eq!(up.len(), 1, "{up:?}");
        assert_eq!(up[0].subject, "tonight");
        assert_eq!(up[0].to, EVERYONE);
    }

    /// **The row as filed was `stopped && standing > 0 && seat`, and this is
    /// why that is false.** Driven against the live store on 2026-08-20: both
    /// seats on the machine were running no turn, with 42 levels addressed to
    /// one and 10 to the other, and both were healthy. Fifty-one of the
    /// fifty-two were `review` — the status worklist work actually reaches,
    /// since `done` is Ed's — so they are levels the seat cannot take down and
    /// they accumulate for the life of a project.
    ///
    /// A seat sitting under a pile of finished work is not stalled. It is doing
    /// what a seat is for.
    #[test]
    fn a_seat_under_a_pile_of_finished_work_is_not_stalled_however_long_it_sits() {
        let night = Night::new("review")
            .projects(&[("nightly", None)])
            .task("nightly-1", "nightly", Status::Review)
            .task("nightly-2", "nightly", Status::Review)
            .task("nightly-3", "nightly", Status::Done)
            .running("tonight", &["nightly-1", "nightly-2", "nightly-3"])
            .seat("nightly", "w1:p1", false);

        assert!(night.read().is_empty(), "review is what a run reaches, not a debt the seat owes");
    }

    /// The exemption's own reason, kept rather than deleted. A governor is idle
    /// *between* the agents it is sequencing, which is most of the time, and a
    /// project seat with no run under it owes nothing at all — which is what
    /// `standing == 0` was reaching for and could not express.
    #[test]
    fn a_seat_with_no_run_under_it_is_silent_however_long_it_sits() {
        let night = Night::new("norun")
            .projects(&[("nightly", None)])
            .task("nightly-1", "nightly", Status::Todo)
            .seat("nightly", "w1:p1", false);

        assert!(night.read().is_empty(), "an open backlog is not a commitment to drive it");
    }

    /// Absence of movement is not evidence — the lesson that killed two
    /// hand-rolled watchdogs in one night. What is evidence is the seat being
    /// still *and everything it answers for* being still too, so one turning
    /// agent anywhere in the scope exempts it, whatever else is true. Here that
    /// agent is not on the run at all: a seat with work moving under it is
    /// being driven by something, whether or not this predicate can see what.
    #[test]
    fn a_seat_with_work_moving_under_it_is_being_driven_by_something() {
        let night = Night::new("moving")
            .projects(&[("nightly", None)])
            .task("nightly-1", "nightly", Status::Todo)
            .task("nightly-9", "nightly", Status::Doing)
            .running("tonight", &["nightly-1"])
            .seat("nightly", "w1:p1", false)
            .worker("w2:p1", "nightly-9", true);

        assert!(night.read().is_empty(), "something in the scope is turning");
    }

    /// **The clause that keeps this from doubling every other signal.** A
    /// member that stops while somebody is on it is reported as *that member* —
    /// `needs-a-person`, `agent-gone`, `blocked` — and adding *and the seat
    /// above it is idle* to each of those is two lines about one stoppage. It
    /// would also be close to false: a governor is prompt-driven and does not
    /// poll, so five minutes of not having reacted is not a governor that died.
    #[test]
    fn a_member_that_stopped_with_somebody_on_it_is_that_members_stall_and_not_the_seats() {
        let night = Night::new("attended")
            .projects(&[("nightly", None)])
            .task("nightly-1", "nightly", Status::Doing)
            .running("tonight", &["nightly-1"])
            .seat("nightly", "w1:p1", false)
            .worker("w2:p1", "nightly-1", false);

        assert!(night.read().is_empty(), "needs-a-person is already saying this, about w2:p1");
    }

    /// And it hands over rather than overlapping. A binding whose pane has gone
    /// is `agent-gone`'s to report, so this stays quiet while the record stands
    /// — and takes over on the tick after `wsp sync` reaps it, when the member
    /// is genuinely unattended and nothing else in the fleet is its subject.
    #[test]
    fn a_binding_nobody_is_in_keeps_this_quiet_until_something_reaps_it() {
        let night = Night::new("reap")
            .projects(&[("nightly", None)])
            .task("nightly-1", "nightly", Status::Doing)
            .running("tonight", &["nightly-1"])
            .seat("nightly", "w1:p1", false);

        let mut night = night.bound("w2:p1", "nightly-1");
        assert!(night.read().is_empty(), "agent-gone owns this one");

        night.bindings.clear();
        assert_eq!(night.read().len(), 1, "and nothing else is left to be its subject");
    }

    /// And the seat turning is the first clause, said the obvious way round.
    #[test]
    fn a_seat_that_is_taking_its_turn_is_not_stalled() {
        let night = Night::new("turning")
            .projects(&[("nightly", None)])
            .task("nightly-1", "nightly", Status::Todo)
            .running("tonight", &["nightly-1"])
            .seat("nightly", "w1:p1", true);

        assert!(night.read().is_empty());
    }

    /// `worklist-035`'s finding, arriving from the other side. A record naming
    /// no pane falls back to the *room*, and a workspace holds more than one
    /// agent — so this is asked per seat and not per row. Asked per row, the
    /// idle pane in a busy custodian's workspace would report its own seat
    /// stalled while the seat sat beside it working.
    #[test]
    fn a_seat_is_judged_by_its_whole_room_and_not_by_one_idle_pane_in_it() {
        let night = Night::new("room")
            .projects(&[("nightly", None)])
            .task("nightly-1", "nightly", Status::Todo)
            .running("tonight", &["nightly-1"])
            .seat("nightly", "w1:p1", false)
            .seat("nightly", "w1:p2", true);

        assert!(night.read().is_empty(), "one pane of the seat is turning");
    }

    /// It settles like a stall and clears like one, and both are the table's
    /// answer rather than this arm's. A run that starts moving again must take
    /// the hand down: the repair is to go and prod a seat, and one that has
    /// started turning on its own must not be prodded.
    #[test]
    fn a_run_that_starts_moving_again_takes_the_hand_down() {
        let s = Signal::new(Kind::SeatStalled, "nightly", "no turn here").settling();
        let mut l = Ledger::default();

        assert!(l.advance(&[s.clone()], &Routing::Machine, 0, 300).is_empty(), "it settles first");
        let out = l.advance(&[s], &Routing::Machine, 300, 300);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].edge, Edge::Up);

        let out = l.advance(&[], &Routing::Machine, 360, 300);
        assert_eq!(out.len(), 1, "and going away is news");
        assert_eq!(out[0].edge, Edge::Down);
    }

    /// Silence is not evidence, and the census is the half that goes missing.
    /// A tick that could not reach herdr says nothing about whether a seat is
    /// turning, so a level of this kind is **held** across it rather than read
    /// as a run that resumed — one herdr restart would otherwise be a false
    /// all-clear per stalled run, at whatever hour it was restarted.
    #[test]
    fn a_stalled_seat_is_held_across_a_tick_that_could_not_see_the_agents() {
        assert!(Kind::SeatStalled.needs_herdr());

        let s = Signal::new(Kind::SeatStalled, "nightly", "no turn here").settling();
        let mut l = Ledger::default();
        l.advance(&[s], &Routing::Machine, 0, 0);
        assert_eq!(l.standing(), 1);

        let blind = Signal::new(Kind::Blind, "herdr", "no herdr socket");
        let out = l.advance(&[blind], &Routing::Machine, 60, 0);
        assert_eq!(l.standing(), 2, "the stall is held, not cleared");
        assert!(out.iter().all(|e| e.edge == Edge::Up), "nothing was reported as over: {out:?}");
    }

    /// A `note` is *act when convenient*. This one says nothing in a run is
    /// moving and the thing responsible for moving it is not turning, so
    /// nothing beneath it will arrive later to say the same thing and nothing
    /// under it will resolve it.
    #[test]
    fn a_stalled_seat_is_the_one_derived_predicate_that_may_take_more_than_a_note() {
        assert_eq!(Kind::SeatStalled.loudness(), crate::message::Kind::Direction);
        assert_eq!(Kind::Review.loudness(), crate::message::Kind::Note);
    }

    // ---- the class of a line -----------------------------------------------

    /// A watch on `wsp`, subscribed to everything, in whichever document.
    fn watching(json: bool) -> Spec {
        Spec {
            scope: scope("wsp", false),
            want: Kind::every().into_iter().collect(),
            about: None,
            every: EVERY,
            settle: SETTLE,
            heartbeat: HEARTBEAT,
            stop_after: None,
            until: None,
            json,
            wake: false,
            defer_max: DEFER_MAX,
        }
    }

    /// The fourteen-wide column a reader scans, off a plain line.
    fn column(l: &str) -> String {
        l.chars().skip(6).take(14).collect::<String>().trim_end().to_string()
    }

    fn parsed(l: &str) -> Value {
        serde_json::from_str(l).unwrap_or_else(|e| panic!("not one JSON document: {e} — {l}"))
    }

    /// **`worklist-033`, as an assertion, and the one test that would have
    /// prevented it.** A monitor dropped the heartbeat by matching the shape of
    /// its line; `REPLACED` prints on a line of the same shape, so it went too,
    /// and the watch ran eight hours and four installs on stale logic. These
    /// two lines are now distinguishable without reading a word of either, in
    /// both documents — and before the class field they were not, because
    /// `aside` put the same `·` in that column for both.
    #[test]
    fn a_build_replaced_notice_is_not_the_same_class_as_a_heartbeat() {
        let p = util::Paint::plain();

        let text = watching(false);
        let beat = note(Class::Beat, 0, &text, "watching wsp · 1h · 0 standing", &p);
        let repl = note(Class::Replaced, 0, &text, REPLACED, &p);
        assert_eq!(column(&beat), "beat");
        assert_eq!(column(&repl), "replaced");
        assert_ne!(column(&beat), column(&repl), "the column a monitor reads separates them");

        let json = watching(true);
        let beat = parsed(&note(Class::Beat, 0, &json, "watching wsp · 1h · 0 standing", &p));
        let repl = parsed(&note(Class::Replaced, 0, &json, REPLACED, &p));
        assert_eq!(beat["class"], "beat");
        assert_eq!(repl["class"], "replaced");
    }

    /// Every class, in the document a machine reads, carrying the word a
    /// consumer keys on — including the four that are not signals, which under
    /// `--json` were not printed at all.
    #[test]
    fn every_line_a_watch_emits_under_json_carries_its_class() {
        let p = util::Paint::plain();
        let spec = watching(true);
        let mut seen: Vec<String> = Vec::new();
        for class in Class::every() {
            let v = match class {
                // News is the one line that is a level, so it is the one line
                // whose JSON form is the signal envelope rather than
                // `Class::envelope`. It must still carry the field.
                Class::News => sig(Kind::Review, "a-1").envelope(Edge::Up, "wsp", "2026-08-21T09:00:00Z"),
                _ => parsed(&note(class, 0, &spec, "something happened", &p)),
            };
            assert_eq!(v["class"], class.word(), "{class:?} did not name itself");
            seen.push(v["class"].as_str().expect("a class is a word").to_string());
        }
        assert_eq!(seen, vec!["open", "news", "beat", "replaced", "over"], "the vocabulary is these five");
    }

    /// **Failure 1, on the path that did not have it.** A `--json` watcher
    /// printed nothing at all until something moved: the opening was written
    /// under `if !spec.json`, so the reader least able to ask a person whether
    /// its watch was alive was the one told nothing. A watcher that says
    /// nothing from the first second is indistinguishable from one that never
    /// started, and that is as true through a pipe as it is on a screen.
    #[test]
    fn a_json_watcher_says_it_started_before_anything_has_happened() {
        let p = util::Paint::plain();
        let spec = watching(true);
        let v = parsed(&render_for(&opening(&spec, 0, false), 0, &spec, &p));
        assert_eq!(v["class"], "open");
        assert_eq!(v["to"], "wsp", "addressed, like every other line");
        let said = v["text"].as_str().expect("it says something");
        assert!(said.contains("0 standing"), "and it says how much is up: {said}");
        assert!(said.contains("watching wsp"), "and what it is watching: {said}");
    }

    // ---- the wake -----------------------------------------------------------

    fn waking(defer_max: i64) -> Spec {
        Spec { wake: true, defer_max, ..watching(false) }
    }

    fn news(kind: Kind, edge: Edge, subject: &str) -> Line {
        Line::News(Emit { edge, to: EVERYONE.into(), signal: sig(kind, subject), held: 0 })
    }

    fn beat() -> Line {
        Line::Note(Class::Beat, "watching wsp · 1h · 0 standing".into())
    }

    /// **Nothing offered to the stream can vanish.** The one outcome that would
    /// make this change worse than doing nothing is a seventh way to be silent,
    /// and it would not look like a bug — it would look like a quiet fleet.
    ///
    /// So this is not a test of the table's cells; it is a test that the table
    /// is *total*. Every line of every class goes in, in both modes, and the
    /// count that comes out plus the count still held must be the count that
    /// went in. An early return, a `continue`, or a filter running before the
    /// table would all show up here as a number that does not add up.
    #[test]
    fn nothing_offered_to_the_stream_is_dropped() {
        let mut every: Vec<Line> = vec![
            Line::Note(Class::Open, "watching wsp".into()),
            beat(),
            Line::Note(Class::Replaced, REPLACED.into()),
            Line::Note(Class::Over, "stopped watching wsp".into()),
        ];
        for kind in Kind::every().into_iter().chain(std::iter::once(Kind::Blind)) {
            for edge in [Edge::Up, Edge::Down, Edge::Left] {
                every.push(news(kind, edge, "a-1"));
            }
        }
        for spec in [waking(DEFER_MAX), watching(false)] {
            let mut stream = Stream::new(&spec);
            let mut spool = Spool::default();
            for l in &every {
                stream.put(0, l.clone());
            }
            let written = stream.tick(0, &mut spool).len();
            assert_eq!(
                written + spool.depth(),
                every.len(),
                "wake={}: {written} written and {} held, out of {} offered",
                spec.wake,
                spool.depth(),
                every.len()
            );
        }
    }

    /// `core-014` d2, and the measured half of the whole row: 45 of the 134
    /// wakes one seat took in two days were a heartbeat saying nothing had
    /// changed, and a heartbeat wake costs the same as a question from Ed
    /// because the context is the price and not the payload.
    ///
    /// It is **held**, not dropped. Absence of a heartbeat only protects a
    /// reader who is still reading; the reader here is asleep by design, and
    /// the register is written every tick where a dead process cannot fake it.
    #[test]
    fn a_heartbeat_never_wakes_a_governor_and_is_never_lost() {
        let spec = waking(DEFER_MAX);
        let mut stream = Stream::new(&spec);
        let mut spool = Spool::default();
        for tick in 1..=5 {
            stream.put(tick * 60, beat());
            assert_eq!(stream.tick(tick * 60, &mut spool).len(), 0, "a heartbeat is not a wake");
        }
        assert_eq!(spool.depth(), 5, "and not one of them was lost");
    }

    /// A level that went away, and a level somebody else answers for now.
    /// There is nothing for this reader to do about either — but *nothing to do
    /// about* is not *not worth knowing*, so it waits for a line that was
    /// already going to be printed and rides out under it at no extra cost.
    #[test]
    fn a_level_going_down_rides_the_next_thing_worth_waking_for() {
        let spec = waking(DEFER_MAX);
        let mut stream = Stream::new(&spec);
        let mut spool = Spool::default();

        stream.put(0, news(Kind::Flag, Edge::Down, "a-1"));
        stream.put(0, news(Kind::Review, Edge::Left, "a-2"));
        assert_eq!(stream.tick(0, &mut spool).len(), 0, "neither is worth a context read on its own");
        assert_eq!(spool.depth(), 2);

        stream.put(60, news(Kind::NeedsAPerson, Edge::Up, "a-3"));
        assert_eq!(stream.tick(60, &mut spool).len(), 3, "the wake, and the two that were waiting");
        assert_eq!(spool.depth(), 0);
    }

    /// Without this a fleet that never earns a wake never delivers its backlog,
    /// and the drop is back in through the door. The number is [`DEFER_MAX`]
    /// and its argument is measured there.
    #[test]
    fn a_spooled_fact_nobody_collected_becomes_a_wake_on_its_own() {
        let spec = waking(4 * 60 * 60);
        let mut stream = Stream::new(&spec);
        let mut spool = Spool::default();

        stream.put(0, news(Kind::Flag, Edge::Up, "a-1"));
        assert_eq!(stream.tick(0, &mut spool).len(), 0);
        // Three hours and fifty-nine minutes of nobody being told.
        assert_eq!(stream.tick(4 * 60 * 60 - 60, &mut spool).len(), 0, "not yet");
        assert_eq!(spool.depth(), 1);
        // The age of the oldest, not of the newest: a steady trickle must not
        // keep pushing the deadline out in front of the fact that is waiting.
        stream.put(4 * 60 * 60, beat());
        assert_eq!(stream.tick(4 * 60 * 60, &mut spool).len(), 2, "it is now justification by itself");
        assert_eq!(spool.depth(), 0);
    }

    /// `over` is a wake, and it is the line the spool rides out on. A spool
    /// that died with its process would be a drop with a different word on it.
    #[test]
    fn a_watch_that_ends_flushes_what_it_was_holding() {
        let spec = waking(DEFER_MAX);
        let mut stream = Stream::new(&spec);
        let mut spool = Spool::default();

        stream.put(0, beat());
        stream.put(0, news(Kind::Flag, Edge::Up, "a-1"));
        assert_eq!(stream.tick(0, &mut spool).len(), 0);

        stream.put(60, Line::Note(Class::Over, "stopped watching wsp — the time asked for is up".into()));
        let written = stream.tick(60, &mut spool);
        assert_eq!(written.len(), 3, "the ending, and the two it carried out");
        assert_eq!(spool.depth(), 0, "nothing is left behind for nobody to read");
        // And the ending is the **last** line, not the first. A consumer that
        // reads until the stream says it is over is the obvious one to write,
        // and it must not lose the backlog that ending carried out with it.
        assert_eq!(
            written.iter().map(|h| h.class.as_str()).collect::<Vec<_>>(),
            vec!["beat", "news", "over"],
            "the backlog, then the ending",
        );
    }

    /// `worklist-033` under `--wake`. The watch is running logic that has since
    /// been fixed, it cannot say so twice, and only the reader can repair it —
    /// so this is the one aside that must never wait for company.
    #[test]
    fn a_build_landing_under_the_watch_wakes_it_at_once() {
        let spec = waking(DEFER_MAX);
        let mut stream = Stream::new(&spec);
        let mut spool = Spool::default();

        stream.put(0, beat());
        assert_eq!(stream.tick(0, &mut spool).len(), 0);
        stream.put(60, Line::Note(Class::Replaced, REPLACED.into()));
        assert_eq!(stream.tick(60, &mut spool).len(), 2, "at once, and it takes the backlog with it");
        assert_eq!(
            Line::Note(Class::Replaced, REPLACED.into()).disposition(),
            Disposition::Wake,
            "and it is a different class from the heartbeat it used to be confused with",
        );
        assert_eq!(beat().disposition(), Disposition::Spool);
    }

    /// The process ending, a restart, an install landing underneath. The spool
    /// goes in the watch record beside the ledger, so it survives all three —
    /// and an entry written by a build whose vocabulary this one does not have
    /// is **kept as its words** rather than skipped, because skipping it would
    /// be the drop this row exists to make unrepresentable.
    #[test]
    fn a_spool_survives_the_process_that_wrote_it() {
        let spec = waking(DEFER_MAX);
        let mut stream = Stream::new(&spec);
        let mut spool = Spool::default();
        stream.put(0, beat());
        stream.put(0, news(Kind::Flag, Edge::Up, "a-1"));
        stream.tick(0, &mut spool);
        assert_eq!(spool.depth(), 2);

        let back = Spool::of_json(&spool.json());
        assert_eq!(back, spool, "byte for byte, through the record and out again");

        // Now the same record as a build that has never heard of one of these.
        let mut raw = spool.json();
        raw.as_array_mut().unwrap()[0].as_object_mut().unwrap().insert("class".into(), json!("aurora"));
        let odd = Spool::of_json(&raw);
        assert_eq!(odd.depth(), 2, "the unreadable entry is kept, not skipped");
        assert!(
            odd.held[0].say(&spec, &util::Paint::plain()).contains("watching wsp"),
            "and it still says what it said: {}",
            odd.held[0].say(&spec, &util::Paint::plain())
        );
    }

    /// `--wake` is opt-in and a bare `wsp watch` is what it always was. A
    /// person in front of a stream wants the heartbeat; d2 traded it away only
    /// for the reader that is asleep.
    #[test]
    fn a_bare_watch_prints_what_it_printed_before_this_change() {
        let spec = watching(false);
        let mut stream = Stream::new(&spec);
        let mut spool = Spool::default();
        stream.put(0, beat());
        stream.put(0, news(Kind::Flag, Edge::Down, "a-1"));
        assert_eq!(stream.tick(0, &mut spool).len(), 2, "both, now, in the order they happened");
        assert_eq!(spool.depth(), 0, "a bare watch holds nothing back and so has nothing to hold");
    }

    /// **This row's whole claim, counted rather than estimated.**
    ///
    /// The corpus is real. `fixtures/wake-2026-08-19-seat.jsonl` is every line
    /// `wsp watch` actually delivered to the seat that filed `core-014`,
    /// recovered from that session's own transcript (`fb09b0f3`, 2026-08-19 and
    /// -20) and classified once into the vocabulary `core-016` closed: 158
    /// lines in 134 deliveries. A delivery is the unit that matters, because a
    /// delivery is what re-invokes a governor and re-reads its whole
    /// conversation — 208k tokens on that seat, the same for a heartbeat as for
    /// a question from Ed.
    ///
    /// **134 deliveries become 71 — 47% of that session's watch wakes were not
    /// worth a context read.** The numbers are asserted exactly, so if a cell
    /// of [`Line::disposition`] moves this test says by how much rather than
    /// merely that something changed.
    #[test]
    fn the_measured_session_is_replayed_and_the_wake_count_is_counted() {
        let corpus: Vec<Value> = include_str!("../fixtures/wake-2026-08-19-seat.jsonl")
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("the fixture is one JSON document per line"))
            .collect();
        assert_eq!(corpus.len(), 158, "every line the session delivered");

        // Grouped by the second they were delivered in, which is the tick they
        // would have shared.
        let mut ticks: Vec<(i64, Vec<Line>)> = Vec::new();
        for rec in &corpus {
            let at = rec["at"].as_i64().expect("a stamp");
            let class = Class::parse(rec["class"].as_str().expect("a class")).expect("a known class");
            let l = match class {
                Class::News => {
                    let kind = Kind::parse(rec["kind"].as_str().expect("a signal")).expect("a known signal");
                    let edge = Edge::parse(rec["edge"].as_str().expect("an edge")).expect("a known edge");
                    news(kind, edge, "replayed")
                }
                c => Line::Note(c, rec["text"].as_str().unwrap_or_default().to_string()),
            };
            match ticks.last_mut() {
                Some((t, batch)) if *t == at => batch.push(l),
                _ => ticks.push((at, vec![l])),
            }
        }
        assert_eq!(ticks.len(), 134, "deliveries, each one a context read");

        let count = |defer_max: i64| {
            let spec = waking(defer_max);
            let mut spool = Spool::default();
            let mut wakes = 0;
            for (at, batch) in &ticks {
                let mut stream = Stream::new(&spec);
                for l in batch {
                    stream.put(*at, l.clone());
                }
                if !stream.tick(*at, &mut spool).is_empty() {
                    wakes += 1;
                }
            }
            wakes
        };

        // The table alone, and then what bounding the silence costs on top of
        // it. `DEFER_MAX` is argued from exactly these numbers.
        // The table alone, and then what bounding the silence costs on top of
        // it. `DEFER_MAX` is argued from exactly these four numbers.
        assert_eq!(count(i64::MAX), 70, "the table alone");
        assert_eq!(count(4 * 60 * 60), 71, "four hours costs one extra context read in two days");
        assert_eq!(count(2 * 60 * 60), 74, "two hours costs four");
        assert_eq!(count(60 * 60), 78, "an hour costs eight");
        assert_eq!(count(DEFER_MAX), 71, "and the default is the four-hour one");

        // **134 wakes to 71.** Said as an assertion so the claim cannot drift
        // away from the prose that quotes it.
        assert_eq!((ticks.len() - count(DEFER_MAX)) * 100 / ticks.len(), 47);
    }

    /// **The fourteen-wide column has one alphabet, and no word in it means
    /// two things.** [`Class`] and [`Kind`] both write into it, and so does
    /// [`line`] with `cleared` and `moved`. Nothing collides today. A signal or
    /// a class added next year that reused a word would break every text
    /// consumer in silence — which is `core-016`'s own failure mode one layer
    /// down, and the reason the field was worth adding at all.
    ///
    /// The width and the absence of whitespace are the same invariant from the
    /// other side: a word longer than the column shifts the field after it, and
    /// a word with a space in it breaks `awk '$2 != "beat"'`, which is the
    /// filter `core-016` d1 tells a governor to use today.
    #[test]
    fn no_word_in_the_column_a_seat_scans_means_two_things() {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let kinds = Kind::every().into_iter().chain(std::iter::once(Kind::Blind)).map(|k| k.word());
        let classes = Class::every().into_iter().map(|c| c.word());
        // `cleared` and `moved` are written once each, in `line`. This is the
        // second place they appear on purpose: renaming one should have to come
        // past this test.
        for word in kinds.chain(classes).chain(["cleared", "moved"]) {
            assert!(seen.insert(word), "{word} is written into that column by two vocabularies");
            assert!(word.len() <= 14, "{word} is wider than the column, so it moves the field after it");
            assert!(!word.contains(char::is_whitespace), "{word} has a space in it, so `awk '$2'` cannot read it");
        }
    }

    /// The stream is read down its second column, by eye, by two seats. The
    /// class word went where the `·` was and nothing else moved: five
    /// characters of clock, a space, exactly fourteen for the class-or-signal
    /// column, a space, then the subject. Asserted as geometry rather than as
    /// words, because the words are the part that is allowed to change.
    #[test]
    fn the_columns_a_seat_reads_by_eye_do_not_move() {
        let p = util::Paint::plain();
        let at = 0;
        let news =
            line(&Emit { edge: Edge::Up, to: EVERYONE.into(), signal: sig(Kind::Review, "a-1"), held: 0 }, at, &p);
        let beat = aside(Class::Beat, at, "watching wsp · 1h · 0 standing", &p);

        assert_eq!(news.chars().take(5).collect::<String>(), clock(at), "the clock is where it was");
        assert_eq!(beat.chars().take(5).collect::<String>(), clock(at));
        for l in [&news, &beat] {
            assert_eq!(l.chars().nth(5), Some(' '), "one space after the clock: {l}");
            assert_eq!(l.chars().nth(20), Some(' '), "the second column is fourteen wide: {l}");
        }
        assert_eq!(column(&news), "review", "and a news line keeps the signal word it already printed");
        // …and the third field starts at the same *column* in both, counted
        // in characters rather than bytes, because what is being protected
        // here is where a reader's eye lands.
        let third = |l: &str| l.chars().skip(21).collect::<String>();
        assert!(third(&news).starts_with("a-1"), "{news}");
        assert!(third(&beat).starts_with("watching wsp"), "{beat}");
    }
}
