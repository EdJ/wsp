//! The message record: an envelope, three shapes, and a question that can be
//! answered back.
//!
//! Built to `wsp-095`, which found that **there is no message record anywhere
//! in wsp**: `wsp tell`, `wsp govern --tell` and a human keyboard are all
//! keystroke streams into a composer, and the three known transport faults are
//! consequences of that one absence rather than three bugs. A message that is
//! not a record cannot be re-read, replied to, deduplicated or attributed —
//! which is `worklist-010` (a retry that may have sent the same paragraph three
//! times), `robustness-094` (a governor's review credited to Ed on
//! `worklist-006`, so the record is wrong about who checked something) and
//! `robustness-093` (fifteen instructions sitting unsubmitted for three days).
//! This module is the record those three lack.
//!
//! # Three shapes, and what separates them
//!
//! | shape | is | closed by | stored? |
//! |---|---|---|---|
//! | [`Shape::Signal`] | a **level**: true now, derived, nobody wrote it | the predicate going false | **never** |
//! | [`Shape::Notification`] | an **edge**: somebody wrote it, nothing owed back | `acknowledged` | yes |
//! | [`Shape::Question`] | an edge with a lifecycle and **somebody sitting still** | `answered` or `abandoned` | yes |
//!
//! A signal is in the envelope because a panel, a watch and a governor's inbox
//! all read one mixed list and all three shapes have to arrive in one type —
//! `wsp-095` Part 13 settles that the payload carries `shape` rather than there
//! being a second envelope for levels. It is never *written* for the reason
//! Part 3 gives: a level that is queued is a fact that has stopped being true
//! sitting on a panel, which is `robustness-088`'s stated fear. So [`raise`]
//! refuses one, and so does [`Store::save_message`]. That is not a special case
//! in the record; it is the definition of a level, and it costs the envelope no
//! field.
//!
//! # Why ONE struct with a discriminant, and not three types
//!
//! The load-bearing choice, so the reasoning is here rather than in a review
//! note. Three arguments, in the order they decided it:
//!
//! 1. **Every consumer reads a mixed list.** The panel's card section, a
//!    governor's inbox and `wsp watch` all draw signals, notifications and
//!    questions together and sort them by [`Kind`]. Three types would each need
//!    an enum wrapper at every one of those boundaries — the discriminant back
//!    again, spelled worse, three times.
//! 2. **The record is a word on disk and must survive a word this build does
//!    not know.** The installed binary is routinely not the tree
//!    (`cmd_install::health` exists for that), and every write here is a
//!    read-modify-write of a shared JSON map. A payload-carrying variant cannot
//!    round-trip an unknown shape; a raw word can, which is the argument
//!    `Task::status_raw` already makes. Unknown words fail **closed** here — see
//!    [`Shape::may`].
//! 3. **The shapes differ in what may CLOSE them, not in what they hold.** The
//!    failure being prevented — `worklist-004`, where a seat answered down a
//!    different channel and then cleared the flag, so *clearing looked like
//!    answering* — is not preventable by a type, because clearing is the
//!    absence of a call. It is prevented by there being no clear verb for a
//!    question anywhere in this module. That safety lives in the API surface,
//!    and one struct buys it at one field's cost.
//!
//! The question-only fields are two [`Option`]s ([`Message::waiting`],
//! [`Message::reply_to`]) rather than a variant payload, for the same reason
//! `to` is not a field at all: see below.
//!
//! # What is deliberately NOT on the record
//!
//! **`to`.** Addressing is derived at read time, by the walk
//! `cmd_govern::seat_for` already makes — and which `wsp-095` Part 12 extends
//! to a `seats_for` returning the whole chain, the head being the addressee and
//! the tail observers who owe no disposition. That extension is routing rather
//! than record and is not in this change. The reason `to` is not stored is
//! unchanged either way: a seat taken after the hand went up must still answer
//! for it, and a seat that stands down must hand its messages upward without a
//! single record being rewritten. `addressed()`'s docstring already argues this
//! and it is right.
//! What is stored instead is [`Message::about`], the **subject**, from which
//! the address is computed; and [`Message::via`], the hops, because *who
//! actually handled it* is a fact about the past and derivation cannot recover
//! it.
//!
//! # The two exposures the first record type taught us, answered here
//!
//! The worklist was this project's first record type and had exactly two
//! exposures, neither designed in, both found by driving it. A message record
//! has ids in it, references tasks, and is drawn on a panel — the identical
//! two. Both are answered in the design rather than left to be found in G4:
//!
//! **Renumbering (`worklist-015`).** `Store::record_dirs` is *not* the place
//! for this record, and saying so is the answer rather than a dodge: that list
//! is committed `.md` records in the store, and this is machine-local state
//! (Part 5 — a raised hand is true while somebody is at the machine and
//! meaningless a week later, so it must not go into the history of the work).
//! The identical hazard for state is the hand-kept list `Store::rename_tasks`
//! walks, which is now `Store::state_files_with_ids` for the same reason
//! `record_dirs` exists. `messages.json` is in it. Both the fields that name a
//! task — [`Message::about`] and [`Waiting::task`] — are rewritten by the raw
//! token substitution that list drives, and **a message id can never be
//! mistaken for a task id**: see [`new_id`].
//!
//! **Refresh (`worklist-009`).** `Store::fingerprint` is likewise not the
//! place: it walks `projects/` and `tasks/`, and flags were deliberately kept
//! out of it with a stamp of their own, because a panel refetch gated on the
//! store's fingerprint would sit on a raised hand until something unrelated
//! moved. A message is the same fact, so it goes in the same stamp —
//! `Store::flags_stamp` is now `Store::attention_stamp` and reads both files in
//! one pass. Every surface that already checked for a raised hand now sees a
//! message, with no call site remembering to.
//!
//! # Nothing calls this yet, deliberately
//!
//! `wsp-095`'s build order puts the record first because everything else in
//! phase two types against it: the routing that derives an address from
//! [`Message::about`], the daemon pass that derives signals, the panel that
//! draws a card, and the verbs that raise and answer. **A record changed after
//! three consumers exist is three rebuilds**, which is why the shapes and the
//! lifecycle land on their own and are driven by their own tests first.

#![allow(dead_code)]

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};

use crate::store::Store;
use crate::util;

// ---- the vocabularies -----------------------------------------------------

/// Is anything owed back?
///
/// The sender's judgement, and the axis [`Kind`] is orthogonal to. A question
/// may be `fyi` (*when you get a moment, which of these two?*) and a
/// notification may be `stop` (*do not run `git stash pop`*), so collapsing the
/// two enums into one would make one of them undecidable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// Derived, level-triggered, self-clearing, ungameable. Never stored.
    Signal,
    /// An edge somebody wrote; nothing is owed back.
    Notification,
    /// An edge with a lifecycle and an agent sitting still while it is open.
    Question,
}

impl Shape {
    pub fn parse(s: &str) -> Option<Shape> {
        match s.trim().to_ascii_lowercase().as_str() {
            "signal" => Some(Shape::Signal),
            "notification" | "note" => Some(Shape::Notification),
            "question" | "ask" => Some(Shape::Question),
            _ => None,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Shape::Signal => "signal",
            Shape::Notification => "notification",
            Shape::Question => "question",
        }
    }

    /// May this shape be closed this way?
    ///
    /// The whole of `wsp-095` Part 6's table in one place, so a surface asks
    /// rather than remembers. Three rules, each closing a dated failure:
    ///
    /// - **A notification may be acknowledged, and acknowledging is a real
    ///   act.** It says *I have this and I am not passing it on*, which is what
    ///   makes the chain auditable.
    /// - **A question may not be cleared** — `Act::Answered` or
    ///   `Act::Abandoned`, both of which require a sentence. This is
    ///   `worklist-004`: the seat answered by another route, cleared the flag,
    ///   and to every surface in the system the matter was closed while the
    ///   asker sat waiting.
    /// - **A signal has no disposition at all.** A person clearing one would
    ///   watch it come straight back, which is how you train somebody to ignore
    ///   a panel.
    ///
    /// [`Act::Noted`] and [`Act::Escalated`] are not dispositions: they append
    /// a hop and leave the record open, which is why they are legal on both
    /// authored shapes and on neither level.
    pub fn may(&self, act: Act) -> bool {
        match (self, act) {
            (Shape::Signal, _) => false,
            (Shape::Notification, Act::Acknowledged | Act::Noted | Act::Escalated) => true,
            (Shape::Notification, _) => false,
            (Shape::Question, Act::Answered | Act::Abandoned | Act::Noted | Act::Escalated) => true,
            (Shape::Question, _) => false,
        }
    }
}

/// How much of the receiver's time this may take.
///
/// Named for **what the receiver should do**, not for topic — topic is
/// [`Message::about`]. Ordered by how much each is allowed to interrupt, and
/// `stop` is the only one that may reach a running turn. Named from Ed's
/// fifteen messages in one session rather than from theory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// *STOP, do not run `git stash pop`.* The only kind that may interrupt.
    Stop,
    /// *Your work is restored, carry on.* A work order. Read at a turn boundary.
    Direction,
    /// *You and your group-mate are both in `worklist.rs`.* Act when convenient.
    Note,
    /// *README's source-map row is convention, not a collision.* Never announced.
    Fyi,
}

impl Kind {
    pub fn parse(s: &str) -> Option<Kind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "stop" => Some(Kind::Stop),
            "direction" | "order" => Some(Kind::Direction),
            "note" => Some(Kind::Note),
            "fyi" | "info" => Some(Kind::Fyi),
            _ => None,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Stop => "stop",
            Kind::Direction => "direction",
            Kind::Note => "note",
            Kind::Fyi => "fyi",
        }
    }
    /// The one question a transport has to ask. `wsp-095` Part 5's hard
    /// refusal: *nothing in this design may push text into a working agent's
    /// composer, except `stop`* — because `agent.prompt` to a working agent
    /// queues, and a queued message is text in a composer that was never
    /// submitted, which is `robustness-093`'s mechanism with seven recorded
    /// instances.
    pub fn may_interrupt(&self) -> bool {
        matches!(self, Kind::Stop)
    }
}

/// What one keypress would answer.
///
/// A third closed vocabulary, and it does not merge into [`Kind`].
/// `wsp flag --ask claim` already argues it (`cmd_agent.rs:777`): *the answer
/// is a key on somebody's panel and that key runs a command*, so a sender names
/// a **question the surface already knows how to answer** rather than naming
/// argv. `kind` is the sender's judgement about the receiver's time; `ask` is a
/// capability of the surface. They change for different reasons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    Nothing,
    /// *Hand me this task.* `y` on the panel runs the claim.
    Claim,
}

impl Ask {
    pub fn parse(s: &str) -> Option<Ask> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "none" => Some(Ask::Nothing),
            "claim" => Some(Ask::Claim),
            _ => None,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Ask::Nothing => "",
            Ask::Claim => "claim",
        }
    }
}

/// Who put this here, and therefore what `x` on a panel may do.
///
/// `robustness-088` blocked on four questions and this answers the first: a
/// derived flag is cleared by its predicate going false and **by nothing else**
/// — a person may not clear one, because it would come straight back.
///
/// # Derived and not stored, which is a change from the sketch
///
/// `wsp-095` Part 5 puts `source` on the record, and Part 9 asks a surface to
/// read it to decide what `x` may do. Both are served — but it is computed by
/// [`Message::source`] rather than written down, because with `shape` on the
/// record it is **entirely a function of `shape` and `from`**, and a stored copy
/// of a derivable fact is a field that can disagree with the two fields it came
/// from. A record saying `shape: signal, source: agent` has no meaning, and
/// nothing would stop one being written. Part 5 named `source` before the three
/// shapes existed; the shapes arrived and subsumed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// An agent raised it.
    Agent,
    /// wsp derived it. Always a [`Shape::Signal`].
    Wsp,
    /// A person typed it.
    Human,
}

impl Source {
    pub fn parse(s: &str) -> Option<Source> {
        match s.trim().to_ascii_lowercase().as_str() {
            "agent" => Some(Source::Agent),
            "wsp" | "derived" => Some(Source::Wsp),
            "human" | "person" => Some(Source::Human),
            _ => None,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Source::Agent => "agent",
            Source::Wsp => "wsp",
            Source::Human => "human",
        }
    }
}

/// The disposition — where the record got to.
///
/// **`cleared` is not in here and that is the whole point.** Lowering a hand is
/// not a disposition for a question; `escalated` is not one either, because an
/// escalation appends a hop and leaves the record `open` at the next level up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Open,
    /// A notification: *I have this and I am not passing it on.*
    Acknowledged,
    /// A question, with a sentence, delivered to the asker.
    Answered,
    /// A question, with a reason, delivered to the asker.
    Abandoned,
}

impl State {
    pub fn parse(s: &str) -> Option<State> {
        match s.trim().to_ascii_lowercase().as_str() {
            "open" => Some(State::Open),
            "acknowledged" | "ack" => Some(State::Acknowledged),
            "answered" => Some(State::Answered),
            "abandoned" => Some(State::Abandoned),
            _ => None,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            State::Open => "open",
            State::Acknowledged => "acknowledged",
            State::Answered => "answered",
            State::Abandoned => "abandoned",
        }
    }
}

/// What somebody did to a record on its way up.
///
/// `wsp-095` Decision 3 names two — `noted` and `escalated`. The three
/// dispositions are in here too, deliberately and as a stated extension: the
/// decision asks that an escalation carry *who wrote each note and who
/// escalated*, and a chain that records every hop except the one that ended it
/// cannot say who answered. One vocabulary, and `via` is then the whole history
/// rather than the history up to the last step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    /// Looked at it, wrote something, left it open.
    Noted,
    /// Could not answer it; passed it up. The record stays open.
    Escalated,
    Acknowledged,
    Answered,
    Abandoned,
}

impl Act {
    pub fn parse(s: &str) -> Option<Act> {
        match s.trim().to_ascii_lowercase().as_str() {
            "noted" | "note" => Some(Act::Noted),
            "escalated" => Some(Act::Escalated),
            "acknowledged" | "ack" => Some(Act::Acknowledged),
            "answered" => Some(Act::Answered),
            "abandoned" => Some(Act::Abandoned),
            _ => None,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Act::Noted => "noted",
            Act::Escalated => "escalated",
            Act::Acknowledged => "acknowledged",
            Act::Answered => "answered",
            Act::Abandoned => "abandoned",
        }
    }
    /// Does this end the record, or leave it open at the next level?
    pub fn closes(&self) -> Option<State> {
        match self {
            Act::Noted | Act::Escalated => None,
            Act::Acknowledged => Some(State::Acknowledged),
            Act::Answered => Some(State::Answered),
            Act::Abandoned => Some(State::Abandoned),
        }
    }
}

// ---- the parts ------------------------------------------------------------

/// The subject. Optional, and the thing addressing is derived *from*.
///
/// A task id, a scope — a project or a worklist, which share one key space —
/// or a group within one. Stored, unlike `to`, because a subject is what the
/// sender meant and an address is where that lands today.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum About {
    Nothing,
    Task(String),
    Scope(String),
    Group { scope: String, group: usize },
}

impl About {
    pub fn task(&self) -> Option<&str> {
        match self {
            About::Task(t) => Some(t),
            _ => None,
        }
    }
    pub fn scope(&self) -> Option<&str> {
        match self {
            About::Scope(s) | About::Group { scope: s, .. } => Some(s),
            _ => None,
        }
    }
    fn to_json(&self) -> Value {
        match self {
            About::Nothing => Value::Null,
            About::Task(t) => json!({ "task": t }),
            About::Scope(s) => json!({ "scope": s }),
            About::Group { scope, group } => json!({ "scope": scope, "group": group }),
        }
    }
    fn from_json(v: Option<&Value>) -> About {
        let Some(v) = v else { return About::Nothing };
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
        let task = s("task");
        if !task.is_empty() {
            return About::Task(task);
        }
        let scope = s("scope");
        if scope.is_empty() {
            return About::Nothing;
        }
        match v.get("group").and_then(Value::as_u64) {
            Some(g) => About::Group { scope, group: g as usize },
            None => About::Scope(scope),
        }
    }
}

/// The sender, and it is a small enum rather than a string.
///
/// **The field that matters most.** wsp cannot make herdr attribute
/// `agent.prompt` — that is `robustness-094` and it is herdr's — so what wsp
/// can do is carry the sender in the payload, which inverts today's default:
///
/// > Today, unattributed text reads as Ed. Under this design, **attributed text
/// > is a seat or an agent and unattributed text is a person** — because the one
/// > channel wsp does not control is a human keyboard, and that is the correct
/// > reading of anything arriving without an envelope.
///
/// That reading is why [`Party::Human`] is the fallback when the field does not
/// parse: a record that cannot say who wrote it is a record that reads as a
/// person, exactly as a tty write does. It also means a consumer has four cases
/// and not five.
///
/// Structured and not a string because the three consumers ask different
/// questions of it and every one of them would otherwise guess from the shape
/// of the text: a panel wants the workspace so its byline can be jumped to, a
/// router wants to know whether the sender is itself a seat, and an inbox wants
/// a byline. Guessing a pane id from its punctuation is the thing this whole
/// design exists to stop.
///
/// The workspace rides beside the pane because `flags.json` already carries
/// both and the panel already needs both: a pane is where an agent is now, and
/// a workspace is what survives it moving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Party {
    /// An agent, by the pane it is in and the workspace that pane is in.
    Pane { pane: String, workspace: String },
    /// A seat, by the scope it is custodian of — a project or a worklist, which
    /// share one key space.
    Seat(String),
    /// An agent addressed by handle rather than by location, for a sender that
    /// is not in a pane.
    Agent(String),
    /// A keyboard. The fallback, and see above for why.
    Human,
}

impl Party {
    pub fn pane(pane: &str, workspace: &str) -> Party {
        Party::Pane { pane: pane.to_string(), workspace: workspace.to_string() }
    }
    pub fn seat(scope: &str) -> Party {
        Party::Seat(scope.to_string())
    }

    /// One line, for a byline. `wsp-095` Part 4's whole point is that this
    /// exists at all: the seat had already adopted the rule by hand — *every
    /// message this seat sends opens by saying it is from the seat* — which is
    /// the strongest possible argument for making the transport carry it.
    pub fn byline(&self) -> String {
        match self {
            Party::Pane { pane, .. } => pane.clone(),
            Party::Seat(scope) => format!("{scope} seat"),
            Party::Agent(handle) => handle.clone(),
            Party::Human => "a person".into(),
        }
    }

    /// Where a surface can jump to, if anywhere.
    pub fn workspace(&self) -> Option<&str> {
        match self {
            Party::Pane { workspace, .. } if !workspace.is_empty() => Some(workspace),
            _ => None,
        }
    }

    fn to_json(&self) -> Value {
        match self {
            Party::Pane { pane, workspace } => json!({ "pane": pane, "workspace": workspace }),
            Party::Seat(scope) => json!({ "seat": scope }),
            Party::Agent(handle) => json!({ "agent": handle }),
            Party::Human => json!({ "human": true }),
        }
    }

    fn from_json(v: Option<&Value>) -> Party {
        let Some(v) = v else { return Party::Human };
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
        let pane = s("pane");
        if !pane.is_empty() {
            return Party::Pane { pane, workspace: s("workspace") };
        }
        let seat = s("seat");
        if !seat.is_empty() {
            return Party::Seat(seat);
        }
        let agent = s("agent");
        if !agent.is_empty() {
            return Party::Agent(agent);
        }
        Party::Human
    }
}

/// Who is sitting still while a question is open.
///
/// **The pane and the task, because the pane can go and the task cannot.** That
/// is what makes *"how long has somebody been blocked"* answerable for the
/// first time — `robustness-051`'s complaint stated as a field rather than a
/// wish — and it is what lets an open question become a derived signal:
/// `now - at`, joined with whether this pane is stopped, is `quiet_note`'s
/// conjunction with a better subject line.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Waiting {
    pub pane: String,
    pub task: String,
}

impl Waiting {
    pub fn new(pane: &str, task: &str) -> Waiting {
        Waiting { pane: pane.to_string(), task: task.to_string() }
    }
    fn to_json(&self) -> Value {
        json!({ "pane": self.pane, "task": self.task })
    }
    fn from_json(v: Option<&Value>) -> Option<Waiting> {
        let v = v?;
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
        let w = Waiting { pane: s("pane"), task: s("task") };
        (!w.pane.is_empty() || !w.task.is_empty()).then_some(w)
    }
}

/// One step of the walk, with an author.
///
/// A hand arriving three levels up carries **its history, not just its
/// origin** — the difference between *nobody looked at this* and *two governors
/// looked, wrote what they thought, and could not answer it*. Those deserve
/// very different amounts of a person's attention and today they are
/// indistinguishable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hop {
    pub at: String,
    /// Who acted. A [`Party`] and not a string for the reason
    /// [`Message::from`] is one: a hop with an author nobody can resolve is a
    /// hop that says somebody looked and not who.
    pub by: Party,
    pub act: Act,
    /// What it wrote.
    pub note: String,
}

impl Hop {
    fn to_json(&self) -> Value {
        json!({
            "at": self.at,
            "by": self.by.to_json(),
            "act": self.act.as_str(),
            "note": self.note,
        })
    }
    fn from_json(v: &Value) -> Hop {
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
        Hop {
            at: s("at"),
            by: Party::from_json(v.get("by").filter(|b| !b.is_null())),
            act: Act::parse(&s("act")).unwrap_or(Act::Noted),
            note: s("note"),
        }
    }
}

// ---- the envelope ---------------------------------------------------------

/// One message. One envelope, three shapes, and a discriminant that is a word.
///
/// The five vocabularies are held as raw strings and parsed on the way out, for
/// the reason `Worklist::status_raw` is: *a word on disk that this build does
/// not know is a word to show somebody, not one to launder on the next write.*
/// Every write here is a read-modify-write of a shared map, so an older binary
/// — and the installed binary is routinely older than the tree — would
/// otherwise rewrite a newer build's records into words it happens to know.
/// Unknown words fail closed: [`Shape::may`] permits nothing for a shape it
/// cannot parse, and [`Message::is_open`] is false for a state it cannot parse,
/// so an old build shows a record it does not understand and refuses to act on
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// Stable, so a resend is idempotent and an answer can find its way home.
    pub id: String,
    pub shape_raw: String,
    pub kind_raw: String,
    pub ask_raw: String,
    pub state_raw: String,
    /// **The sender.** See [`Party`], which is where the argument for it is.
    pub from: Party,
    pub about: About,
    pub at: String,
    pub text: String,
    /// The hops it escalated through, each with an author.
    pub via: Vec<Hop>,
    /// Read, and still up. `esc` says *not now*, `x` says *dealt with*, and a
    /// panel that could only do the second would train you to clear things you
    /// had not answered. Per record and belonging to whoever read it; a
    /// disposition is a decision and belongs to whoever took it.
    pub seen: bool,
    /// Who is sitting still while this is open — a question's field.
    ///
    /// It rides on one other thing, and only one: **an answer carries the
    /// question's waiting party as its own**, because Part 7 addresses a reply
    /// to `question.waiting` and not to whoever answered. That is the link the
    /// `worklist-004` incident did not have. Everywhere else it is `None`, and
    /// a notification with a waiting party is a question that forgot to say so.
    pub waiting: Option<Waiting>,
    /// On an answer: the id of the question it closes. The presence of this is
    /// what makes a message a reply, which is how [`replies_for`] finds them.
    pub reply_to: Option<String>,
}

impl Message {
    /// A notification — the default shape, because a question commits somebody
    /// to answering it and starts a clock, and that is not a thing to acquire
    /// by accident.
    pub fn new(from: Party, kind: Kind, text: &str) -> Message {
        Message {
            id: new_id(),
            shape_raw: Shape::Notification.as_str().into(),
            kind_raw: kind.as_str().into(),
            ask_raw: Ask::Nothing.as_str().into(),
            state_raw: State::Open.as_str().into(),
            from,
            about: About::Nothing,
            at: util::now_iso(),
            text: text.to_string(),
            via: Vec::new(),
            seen: false,
            waiting: None,
            reply_to: None,
        }
    }

    /// A question: the same envelope, plus the party that is sitting still.
    pub fn question(from: Party, kind: Kind, text: &str, waiting: Waiting) -> Message {
        let mut m = Message::new(from, kind, text);
        m.shape_raw = Shape::Question.as_str().into();
        m.waiting = Some(waiting);
        m
    }

    /// A level wsp derived. Never stored — see [`raise`] and
    /// [`Store::save_message`], both of which refuse one.
    pub fn signal(kind: Kind, text: &str) -> Message {
        let mut m = Message::new(Party::Agent("wsp".into()), kind, text);
        m.shape_raw = Shape::Signal.as_str().into();
        m
    }

    pub fn shape(&self) -> Option<Shape> {
        Shape::parse(&self.shape_raw)
    }
    pub fn kind(&self) -> Option<Kind> {
        Kind::parse(&self.kind_raw)
    }
    pub fn ask(&self) -> Option<Ask> {
        Ask::parse(&self.ask_raw)
    }
    /// Who put this here. Derived — see [`Source`] for why.
    pub fn source(&self) -> Source {
        match (self.shape(), &self.from) {
            (Some(Shape::Signal), _) => Source::Wsp,
            (_, Party::Human) => Source::Human,
            _ => Source::Agent,
        }
    }
    pub fn state(&self) -> Option<State> {
        State::parse(&self.state_raw)
    }

    pub fn set_shape(&mut self, s: Shape) {
        self.shape_raw = s.as_str().to_string();
    }
    pub fn set_kind(&mut self, k: Kind) {
        self.kind_raw = k.as_str().to_string();
    }
    pub fn set_ask(&mut self, a: Ask) {
        self.ask_raw = a.as_str().to_string();
    }
    pub fn about(mut self, about: About) -> Message {
        self.about = about;
        self
    }

    /// The headline: the first line of [`Message::text`].
    ///
    /// # Why the split is a convention and not two fields
    ///
    /// A card needs a headline and a detail — `render-079` was a flag card
    /// showing ~200 characters of a paragraph with a second card unreachable
    /// behind it, fixed by budgeting in characters rather than rows. So a
    /// surface has to be able to ask for the short form.
    ///
    /// Two fields is how `flags.json` does it today, and `worklist-018` is the
    /// bill: **a flag raised with `--body` and no `--title` shows nothing at the
    /// card**, because the field the card drew was the empty one. A record with
    /// two fields has a state where the important one is missing; a record with
    /// one line and a rest does not — there is always a first line, and it is
    /// always the thing to draw. Git commit messages settled this argument
    /// decades ago and for the same reason.
    pub fn title(&self) -> &str {
        self.text.lines().next().unwrap_or("").trim()
    }

    /// Everything after the headline, leading blank lines removed. Empty when
    /// the message is one line, which is the common case and is not a fault.
    pub fn body(&self) -> &str {
        match self.text.split_once('\n') {
            Some((_, rest)) => rest.trim_start_matches('\n').trim_end(),
            None => "",
        }
    }

    /// Still wanting a disposition. A word this build cannot parse is **not**
    /// open, so an older binary never answers a question it does not
    /// understand.
    pub fn is_open(&self) -> bool {
        self.state() == Some(State::Open)
    }

    /// Is this shape's disposition somebody's to take, or does it self-clear?
    pub fn owes_a_disposition(&self) -> bool {
        self.is_open() && self.shape().is_some_and(|s| s != Shape::Signal)
    }

    /// Should a person look at this?
    ///
    /// # Why this is not [`Message::is_open`] widened
    ///
    /// [`Message::is_open`] fails closed on a word it cannot parse, which is
    /// right for **action** and silent for **attention**, and they are
    /// different questions. An older binary — and the installed binary is
    /// routinely not the tree — meeting a state a newer one wrote does not
    /// merely refuse to act on it: with only `is_open` it reports that nothing
    /// is standing. A question that cannot be parsed becomes a question that
    /// raises nothing, which is `robustness-051` arriving through the module
    /// built to remove it.
    ///
    /// The repair is the one this project has already made twice, in
    /// `cmd_checkout`: give the unknown case **its own answer** rather than
    /// folding it into one that already means something. `Landing::NoBranch`
    /// and then `Landing::Nothing` are readings rather than verdicts, and
    /// widening `is_open` would undo both — a consumer asking *is this
    /// question open* would get `true` for a record whose state is unreadable,
    /// which is not an answer, it is a different wrong answer.
    ///
    /// So the two stay separate and both stay honest. `is_open` answers false,
    /// meaning *I do not know that this is open*. This answers true for open
    /// **and** for unreadable, meaning *somebody should look at this*, and it
    /// is what a surface draws from. **"I do not understand this" is a
    /// first-class reason to fetch a human, not a reason to say nothing.**
    ///
    /// [`Shape::may`] is unchanged and still permits nothing it cannot parse:
    /// fail closed for action, loud for attention.
    ///
    /// It catches an unreadable *shape* too, and by the same route rather than
    /// by a second clause: a record whose shape is a word this build does not
    /// know still has a state, and while that state says `open` this says so —
    /// where [`Message::owes_a_disposition`] cannot, because it has to name a
    /// shape to know whose disposition it is.
    pub fn needs_attention(&self) -> bool {
        self.is_open() || self.state().is_none()
    }

    /// Is this an answer going home, rather than a hand raised outward?
    ///
    /// The one axis a raised-hand surface has to split on. Everything else in
    /// the record is addressed by [`Message::about`], which is derived to a
    /// seat and then to a person; a reply is addressed to
    /// [`Message::waiting`], which is whoever asked, and it already has two
    /// deliveries of its own — the asker's task log, written before the reply
    /// was minted, and the asker's own inbox. Drawing one in the flags section
    /// would put an answer on the panel of the person who wrote it.
    ///
    /// The presence of `reply_to` is what makes a message a reply — the same
    /// test [`replies_for`] uses, so the two cannot come to disagree.
    pub fn is_reply(&self) -> bool {
        self.reply_to.is_some()
    }

    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "shape": self.shape_raw,
            "kind": self.kind_raw,
            "ask": self.ask_raw,
            // Derived, and written out anyway: a hook reading `messages.json`
            // or an `events.jsonl` line gets the answer without reimplementing
            // the derivation. It is never read back — `from_json` ignores it —
            // so an edited file cannot make it disagree with `shape` and
            // `from`.
            "source": self.source().as_str(),
            "state": self.state_raw,
            "from": self.from.to_json(),
            "about": self.about.to_json(),
            "at": self.at,
            "text": self.text,
            "via": self.via.iter().map(Hop::to_json).collect::<Vec<_>>(),
            "seen": self.seen,
            "waiting": self.waiting.as_ref().map(Waiting::to_json).unwrap_or(Value::Null),
            "reply_to": self.reply_to.clone().map(Value::from).unwrap_or(Value::Null),
        })
    }

    pub fn from_json(id: &str, v: &Value) -> Message {
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
        let word = |k: &str, fallback: &str| {
            let got = s(k);
            if got.is_empty() { fallback.to_string() } else { got }
        };
        Message {
            id: {
                let own = s("id");
                if own.is_empty() { id.to_string() } else { own }
            },
            // The default is the quiet one, and it is the shape that owes
            // nothing back: a record whose shape did not survive must not
            // become a question somebody is waiting on.
            shape_raw: word("shape", Shape::Notification.as_str()),
            kind_raw: word("kind", Kind::Note.as_str()),
            ask_raw: s("ask"),
            state_raw: word("state", State::Open.as_str()),
            from: Party::from_json(v.get("from").filter(|f| !f.is_null())),
            about: About::from_json(v.get("about").filter(|a| !a.is_null())),
            at: s("at"),
            text: s("text"),
            via: v
                .get("via")
                .and_then(Value::as_array)
                .map(|a| a.iter().map(Hop::from_json).collect())
                .unwrap_or_default(),
            seen: v.get("seen").and_then(Value::as_bool).unwrap_or(false),
            waiting: Waiting::from_json(v.get("waiting").filter(|w| !w.is_null())),
            reply_to: v
                .get("reply_to")
                .and_then(Value::as_str)
                .filter(|r| !r.is_empty())
                .map(str::to_string),
        }
    }
}

/// A new message id.
///
/// # Why it cannot be mistaken for a task id
///
/// `Store::rename_tasks` rewrites a renumbering through the raw text of every
/// state file that holds ids, matching whole `[A-Za-z0-9_-]` tokens against a
/// map keyed by old task ids. That is what makes [`About`] and [`Waiting::task`]
/// survive a `wsp mv` — and it is also why a message id must live outside the
/// task id space, or a renumbering could rewrite the identity an answer uses to
/// find its way home.
///
/// Task ids have two shapes and both end in digits after their last `-`:
/// `<project>-NNN` and the older `t-YYMMDD-NNN`. This ends in base-36 that
/// always contains the process id's letters or, at worst, digits from a
/// different arrangement — so the guarantee is made explicit rather than
/// assumed: the last segment is prefixed with `p`, which no task id has ever
/// had, and `is_message_id` is the assertion the test drives.
pub fn new_id() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    // Nanoseconds because two messages inside one second are two messages, and
    // the counter because two inside one nanosecond are still two.
    format!("m-{}-p{:x}{:x}", base36(util::epoch_nanos()), std::process::id(), n)
}

/// Is this one of ours? The complement of a task id, and the test drives both
/// directions.
pub fn is_message_id(s: &str) -> bool {
    s.starts_with("m-") && s.rsplit('-').next().is_some_and(|tail| tail.starts_with('p'))
}

fn base36(mut n: u64) -> String {
    const D: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if n == 0 {
        return "0".into();
    }
    let mut out = Vec::new();
    while n > 0 {
        out.push(D[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

// ---- refusals -------------------------------------------------------------

/// Why a lifecycle call did not happen.
///
/// Every one of these is a sentence a caller can print, because a verb whose
/// whole effect is on somebody else's screen has to say what it did instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    /// No record with that id.
    NoSuchMessage(String),
    /// A level is not stored and has no disposition: it self-clears when the
    /// predicate that derived it goes false.
    IsALevel,
    /// This shape cannot be closed that way — the whole of Part 6's table.
    /// Carries what was tried and what the shape was.
    WrongShape { shape: String, act: Act },
    /// Already disposed of, or in a state this build does not know.
    NotOpen(String),
    /// Still open, and the caller asked for it to go away. Only a disposition
    /// closes a question, and a disposition takes a sentence.
    StillOpen,
    /// A disposition that requires a sentence was given none. *What wsp
    /// contributes is the obligation to write a sentence, not the judgement* —
    /// the same refusal `wsp worklist go` already makes at a barrier.
    NeedsASentence,
    /// A question with no waiting party. The cost of an open question is the
    /// agent sitting still while it is open; a question that names nobody
    /// cannot be answered home and cannot become a signal.
    NobodyWaiting,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refused::NoSuchMessage(id) => write!(f, "no message {id}"),
            Refused::IsALevel => write!(
                f,
                "that is a derived signal — it clears when the fact does, and nobody clears it by hand"
            ),
            Refused::WrongShape { shape, act } => {
                write!(f, "a {shape} cannot be {}", act.as_str())
            }
            Refused::NotOpen(state) => write!(f, "already {state}"),
            Refused::StillOpen => write!(
                f,
                "that question is still open — answer it or abandon it with a reason; \
                 taking it down is what looks like an answer and is not one"
            ),
            Refused::NeedsASentence => write!(f, "that needs a sentence, not a keystroke"),
            Refused::NobodyWaiting => write!(f, "a question has to name who is waiting on it"),
        }
    }
}

// ---- the lifecycle --------------------------------------------------------
//
// Free functions over `&Store`, beside the record rather than on it, for the
// reason `worklist.rs` gives: these are computation over the store, and hung
// off `Message` they would look like methods that only touch the record.

/// Write a message, and log the edge.
///
/// Refuses a level. A signal is derived on the pass that needs it and is never
/// queued: a queued level is a fact that has stopped being true sitting on a
/// panel, which is `robustness-088`'s named failure — *a panel full of flags
/// nobody reads, which is worse than the silence it replaces.*
///
/// Refuses a question with nobody waiting, because an open question's whole
/// cost is the party sitting still while it is open.
///
/// Under the state lock, like every other read-modify-write here: the resend
/// check below is a look followed by a write, and two of those interleaved is
/// how one of them silently loses.
pub fn raise(store: &Store, m: &Message) -> Result<(), Refused> {
    store.locked(|| raise_locked(store, m))
}

fn raise_locked(store: &Store, m: &Message) -> Result<(), Refused> {
    if m.shape() == Some(Shape::Question) {
        let named = m.waiting.as_ref().is_some_and(|w| !w.pane.is_empty() || !w.task.is_empty());
        if !named {
            return Err(Refused::NobodyWaiting);
        }
    }
    // A resend is a no-op, which is the whole point of the id.
    //
    // `worklist-010`: `govern --tell` reported failure *after* delivering,
    // because the error it surfaced came from a status probe rather than from
    // the delivery, so the governor retried and may have sent the same
    // paragraph three times. With a stable id the retry lands on a record that
    // is already there and changes nothing — and, crucially, does not reopen a
    // record somebody has since acknowledged or answered. The false report
    // becomes an annoyance instead of a duplicate generator, which is the half
    // of that fault a record can fix.
    if store.message(&m.id).is_some() {
        return Ok(());
    }
    store.save_message(m)?;
    store.log_event(
        "message-raised",
        json!({
            "id": m.id,
            "shape": m.shape_raw,
            "kind": m.kind_raw,
            "from": m.from.to_json(),
            "about": m.about.to_json(),
            // The headline, because `~/wsp/hooks/on-message-raised` is the
            // escape to anything louder and a doorbell that has to open
            // `messages.json` to find out what was said is a doorbell nobody
            // writes. One line, deliberately: the hook is a notification, not
            // a reader.
            "title": m.title(),
            "reply_to": m.reply_to,
        }),
    );
    Ok(())
}

/// Read, and still up.
pub fn see(store: &Store, id: &str) -> Result<Message, Refused> {
    store.locked(|| {
        let mut m = store.message(id).ok_or_else(|| Refused::NoSuchMessage(id.into()))?;
        m.seen = true;
        store.save_message(&m)?;
        Ok(m)
    })
}

/// Looked at it, wrote something, left it open.
pub fn note(store: &Store, id: &str, by: &Party, note: &str) -> Result<Message, Refused> {
    hop(store, id, by, Act::Noted, note)
}

/// Could not answer it; passed it up.
///
/// **The record stays open.** Escalation is a hop and not a disposition —
/// `open ──escalated──▶ open (at the next level)` — because the addressee is
/// derived from the chain and the chain has one more level in it, not because
/// anything about the question changed. Nothing decides to escalate; the walk
/// simply does not stop at a level that has nobody in it.
pub fn escalate(store: &Store, id: &str, by: &Party, note: &str) -> Result<Message, Refused> {
    hop(store, id, by, Act::Escalated, note)
}

/// *I have this and I am not passing it on.* A notification's disposition, and
/// a real act: it is what makes the chain auditable, it costs one keystroke,
/// and it is not an answer.
pub fn acknowledge(store: &Store, id: &str, by: &Party) -> Result<Message, Refused> {
    hop(store, id, by, Act::Acknowledged, "")
}

/// What a hop and a disposition have in common, which is all of it except the
/// task log.
/// Read, decide, and write, with the state files to ourselves.
///
/// The lock is around the *whole* cycle and not only the write, for the reason
/// `Store::locked` exists: `write_atomic` makes a write indivisible, which is
/// not the same thing. Two seats escalating one hand at the same moment both
/// read the record, both append their hop, and the second write drops the
/// first — a governor's note simply vanishes, and the walk arrives upstairs
/// saying one level looked at it when two did. The lock is reentrant, so the
/// `save_message` inside takes it again without deadlocking.
fn hop(store: &Store, id: &str, by: &Party, act: Act, note: &str) -> Result<Message, Refused> {
    store.locked(|| hop_locked(store, id, by, act, note))
}

fn hop_locked(
    store: &Store,
    id: &str,
    by: &Party,
    act: Act,
    note: &str,
) -> Result<Message, Refused> {
    let mut m = store.message(id).ok_or_else(|| Refused::NoSuchMessage(id.into()))?;
    let Some(shape) = m.shape() else {
        return Err(Refused::WrongShape { shape: m.shape_raw.clone(), act });
    };
    if shape == Shape::Signal {
        return Err(Refused::IsALevel);
    }
    if !shape.may(act) {
        return Err(Refused::WrongShape { shape: m.shape_raw.clone(), act });
    }
    if !m.is_open() {
        return Err(Refused::NotOpen(m.state_raw.clone()));
    }
    m.via.push(Hop {
        at: util::now_iso(),
        by: by.clone(),
        act,
        note: note.to_string(),
    });
    if let Some(state) = act.closes() {
        m.state_raw = state.as_str().to_string();
    }
    store.save_message(&m)?;
    store.log_event(
        &format!("message-{}", act.as_str()),
        json!({
            "id": m.id,
            "shape": m.shape_raw,
            "by": by.to_json(),
            "state": m.state_raw,
            "note": note,
        }),
    );
    Ok(m)
}

/// Where the words went, which a receipt has to name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Landed {
    /// On the asker's task log. The normal case, and the one that has no drop.
    Task(String),
    /// The record only, and why. The asker's task is gone — not the asker,
    /// which is fine and expected, but the task itself.
    RecordOnly(String),
}

/// The task an answer to this question will durably land on, if any.
///
/// **The asker's own task first**, which is `wsp-095` Part 7 unchanged: an
/// agent is respawned onto the task it holds and reads its log in the brief,
/// so that is where an answer is waiting for it whether it is alive, stopped
/// or gone.
///
/// # The fallback exists because a seat holds no task
///
/// And a seat is the party this whole task is about. `worklist-014`: for the
/// whole of 2026-08-19 the two governor seats exchanged perhaps thirty
/// messages and **a person carried most of them**, because a reply from a seat
/// has nowhere to live — [`Waiting`] names a pane, the pane's scrollback is not
/// a record, and the pane closes. Landing the answer on `waiting.task` serves
/// every agent and none of the seats.
///
/// So when the asker holds no task, the answer lands on the task the question
/// was **about**. That is not a second address; it is the same rule — *put the
/// answer where the next reader will look for it* — applied to a party whose
/// next reader is not itself. The evidence is what survived the day: the `w30`
/// decode, the `needs_you` correction, the join shape and the stash repair are
/// all readable now because somebody put them on a task, and the prose that
/// carried them between two panes is gone.
///
/// It is a fallback and never an override, so nothing about an agent's answer
/// changes: an agent asking about somebody *else's* task is still answered on
/// its own, because its own is the one it will be handed again.
///
/// `None` is a real answer and not a failure — see [`Landed::RecordOnly`],
/// which names it rather than swallowing it. The verb refuses to raise a
/// question that would reach it (`cmd_message::ask`); this is what the record
/// says when one arrives by some other road.
pub fn homes_to(store: &Store, q: &Message) -> Option<String> {
    let mine = q.waiting.as_ref().map(|w| w.task.as_str()).unwrap_or_default();
    if !mine.is_empty() && store.task(mine).is_some() {
        return Some(mine.to_string());
    }
    let about = q.about.task()?;
    store.task(about).map(|_| about.to_string())
}

/// A question closed, and the two things a caller then has in hand.
#[derive(Debug, Clone)]
pub struct Closed {
    /// The question, now `answered` or `abandoned`, with the hop that ended it.
    pub question: Message,
    /// The reply, addressed to whoever was waiting. **Not yet delivered** —
    /// delivery is a transport's, and `wsp-095` Part 5 governs it: nothing may
    /// push into a working agent's composer except `stop`.
    pub reply: Message,
    pub landed: Landed,
}

/// Answer a question. **The answer is written to the asker's task log first and
/// delivered second.**
///
/// The load-bearing decision in Part 7, and the reason is that the delivery is
/// best-effort and the record is not:
///
/// - **Nothing is silently dropped.** A despawned agent's question is neither
///   dropped nor left open — the write happens whether the asker is alive,
///   stopped or gone, so there is no drop case to get wrong. Dropping silently
///   is the same failure as clearing a flag.
/// - **A respawned agent reads it.** Task prose is injected into every spawn on
///   that task. That is normally a cost this project spends care avoiding; here
///   it is the delivery mechanism, for free, and it is the one case where the
///   injection is a feature.
/// - **The record outlives the worktree.** `worklist-012` found that agent
///   memory dies with the worktree it was written in. A task log does not.
///
/// On the cost, because `wsp tell` argues *against* logging sentences to a task
/// — *"a line per sentence would be paid for by every session afterwards; this
/// is forensics"* — and that argument is right for a sentence and does not
/// apply here. The discriminator is clean: **an answered question is a decision
/// about the work, which is what `## Log` is for; a sentence in passing is
/// not.** Ordinary messages go to `events.jsonl`. Only answers land on the
/// task, one line each, bounded by the number of questions actually asked.
///
/// The reply's kind is `direction` and not the question's own, because the
/// asker is by definition sitting still: whatever it cost to ask, the answer is
/// the thing that unblocks it. A caller with a reason may set it afterwards.
pub fn answer(store: &Store, id: &str, by: &Party, text: &str) -> Result<Closed, Refused> {
    close(store, id, by, Act::Answered, text)
}

/// Abandon a question, with a reason, and tell the asker.
///
/// The other way a question may end, and it goes home by the same path for the
/// same reason: an asker that learns nothing from a question closing is an
/// asker that sits waiting for ever. **This is what `clear` is not.** A hand
/// lowered without a sentence looks exactly like an answer to every surface in
/// the system, which is `worklist-004` and is the failure this whole shape
/// exists to prevent.
pub fn abandon(store: &Store, id: &str, by: &Party, reason: &str) -> Result<Closed, Refused> {
    close(store, id, by, Act::Abandoned, reason)
}

fn close(store: &Store, id: &str, by: &Party, act: Act, text: &str) -> Result<Closed, Refused> {
    store.locked(|| close_locked(store, id, by, act, text))
}

fn close_locked(
    store: &Store,
    id: &str,
    by: &Party,
    act: Act,
    text: &str,
) -> Result<Closed, Refused> {
    if text.trim().is_empty() {
        return Err(Refused::NeedsASentence);
    }
    let q = store.message(id).ok_or_else(|| Refused::NoSuchMessage(id.into()))?;
    match q.shape() {
        Some(Shape::Signal) => return Err(Refused::IsALevel),
        Some(s) if s.may(act) => {}
        _ => return Err(Refused::WrongShape { shape: q.shape_raw.clone(), act }),
    }
    if !q.is_open() {
        return Err(Refused::NotOpen(q.state_raw.clone()));
    }
    let waiting = q.waiting.clone().ok_or(Refused::NobodyWaiting)?;

    // The task log FIRST. Everything after this point is best-effort delivery;
    // this is the part that is not.
    let landed = match homes_to(store, &q).and_then(|id| store.task(&id)) {
        Some(mut t) => {
            t.log(&format!("{} by {}: {text}", act.as_str(), by.byline()));
            t.touch();
            match store.save_task(&t) {
                Ok(()) => Landed::Task(t.id.clone()),
                Err(e) => Landed::RecordOnly(format!("could not write {}: {e}", t.id)),
            }
        }
        // Both roads are named, because "it went nowhere" is a sentence a
        // receipt has to be able to say specifically. The asker's task is gone
        // *and* the subject is not a task either — so the words are on the
        // message record and nowhere else, which is a reading and not a drop.
        None => Landed::RecordOnly(match (waiting.task.as_str(), q.about.task()) {
            ("", None) => "the asker holds no task and the question names none".into(),
            ("", Some(a)) => format!("the asker holds no task and there is no task {a}"),
            (w, None) => format!("no task {w} to write it on"),
            (w, Some(a)) => format!("neither {w} nor {a} is a task to write it on"),
        }),
    };

    let question = hop(store, id, by, act, text)?;

    let mut reply = Message::new(by.clone(), Kind::Direction, text);
    reply.about = question.about.clone();
    reply.reply_to = Some(question.id.clone());
    reply.waiting = Some(waiting);
    raise(store, &reply)?;

    Ok(Closed { question, reply, landed })
}

/// Every open question, with who is waiting on each.
///
/// The join `robustness-051` asked for: a caller adds *is that pane stopped*
/// and *how long has it been* and has `quiet_note`'s conjunction with a better
/// subject line — it can say **what** the agent is waiting for. A question
/// nobody has answered for an hour is exactly the class of thing wsp knows and
/// tells nobody.
pub fn open_questions(store: &Store) -> Vec<Message> {
    let mut out: Vec<Message> = store
        .messages()
        .into_values()
        .filter(|m| m.shape() == Some(Shape::Question) && m.is_open())
        .collect();
    out.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.id.cmp(&b.id)));
    out
}

/// The answers waiting for one asker, oldest first.
///
/// Reads by `reply_to` rather than by address, because the answer is addressed
/// to whoever *asked* and not to the level that answered — the link the
/// `worklist-004` incident did not have.
pub fn replies_for(store: &Store, task: &str) -> Vec<Message> {
    let mut out: Vec<Message> = store
        .messages()
        .into_values()
        .filter(|m| m.reply_to.is_some())
        .filter(|m| m.waiting.as_ref().is_some_and(|w| w.task == task))
        .collect();
    out.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.id.cmp(&b.id)));
    out
}

/// Everything raised about one task, oldest first.
///
/// `worklist-017` is the reason this returns a list: a flag is keyed by task id
/// and a second flag on the same task silently replaces the first. A message is
/// keyed by its own id, so two are two.
pub fn about_task(store: &Store, task: &str) -> Vec<Message> {
    let mut out: Vec<Message> = store
        .messages()
        .into_values()
        .filter(|m| m.about.task() == Some(task))
        .collect();
    out.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.id.cmp(&b.id)));
    out
}

/// Every stored message still wanting somebody, oldest first.
///
/// **This is what `Store::flags` was**, and the substitution is the whole of
/// `worklist-017`: a raised hand used to be a row in `flags.json` keyed by the
/// task it was about, so a second hand on one task silently replaced the first
/// — the mechanism whose entire job is to not be lost, losing the request for
/// attention itself. A message is keyed by its own id, so two are two, and
/// there is nothing here to overwrite.
///
/// [`Message::needs_attention`] and not [`Message::is_open`], because the
/// question a surface is asking here is *should somebody look at this* and not
/// *may I act on this*: a record whose state this build cannot parse still
/// wants a person, and answering `false` for it would be this task's own fault
/// arriving through the module built to remove it.
///
/// Only notifications and questions can be in here — a level is never stored
/// ([`raise`] and [`Store::save_message`] both refuse one) — so this is exactly
/// the set the flags section used to draw and nothing wider.
///
/// Oldest first, like [`all`] and [`about_task`]. A surface that wants the
/// newest interruption at the top reverses it, because which end of a queue an
/// interruption is read from is a decision about a panel and not about a
/// record.
pub fn raised(store: &Store) -> Vec<Message> {
    let mut out: Vec<Message> =
        store.messages().into_values().filter(Message::needs_attention).collect();
    out.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.id.cmp(&b.id)));
    out
}

/// What one name resolves to, when a person or a panel says *that one*.
///
/// The reason this is three answers and not an [`Option`] is `worklist-017`
/// again, from the reading side. Under `flags.json` a task id named at most one
/// raised hand, so every verb that took one — `wsp flag --clear <id>`,
/// `--seen`, the panel's `x` — could take a task id and be certain. Under the
/// record a task can carry several, and a verb that quietly picked one would be
/// the same fault as the store that quietly replaced one: an act on somebody's
/// raised hand that nobody was told about.
///
/// So the ambiguity gets a name and comes back to the caller, which is the
/// cheapest of the three shapes this task's overview offered — *one per task,
/// but say so* — arriving at the one place where the ambiguity is real.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hand {
    /// Exactly one, and this is it.
    One(Box<Message>),
    /// Several hands are up about that task. Oldest first, and the caller has
    /// to say which — printing them is the answer, not picking one.
    Several(Vec<Message>),
    /// Nothing raised under that name.
    Nothing,
}

/// Find the raised hand a name means: a message id, or a task with one up.
///
/// A message id is answered directly and in whatever state it is in, because a
/// caller holding an id got it from a surface that was looking at that record
/// and means that record. A task id is answered out of [`raised`] only, because
/// a hand that has been dealt with is not a hand a task still names.
///
/// [`is_message_id`] is what separates the two and it can never be wrong in
/// either direction — that is the guarantee `new_id` exists to make.
pub fn hand(store: &Store, needle: &str) -> Hand {
    if is_message_id(needle) {
        return match store.message(needle) {
            Some(m) => Hand::One(Box::new(m)),
            None => Hand::Nothing,
        };
    }
    let mut up: Vec<Message> =
        raised(store).into_iter().filter(|m| m.about.task() == Some(needle)).collect();
    match up.len() {
        0 => Hand::Nothing,
        1 => Hand::One(Box::new(up.remove(0))),
        _ => Hand::Several(up),
    }
}

/// The one text field, out of the three a raised hand used to be written in.
///
/// `worklist-018` is why this is a function rather than three fields joined at
/// each call site: a flag raised with a body and no title drew as **nothing**,
/// because the field the card drew was the empty one and no record forbade
/// that. [`Message::title`] is `lines().next()`, so the same hole exists here
/// the moment a `text` is allowed to begin with a blank line — and the repair
/// belongs at the writer, where there is one of it, rather than at every
/// surface that draws.
///
/// The order is the order the row already read them in: `render_row` drew
/// `said` and fell back to `title`, so `said` is the headline in practice and
/// the other two are what goes under it. Empties drop out, which is what makes
/// the first line non-empty whenever anything at all was given.
///
/// Everything given is kept. A refusal would lose the message, and the message
/// was the point — that is `worklist-018`'s third recommendation and it is
/// right.
pub fn compose(said: &str, title: &str, body: &str) -> String {
    [said.trim(), title.trim(), body.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Carry any hand still up in `flags.json` across into the record, once.
///
/// **The migration has to exist, and this task is why.** A raised hand is
/// machine-local state and meaningless a week later, so the tempting answer is
/// to let the old file go — but the moment `wsp flag` stops writing it, every
/// hand the *installed* binary raised stops being drawn, and the installed
/// binary is routinely not the tree (`cmd_install::health` exists for exactly
/// that). Dropping them would be `worklist-017` committed by the change that
/// fixes it: a request for attention lost, with success reported.
///
/// Idempotent by emptying the file it read, and safe to call from anywhere for
/// that reason. Under the state lock so that a `wsp flag` running in another
/// pane cannot read the same entry and adopt it twice.
///
/// `at` and `seen` ride across, because when a hand went up and whether
/// somebody has already looked at it are the two facts a person acts on and
/// resetting either would be a lie about the past.
///
/// Returns what it adopted, so a caller can say so. Nothing to say is the
/// overwhelmingly common case, which is why every caller of this is somewhere
/// a person is already being spoken to.
pub fn adopt_legacy_flags(store: &Store) -> Vec<Message> {
    store.locked(|| {
        let legacy = store.flags();
        if legacy.is_empty() {
            return Vec::new();
        }
        let mut adopted = Vec::new();
        for (task, f) in legacy {
            let field = |k: &str| {
                f.get(k).and_then(Value::as_str).unwrap_or_default().trim().to_string()
            };
            let (pane, workspace) = (field("pane"), field("workspace"));
            let from = match pane.is_empty() {
                true => Party::Human,
                false => Party::pane(&pane, &workspace),
            };
            let mut m = Message::new(
                from,
                Kind::Note,
                &compose(&field("said"), &field("title"), &field("body")),
            );
            m.about = About::Task(task.clone());
            m.seen = f.get("seen").and_then(Value::as_bool).unwrap_or(false);
            let at = field("at");
            if !at.is_empty() {
                m.at = at;
            }
            // An ask is a question, and a question has to name who is sitting
            // still. The old record named a pane and the CLI required one, so
            // this is the ordinary case — but a hand-edited file could carry an
            // ask with nobody behind it, and a question `raise` would refuse is
            // a hand that would be silently dropped here. It keeps its ask and
            // stays the shape that owes nothing back.
            if let Some(Ask::Claim) = Ask::parse(&field("ask")) {
                m.set_ask(Ask::Claim);
                if !pane.is_empty() {
                    m.set_shape(Shape::Question);
                    m.waiting = Some(Waiting::new(&pane, ""));
                }
            }
            if raise(store, &m).is_ok() {
                adopted.push(m);
            }
        }
        // Only after every one of them is in the record. A crash between the
        // two leaves a hand in both places, which a panel draws twice; a crash
        // the other way round leaves it in neither.
        for (task, _) in store.flags() {
            store.clear_flag(&task);
        }
        adopted
    })
}

/// Every stored message, newest last. A convenience over [`Store::messages`]
/// for the surfaces that draw a list rather than look one up.
pub fn all(store: &Store) -> Vec<Message> {
    let mut out: Vec<Message> = store.messages().into_values().collect();
    out.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.id.cmp(&b.id)));
    out
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Task;

    /// The seat every one of these tests answers from.
    fn seat() -> Party {
        Party::seat("worklist")
    }

    fn scratch(tag: &str) -> Store {
        let root = std::env::temp_dir().join(format!("wsp-msg-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = Store::at(root.clone(), root.join("state"));
        store.ensure_dirs().unwrap();
        store
    }

    fn asker(store: &Store, id: &str) -> Task {
        let mut t = Task::new("the work", id);
        t.project = Some("worklist".into());
        t.body = String::from("## Overview\nthe brief\n\n## Log\n- 2026-08-19 claimed\n");
        store.save_task(&t).unwrap();
        t
    }

    fn a_question(store: &Store, task: &str) -> Message {
        let m = Message::question(
            Party::pane("w4J:p1", "wsp-worklist-004"),
            Kind::Note,
            "move story.rs to ui-002, or restore it to HEAD?",
            Waiting::new("w4J:p1", task),
        )
        .about(About::Task(task.to_string()));
        raise(store, &m).expect("a question with a waiting party is storable");
        m
    }

    /// The single most important rule in Part 6, and it is a real failure from
    /// 2026-08-19 rather than a principle: `worklist-004` raised a flag saying
    /// *"stopped … flagged and awaiting direction"* — a question with a blocked
    /// asker. The seat answered it down `wsp tell`, a different mechanism with
    /// nothing linking the two, and then cleared the flag. **Clearing looked
    /// like answering.** To every surface in the system the matter was closed;
    /// the asker learned nothing and would have sat waiting for ever.
    ///
    /// So the table is asserted whole rather than the one cell: a rule that is
    /// only true for the case somebody remembered is the rule that let this
    /// through.
    #[test]
    fn a_question_may_be_answered_or_abandoned_and_never_merely_cleared() {
        use Act::*;
        let table = [
            (Shape::Notification, Acknowledged, true),
            (Shape::Notification, Noted, true),
            (Shape::Notification, Escalated, true),
            (Shape::Notification, Answered, false),
            (Shape::Notification, Abandoned, false),
            (Shape::Question, Answered, true),
            (Shape::Question, Abandoned, true),
            (Shape::Question, Noted, true),
            (Shape::Question, Escalated, true),
            // Acknowledging a question is the whole failure: it is one
            // keystroke, it says nothing, and it leaves somebody waiting.
            (Shape::Question, Acknowledged, false),
            (Shape::Signal, Noted, false),
            (Shape::Signal, Escalated, false),
            (Shape::Signal, Acknowledged, false),
            (Shape::Signal, Answered, false),
            (Shape::Signal, Abandoned, false),
        ];
        for (shape, act, want) in table {
            assert_eq!(
                shape.may(act),
                want,
                "{} may be {}: expected {want}",
                shape.as_str(),
                act.as_str()
            );
        }
    }

    /// Neither ending is a keystroke. `wsp worklist go` already declines to
    /// pass a barrier until it is given a sentence, and *what wsp contributes
    /// is the obligation to write one, not the judgement.*
    #[test]
    fn ending_a_question_takes_a_sentence_and_not_a_keystroke() {
        let store = scratch("sentence");
        asker(&store, "worklist-004");
        let q = a_question(&store, "worklist-004");

        assert_eq!(answer(&store, &q.id, &seat(), "   ").unwrap_err(), Refused::NeedsASentence);
        assert_eq!(abandon(&store, &q.id, &seat(), "").unwrap_err(), Refused::NeedsASentence);
        assert!(
            store.message(&q.id).unwrap().is_open(),
            "a refused disposition must leave the question exactly where it was"
        );
        assert_eq!(
            acknowledge(&store, &q.id, &seat()).unwrap_err(),
            Refused::WrongShape { shape: "question".into(), act: Act::Acknowledged },
            "one keystroke must not close a question, whatever it is called"
        );
    }

    /// Part 7's load-bearing decision: **the answer is appended to the asker's
    /// task log first and delivered second**, because the delivery is
    /// best-effort and the record is not.
    ///
    /// Three things are asserted together because they are one property: the
    /// words are on the task, the reply names the question it closes, and the
    /// reply is addressed to whoever *asked* rather than to the level that
    /// answered.
    #[test]
    fn an_answer_lands_on_the_askers_task_before_it_is_anybodys_to_deliver() {
        let store = scratch("answer");
        asker(&store, "worklist-004");
        let q = a_question(&store, "worklist-004");

        let closed = answer(
            &store,
            &q.id,
            &seat(),
            "move it — restoring would destroy ui-002's fixture",
        )
        .expect("an open question with a sentence is answerable");

        assert_eq!(closed.landed, Landed::Task("worklist-004".into()));
        let back = store.task("worklist-004").unwrap();
        assert!(
            back.body.contains("answered by worklist seat: move it"),
            "the answer is not on the task: {}",
            back.body
        );
        assert_eq!(closed.reply.reply_to.as_deref(), Some(q.id.as_str()));
        assert_eq!(closed.reply.waiting.unwrap().pane, "w4J:p1");
        assert_eq!(closed.reply.from, seat(), "an answer carries who answered it");
        assert_eq!(closed.question.state(), Some(State::Answered));
        assert_eq!(
            closed.question.via.last().map(|h| h.act),
            Some(Act::Answered),
            "who ended it is a fact about the past and has to be written",
        );
    }

    /// Ed's fifth bullet: *a despawned agent's question is either dropped or
    /// becomes a note on the task, and dropping it silently is the same failure
    /// as clearing a flag.* There is no drop case here because the write does
    /// not depend on the asker being alive — the pane in `waiting` may be long
    /// gone and the task cannot be.
    #[test]
    fn the_asker_being_gone_is_not_a_drop_case() {
        let store = scratch("gone");
        asker(&store, "worklist-004");
        let mut q = Message::question(
            Party::pane("w4J:p1", "wsp-worklist-004"),
            Kind::Note,
            "which of these two?",
            // The pane has gone; the task has not.
            Waiting { pane: String::new(), task: "worklist-004".into() },
        );
        q.about = About::Task("worklist-004".into());
        raise(&store, &q).unwrap();

        let closed = answer(&store, &q.id, &seat(), "the second one").unwrap();
        assert_eq!(closed.landed, Landed::Task("worklist-004".into()));
        assert!(store.task("worklist-004").unwrap().body.contains("the second one"));
    }

    /// And when even the task is gone, the receipt says where the words went
    /// instead of reporting a success that happened nowhere. A verb whose whole
    /// effect is elsewhere has to name where it went.
    #[test]
    fn a_question_whose_task_is_gone_reports_where_the_answer_went() {
        let store = scratch("no-task");
        let q = a_question(&store, "worklist-999");
        let closed = answer(&store, &q.id, &seat(), "yes").unwrap();
        match closed.landed {
            Landed::RecordOnly(why) => assert!(why.contains("worklist-999"), "{why}"),
            other => panic!("a task that does not exist reported {other:?}"),
        }
        assert_eq!(closed.question.state(), Some(State::Answered), "the question still closed");
    }

    /// A level is never queued. `robustness-088`'s named failure is *a panel
    /// full of flags nobody reads, which is worse than the silence it
    /// replaces*, and a stored signal is exactly that: a fact that has stopped
    /// being true, drawn until somebody clears it by hand.
    ///
    /// The guard is asserted at the door as well as in the lifecycle, because a
    /// consumer reaching for the store directly must meet it too.
    #[test]
    fn a_derived_signal_is_never_written_down() {
        let store = scratch("signal");
        let s = Message::signal(Kind::Note, "stopped on a prompt only a person can answer");
        assert_eq!(raise(&store, &s).unwrap_err(), Refused::IsALevel);
        assert_eq!(store.save_message(&s).unwrap_err(), Refused::IsALevel);
        assert!(store.messages().is_empty(), "a level reached the store anyway");
        assert!(!s.owes_a_disposition(), "a level is nobody's to dispose of");
    }

    /// `open ──escalated──▶ open (at the next level)`. Escalation is a hop and
    /// not a disposition: the addressee is derived from the chain and the chain
    /// has one more level in it, and nothing about the question changed.
    ///
    /// What the hops buy is the difference between *nobody looked at this* and
    /// *two governors looked, wrote what they thought, and could not answer
    /// it*. Those deserve very different amounts of a person's attention and
    /// today they are indistinguishable.
    #[test]
    fn an_escalation_leaves_it_open_and_carries_everyone_who_looked() {
        let store = scratch("escalate");
        asker(&store, "worklist-004");
        let q = a_question(&store, "worklist-004");

        note(&store, &q.id, &seat(), "I think it is the fixture, not sure").unwrap();
        let up = escalate(&store, &q.id, &seat(), "cannot answer this without ui-002").unwrap();

        assert!(up.is_open(), "an escalation must not close what it passes on");
        assert_eq!(up.via.len(), 2);
        assert_eq!(up.via[0].by, seat());
        assert_eq!(up.via[1].act, Act::Escalated);
        assert!(up.via[1].note.contains("ui-002"), "the note has to travel with the hop");

        let closed = answer(&store, &q.id, &Party::seat("wsp"), "move it").unwrap();
        assert_eq!(closed.question.via.len(), 3, "the answer is the third hop, not a fresh record");
    }

    /// A notification's disposition is a real act — *I have this and I am not
    /// passing it on* — and it is not an answer.
    #[test]
    fn a_notification_is_acknowledged_and_that_is_a_decision() {
        let store = scratch("ack");
        let mut m = Message::new(Party::pane("w4J:p1", "ws"), Kind::Note, "you and I are both in worklist.rs");
        m.about = About::Task("worklist-022".into());
        raise(&store, &m).unwrap();

        assert_eq!(
            answer(&store, &m.id, &seat(), "noted").unwrap_err(),
            Refused::WrongShape { shape: "notification".into(), act: Act::Answered },
            "a notification is not a question and answering one owes nobody anything",
        );
        let done = acknowledge(&store, &m.id, &seat()).unwrap();
        assert_eq!(done.state(), Some(State::Acknowledged));
        assert!(!done.is_open());
        assert_eq!(
            acknowledge(&store, &m.id, &seat()).unwrap_err(),
            Refused::NotOpen("acknowledged".into()),
            "a disposition is taken once",
        );
    }

    /// A question with nobody waiting cannot be answered home and cannot become
    /// a signal — the cost of an open question *is* the party sitting still
    /// while it is open, which is `robustness-051` stated as a field.
    #[test]
    fn a_question_has_to_name_who_is_waiting_on_it() {
        let store = scratch("nobody");
        let mut m = Message::new(Party::pane("w4J:p1", "ws"), Kind::Note, "which of these two?");
        m.set_shape(Shape::Question);
        assert_eq!(raise(&store, &m).unwrap_err(), Refused::NobodyWaiting);
    }

    /// `worklist-010`: `govern --tell` reported failure after delivering,
    /// because the error it surfaced came from a status probe rather than from
    /// the delivery — so a governor retried and may have sent the same
    /// paragraph three times. The id is what makes the retry harmless, and the
    /// case that matters is a retry arriving *after* somebody dealt with it.
    #[test]
    fn a_resend_is_a_no_op_and_never_reopens_what_somebody_closed() {
        let store = scratch("resend");
        let m = Message::new(Party::seat("wsp"), Kind::Direction, "your work is restored, carry on");
        raise(&store, &m).unwrap();
        acknowledge(&store, &m.id, &seat()).unwrap();

        raise(&store, &m).expect("a retry is not an error");
        let back = store.message(&m.id).unwrap();
        assert_eq!(
            back.state(),
            Some(State::Acknowledged),
            "the retry reopened a message somebody had already dealt with",
        );
        assert_eq!(all(&store).len(), 1, "the retry wrote a second copy");
    }

    /// `worklist-017`: `flags.json` is keyed by task id, so a second flag on
    /// one task silently replaces the first. A message is keyed by its own id,
    /// so two are two.
    #[test]
    fn a_second_message_about_one_task_does_not_replace_the_first() {
        let store = scratch("two");
        for text in ["the index is behind HEAD", "and the tree is shared"] {
            let mut m = Message::new(Party::pane("w4J:p1", "ws"), Kind::Note, text);
            m.about = About::Task("worklist-011".into());
            raise(&store, &m).unwrap();
        }
        let both = about_task(&store, "worklist-011");
        assert_eq!(both.len(), 2, "the second raised hand replaced the first");
        assert_ne!(both[0].id, both[1].id);
    }

    /// One text and one first line, which is the invariant the writer keeps so
    /// that no surface has to.
    ///
    /// `worklist-018`: a hand raised with a body and no title drew as nothing.
    /// `Message::title` is `lines().next()`, so the same hole exists here the
    /// moment a `text` is allowed to begin blank — and the case that matters is
    /// the one that used to fail, a paragraph with nothing said above it.
    #[test]
    fn whatever_was_given_the_first_line_is_never_blank() {
        let cases = [
            ("said", "title", "body"),
            ("", "title", "body"),
            ("", "", "body"),
            ("said", "", ""),
            ("", "", "  \n\n  body  "),
        ];
        for (said, title, body) in cases {
            let text = compose(said, title, body);
            let m = Message::new(Party::seat("wsp"), Kind::Note, &text);
            assert!(!m.title().is_empty(), "{said:?}/{title:?}/{body:?} has no headline");
        }
        // Everything is kept, and in the order a row already read them in: a
        // refusal would lose the message, and the message was the point.
        assert_eq!(compose("said", "title", "body"), "said\n\ntitle\n\nbody");
        // And the one case that is honestly empty — `wsp flag <id>` on its own,
        // "look at this task, it exists" — stays empty, because a surface
        // falling back to the task is right and a writer inventing words is
        // not.
        assert_eq!(compose("", "", ""), "");
    }

    /// A name resolves to *the hand meant*, or to a question, and never to a
    /// guess.
    ///
    /// The reading half of `worklist-017`. Under `flags.json` a task named at
    /// most one raised hand and every verb could take a task id and be certain
    /// — certain because the store was losing the second. With both kept, a
    /// verb that quietly picked one would be the same fault wearing the other
    /// hat.
    #[test]
    fn a_task_names_one_raised_hand_until_it_names_two() {
        let store = scratch("hand");
        let one = |text: &str| {
            let mut m = Message::new(Party::pane("w4J:p1", "ws"), Kind::Note, text);
            m.about = About::Task("worklist-011".into());
            raise(&store, &m).unwrap();
            m
        };
        assert_eq!(hand(&store, "worklist-011"), Hand::Nothing);

        let first = one("the index is behind HEAD");
        assert_eq!(hand(&store, "worklist-011"), Hand::One(Box::new(first.clone())));
        // And by its own id, which is the name that never becomes ambiguous.
        assert_eq!(hand(&store, &first.id), Hand::One(Box::new(first.clone())));

        one("and the tree is shared");
        match hand(&store, "worklist-011") {
            Hand::Several(up) => assert_eq!(up.len(), 2, "one of the two was picked silently"),
            other => panic!("two hands read as {other:?}"),
        }

        // A hand that has been dealt with is not one its task still names — but
        // its own id still finds it, because a caller holding one got it from a
        // surface that was looking at that record.
        acknowledge(&store, &first.id, &seat()).unwrap();
        assert!(matches!(hand(&store, "worklist-011"), Hand::One(_)));
        assert!(matches!(hand(&store, &first.id), Hand::One(_)));
        assert_eq!(hand(&store, "m-nothing-p1"), Hand::Nothing);
    }

    /// The migration, and it exists because the alternative is this task's own
    /// fault committed by the change that fixes it.
    ///
    /// The installed binary is routinely not the tree. On the day `wsp flag`
    /// stops writing `flags.json`, every hand the installed one raised would
    /// stop being drawn — a request for attention lost, with success reported
    /// everywhere. So the old file is drained into the record once, and `at`
    /// and `seen` ride across: when a hand went up and whether somebody has
    /// already looked at it are the two facts a person acts on.
    #[test]
    fn a_hand_raised_by_the_old_binary_is_carried_across_and_not_dropped() {
        let store = scratch("adopt");
        store.set_legacy_flag(
            "worklist-011",
            json!({
                "said": "the index is behind HEAD",
                "title": "",
                "body": "and the tree is shared",
                "ask": "claim",
                "pane": "w4J:p1",
                "workspace": "wsp-worklist-011",
                "at": "2026-08-19T09:12:00Z",
                "seen": true,
            }),
        );

        let carried = adopt_legacy_flags(&store);
        assert_eq!(carried.len(), 1);
        let m = &about_task(&store, "worklist-011")[0];
        assert_eq!(m.title(), "the index is behind HEAD");
        assert_eq!(m.body(), "and the tree is shared");
        assert_eq!(m.at, "2026-08-19T09:12:00Z", "the age it is drawn by was reset");
        assert!(m.seen, "a hand somebody had already read came back as unread");
        assert_eq!(m.from, Party::pane("w4J:p1", "wsp-worklist-011"));
        assert_eq!(m.ask(), Some(Ask::Claim));
        assert_eq!(m.shape(), Some(Shape::Question), "an ask is a question under the record");

        // Once. A second pass over an emptied file is not a second copy of
        // anybody's raised hand.
        assert!(store.flags().is_empty(), "the old file was left holding the same hand");
        assert!(adopt_legacy_flags(&store).is_empty());
        assert_eq!(about_task(&store, "worklist-011").len(), 1);
    }

    /// The renumbering hazard, from the record's side. `Store::rename_tasks`
    /// rewrites state files by matching whole tokens against a map keyed by old
    /// task ids, so an id that *looks* like a task id is an id a `wsp mv` can
    /// rewrite — and the identity is what an answer uses to find its way home.
    #[test]
    fn a_message_id_can_never_be_mistaken_for_a_task_id() {
        let id = new_id();
        assert!(is_message_id(&id), "{id}");
        let last = id.rsplit('-').next().unwrap();
        assert!(
            !last.chars().all(|c| c.is_ascii_digit()),
            "{id} ends in digits, which is the shape of every task id there has ever been",
        );
        for task in ["worklist-022", "t-260815-014", "data-002", "m-001"] {
            assert!(!is_message_id(task), "{task} reads as a message id");
        }
        let (a, b) = (new_id(), new_id());
        assert_ne!(a, b, "two messages in one process must not share an identity");
    }

    /// The installed binary is routinely not the tree — `cmd_install::health`
    /// exists for that — and every write here is a read-modify-write of a
    /// shared map. So an older build must show a word it does not know and
    /// refuse to act on it, rather than launder it into a word it does.
    #[test]
    fn a_word_this_build_does_not_know_fails_closed_and_survives_a_rewrite() {
        let store = scratch("unknown");
        let raw = json!({
            "id": "m-zz-p1",
            "shape": "reminder",
            "kind": "shout",
            "state": "withdrawn",
            "from": "some-newer-wsp",
            "text": "from a build that came after this one",
        });
        let m = Message::from_json("m-zz-p1", &raw);

        assert_eq!(m.shape(), None);
        assert_eq!(m.kind(), None);
        assert_eq!(m.state(), None);
        assert!(!m.is_open(), "an unparseable state must not read as open");
        assert!(!m.owes_a_disposition());

        store.save_message(&m).unwrap();
        let back = store.message("m-zz-p1").unwrap();
        assert_eq!(back.shape_raw, "reminder", "the word was laundered on the way through");
        assert_eq!(back.kind_raw, "shout");
        assert_eq!(back.state_raw, "withdrawn");

        assert_eq!(
            acknowledge(&store, "m-zz-p1", &seat()).unwrap_err(),
            Refused::WrongShape { shape: "reminder".into(), act: Act::Acknowledged },
        );
    }

    /// A record whose shape did not survive must not become a question
    /// somebody is waiting on: the default is the quiet shape, the one that
    /// owes nothing back.
    #[test]
    fn the_default_shape_is_the_one_that_commits_nobody() {
        let m = Message::from_json("m-1-p1", &json!({ "text": "hello" }));
        assert_eq!(m.shape(), Some(Shape::Notification));
        assert_eq!(Message::new(Party::pane("w4J:p1", "ws"), Kind::Note, "hi").shape(), Some(Shape::Notification));
        assert_eq!(
            Message::new(Party::pane("w4J:p1", "ws"), Kind::Note, "hi").kind().unwrap().may_interrupt(),
            false,
            "only stop may reach a running turn, and it has to be typed",
        );
        assert!(Kind::Stop.may_interrupt());
    }

    /// Forgetting an open question is the `worklist-004` failure with a
    /// different verb on it: the hand goes down, every surface reads the matter
    /// as closed, and the asker learns nothing.
    #[test]
    fn an_open_question_may_not_simply_be_forgotten() {
        let store = scratch("forget");
        asker(&store, "worklist-004");
        let q = a_question(&store, "worklist-004");

        assert_eq!(store.forget_message(&q.id).unwrap_err(), Refused::StillOpen);
        abandon(&store, &q.id, &seat(), "ui-002 answered it directly").unwrap();
        assert!(store.forget_message(&q.id).unwrap(), "a closed question is prunable");

        let t = store.task("worklist-004").unwrap();
        assert!(
            t.body.contains("abandoned by worklist seat: ui-002 answered it directly"),
            "abandoning has to reach the asker too, or it is a clear with a nicer name: {}",
            t.body
        );
    }

    /// The join `robustness-051` asked for, in the shape a caller needs it:
    /// every open question with the party sitting still on each. A caller adds
    /// *is that pane stopped* and *how long has it been* and has `quiet_note`'s
    /// conjunction with a subject line that can say **what** is being waited on.
    #[test]
    fn the_open_questions_carry_who_is_sitting_still() {
        let store = scratch("open-q");
        asker(&store, "worklist-004");
        let q = a_question(&store, "worklist-004");
        let mut n = Message::new(Party::pane("w4J:p1", "ws"), Kind::Fyi, "a notification owes nobody a wait");
        n.about = About::Task("worklist-004".into());
        raise(&store, &n).unwrap();

        let open = open_questions(&store);
        assert_eq!(open.len(), 1, "a notification is not somebody waiting");
        assert_eq!(open[0].waiting.as_ref().unwrap().pane, "w4J:p1");

        answer(&store, &q.id, &seat(), "move it").unwrap();
        assert!(open_questions(&store).is_empty(), "an answered question is nobody's wait");
        assert_eq!(
            replies_for(&store, "worklist-004").len(),
            1,
            "the answer is addressed to whoever asked, not to whoever answered",
        );
    }

    /// `worklist-018`: a flag raised with `--body` and no `--title` shows
    /// nothing at the card, because the field the card drew was the empty one.
    /// One field with a first line has no such state.
    #[test]
    fn there_is_always_a_headline_to_draw() {
        let one = Message::new(Party::Human, Kind::Note, "both in worklist.rs");
        assert_eq!(one.title(), "both in worklist.rs");
        assert_eq!(one.body(), "", "a one-line message has no detail and that is not a fault");

        let long = Message::new(
            Party::Human,
            Kind::Note,
            "the index is behind HEAD\n\n4,962 lines, and a plain commit would drop them",
        );
        assert_eq!(long.title(), "the index is behind HEAD");
        assert_eq!(long.body(), "4,962 lines, and a plain commit would drop them");

        // The failing shape: detail and no headline. There is no way to write
        // it, because the detail's own first line is the headline.
        let body_only = Message::new(Party::Human, Kind::Note, "\nonly a paragraph");
        assert_eq!(body_only.title(), "", "an empty first line is the writer's, not the record's");
        assert!(!body_only.text.is_empty(), "and the words are still all there to draw");
    }

    /// `source` says what `x` may do — clear a hand a person raised, `seen` a
    /// derived one — and it is a function of the two fields it used to sit
    /// beside, so it is computed rather than stored. There is no way to write a
    /// record that says `signal` and `agent` at once.
    #[test]
    fn who_put_it_here_is_read_off_the_shape_and_the_sender() {
        assert_eq!(Message::signal(Kind::Note, "stalled").source(), Source::Wsp);
        assert_eq!(Message::new(Party::Human, Kind::Stop, "stop").source(), Source::Human);
        assert_eq!(Message::new(Party::pane("w4J:p1", "ws"), Kind::Note, "hi").source(), Source::Agent);
        assert_eq!(Message::new(Party::seat("wsp"), Kind::Note, "hi").source(), Source::Agent);
        // A shape this build cannot parse is not a level, so it is not wsp's.
        let odd = Message::from_json("m-1-p1", &json!({ "shape": "reminder", "from": {"human": true} }));
        assert_eq!(odd.source(), Source::Human);
    }

    /// The byline `robustness-094` says is missing everywhere. `worklist-006`
    /// credited a governor's review finding to Ed, and the task's log now
    /// records a decision as reviewed by Ed when a governor reviewed it.
    #[test]
    fn every_message_says_who_sent_it_and_an_unsigned_one_reads_as_a_person() {
        assert_eq!(Party::pane("w4J:p1", "wsp-022").byline(), "w4J:p1");
        assert_eq!(Party::seat("worklist").byline(), "worklist seat");
        assert_eq!(Party::Human.byline(), "a person");
        assert_eq!(Party::pane("w4J:p1", "wsp-022").workspace(), Some("wsp-022"));
        assert_eq!(Party::seat("worklist").workspace(), None);

        // Unattributed is a person — the correct reading of anything arriving
        // without an envelope, because the one channel wsp does not control is
        // a human keyboard.
        let unsigned = Message::from_json("m-1-p1", &json!({ "text": "carry on" }));
        assert_eq!(unsigned.from, Party::Human);
    }

    /// Everything on the envelope survives the round trip, including the parts
    /// three consumers will type against.
    #[test]
    fn the_envelope_round_trips_whole() {
        let mut m = Message::question(
            Party::pane("w4J:p1", "wsp-worklist-004"),
            Kind::Stop,
            "do not run git stash pop",
            Waiting::new("w4J:p1", "worklist-011"),
        );
        m.set_ask(Ask::Claim);
        m.about = About::Group { scope: "phase-two".into(), group: 1 };
        m.seen = true;
        m.reply_to = Some("m-earlier-p1".into());
        m.via.push(Hop {
            at: "2026-08-19T16:44:02Z".into(),
            by: Party::seat("worklist"),
            act: Act::Escalated,
            note: "cannot answer this".into(),
        });

        let back = Message::from_json(&m.id, &m.to_json());
        assert_eq!(back, m);
        assert_eq!(back.about, About::Group { scope: "phase-two".into(), group: 1 });
        assert_eq!(back.ask(), Some(Ask::Claim));
        assert_eq!(back.source(), Source::Agent, "a pane's message is an agent's");
        assert_eq!(back.kind(), Some(Kind::Stop));
    }

    /// The `wsp` seat's objection to `is_open()`, and the decision the worklist
    /// custodian took on it, 2026-08-19: **an unreadable record needs a person,
    /// and it gets its own answer rather than being folded into a state that
    /// means something else.**
    ///
    /// `is_open()` is fail-closed for *action* and fail-silent for *attention*,
    /// and attention is what this phase exists for. An older binary meeting a
    /// state a newer one wrote does not merely refuse to act: with only
    /// `is_open()` it reports that nothing is standing, so a question that
    /// cannot be parsed raises nothing — `robustness-051` arriving through the
    /// module built to remove it. The premise is not hypothetical: the
    /// installed binary was not the tree for the whole of that day.
    ///
    /// Both readings are asserted together on one record, because the value of
    /// the split is that the two answers **differ**, and a test that checked
    /// only the new one would pass just as well if `is_open()` had been widened
    /// instead — which is the repair that was refused.
    #[test]
    fn a_state_this_build_cannot_read_fetches_a_person_and_still_refuses_to_act() {
        let mut newer = Message::new(seat(), Kind::Note, "the group is stalled");
        newer.state_raw = "deferred".into();
        assert_eq!(newer.state(), None, "the premise: a word this build does not know");
        assert!(!newer.is_open(), "the lifecycle answer stays honest: I do not know that this is open");
        assert!(!newer.owes_a_disposition(), "and nothing acts on a state it has not understood");
        assert!(
            newer.needs_attention(),
            "\"I do not understand this\" is a reason to fetch somebody, not a reason to say nothing",
        );

        // An unreadable *shape* is the same fault one field over, and it is
        // caught by the same predicate rather than by a second clause.
        let mut odd = Message::new(seat(), Kind::Note, "a shape from a later build");
        odd.shape_raw = "proposal".into();
        assert!(!odd.owes_a_disposition(), "nothing can say whose disposition an unknown shape owes");
        assert!(odd.needs_attention(), "…and it is still standing, so somebody must look");

        // And the ordinary two are unchanged, which is what makes the new
        // predicate an addition rather than a widening.
        assert!(Message::new(seat(), Kind::Note, "open").needs_attention());
        let mut done = Message::new(seat(), Kind::Note, "dealt with");
        done.state_raw = State::Acknowledged.as_str().into();
        assert!(!done.needs_attention(), "a record somebody disposed of is not standing");
    }

    /// **The failure `worklist-014` exists for**: for the whole of 2026-08-19 a
    /// person carried messages between two governor seats, because a reply from
    /// a seat has nowhere to live. A seat holds no task, so an answer routed
    /// only by `waiting.task` lands nowhere and the words stay in a pane that
    /// closes.
    ///
    /// So the subject is the fallback home, and the rule is the same one stated
    /// twice: *put the answer where the next reader will look for it.*
    #[test]
    fn a_seat_holding_no_task_is_answered_onto_the_task_it_asked_about() {
        let store = scratch("seat-home");
        asker(&store, "wsp-095");
        let q = Message::question(
            Party::seat("worklist"),
            Kind::Note,
            "does the barrier reading hold at a branch cut from the trunk tip?",
            Waiting::new("w3R:p1", ""),
        )
        .about(About::Task("wsp-095".into()));
        raise(&store, &q).expect("a seat's question names the pane that is waiting");

        assert_eq!(
            homes_to(&store, &q).as_deref(),
            Some("wsp-095"),
            "a seat's answer lives on the task the question was about, or nowhere",
        );
        let closed = answer(
            &store,
            &q.id,
            &Party::seat("wsp"),
            "it holds — cmd_checkout::ahead() reads the branch off the tree",
        )
        .expect("a question with a home can be answered");
        assert_eq!(closed.landed, Landed::Task("wsp-095".into()));
        let back = store.task("wsp-095").expect("the subject is still there");
        assert!(
            back.body.contains("answered by wsp seat"),
            "the answer is on the record with a byline, which is the half Ed was carrying by hand",
        );
    }

    /// The fallback is a fallback and never an override, so nothing about an
    /// agent's answer changes. An agent asking about somebody else's task is
    /// still answered on **its own**, because its own is the one it will be
    /// handed again on the next spawn — which is the whole reason the task log
    /// is the delivery mechanism rather than merely the archive.
    #[test]
    fn an_agent_is_answered_on_its_own_task_even_when_it_asked_about_another() {
        let store = scratch("home-order");
        asker(&store, "worklist-014");
        asker(&store, "worklist-020");
        let q = Message::question(
            Party::pane("w4N:p1", "wsp-worklist-014"),
            Kind::Note,
            "may I take worklist-020 as well?",
            Waiting::new("w4N:p1", "worklist-014"),
        )
        .about(About::Task("worklist-020".into()));
        raise(&store, &q).expect("an agent's question names its own task");

        assert_eq!(
            homes_to(&store, &q).as_deref(),
            Some("worklist-014"),
            "the asker's own task wins, because it is the one the asker will read",
        );
        answer(&store, &q.id, &seat(), "yes — nobody else is in it").unwrap();
        assert!(
            store.task("worklist-014").unwrap().body.contains("answered by worklist seat"),
            "on the asker's task",
        );
        assert!(
            !store.task("worklist-020").unwrap().body.contains("answered by"),
            "and not on the subject's, which nobody is waiting on",
        );
    }

    /// Nowhere to land is a **reading and not a drop**, and the receipt names
    /// both roads it did not take. The same repair `cmd_checkout` made twice:
    /// give the unknown case its own answer rather than a silence that looks
    /// like success.
    #[test]
    fn an_answer_with_nowhere_to_land_says_where_it_did_not_go() {
        let store = scratch("no-home");
        let q = Message::question(
            Party::seat("worklist"),
            Kind::Note,
            "are we agreed on the panel/rows.rs protocol?",
            Waiting::new("w3R:p1", ""),
        )
        .about(About::Scope("wsp".into()));
        raise(&store, &q).unwrap();
        assert_eq!(homes_to(&store, &q), None, "a scope is not a place a log line can live");

        let closed = answer(&store, &q.id, &Party::seat("wsp"), "agreed").unwrap();
        match &closed.landed {
            Landed::RecordOnly(why) => assert!(
                why.contains("holds no task") && why.contains("names none"),
                "a receipt that cannot say where the words went is the fault it is reporting: {why}",
            ),
            other => panic!("expected the record-only reading, got {other:?}"),
        }
        assert_eq!(
            closed.reply.reply_to.as_deref(),
            Some(q.id.as_str()),
            "the answer still knows which question it closes, which is how it finds the asker",
        );
    }
}
