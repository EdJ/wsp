//! The watch's level model (wsp-438): what a level is ([`Kind`], [`Signal`]),
//! how a ledger diffs one tick's levels into edges ([`Ledger`], [`Edge`],
//! [`Emit`]), who a level is for ([`Routing`], [`Scope`]), and the port a tick
//! reads its levels through ([`Source`]).
//!
//! Moved out of `cmd_watch` so the daemon's attention pass ([`crate::attention`])
//! and `wsp watch` share one model from the engine row, and the attention pass
//! no longer reaches up into the seats: the reading that joins the seats'
//! census (`cmd_watch::Poll`) stays in `cmd_watch` and is handed to the pass as
//! a `&mut dyn Source`. `cmd_watch` re-exports everything here, so its paths
//! still name these. No behaviour change.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use crate::util;

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
pub enum Kind {
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
    /// A binding whose seat no backend lists any more, or whose seat is alive with
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
    pub fn word(&self) -> &'static str {
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
    pub fn every() -> Vec<Kind> {
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

    pub(crate) fn parse(word: &str) -> Option<Kind> {
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
    pub(crate) fn clears(&self) -> bool {
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

    /// Whether evaluating this predicate needs the census to answer.
    ///
    /// **Read by [`Ledger::advance`], and it is the difference between a level
    /// that went away and one nobody could look at.** [`Poll::sample`] derives
    /// the agent half from a census, so when a backend is unreachable that half of
    /// every tick is simply absent — no rows, no bindings, nothing. A diff
    /// taken against that reads every stall on the machine as having resolved,
    /// and the next tick after it comes back reads them all as new.
    ///
    /// One backend restart is therefore two edges per standing signal, neither of
    /// which happened. On a stream a person discounts it, having just seen
    /// `blind` go up; through a hook it is a phone buzzing twice for every
    /// stalled agent on the box, at whatever hour it was restarted. That is
    /// `robustness-088`'s named failure — *noise is worse than the silence it
    /// replaces* — arriving by the one route that has nobody reading it.
    ///
    /// So: silence is not evidence. A level of a kind that could not be read this tick is
    /// **held**, not cleared, and it goes down on the first tick that can
    /// actually see it.
    pub(crate) fn needs_census(&self) -> bool {
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
    /// *"A derived signal → `note`, except `blocked` — a modal holding
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
    pub(crate) fn loudness(&self) -> crate::message::Kind {
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
pub enum Class {
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
    /// **A sentence somebody wrote, addressed to this seat**, and the class
    /// exists because of the sentence in [`Line::disposition`] that says it is
    /// always a wake.
    ///
    /// The five above are the stream talking about itself — its own start, its
    /// own liveness, its own ending — and every one of them is a decision made
    /// by a reader about its own reporting. This one is *content*: a member's
    /// question, a verdict at a barrier, a governor's own `--tell`, put in the
    /// spool by whoever had it in hand rather than derived by the pass that
    /// decides what a seat is worth waking for. `wsp-146`: an agent had four
    /// verbs for reaching its governor and every one of them asked the pane
    /// directly, so a governor mid-turn refused a question that had already been
    /// written down — *"not ready — working"* is what `cycle.rs` was told, and
    /// its answer was to raise a hand on a member instead.
    ///
    /// It is a class and not a [`Kind`] because the difference is authorship,
    /// not urgency: a level is a reading of state and has nobody to reply to,
    /// and this has both. Rendering is the same column as every other line, so
    /// `--drain` and a governor's composer read one shape.
    Message,
}

impl Class {
    /// The word, in the JSON field and in the column a reader scans.
    pub fn word(&self) -> &'static str {
        match self {
            Class::Open => "open",
            Class::News => "news",
            Class::Beat => "beat",
            Class::Replaced => "replaced",
            Class::Over => "over",
            Class::Message => "message",
        }
    }

    /// Back from the word, for a spool written by another build.
    pub(crate) fn parse(word: &str) -> Option<Class> {
        Class::every().into_iter().find(|c| c.word() == word)
    }

    /// The whole vocabulary, so a reader can be handed it and a test can assert
    /// it is closed.
    pub fn every() -> [Class; 6] {
        [Class::Open, Class::News, Class::Beat, Class::Replaced, Class::Over, Class::Message]
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
    pub(crate) fn envelope(&self, at: i64, to: &str, text: &str) -> Value {
        json!({ "class": self.word(), "at": util::iso_at(at), "to": to, "text": text })
    }
}

/// One fact that is true now, and worth acting on.
///
/// The *key* — kind plus subject — is what the ledger diffs on, and `detail` is
/// deliberately outside it. A stall whose duration grows every tick must be one
/// unchanged fact rather than sixty of them, which is the whole of "emit on
/// change, never on state" written as a type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signal {
    pub kind: Kind,
    /// A task id, mostly. A pane for a binding whose task is gone, the source's
    /// own name (`compound`, …) for [`Kind::Blind`], and **a seat's
    /// scope** for
    /// [`Kind::SeatStalled`] — a project id or a worklist slug, which share one
    /// key space with each other and with nothing else here.
    ///
    /// *Mostly* is load-bearing and was nearly a defect. [`Poll::sample`]
    /// addresses on the way out from a map keyed by task id, so a subject that
    /// is not a task silently keeps the [`EVERYONE`] it was built with. That is
    /// right for a backend's name and for a record wsp cannot place — nobody in
    /// particular owns either — and it is exactly wrong for a stalled seat,
    /// whose whole point is that it must reach somebody *other than itself*. So
    /// [`stalled_seats`] addresses its levels where it builds them, after that
    /// map has been applied rather than before.
    pub subject: String,
    /// The clause after the id: what to do, or why. Never the task's title —
    /// `wsp-095` Part 13 holds this to `wsp worklist next`'s bar, *ids not
    /// titles*, because a governor knows its own ids and pays for every line on
    /// every request of every session.
    pub detail: String,
    /// The record this level is a reading of, when it is a reading of one.
    ///
    /// A message id, and it is here for two reasons a task id cannot serve.
    /// **It is part of [`Signal::key`]**, so two questions about one task are
    /// two facts rather than one that silently replaces the other — which is
    /// `worklist-017`'s finding about the flag record, kept out of this one by
    /// construction. And it is what the reader needs *in hand* to act: the
    /// subject says which work, the record says which question, and
    /// `wsp answer` takes the second.
    pub record: Option<String>,
    /// How much of the reader's time this may take, when it is not the kind's
    /// own default.
    ///
    /// `None` is [`Kind::loudness`] — `note`, because a derived level is
    /// nobody's request. It is overridden for exactly two readings and both are
    /// facts about the *reading* rather than about the predicate: a seat's
    /// `blocked`, a modal on the keyboard that one keypress fixes, which
    /// `quiet_note` already promotes past the hour for the same reason; and a
    /// question, which carries the asker's own [`crate::message::Kind`] because
    /// **the sender already said how loud it was** and deriving a second answer
    /// beside it is the two-vocabularies fault this file keeps refusing.
    pub loud: Option<crate::message::Kind>,
    /// This one may be emitted the instant it is seen. False for the states
    /// that flap — an agent is briefly "stopped" between every pair of turns —
    /// and true for the ones that do not.
    pub at_once: bool,
    /// **Who answers for this level right now** — a seat's scope, or
    /// [`EVERYONE`].
    ///
    /// Read off the level rather than re-derived at delivery, and that is the
    /// whole of `worklist-039`'s daemon half. [`crate::attention::deliver`]
    /// used to walk [`crate::governors::seat_for`] per *edge*, at the moment the edge
    /// was sent, which addresses a raise and its clearing by two different
    /// readings of a routing that moves under both. Driven on 2026-08-20
    /// against a fake backend: one flag, one seat swap in between, and the log
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
    pub to: String,
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
pub const EVERYONE: &str = "everyone";

impl Signal {
    pub fn new(kind: Kind, subject: &str, detail: &str) -> Signal {
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
    pub fn to(mut self, seat: &str) -> Signal {
        self.to = seat.to_string();
        self
    }

    pub fn settling(mut self) -> Signal {
        self.at_once = false;
        self
    }

    pub fn loud(mut self) -> Signal {
        self.loud = Some(crate::message::Kind::Direction);
        self
    }

    /// Said as loud as whoever wrote the record asked for.
    pub(crate) fn as_loud_as(mut self, k: crate::message::Kind) -> Signal {
        self.loud = Some(k);
        self
    }

    /// The message this is a reading of. See [`Signal::record`].
    pub fn of(mut self, record: &str) -> Signal {
        self.record = Some(record.to_string());
        self
    }

    /// How loud this reading is, which is the kind's default unless the
    /// reading overrode it.
    pub fn loudness(&self) -> crate::message::Kind {
        self.loud.unwrap_or_else(|| self.kind.loudness())
    }

    /// Whether this is worth the red column rather than the yellow one.
    pub(crate) fn shouts(&self) -> bool {
        self.loudness() < crate::message::Kind::Note
    }

    /// What the ledger diffs on: the predicate, what it is about, and — when
    /// there is one — the record it read. See [`Signal::record`].
    pub fn key(&self) -> String {
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
    /// subscribes only to what [`crate::governors::seat_for`] addresses here, so the
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
    pub fn json(&self) -> Value {
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
    pub fn of_json(rec: &Value) -> Option<Signal> {
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

    pub fn envelope(&self, edge: Edge, to: &str, at: &str) -> Value {
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
pub enum Edge {
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
    pub fn word(&self) -> &'static str {
        match self {
            Edge::Up => "up",
            Edge::Down => "down",
            Edge::Left => "left",
        }
    }

    /// Back from the word, for a spool written by another build.
    pub(crate) fn parse(word: &str) -> Option<Edge> {
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
pub struct Ledger {
    up: BTreeMap<String, Held>,
}

/// One line's worth of news.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Emit {
    pub edge: Edge,
    pub signal: Signal,
    /// How long the level had been up when this was said. Zero on the way up
    /// for anything that did not settle.
    pub held: i64,
    /// **Who this line is for**, which is [`Signal::to`] for everything except
    /// an [`Edge::Left`] — where the level has already moved on and the line is
    /// for the seat it left.
    ///
    /// Carried on the emit rather than worked out where it is sent, because
    /// *where it is sent* is a tick later than *what happened* and the routing
    /// moves in between: see [`Signal::to`] for the pair of log lines that cost.
    pub to: String,
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
pub enum Routing {
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
    pub fn advance(&mut self, now: &[Signal], routing: &Routing, at: i64, settle: i64) -> Vec<Emit> {
        let mut out = Vec::new();
        let seen: BTreeSet<String> = now.iter().map(Signal::key).collect();
        // Half the predicates could not be evaluated at all this tick, so their
        // absence from `now` says nothing. See [`Kind::needs_census`] for what
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
            .filter(|(k, h)| !seen.contains(*k) && !(blind && h.signal.kind.needs_census()))
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
    pub fn prime(&mut self, now: &[Signal], at: i64) -> Vec<Emit> {
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
    pub fn standing(&self) -> usize {
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
    pub fn told(&self) -> impl Iterator<Item = &Signal> {
        self.up.values().filter(|h| h.told).map(|h| &h.signal)
    }

    /// Forget every level `gone` names, saying nothing about it (`wsp-649`).
    pub fn drop_where(&mut self, gone: &dyn Fn(&Signal) -> bool) {
        self.up.retain(|_, h| !gone(&h.signal));
    }

    pub fn json(&self) -> Value {
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
    pub fn of_json(v: &Value) -> Ledger {
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
pub fn resume(rec: &Value) -> (Ledger, bool) {
    let Some(v) = rec.get("ledger") else { return (Ledger::default(), false) };
    let keyed_by = rec.get("build").and_then(Value::as_str).unwrap_or_default();
    (Ledger::of_json(v), keyed_by == crate::base::stamp::build_stamp())
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
pub trait Source {
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
pub struct Scope {
    pub name: String,
    pub seated: bool,
    /// The workspace holding the seat, when there is one. The watch ends when
    /// this workspace is no longer in the slot — an exit condition that costs
    /// nothing and is the natural end of a seat's subscription.
    pub workspace: String,
    /// And the pane in it, so that end condition is read exactly. A workspace
    /// holds more than one agent, and a subscription that survived because a
    /// *neighbour* was still in the room would be a seat's watch going on after
    /// the seat did — see [`crate::place::governs`]. Empty for a scope that was
    /// named rather than sat in, where there is no pane to speak of.
    pub pane: String,
    /// Every task, with no membership test at all. See [`Scope::machine`].
    pub all: bool,
}

impl Scope {
    /// The whole machine, for a reader that is nobody's seat.
    ///
    /// Deliberately not reachable from argv. `wsp watch` is a *subscription*
    /// and a subscription is to a scope somebody answers for; the daemon is not
    /// a subscriber, it is the process that runs anyway, and what it derives is
    /// addressed per signal by [`crate::governors::seat_for`] rather than by having
    /// been asked for. Putting a `--all` on the verb would give a governor a
    /// way to subscribe to work it cannot act on, which is the noise
    /// `wsp-095` Part 9 asks to be kept out.
    pub fn machine() -> Scope {
        Scope { name: "this machine".into(), seated: false, workspace: String::new(), pane: String::new(), all: true }
    }
}
