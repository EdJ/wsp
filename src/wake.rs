//! Delivering an earned wake to the governor it is addressed to.
//!
//! `core-014` measured what a wake costs — every line that reaches a governor
//! re-invokes it and its whole conversation is re-read, 208k tokens on the seat
//! that filed the row, the same for a heartbeat as for a question from Ed —
//! and `core-017` built the table that decides which lines are worth one. Both
//! of those assumed the wake was somebody else's to deliver: `wsp watch` prints
//! and a consumer's monitor decides. This is the half that corrects it.
//!
//! # The wake is wsp's, and the daemon is what delivers it
//!
//! `core-021` d1 is the argument and it is not repeated here. The shape it
//! settles on: [`crate::attention::tick`] already derives the level set every
//! minute for the whole machine, already keeps a ledger across restarts, and
//! already computes [`Emit::to`] — *per-signal* addressing, which a
//! subscription structurally cannot do. So the wake is a third audience of a
//! pass that is already running, beside the event log a hook reads and the
//! tokens the sidebar draws.
//!
//! What arrives here is therefore `news` and only `news`. Four of
//! [`crate::cmd_watch::Class`]'s five words are a *stream* telling a reader who
//! is watching it that it is alive, and a told wake has no stream: liveness is
//! the register's job (`core-014` d2), and `replaced` cannot arise because the
//! daemon `exec`s itself when a build lands on its path.
//!
//! # The spool is written before anything is attempted
//!
//! And that is the one place this path is deliberately *unlike*
//! [`crate::cmd_watch::run`]. There, a line is offered to a [`Stream`] which
//! decides and writes in one motion, and a crash between the two costs a
//! duplicate. Here the pass has already saved its ledger — `attention::tick`
//! writes it *before* delivering, on purpose, so a hook that dies takes one
//! notification with it rather than getting it again for ever — and that
//! at-most-once is affordable for a hook and is not affordable for a wake. A
//! hook that misses a line loses an interruption; a governor that misses one
//! sleeps through the thing it was watching for.
//!
//! So every emit is put in the spool and written down first, and only then does
//! anything try to leave. The decision to flush is [`Spool::owed`] — does this
//! seat hold something the table judged worth a context read — which is the
//! same table, asked of the record rather than of what is fresh.
//!
//! # The gate is two questions, and only one of them is durability
//!
//! `core-021` d2, driven against a real Claude Code rather than a fake. A
//! prompt delivered mid-turn is *queued* by Claude Code and answered when the
//! turn ends; nothing is typed at a live composer. What is not durable is the
//! queue: it lives in the agent, so a wake handed to it and then `/clear`ed,
//! restarted or killed is a wake wsp has recorded as delivered and nobody ever
//! read. Holding it costs ninety seconds and keeps the spool as the record for
//! the whole window.
//!
//! The second question is not about durability at all — it is about *where the
//! keystrokes land*, and `core-019` is where this path was found not asking it.
//! A wake is delivered by typing at a pane, and there are three states in which
//! that is not a delivery: a pane stopped on a permission dialog takes the text
//! *into the dialog*, an agent still coming up refuses it, and a pane herdr
//! cannot be asked about may not hold an agent at all. `cmd_agent::tell` has
//! refused all three since robustness-083; this path asked only
//! `turn_in_flight`, which is `Working` and nothing else. It asks
//! [`crate::place::State::will_take_a_prompt`] now — the question already
//! written down for every caller that used to spell it `state == "idle"`.
//!
//! Both asked of the *kind* rather than written into this path, because both
//! are facts about a transport that types at a pane and not about waking a
//! governor — see [`crate::agent_commands::Kind::queue_is_the_agents`].
//!
//! # Always on, and the seat is the off switch
//!
//! `core-019`. Nothing here is opt-in and nothing is tunable, which was a
//! constructor's side effect before it was a decision; it is a decision now,
//! and [`crate::cmd_watch::Spec::for_wake`] carries the number it is argued
//! from. The switch a person has is the seat itself — `wsp govern <scope>
//! --clear` vacates it, this path answers *the seat is empty*, and the spool
//! keeps everything until somebody sits down again. It is named on every wake
//! by [`preamble`], reported by `wsp watch --status`, and a spool that has
//! silently outlived `--defer-max` behind it is a `wsp doctor` problem — see
//! [`crate::cmd_watch::health`], which is the seventh guard against a silence
//! nobody can see in a file that already carried six.
//!
//! # `wsp-146`: one way in, and a turn is the receipt
//!
//! This module was already the path everything a governor is told arrives by,
//! and four verbs did not use it. `cycle.rs` asked `cmd_govern::govern`, which
//! types at a pane through [`crate::agent_commands::Kind::tell`] and **refuses a
//! seat that is `Working`** — *"not ready — working"* — and a run's verdict is
//! exactly the sentence a governor is mid-turn when it matters most. Its answer
//! was to raise a hand on a member of the run instead, so the governor was told
//! about its own barrier on a row it did not own. `wsp ask`, `wsp flag` and
//! `wsp govern --tell` each wrote somewhere of their own, and nothing in wsp
//! could say where a sentence it had accepted had got to.
//!
//! Two changes, and they are the two halves of the row.
//!
//! **One way in: [`say`].** Every sentence wsp or an agent sends to a seat is
//! put in that seat's spool and nothing else is attempted from a verb. The
//! spool is written inside the lock and the verb then calls the *same*
//! [`deliver_to`] this pass calls, so "delivered now" and "delivered on the next
//! tick" are the same code with the same gate — [`crate::place::State::will_take_a_prompt`],
//! asked once, here. **Nothing is refused because a governor is busy: busy means
//! later, and the spool is the record of the waiting.** With no seat on the
//! scope there is no spool and no post, so the caller does what it did before —
//! `cycle.rs` raises the hand on the run's first member, where a person looking
//! at the run is looking.
//!
//! **A turn is the receipt.** `Delivery::Unconfirmed` used to be the end of the
//! story: the text was typed, nothing moved, the entry cleared, and the line
//! said so in a way nobody read — *delivered, no turn seen*, printed by
//! [`crate::cmd_agent::delivered`] to a sender who had no way to act on it. An
//! entry now clears only on [`crate::place::Delivery::Started`], or on a turn
//! observed on a later tick at the same seat: the gate means the seat was
//! [`crate::place::State::Idle`] at the moment of the type, so a turn in flight
//! now started after it, and that is the confirmation. A type that starts
//! nothing stays owed and is stamped ([`crate::cmd_watch::Spooled::typed`]),
//! and is not typed again inside [`RETYPED`]. So the sentence that was sitting in
//! a composer is not typed twice, the record still says the governor has not
//! read it, and `--status` names the state in the words [`Tell::why`] prints.
//!
//! **The pass does not re-announce what it has already handed over.** A record
//! put in a seat's spool by `wsp ask` is a level the pass will derive a minute
//! later — [`crate::cmd_watch::Kind::Unanswered`] — and two lines about one
//! question in one composer is how a governor learns to skim its inbox. So
//! `wsp ask` and `wsp flag` record the handover on the message itself (a
//! [`crate::message::Act::Sent`] hop), and [`already_handed`] skips the edge for
//! a record this seat was given. The level still goes everywhere else: a hook,
//! a panel, and a seat that was *not* the one handed it — which is the case that
//! matters when the addressee stands down and the routing walks up a level.

use crate::agent_commands;
use crate::cmd_govern;
use crate::cycle::Seats;
use crate::cmd_watch::{Emit, Line, Sink, Spec, Spool, Spooled, Stream, EVERYONE};
use crate::place::Seat as Pane;
use crate::store::Store;
use crate::util;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// How long a typed-but-unanswered line waits before it is typed again.
///
/// **Two minutes, and it is `cmd_agent::SAME_BREATH`'s number for the same
/// reason.** That constant exists because a governor retried a message that had
/// arrived and sent one paragraph three times; this exists because a type that
/// starts no turn has left its text in a composer, and a second type inside the
/// window appends to what is already there. `robustness-093` measured the other
/// half of it — fifteen of Ed's own instructions sat unsubmitted for three days
/// — so "still owed" cannot mean "type it again now", and two minutes is the
/// window `worklist-010` measured the retry inside.
///
/// What happens after it is a fresh type, which is right rather than cautious:
/// the text was not taken and nothing has been confirmed about it, and the spool
/// is still the record of the fact either way.
const RETYPED: i64 = 120;

/// How a seat's wake spool is named in the register.
///
/// Beside `wsp watch`'s own records and in the same file, so `--status` and
/// `doctor` read one place. The prefix is what tells the two apart: a watch key
/// is a pane and this is a scope, and they share a key space.
pub(crate) fn key_for(scope: &str) -> String {
    format!("wake:{scope}")
}

/// Every scope that currently owes a seat something — **the question the
/// delivery path asks, and not the one a spool's depth answers**.
///
/// `wsp-178` found this live: `tokenhub-spec-sync` is a `done` list, all three of
/// its spool entries are `edge: left`, and `wsp-166` withholds those from a seat
/// — so it owes nothing and is owed nothing, while `depth() > 0` says it is
/// holding three. The reconciler's second trigger walked
/// a spool's depth, and so read it as a scope owing an answer, and reseated it
/// every twenty minutes for hours on a compound agent that never comes up
/// (`unseated: 250`). **Held is not owed**, and since `wsp-166` those have been
/// different things on purpose.
///
/// This is the eighth instance of this row's own lesson — two readers of one
/// record — and the second where the narrower question was the right one and the
/// wider was the guess. It is also the one where the cost was a whole scope being
/// reseated rather than a sentence being dropped, which is the more expensive of
/// the two mistakes to make silently.
/// **A record's existence is not this either**, and never was: `wake:core` is
/// written the moment anything is addressed and kept after it clears, so an empty
/// array is a scope with nothing owed and a scope whose line was delivered is the
/// same. The spool's *depth* was the second wrong answer — see below.
///
/// **The count is returned beside the scope, and that is the third of
/// `wsp-178`'s items rather than a convenience.** The reconciler logs a sentence
/// saying this scope owes an answer, and a sentence that asserts a reason should
/// carry the number the reason was counted from — so `cycle.log` reads `it owes 3`
/// or `it owes 96`, and a scope that owes none cannot be described in those words
/// at all. The alternative was a fixed string and a trust that the walk had asked
/// the right question, which is precisely the trust that was misplaced twice on
/// this row.
pub(crate) fn scopes_owed(store: &Store) -> Vec<(String, usize)> {
    let prefix = "wake:";
    store
        .watches()
        .keys()
        .filter(|k| k.starts_with(prefix))
        .filter_map(|k| {
            let owed = owed_to_a_seat(store, k);
            (owed > 0).then(|| (k[prefix.len()..].to_string(), owed))
        })
        .collect()
}

/// The pass's third audience: what each governor is owed, and whether now is
/// the moment to say it.
///
/// Grouped by [`Emit::to`] because that is the whole reason this sits on the
/// daemon rather than on a subscription — one pass, many addressees, and each
/// seat's backlog is its own.
pub(crate) fn wake(store: &Store, emits: &[Emit], at: i64) {
    let mut mine: BTreeMap<String, Vec<&Emit>> = BTreeMap::new();
    // **Every seat that is owed something, not only the seats with news.**
    //
    // Driven 2026-08-21, and it is the failure that does not show up in a unit
    // test: a wake held because the seat was mid-turn sat untouched for two
    // minutes with its record's stamp frozen, because nothing in that scope had
    // happened since. A spool is only reconsidered when this function visits
    // its scope, so visiting only the scopes with fresh emits means a held wake
    // waits on *unrelated* news to be delivered, and `--defer-max` never fires
    // at all on a quiet fleet — which is precisely the fleet escalation exists
    // for. The retry and the escalation are both properties of the record, so
    // the record is what decides who gets looked at.
    for key in store.watches().keys() {
        if let Some(scope) = key.strip_prefix("wake:") {
            mine.entry(scope.to_string()).or_default();
        }
    }
    for e in emits {
        // `core-014` §4: a level nobody in particular owns is the *hook's*
        // audience, not a governor's. `hooks/on-attention-raised` already
        // reaches a person for free, and waking a governor for something it is
        // not the addressee of is the noise this row exists to remove.
        if e.to == EVERYONE || e.to.is_empty() {
            continue;
        }
        // And the level this seat was handed the record for *before* the pass
        // ever saw it. See the module docs: `wsp ask` and `wsp flag` put the
        // record in the spool themselves, so without this the question arrives
        // twice — once as the message, once as `unanswered` — and a governor
        // paying 208k a wake is the one reader who cannot afford the pair.
        if already_handed(store, e) {
            continue;
        }
        mine.entry(e.to.clone()).or_default().push(e);
    }
    for (scope, theirs) in mine {
        deliver_to(store, &crate::cycle::Fleet, &scope, &theirs, at);
    }
}

/// Whether this seat has already been given the record this edge is about.
///
/// **An `Up` edge only, and the other direction is the reason.** The handover is
/// a fact about the past — the seat has the words in front of it — so it
/// suppresses the news that it arrived and nothing else: the level going *down*
/// when the question is answered is the governor's cue to stop watching for it,
/// and suppressing that would strand the answer.
///
/// The reads are two and both are bounded: an edge with no record is skipped
/// without touching the store, which is every derived level and the great
/// majority of a tick, and the records are read once for the whole pass rather
/// than once per edge.
fn already_handed(store: &Store, e: &Emit) -> bool {
    if e.edge != crate::cmd_watch::Edge::Up {
        return false;
    }
    let Some(record) = e.signal.record.as_deref() else { return false };
    crate::message::raised(store)
        .iter()
        .any(|m| m.id == record && m.handed_to(&e.to))
}

/// One seat's turn: spool everything, write it down, then see whether it is
/// owed a wake.
///
/// Returns what happened, because [`say`] has to tell a caller where its
/// sentence went and this is the only place that knows. The pass throws it away.
fn deliver_to(store: &Store, seats: &dyn Seats, scope: &str, emits: &[&Emit], at: i64) -> Report {
    let key = key_for(scope);
    let spec = Spec::for_wake(scope);
    let delivered = record(store, &key).0;
    // Nothing new and nothing owed is nothing to type. Said here rather than at
    // the caller because the caller visits every seat with a record, and a seat
    // at rest must not cost a store write every twenty seconds for ever.
    //
    // **The gate on typing, and not on the sentence.** It used to be both: the
    // only writer of `holding` is below it, so a scope whose whole spool is
    // withheld kept whatever it last said — `tokenhub-spec-sync` reading *nothing
    // worth a wake yet* about a seat with nobody in it, and `compound-parity`
    // reading *no seat on this scope*, a sentence from a reader `4719681` deleted,
    // for twenty-one hours. `wsp-166` made owed and held two questions on purpose,
    // and "is anybody in this seat" is a third. See [`restate`].
    if emits.is_empty() && owed_to_a_seat(store, &key) == 0 {
        restate(store, seats, scope, &key);
        return Report::at_rest();
    }

    // **Appended inside the lock, and what comes back is the record as it
    // actually is.** Two things follow, and they are the whole of `core-022`
    // on this path: the fact is durable before anything is attempted (see the
    // module docs), and a `wsp watch --drain` from a governor's own pane
    // between two ticks is not written over by this one.
    let mut spool = store.update_watch(&key, |rec| {
        let fresh: Vec<Spooled> =
            emits.iter().map(|e| Spooled::of(at, Line::News((*e).clone()))).collect();
        let s = Spool::append(rec, fresh);
        stamp(rec, scope, delivered, NOT_YET, &s);
        s
    });

    // **`wsp-166`: a governor is typed decisions and nothing else.** What is
    // withheld stays in the record, so `wsp watch --drain` still reads it and
    // the logs still say it, and it is neither typed nor counted as owed.
    spool.withhold_for_a_seat(store);
    // What a typing stamps: this, and not the withheld entries beside it, which
    // were not at the seat and are never settled — see [`Tell::deliver`].
    let typed_batch: Vec<u64> = spool.held.iter().map(|h| h.seq).collect();

    // Nothing is offered as *hot*: everything this pass produced is already in
    // the spool by the line above, so the only question left is whether that
    // spool owes somebody a context read. `Stream::tick` answers it with the
    // same table `wsp watch --wake` uses, and returns what actually left —
    // which is empty unless the sink took it.
    // Taken before the flush, because a flush empties the copy in hand and the
    // number is what says *everything up to here has gone*.
    let mut tell = Tell::new(store, seats, scope);
    let sent: Vec<u64> =
        Stream::new(&spec, &mut tell).tick(at, &mut spool).iter().map(|h| h.seq).collect();
    let written = sent.len();
    // What is left is read out of the record *inside* the write that settles it,
    // rather than by a second read of `watches.json` after it: the pass asks
    // this question for every seat with anything held, every tick, and the
    // record is already open.
    let (left, why) = store.update_watch(&key, |rec| {
        // Delivered, so gone — by identity, so a drain that took them first is
        // not undone and anything appended since is not swept away with them.
        //
        // **Only when a turn came of it.** `Stream::tick` hands the batch back
        // when the sink says nothing arrived, and for a seat the honest reading
        // of `Delivery::Unconfirmed` is *the text is there and nothing has read
        // it yet* — so it is stamped rather than cleared, and the stamp is what
        // stops the next tick typing it again. See the module docs.
        if written > 0 {
            Spool::settle_these(rec, &sent);
        } else if tell.typed {
            Spool::stamp_typed(rec, at, &typed_batch);
        }
        let now = Spool::of_json(rec.get("spool").unwrap_or(&serde_json::Value::Null));
stamp(rec, scope, delivered + written, &tell.why, &now);
        // What is *owed* is what a seat would be typed, not everything held.
        let mut owed = now.clone();
        owed.withhold_for_a_seat(store);
        (owed.depth(), tell.why)
    });
    Report { scope: scope.to_string(), settled: written, held: left, typed_at: tell.typed_at, why }
}

/// What happened to a seat's spool, in the words a receipt prints.
///
/// **`wsp-146`'s answer to "where did my sentence go"**, and the reason a verb
/// can hand a sentence over and still say something true about it. A caller
/// that used to type at a pane and print `delivered, no turn seen` has one of
/// three things to report and this is the type that says which.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Report {
    /// The scope whose seat this was.
    pub(crate) scope: String,
    /// How many entries cleared, which is zero on every tick that merely held.
    pub(crate) settled: usize,
    /// How many this seat is still owed.
    pub(crate) held: usize,
    /// When its text last went to the seat and no turn came of it, or `None`
    /// if it never has been.
    pub(crate) typed_at: Option<i64>,
    /// Why it is still holding, in the words `--status` prints.
    pub(crate) why: String,
}

impl Report {
    fn at_rest() -> Report {
        Report { scope: String::new(), settled: 0, held: 0, typed_at: None, why: NOT_YET.into() }
    }

    /// Whether the seat has it now — a turn, not a send. See [`Delivery`].
    ///
    /// [`Delivery`]: crate::place::Delivery
    pub(crate) fn arrived(&self) -> bool {
        self.settled > 0
    }

    /// One line, for a caller that has to say where a sentence went.
    ///
    /// **Both halves are load-bearing and the second one is the brief's.** A
    /// receipt that only says *delivered* leaves the sender unable to tell a
    /// governor that is busy from a governor that has stopped answering, and a
    /// receipt that only says *held* teaches an agent to stop trusting the
    /// channel. So it names the post, says whether the seat has it or is owed
    /// it, says why in the same words `doctor` would, and points at the drain.
    pub(crate) fn said(&self, what: &str, p: &util::Paint) -> String {
        if self.arrived() {
            return format!("{} {} · the seat has it", p.dim("→"), p.bold(&self.scope));
        }
        let held = match self.held {
            0 => String::new(),
            n => format!(" · {n} still owed"),
        };
        let typed = match self.typed_at {
            Some(at) => format!(" · typed {}s ago", (util::epoch_secs() - at).max(0)),
            None => String::new(),
        };
        format!(
            "{} {}{held}{typed} · {} · `wsp watch --drain` shows what is held",
            p.dim("→"),
            p.bold(what),
            self.why
        )
    }
}

/// Put a sentence in a seat's spool and try to deliver it once.
///
/// **The one way anything reaches a governor, and the reason a verb may not grow
/// a second one.** `wsp-146` found four verbs with four answers and no way for an
/// agent to tell which one arrives; the fault was not that they differed but
/// that three of them typed at a pane themselves, so *busy* came back as a
/// refusal on a sentence that was already written down.
///
/// So: the spool first, inside the lock, and then the very same
/// [`deliver_to`] the daemon's pass calls — which means the gate, the retry
/// refusal, the preamble and the acknowledgement are all shared, and "the daemon
/// will get there" is not a hope but the code path just taken.
///
/// `None` when there is no post on the scope at all — the one case this cannot
/// serve, and the caller does what it did before. The distinction is deliberate
/// rather than an oversight: a post that has been **stood down from** keeps its
/// record with no workspace in it, so it is not a seat and its spool does not
/// exist; a seat whose occupant has died is a post with nobody in it, and the
/// whole row is that one of those *holds* what it is sent.
///
/// The hop on the record is what stops the attention pass announcing the same
/// thing again a minute later; see [`already_handed`].
///
/// # What does *not* come through here, and why
///
/// The brief lists four senders and this is four of the five callers. The two
/// that are deliberately not here are named because "one path" is only
/// believable if the exceptions are written down:
///
/// - **`wsp tell <task>`.** It addresses an *agent's* pane by way of the task it
///   holds, and a governor holds no task — so it cannot reach a seat at all, and
///   the row that wants a governor is asking it for the wrong reason. It keeps
///   its own `Sent`/`twice`/`Refusal::NotTaken` handling, which is a contract
///   with the person at the keyboard: *I have it and you did not submit it* is a
///   rescue, and there is nobody to hand a rescue to here.
/// - **`wsp block`.** It was never refused by anybody: it records the question on
///   the task and the status, and [`crate::cmd_watch::Kind::Blocked`] takes it from
///   the store, spools it and wakes the seat. A second line for the same fact
///   would cost a second context read — measured at 208k — for the sentence, which
///   the level's own line already points at with `wsp show <id>`. What a block
///   lacked was the acknowledgement, and that is item 2, which is this module.
/// **The spool first, the seat second, and always a [`Report`] — all three,
/// and the third is the one this row's verifier found.** `say` used to ask
/// whether the scope had a seat *before* writing anything and returned `None` if
/// it did not, so a sentence addressed to a scope whose governor had died was
/// dropped on the floor rather than held: `wsp ask`, `wsp flag` and
/// `wsp govern --tell` all reached this, and all of them lost. It also asked with
/// [`cmd_govern::seat_of_scope`], which stops at the top level of a record — so a
/// record `reconcile` vacated reads as *no seat at all* rather than as a seat
/// with nobody in it. Live on the trunk: `tokenhub-spec-sync` reading `no seat
/// on this scope` while `governors.json` for the same scope carried
/// `unseated: 156` and a live claim.
///
/// **A vacancy is later, not nowhere.** That is what the spool is *for*: the
/// whole of `wsp-146` is that a sentence held for a busy seat clears on a turn,
/// and a seat filled tomorrow morning is told what it missed. There is no
/// reading under which a dead governor's scope is a place sentences go to die.
/// The addressee is [`crate::repair::reading`]'s question now, asked once in
/// [`Tell::deliver`] — the same question the reconciler asks, which is the sixth
/// instance of this row's own lesson about two readers of one record.
pub(crate) fn say(store: &Store, scope: &str, text: &str, record: Option<&str>) -> Report {
    say_through(store, &crate::cycle::Fleet, scope, text, record)
}

/// [`say`], with the port that reads the seat named — so a test can say what the
/// seat is doing rather than inherit whatever a machine with no backend says.
fn say_through(store: &Store, seats: &dyn Seats, scope: &str, text: &str, record: Option<&str>) -> Report {
    let at = util::epoch_secs();
    let key = key_for(scope);
    // Written before anything is attempted, and the durability is the reason
    // this is inside the lock rather than after the send: a sentence that has
    // been accepted and lost between the two is the one loss nothing can detect.
    store.update_watch(&key, |rec| {
        Spool::append(rec, vec![Spooled::of(at, Line::Note(crate::cmd_watch::Class::Message, text.to_string()))]);
    });
    if let Some(id) = record {
        // Best effort, and deliberately not fatal: the sentence is in the spool
        // either way, and the only thing this hop suppresses is the duplicate.
        let _ = crate::message::sent(store, id, scope);
    }
    deliver_to(store, seats, scope, &[], at)
}

/// A wake, told.
///
/// **The retry refusal is not passed, it is bypassed, and that is the point.**
/// `cmd_agent::twice` refuses a sentence that has just reached this pane and
/// returns exit code 0 — right for a person retrying a send that never failed,
/// and for a wake it is a silent drop reported as delivery. A level re-raising
/// on the same subject with the same wording is an ordinary thing for a level
/// to do. So this path never consults [`crate::cmd_agent::Sent::already_sent`]
/// at all: it calls [`agent_commands::Kind::tell`] directly, and **the spool —
/// not the send — is the record that a fact was delivered.**
pub(crate) struct Tell<'a> {
    store: &'a Store,
    /// What answers [`nobody_there`] — the reconciler's port, so the seat this
    /// path will not type at is the seat the reconciler is filling.
    seats: &'a dyn Seats,
    scope: String,
    /// Why the last attempt did not land, in the words `--status` prints.
    ///
    /// **A delivery path nobody can inspect is the seventh way silence lies**,
    /// and this field is that lesson learned the hard way twice in one hour:
    /// driving this row, a wake sat at `0 delivered · holding 2` and the record
    /// could not say whether nobody held the seat, the agent was mid-turn, or
    /// the table simply had not judged anything worth a wake. Those want three
    /// different repairs and the first two are faults. A fourth joined them in
    /// `wsp-146`: the text was at the seat and nothing had read it, which is
    /// [`UNREAD`] and looks exactly like the three.
    why: String,
    /// The text went in and no turn came of it — set only when this attempt
    /// typed, and read by [`deliver_to`] to stamp the batch it typed. The
    /// distinction matters because *typed* is the one state that must not be
    /// typed again straight away.
    pub(crate) typed: bool,
    /// When that happened, carried out of the sink so a receipt can say how long
    /// the seat has been sitting on it.
    pub(crate) typed_at: Option<i64>,
}

impl<'a> Tell<'a> {
    fn new(store: &'a Store, seats: &'a dyn Seats, scope: &str) -> Tell<'a> {
        Tell { store, seats, scope: scope.to_string(), why: NOT_YET.into(), typed: false, typed_at: None }
    }
}

/// Nothing here is worth a context read on its own. The ordinary state, and
/// not a fault: it is what `core-017`'s table is for.
const NOT_YET: &str = "nothing worth a wake yet";

/// A scope nobody has ever governed: no record, so nothing to fill and nobody
/// filling it. The one case with genuinely no addressee — see [`nobody_there`].
const NO_SEAT: &str = "no seat on this scope";

/// The seat reads occupied and the agent in it could not be found to type at.
/// Not a vacancy, and deliberately not worded as one: see [`Tell::deliver`].
const NOT_FOUND: &str = "somebody is in the seat and wsp could not find them to type at";

/// The text is at the seat and no turn has started on it.
///
/// **The seventh state, and it is the one `wsp-146` had to add a word for.**
/// `cmd_agent::delivered` had been printing *"delivered, no turn seen"* for
/// years, and it was true — but it was said to a sender as though it were the
/// end of the matter, when for this path the matter is that the governor has
/// not read the sentence and the spool still owes it. The type stays owed, the
/// stamp says when it was last typed, and the retry waits [`RETYPED`].
const UNREAD: &str = "typed at the seat — no turn seen yet, and still owed";

/// Why a pane would not take a wake, in the words `--status` and `doctor`
/// print.
///
/// **One sentence per state rather than one for all of them**, because they
/// want four different repairs and a reader who cannot tell them apart goes
/// looking in the wrong place: mid-turn clears itself and is not a fault at
/// all, `Blocked` needs a person at a keyboard, `Starting` clears itself in
/// seconds, and `Unknown` means the seat could not be read — which is a fault
/// about the *reporter* and not about the seat.
///
/// No wildcard, for [`crate::cmd_watch::Line::disposition`]'s reason: a state
/// added later has to come past here and say what a wake does about it, rather
/// than inheriting a sentence that was written before it existed.
pub(crate) fn held_because(state: crate::place::State) -> Option<&'static str> {
    use crate::place::State;
    match state {
        // The one state a wake may be typed into, and it is
        // [`crate::place::State::will_take_a_prompt`]'s whole definition. This
        // function is the only thing [`Tell::deliver`] asks, so a test that
        // enumerates it is reading the gate rather than a copy of it.
        State::Idle => None,
        State::Working => Some("the seat is mid-turn"),
        State::Blocked => Some("the seat is stopped on a prompt only a person can answer"),
        State::Starting => Some("the agent is still coming up"),
        State::Empty | State::Gone => Some("the seat is empty"),
        State::Unknown => Some("wsp cannot say what the seat is doing"),
    }
}

/// Whether anybody is in this scope's seat, said as `--status` prints it —
/// `None` where somebody is, and the sentence a wake is held under where nobody
/// is.
///
/// **The one place a sentence about occupancy is decided, and it is
/// [`crate::repair::reading`] rather than a question of its own.** `wsp-148` was
/// sent back four times for the same shape: two readers of one record, or one
/// gate asking a different question from the sentence it writes. Here there were
/// three — `Tell::deliver` asked `seat_held` and then `occupant`, its own pane
/// state asked a third time, and the reconciler asked `reading`. They parted on
/// a backend that could not be reached (the locator said `unseated`, the
/// reconciler said it could not tell and would not act) and on a slot with an
/// empty pane (one said *no seat on this scope*, the other was filling it). Every
/// writer of `holding` that is about a seat now comes through here: the typing
/// path in [`Tell::deliver`] and the at-rest one in [`restate`].
///
/// **No record at all is the one reading `reading` does not name**, because the
/// reconciler does not need to: a scope nobody has ever governed is not filled
/// unless a list is running on it, and *no seat on this scope* is the sentence
/// that does not promise a governor is on its way.
fn nobody_there(store: &Store, seats: &dyn Seats, scope: &str) -> Option<String> {
    use crate::repair::Occupancy;
    if !store.governors().contains_key(scope) {
        return Some(NO_SEAT.into());
    }
    match crate::repair::reading(store, seats, scope) {
        Occupancy::Occupied => None,
        Occupancy::Vacant(_) => Some(unseated(store, scope)),
        // An absence is not a vacancy: the reconciler will not act on it, so
        // this must not say a governor is on its way.
        Occupancy::Unreadable => held_because(crate::place::State::Unknown).map(String::from),
    }
}

/// What a scope with nobody in its seat reads as `holding`, and why it is not
/// the seat's state.
///
/// **`the seat is empty` was three faults wearing one sentence, and `wsp-148` is
/// the one that made them expensive.** It was the answer for a pane with nobody
/// in it, for an agent that exited, and for a slot that has been standing empty
/// for a month — and a reader could not tell them apart, so the one case that
/// was a *stuck run* read exactly like the one that was a seat somebody had
/// chosen to leave. Now that wsp fills a vacancy on its own ([`crate::repair::
/// seat_kept`]) the difference is worth money: `reseating` says a governor is on
/// its way and there is nothing for a person to do, and `daemon down` says the
/// thing that will fix it is not running, which is the one of the two a person
/// can act on tonight.
///
/// **`daemon down` is a reading of the marker, not a guess.** `Store::
/// daemon_holder` is the pid that last claimed this store and the one-daemon-
/// per-store rule keeps it honest, so `alive` on that pid is whether a
/// reconciler will act on this vacancy. An absent marker is down — a store that
/// has never had a daemon has nothing running the pass, whatever else is true.
///
/// **The count is the spool's own depth**, read here rather than carried in, so
/// the sentence cannot disagree with `wsp watch --status`: both are
/// `Spool::depth` over the same record.
fn unseated(store: &Store, scope: &str) -> String {
    let held = load(store, &key_for(scope)).depth();
    let v = cmd_govern::vacancy(&store.governors(), scope);
    let up = store
        .daemon_holder()
        .map(|(pid, _)| pid)
        .is_some_and(|pid| crate::place_super::alive(&[pid]).contains(&pid));
    // **Five readings and not two, and each is one a reader can act on
    // differently.** `reseating` and `daemon down` were the two `wsp-148` asked
    // for; the other three are states that arrived with it and that those two
    // cannot honestly cover.
    //
    // `stood down` is first because it is a decision and everything else is a
    // measurement — a seat somebody closed is not a seat the machine is counting
    // towards filling, and `wsp-148`'s `stood_down` marker made that true in the
    // store while this sentence went on saying `counting` for 201 held items on
    // `tooling`. Found by reading this output after landing the marker, which is
    // the usual way: the fix and the sentence it made untrue were the same row.
    let tail = match (v.reseating, v.failed, up) {
        (Some(_), Some(_), _) => "retrying".to_string(),
        (Some(_), None, _) => "reseating".to_string(),
        (None, _, false) => "daemon down".to_string(),
        (None, _, true) => "counting".to_string(),
    };
    // A kind compound cannot run is a seat nothing will fill, and the one of
    // these a person has to answer: `counting` there would be a promise.
    let kind = crate::cmd_spawn::seat_kind(&store.governors(), scope);
    let tail = match (crate::place_compound::reads_kind(&kind), crate::repair::will_seat(store, scope)) {
        (false, _) => format!("a {kind} seat compound cannot run"),
        // Nothing owed and no running list: nothing will fill it, and nothing
        // needs to until something is owed.
        (true, false) => "nothing owed, so nobody is coming".to_string(),
        (true, true) => tail,
    };
    let tail = match cmd_govern::stood_at(&store.governors(), scope) {
        Some(at) => format!("stood down at {}", util::local_hm(at)),
        None => tail,
    };
    format!("unseated · {held} held · {tail}")
}

/// What a governor is told about the thing that just typed at it.
///
/// **This is `core-019` question 3, and the answer is that it rides the wake
/// rather than the work order.** Nothing in `cmd_spawn::work_order`, `wsp
/// brief` or the handbook says a governor's session may receive prompts nobody
/// typed, and a wake arrives through `Kind::tell` — herdr typing at the pane —
/// so it is indistinguishable from Ed at a keyboard. That is a strange thing to
/// meet unexplained, and the obvious repair is a clause in the custodian work
/// order.
///
/// Two arguments against that clause, and they are both decisive:
///
/// - **It is absent exactly when it is needed.** A governor that has been
///   `/clear`ed is still in the slot, still addressed, and still woken — and
///   the work order it read is gone. The explanation has to travel with the
///   thing it explains.
/// - **It is the most expensive real estate in the system.** The work order is
///   read on every spawn of every governor, for ever, whether or not a wake
///   ever arrives. This is ~50 tokens against a context read measured at 208k
///   — 0.02% of the thing it is explaining — and it is paid only by the seats
///   that are actually woken, only when they are woken.
///
/// Repeated on every wake rather than only the first, for the first argument
/// again: "only the first" is state, and the session it was told to may not be
/// the session that takes the next one.
///
/// It also names the stand-down, which is the whole of this row's answer to
/// *what is the off switch* — the seat is the switch, and a wake is the one
/// place a governor reliably reads.
fn preamble(scope: &str) -> String {
    format!(
        "wsp · nobody typed this. You are the custodian of `{scope}`, and wsp's attention pass judged the lines\n\
         below worth a read of your context. No reply is expected — act on them, or don't, then stop.\n\
         `wsp watch --drain` prints what else is held for this seat · `wsp govern {scope} --clear` stands the\n\
         seat down and stops the wakes."
    )
}

impl Sink for Tell<'_> {
    /// **Never this process's stdout.** The far end is a composer, and
    /// [`Sink::paint`] carries why that is a question about the destination
    /// rather than about the daemon.
    fn paint(&self) -> util::Paint {
        util::Paint::plain()
    }

    fn deliver(&mut self, said: &[String]) -> bool {
        if said.is_empty() {
            return true;
        }
        // Whether anybody is there is [`nobody_there`]'s, and is not asked again
        // below. Not a failure and not a drop either way: the spool keeps it, and
        // a seat filled tomorrow morning is told what it missed.
        if let Some(why) = nobody_there(self.store, self.seats, &self.scope) {
            self.why = why;
            return false;
        }
        // Somebody is there, and this is only *where* — the agent to type at,
        // which for a compound seat may be the room rather than the pane. A
        // locator that cannot find what the reading saw is not a vacancy, and
        // saying `unseated` here was the second reader this row kept finding.
        let Some(seat) = cmd_govern::seat_held(&self.scope, &self.store.governors()) else {
            self.why = NOT_FOUND.into();
            return false;
        };
        let backends = crate::cmd_spawn::local_backends();
        let Some((place, found)) = cmd_govern::occupant(self.store, &backends, &seat) else {
            self.why = NOT_FOUND.into();
            return false;
        };
        let how = agent_commands::of(&found.agent.kind);
        // The gate: one state read, two questions, and both of them are about
        // a transport that delivers by *typing at a pane*.
        // [`agent_commands::Kind::queue_is_the_agents`] is that transport
        // named — its docs open on `Place::tell`, herdr typing — so a kind
        // carrying a queue of its own pays for neither question.
        //
        // **Mid-turn is the durability half.** `core-021` d2: not corruption —
        // Claude Code queues a mid-turn prompt and answers it at the boundary —
        // but durability, because that queue is the agent's and dies with it
        // while the spool does not.
        //
        // **Asked of `agent.get` and not of the census row `occupant` hands
        // back.** `herdr::panes` is `pane.list`, and the whole reason
        // `Herdr::census` makes two calls is that `pane.list` alone cannot tell
        // a starting agent from an idle one — so a status read off that row is
        // not a reading of whether a turn is in flight. Driven 2026-08-21: with
        // the row's status the gate never fired once, and four wakes were
        // delivered into an agent that `agent.get` reported as `working`
        // throughout. `place_herdr::turning` carries the same warning for the
        // same reason.
        //
        // **And where the keystrokes land is the other half**, which
        // [`crate::place::State::turn_in_flight`] does not answer: it is
        // `Working` and nothing else, so this path would type into a permission
        // dialog, into an agent still coming up, and into a pane herdr could
        // not be asked about — the three `cmd_agent::tell` has refused since
        // robustness-083. The module docs carry it; the question is
        // [`crate::place::State::will_take_a_prompt`], and asking the wider one
        // costs nothing this path was not already paying, because refusing is
        // the ordinary answer here.
        let addressee: Pane = found.seat.clone();
        // Read once and used three times, which is the shape `wsp-146` needs:
        // whether to type at all, whether a turn confirms a batch typed on an
        // earlier tick, and the sentence `--status` prints. One reading of one
        // fact, because the three can disagree if they are taken separately and
        // the record then says a seat is mid-turn on the tick it started a turn.
        let state = place.state(&addressee).unwrap_or_default();

        // **The acknowledgement, and it is asked before the gate.**
        //
        // This batch has already been at the seat and no turn came of it, so
        // there are two questions and only one of them may be answered by typing
        // again. A turn in flight *now* is that answer: the gate below refuses
        // anything but an `Idle` seat, so the seat was idle when the text went
        // in, and a turn in flight since then started after it — which is the
        // confirmation `Delivery::Started` would have given and could not give
        // across a tick boundary.
        //
        // **First, deliberately.** A governor that read the sentence is by
        // definition mid-turn, so the gate would refuse exactly the state that
        // ends the wait, and the entry would sit owed until the seat fell idle
        // again — reporting a governor that has read the message as one that has
        // not. `wsp-146` d2, and the reason `delivered, no turn seen` is no
        // longer a place a sentence comes to rest.
        //
        // **Only what a seat is owed has a stamp that counts (`wsp-180`).** This
        // read the whole record, and a withheld entry carries the stamp of a
        // batch it rode in on and is never settled — so one stamp from 14:50 on
        // a flag that went down outlived every batch after it, each later
        // sentence read *a turn began since*, was cleared as delivered without
        // being typed, and three verifier blocks were answered `nothing worth a
        // wake yet · 0 held`.
        let mut owed = load(self.store, &key_for(&self.scope));
        owed.withhold_for_a_seat(self.store);
        if let Some(at) = owed.typed_at() {
            // **Or a turn that began and ended since the type**, which a sample
            // cannot see: `wsp-166` measured cpd-250's replies at one to two
            // seconds against a twenty-second tick, so most were never seen
            // and the batch was typed again every [`RETYPED`] all night.
            let read = state.turn_in_flight() || place.turn_began_since(&addressee, at) == Some(true);
            // **A turn confirms what was typed and nothing raised after it.** An
            // entry with no stamp has never been at the seat, so clearing the
            // batch on a turn would report it read; it falls through to the gate
            // and is typed with what is owed beside it.
            let unsent = owed.held.iter().any(|h| h.typed == 0);
            if read && !unsent {
                self.why = NOT_YET.into();
                return true;
            }
            if !read && util::epoch_secs() - at < RETYPED {
                self.why = UNREAD.into();
                self.typed_at = Some(at);
                return false;
            }
        }

        // The gate, asked only when the answer is *not* already known and the
        // only thing left to do is type. [`agent_commands::Kind::queue_is_the_agents`]
        // is that transport named — its docs open on `Place::tell`, herdr typing
        // — so a kind carrying a queue of its own pays for neither question.
        //
        // **Mid-turn is the durability half.** `core-021` d2: not corruption —
        // Claude Code queues a mid-turn prompt and answers it at the boundary —
        // but durability, because that queue is the agent's and dies with it
        // while the spool does not.
        //
        // **Asked of `agent.get` and not of the census row `occupant` hands
        // back.** `herdr::panes` is `pane.list`, and the whole reason
        // `Herdr::census` makes two calls is that `pane.list` alone cannot tell
        // a starting agent from an idle one — so a status read off that row is
        // not a reading of whether a turn is in flight. Driven 2026-08-21: with
        // the row's status the gate never fired once, and four wakes were
        // delivered into an agent that `agent.get` reported as `working`
        // throughout. `place_herdr::turning` carries the same warning for the
        // same reason.
        //
        // **And where the keystrokes land is the other half**, which
        // [`crate::place::State::turn_in_flight`] does not answer: it is
        // `Working` and nothing else, so this path would type into a permission
        // dialog, into an agent still coming up, and into a pane herdr could
        // not be asked about — the three `cmd_agent::tell` has refused since
        // robustness-083. The module docs carry it; the question is
        // [`crate::place::State::will_take_a_prompt`], and asking the wider one
        // costs nothing this path was not already paying, because refusing is
        // the ordinary answer here.
        if how.queue_is_the_agents() {
            // `State::Unknown` when herdr could not be asked, which
            // [`held_because`] refuses — an absence is not a fact, least of all
            // the fact that somebody is there to read this.
            //
            // `Empty` and `Gone` here are the pane emptying between
            // [`nobody_there`]'s reading and this one, and they are said as a
            // state, not as `unseated`: the next pass reads the vacancy where
            // vacancies are read, and a second place deciding it is the thing
            // this row has been sent back for four times.
            if let Some(why) = held_because(state) {
                self.why = why.into();
                return false;
            }
        }

        let text = format!("{}\n{}", preamble(&self.scope), said.join("\n"));
        // No `Sent`, no `already_sent`, no `twice` — see this type's docs.
        //
        // **Three answers and the middle one is the row.** `Started` is a turn
        // and clears the batch. `Unconfirmed` is *delivered, no turn claimed* —
        // and `NotTaken` is the same fact said by a backend that watched: the
        // sentence arrived and the agent did nothing, which is
        // `robustness-093`'s fifteen instructions sitting unsubmitted and, for a
        // wake, a governor that has not read the sentence wsp has already counted
        // as delivered. Both leave the entry owed and stamp the batch.
        // Taken *before* the type: a turn's start is stamped by its seat in
        // whole seconds while the type is still returning, so a stamp taken
        // after would postdate the very turn it is waiting for.
        let before = util::epoch_secs();
        match how.tell(place.as_ref(), &addressee, &text) {
            Ok(crate::place::Delivery::Started) => true,
            Ok(crate::place::Delivery::Unconfirmed) | Err(crate::place::Refusal::NotTaken) => {
                // Typed, and nothing read it. Returning false puts the whole
                // batch back, and the stamp is what tells the next tick to look
                // for a turn rather than to type again.
                self.why = UNREAD.into();
                self.typed = true;
                self.typed_at = Some(before);
                false
            }
            Err(_) => {
                self.why = "the seat would not take it".into();
                false
            }
        }
    }
}

// ---------------------------------------------------------------------------
// the record
// ---------------------------------------------------------------------------

fn record(store: &Store, key: &str) -> (usize, Value) {
    let rec = store.watches().get(key).cloned().unwrap_or(Value::Null);
    let n = rec.get("delivered").and_then(Value::as_u64).unwrap_or(0) as usize;
    (n, rec)
}

/// How many entries in this seat's spool it would be typed.
fn owed_to_a_seat(store: &Store, key: &str) -> usize {
    let mut spool = load(store, key);
    spool.withhold_for_a_seat(store);
    spool.depth()
}

/// A seat owed nothing, re-read for the sentence it is holding under — and the
/// record written only when that sentence has changed.
///
/// **Compare, then write: `repair::told_once`'s bound, and the reason the gate
/// above exists.** An at-rest seat must not cost a store write every twenty
/// seconds for ever, so this pays a read — one seat's state, which the reconciler
/// already spends on the same scopes — and writes on a change of sentence, which
/// is a change of seat.
///
/// **Against what it would say now, not on a transition into vacancy**, and that
/// is what makes an old sentence correctable: `compound-parity`'s *no seat on
/// this scope* was written by a reader that no longer exists, and a writer that
/// fired only on the way into a vacancy would have left it there for ever. It
/// also runs the other way — a seat refilled while its spool is all withheld
/// goes back to *nothing worth a wake yet* rather than reading `unseated` about a
/// governor that is sitting in it.
///
/// An empty spool is left alone: `stamp` writes no reason for holding nothing,
/// and comparing that `""` against a sentence would rewrite it every pass.
fn restate(store: &Store, seats: &dyn Seats, scope: &str, key: &str) {
    let (delivered, rec) = record(store, key);
    if Spool::of_json(rec.get("spool").unwrap_or(&Value::Null)).depth() == 0 {
        return;
    }
    let why = nobody_there(store, seats, scope).unwrap_or_else(|| NOT_YET.into());
    if rec.get("holding").and_then(Value::as_str) == Some(why.as_str()) {
        return;
    }
    store.update_watch(key, |rec| {
        let spool = Spool::of_json(rec.get("spool").unwrap_or(&Value::Null));
        stamp(rec, scope, delivered, &why, &spool);
    });
}

fn load(store: &Store, key: &str) -> Spool {
    Spool::of_json(store.watches().get(key).and_then(|v| v.get("spool")).unwrap_or(&Value::Null))
}

/// What `--status` and `doctor` read: what has been delivered to this seat, and
/// what is being held for it.
///
/// A delivery path nobody can inspect is the seventh way silence lies, and this
/// file's neighbour already carries six.
fn stamp(rec: &mut Value, scope: &str, delivered: usize, why: &str, spool: &Spool) {
    if !rec.is_object() {
        *rec = json!({});
    }
    let Some(o) = rec.as_object_mut() else { return };
    o.insert("scope".into(), json!(scope));
    o.insert("host".into(), json!(util::hostname()));
    // No process. This is a record of what the daemon owes a seat, not of a
    // reporter that could die — see `cmd_watch::Registered::watching`.
    o.insert("pid".into(), json!(0));
    o.insert("tick".into(), json!(util::now_iso()));
    o.insert("wake".into(), json!(true));
    o.insert("delivered".into(), json!(delivered));
    // Why it is still holding whatever it is holding. Empty when it is holding
    // nothing, because a reason for a thing that is not happening is noise —
    // see [`Tell::why`].
    o.insert(
        "holding".into(),
        json!(match spool.depth() {
            0 => String::new(),
            _ => why.to_string(),
        }),
    );
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd_watch::{Edge, Kind, Signal};
    use crate::Args;

    fn emit(kind: Kind, to: &str, subject: &str) -> Emit {
        Emit {
            edge: Edge::Up,
            signal: Signal::new(kind, subject, "because").to(to),
            held: 0,
            to: to.to_string(),
        }
    }

    fn spool_of(store: &Store, scope: &str) -> Spool {
        load(store, &key_for(scope))
    }

    /// **`core-014` §4, and it is half the noise.** A level nobody in
    /// particular is the addressee of belongs to `on-attention-raised`, which
    /// reaches a person for free. Waking a governor for it spends 208k tokens
    /// to tell somebody about work they are not answerable for.
    #[test]
    fn a_level_addressed_to_everyone_never_wakes_a_governor() {
        let env = util::isolated("wake-everyone");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();

        wake(&store, &[emit(Kind::Review, EVERYONE, "a-1"), emit(Kind::Review, "core", "a-2")], 0);

        assert_eq!(spool_of(&store, EVERYONE).depth(), 0, "nobody's level made nobody's spool");
        assert_eq!(spool_of(&store, "core").depth(), 1, "and the addressed one is held for its seat");
    }

    /// **The record is written before anything is attempted**, which is the one
    /// place this path is deliberately unlike `wsp watch`. `attention::tick`
    /// saves its ledger first on purpose, so an emit that is not written down
    /// here before the send is an emit the ledger already believes was told —
    /// and at-most-once is affordable for a hook and not for a reader that is
    /// asleep.
    ///
    /// There is no herdr in a test, so nothing can be delivered: what is left
    /// on disk is exactly what would survive the process dying mid-send.
    #[test]
    fn what_the_pass_produced_is_on_disk_before_anything_is_attempted() {
        let env = util::isolated("wake-durable");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();

        wake(&store, &[emit(Kind::NeedsAPerson, "core", "a-1")], 0);

        let rec = store.watches();
        let held = rec.get(&key_for("core")).expect("a record for the seat");
        assert_eq!(held.get("spool").and_then(|v| v.as_array()).map(Vec::len), Some(1));
        assert_eq!(held.get("delivered").and_then(serde_json::Value::as_u64), Some(0), "nothing arrived");
    }

    /// **The spool is the record of delivery, not of the send.** Nobody holds
    /// the post here, so the send cannot happen; the entry stays, and the next
    /// tick — or the seat somebody fills tomorrow morning — gets it. An entry
    /// that cleared on the attempt would be `core-017`'s failure 1 wearing a
    /// different hat.
    #[test]
    fn a_wake_nobody_took_is_still_owed_and_the_count_says_so() {
        let env = util::isolated("wake-unsent");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();

        for tick in 0..3 {
            wake(&store, &[emit(Kind::Review, "core", &format!("a-{tick}"))], tick * 60);
        }
        assert_eq!(spool_of(&store, "core").depth(), 3, "three ticks, three facts, none delivered and none lost");
        let rec = store.watches();
        let held = rec.get(&key_for("core")).unwrap();
        assert_eq!(held.get("delivered").and_then(serde_json::Value::as_u64), Some(0));
    }

    /// **The retry refusal is bypassed, and this is the assertion that it is.**
    ///
    /// `wsp tell` and `wsp govern --tell` both ask [`crate::cmd_agent::Sent::already_sent`]
    /// and refuse a sentence that has just reached this pane, returning exit
    /// code 0 — right for a person retrying a send that never failed, and for a
    /// wake it is a silent drop reported as delivery. A level re-raising on the
    /// same subject with the same wording is an ordinary thing for a level to
    /// do, so the two paths have to disagree, and this shows them disagreeing:
    /// the verb a person uses would refuse the second one, and the wake path
    /// never asks the question.
    #[test]
    fn the_same_wake_twice_is_owed_twice_where_a_person_would_be_refused() {
        let env = util::isolated("wake-twice");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let text = "a-1  finished and waiting on you";

        // What the person's verb would say about sending this a second time.
        let args = crate::Args::parse(vec!["wsp".into(), "tell".into()]);
        let sent = crate::cmd_agent::Sent::new("core", "the core seat", "w1:p1", "core", text, &args);
        assert_eq!(sent.already_sent(&store), None);
        store.log_event(
            "agent-told",
            json!({ "target": "core", "pane": "w1:p1", "chars": text.len(), "id": sent.id, "at": util::epoch_secs() }),
        );
        assert!(sent.already_sent(&store).is_some(), "`wsp tell` would now refuse this sentence");

        // The wake path, asked the same thing twice, owes it twice.
        wake(&store, &[emit(Kind::Review, "core", "a-1")], 0);
        wake(&store, &[emit(Kind::Review, "core", "a-1")], 60);
        assert_eq!(spool_of(&store, "core").depth(), 2, "both are owed; neither was swallowed as a repeat");
    }

    fn holding(store: &Store, scope: &str) -> String {
        store
            .watches()
            .get(&key_for(scope))
            .and_then(|v| v.get("holding"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// **A seat that is owed something is looked at every tick, not only when
    /// something new happens in its scope.**
    ///
    /// Driven, and it is the failure a unit test would not have found: a wake
    /// held because the seat was mid-turn sat for two minutes with its record's
    /// stamp frozen, because nothing else had happened in that scope. Both the
    /// retry and the escalation are properties of the record rather than of the
    /// news, so the record has to be what decides who gets visited — otherwise
    /// a held wake waits on *unrelated* news, and `--defer-max` never fires at
    /// all on the quiet fleet it exists for.
    ///
    /// A `flag` is the right subject because the table spools it: it is owed to
    /// somebody and never justifies a wake on its own, so only escalation can
    /// ever get it out.
    #[test]
    fn a_seat_that_is_owed_something_is_reconsidered_on_a_tick_with_no_news() {
        let env = util::isolated("wake-sweep");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();

        wake(&store, &[emit(Kind::Flag, "core", "a-1")], 0);
        assert_eq!(holding(&store, "core"), NOT_YET, "a flag never wakes anybody by itself");

        // Four hours later, with nothing at all having happened since.
        wake(&store, &[], crate::cmd_watch::DEFER_MAX);
        assert_ne!(
            holding(&store, "core"),
            NOT_YET,
            "escalation was reached, so a delivery was attempted and the reason it failed is on the record",
        );
        assert_eq!(spool_of(&store, "core").depth(), 1, "and it is still owed, because nothing took it");
    }

    // ---- what a wake is typed into, and what it says when it arrives -------

    /// **The whole path, over a socket, into a seat in each of the six states
    /// a pane can be in.**
    ///
    /// Everything else here asserts a predicate; this asserts what actually
    /// leaves. It is the shape `core-021` d2 was driven in and the reason two
    /// of that group's faults were found by driving rather than by reading —
    /// the gate read a census row that could not tell a working agent from an
    /// idle one, and nothing above the socket could have said so.
    ///
    /// What it pins: the wake reaches an `Idle` seat and nothing else reaches
    /// anything; what arrives carries [`preamble`] and the news under it; and
    /// there is not an escape code in it, because the far end is a composer.
    #[test]
    fn a_wake_reaches_an_idle_seat_and_arrives_as_something_a_composer_can_read() {
        use crate::fake::{Fake, Spot, Stage};
        use crate::place::State;

        let env = util::isolated("wake-driven");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();

        // One seat per state, each governing a scope of its own, so one pass
        // asks the question six times and the answers cannot be confused.
        let states = [
            (State::Idle, "s-idle"),
            (State::Working, "s-working"),
            (State::Blocked, "s-blocked"),
            (State::Starting, "s-starting"),
            (State::Gone, "s-gone"),
            (State::Unknown, "s-unknown"),
        ];
        let mut stage = Stage::new();
        for (i, (state, scope)) in states.iter().enumerate() {
            let (space, pane) = (format!("w{}", i + 1), format!("w{}:p1", i + 1));
            let mut spot = Spot::agent(&pane, "claude", scope, *state);
            spot.space = space.clone();
            stage.put(spot);
            cmd_govern::take(&store, scope, &space, &pane).unwrap();
        }
        let fake = Fake::bind(env.path("herdr.sock"), stage).expect("a socket");
        let (k, v) = fake.socket_env();
        std::env::set_var(k, v);

        let news: Vec<Emit> =
            states.iter().map(|(_, scope)| emit(Kind::Review, scope, &format!("{scope}-1"))).collect();
        wake(&store, &news, 0);

        // One delivery, to the one seat that will take a prompt.
        let told: Vec<crate::fake::Asked> =
            fake.asked().into_iter().filter(|a| a.verb == crate::fake::Verb::Tell).collect();
        assert_eq!(told.len(), 1, "only the idle seat was typed at: {told:?}");
        assert_eq!(told[0].seat.as_ref().map(|s| s.as_str()), Some("w1:p1"));

        let said = &told[0].said;
        assert!(said.contains("nobody typed this"), "the preamble rides the wake: {said}");
        assert!(said.contains("`s-idle`"), "and names the scope this seat holds: {said}");
        assert!(said.contains("s-idle-1"), "and the news is under it: {said}");
        assert!(!said.contains('\x1b'), "a composer is not a terminal: {said:?}");

        // And the five that were held say which of the five reasons it was, so
        // `--status` and `doctor` can tell a fault from the design.
        //
        // **`Gone` is not one of the five, and `wsp-148` is why.** A pane that
        // has gone is not a fact about a pane any more — it is a seat nobody is
        // in, and the reconciler is putting somebody in it. So it says the count
        // and whether the daemon is the thing that will do it, and the other four
        // keep their own sentences, because they are all about a seat that *is*
        // there.
        for (state, scope) in states.iter().skip(1) {
            assert_eq!(spool_of(&store, scope).depth(), 1, "{scope} is still owed it");
            let want = match state {
                State::Gone | State::Empty => unseated(&store, scope),
                other => held_because(*other).expect("a refusal has a sentence").to_string(),
            };
            assert_eq!(holding(&store, scope), want, "{scope} says why");
        }
        assert!(
            holding(&store, "s-gone").contains("unseated · 1 held ·"),
            "and the unseated reading counts what is held for it: {}",
            holding(&store, "s-gone")
        );
        assert!(
            holding(&store, "s-gone").ends_with("daemon down"),
            "with no daemon running, nothing is coming to fill it — which is the one a person can act on: {}",
            holding(&store, "s-gone")
        );
    }

    /// **The shape no fixture in this file supplies: a record carrying only `last`.**
///
/// Every test here writes a *filled* record, which is why this reached the trunk.
/// `wsp-174` found it live — `tokenhub-spec-sync` reading `no seat on this scope`
/// while `governors.json` for the same scope carried `unseated: 156` and a live
/// claim — and this is the third time across `wsp-148` that a fixture has supplied
/// its own `State` and hidden the real one.
///
/// **The record is written by [`crate::cmd_govern::vacate`] and not by hand**,
/// because the hand-written version is exactly what the other fixtures do wrong:
/// a real vacate moves the pane under `last` and leaves nothing at the top level,
/// and that is the whole fact under test.
///
/// **Both halves, because the drop and the reading are separable.** `say`
/// returning `None` lost the sentence; `say` returning a report that says `no seat
/// on this scope` keeps it and lies about it. A test that only checked the depth
/// would pass against the second.
#[test]
fn a_scope_whose_governor_vacated_keeps_what_is_said_to_it() {
    let env = util::isolated("wake-vacated");
    let store = Store::at(env.home(), env.state());
    store.ensure_dirs().unwrap();

    // A seat somebody was in, then gone — so the record is `last` and nothing else.
    store.set_governor(
        "core",
        json!({ "workspace": "w9", "pane": "cpd-9", "host": util::hostname(), "kind": "claude" }),
    );
    assert!(crate::cmd_govern::vacate(&store, "core"));
    let rec = &store.governors()["core"];
    assert!(
        rec.get("workspace").is_none() && rec["last"]["pane"] == "cpd-9",
        "the record really is only `last`: {rec}"
    );

    let report = say_through(&store, &Gone, "core", "somebody needs an answer", None);
    assert_eq!(
        spool_of(&store, "core").depth(),
        1,
        "a vacancy is later, not nowhere: the sentence is owed"
    );
    assert!(
        report.why.contains("unseated"),
        "and it says the seat is standing empty rather than that there is nowhere to put it: {}",
        report.why
    );
    assert!(
        holding(&store, "core").contains("unseated"),
        "which is what `wsp watch --status` prints: {}",
        holding(&store, "core")
    );

    let _ = std::fs::remove_dir_all(&env.state());
}

    /// A store with `core`'s spool holding three lines nobody is owed — `edge:
    /// left`, a level that moved on — and nothing else.
    ///
    /// **Built from the emit, not from `held_for` or a message**, and that is the
    /// whole of it: a message always counts as owed, so every fixture that used
    /// one went through the typing path and passed against a defect that lived
    /// only on the path that types nothing. `tokenhub-spec-sync`'s live shape.
    fn withheld(name: &str) -> (util::Isolated, Store, String) {
        let env = util::isolated(name);
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let key = key_for("core");
        for i in 0..3 {
            let e = Emit {
                edge: Edge::Left,
                signal: Signal::new(Kind::Review, "m-1", "the level moved on").to("core"),
                held: i,
                to: "core".into(),
            };
            store.update_watch(&key, |rec| {
                Spool::append(rec, vec![Spooled::of(0, Line::News(e))]);
            });
        }
        assert_eq!(owed_to_a_seat(&store, &key), 0, "withheld, so nothing is owed and the gate fires");
        assert_eq!(load(&store, &key).depth(), 3, "though three are held");
        (env, store, key)
    }

    /// A seat somebody was in and has left — vacated by `cmd_govern::vacate`
    /// rather than written by hand, so the pane is under `last` the way a real
    /// vacate leaves it.
    fn vacated(store: &Store) {
        store.set_governor(
            "core",
            json!({ "workspace": "w9", "pane": "cpd-9", "host": util::hostname(), "kind": "claude" }),
        );
        assert!(cmd_govern::vacate(store, "core"));
    }

    /// Marks the record so a test can tell whether the next pass wrote it — the
    /// stamp's `tick` is whole seconds and a pass is faster than that.
    fn mark(store: &Store, key: &str) {
        store.update_watch(key, |rec| rec["tick"] = json!("unwritten"));
    }

    fn written(store: &Store, key: &str) -> bool {
        store.watches()[key]["tick"] != "unwritten"
    }

    /// **A seat nothing will fill says so, rather than `counting`.** A kind
    /// compound cannot run is never reseated, so the sentence that promises a
    /// count is the one thing it must not say.
    #[test]
    fn a_vacant_seat_of_a_kind_compound_cannot_run_says_so() {
        let (_env, store, _key) = withheld("wakes-codex");
        vacated(&store);
        let mut rec = store.governors()["core"].clone();
        rec["last"]["kind"] = json!("codex");
        store.set_governor("core", rec);
        let said = unseated(&store, "core");
        assert!(said.ends_with("a codex seat compound cannot run"), "{said}");
    }

    /// **A vacant seat that owes nothing and is on no running list is never
    /// counted, so it must not say `counting`.** `compound-parity`, live on
    /// 2026-10-06: vacated a month, eight lines held and none owed, reading
    /// `counting` while the reconciler never looked at it.
    #[test]
    fn a_vacant_seat_that_owes_nothing_says_nobody_is_coming() {
        let (_env, store, _key) = withheld("wakes-unowed");
        vacated(&store);
        assert!(!crate::repair::will_seat(&store, "core"), "the fixture is the case: nothing owed, no list");
        let said = unseated(&store, "core");
        assert!(said.ends_with("nothing owed, so nobody is coming"), "{said}");
    }

    /// **`wsp-179`'s third item: a whole spool withheld *and* an empty seat,
    /// asserting the sentence.** The at-rest gate returned before the only writer
    /// of `holding`, so this scope kept whatever it last said for ever — and the
    /// defect hid behind every fixture that appended a message, because a message
    /// is owed and goes the other way. Through `deliver_to` with no emits, which
    /// is the pass's own call for a scope with nothing new.
    #[test]
    fn a_scope_whose_whole_spool_is_withheld_and_whose_seat_is_empty_says_unseated() {
        let (env, store, key) = withheld("wakes-withheld");
        vacated(&store);
        mark(&store, &key);

        deliver_to(&store, &Gone, "core", &[], 0);
        let said = holding(&store, "core");
        assert!(said.starts_with("unseated · 3 held"), "empty is not quiet: {said}");

        // **Bounded, which is the reason the gate exists.** The same sentence on
        // the next pass is no write at all.
        mark(&store, &key);
        deliver_to(&store, &Gone, "core", &[], 20);
        assert!(!written(&store, &key), "an at-rest seat must not cost a store write every twenty seconds for ever");
        assert_eq!(holding(&store, "core"), said);
        let _ = std::fs::remove_dir_all(&env.state());
    }

    /// **`wsp-179`'s second item: a stale sentence is overwritten, not only an
    /// absent one filled.** `compound-parity` held *no seat on this scope* for
    /// twenty-one hours — a sentence from the `seat_of_scope` reader `4719681`
    /// replaced, false because its record remembers a pane. And the same writer
    /// has to run the other way: a seat refilled while everything it holds is
    /// withheld must stop reading `unseated` about the governor sitting in it.
    #[test]
    fn a_withheld_scope_corrects_a_sentence_that_is_no_longer_true_in_either_direction() {
        let (env, store, key) = withheld("wakes-withheld-stale");
        vacated(&store);
        store.update_watch(&key, |rec| rec["holding"] = json!("no seat on this scope"));

        deliver_to(&store, &Gone, "core", &[], 0);
        let said = holding(&store, "core");
        assert!(said.starts_with("unseated"), "a retired sentence is corrected rather than kept: {said}");

        // Refilled. Nothing is owed, so nothing is typed — and the seat is not
        // empty, so it must not go on saying it is.
        deliver_to(&store, &Working, "core", &[], 20);
        assert_eq!(holding(&store, "core"), NOT_YET, "a seat with somebody in it is not a vacancy");
        mark(&store, &key);
        deliver_to(&store, &Working, "core", &[], 40);
        assert!(!written(&store, &key), "and saying so once is enough");
        let _ = std::fs::remove_dir_all(&env.state());
    }

    /// **A seat nobody can be asked about is not reported empty, on either
    /// path.** The reconciler refuses to act on an unreadable seat, so a holding
    /// that said `unseated · … · counting` there would promise a governor nothing
    /// is sending. The typing path used to say exactly that: its locator found no
    /// agent when the backend was down and called the seat unseated, while
    /// `repair::reading` called it unreadable. One decider now, so both paths
    /// read the same.
    #[test]
    fn a_seat_nobody_can_ask_about_is_never_called_unseated() {
        let (env, store, key) = withheld("wakes-withheld-silent");
        vacated(&store);

        deliver_to(&store, &Silent, "core", &[], 0);
        let at_rest = holding(&store, "core");
        assert_eq!(at_rest, held_because(crate::place::State::Unknown).unwrap(), "at rest");

        // And with something owed, which is `Tell::deliver`'s path.
        store.update_watch(&key, |rec| {
            Spool::append(rec, vec![Spooled::of(0, Line::Note(crate::cmd_watch::Class::Message, "an answer".into()))]);
        });
        let report = deliver_to(&store, &Silent, "core", &[], 20);
        assert_eq!(report.why, at_rest, "typing path and at-rest path are one reading");
        assert_eq!(holding(&store, "core"), at_rest);
        let _ = std::fs::remove_dir_all(&env.state());
    }

    /// **A slot with no pane in it is a vacancy, not *no seat*.** `tooling`'s live
    /// record is `workspace: compound, pane: ""`, which the reconciler reads as
    /// unseated and fills; the typing path read it as no post at all. And a scope
    /// with no record is the one that genuinely has no seat, at rest as well.
    #[test]
    fn a_slot_with_no_pane_reads_unseated_and_a_scope_with_no_record_reads_no_seat() {
        let (env, store, key) = withheld("wakes-withheld-slot");
        deliver_to(&store, &Gone, "core", &[], 0);
        assert_eq!(holding(&store, "core"), NO_SEAT, "nobody has ever governed this scope");

        store.set_governor("core", json!({ "workspace": "compound", "pane": "", "host": util::hostname() }));
        store.update_watch(&key, |rec| {
            Spool::append(rec, vec![Spooled::of(0, Line::Note(crate::cmd_watch::Class::Message, "an answer".into()))]);
        });
        let report = deliver_to(&store, &Working, "core", &[], 20);
        assert!(report.why.starts_with("unseated"), "an empty slot is a vacancy: {}", report.why);
        let _ = std::fs::remove_dir_all(&env.state());
    }

    /// The three readings a seat can give [`crate::repair::reading`], as fakes.
    struct Gone;
    struct Working;
    struct Silent;

    impl crate::cycle::Seats for Gone {
        fn state(&self, _: &str) -> Option<crate::place::State> {
            Some(crate::place::State::Gone)
        }
    }
    impl crate::cycle::Seats for Working {
        fn state(&self, _: &str) -> Option<crate::place::State> {
            Some(crate::place::State::Working)
        }
    }
    impl crate::cycle::Seats for Silent {
        fn state(&self, _: &str) -> Option<crate::place::State> {
            None
        }
    }

    /// **The two readings `wsp-148` did not ask for and `wsp-148` caused.**
    /// `reseating` and `daemon down` were true of the two states its first
    /// version could produce, and untrue of the other two: a claim whose holder
    /// has already failed, and a seat with no claim on a live daemon whose count
    /// has not reached the threshold. The second of those told a reader to wait
    /// for a governor that was not going to be launched on that tick.
    #[test]
    fn a_seat_with_nothing_in_flight_says_counting_and_not_reseating() {
        let env = util::isolated("wakes-unseated");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();

        // Something owed, so the scope is one `unseated` is ever read for, and a
        // governor record so `seat_held` has something to read: a workspace and
        // an empty pane, which is `tooling`'s live shape.
        // A real pane, because `seat_held` refuses an empty one — and the point of
        // this test is the reading *past* that refusal.
        store.set_governor(
            "core",
            json!({ "workspace": "w9", "pane": "cpd-9", "host": util::hostname() }),
        );
        // Said through the real path rather than by stamping the field, because
        // what is under test is the sentence a reader gets.
        let why = |store: &Store| {
            say_through(store, &Gone, "core", "owed", None).why
        };
        assert_eq!(
            crate::cmd_govern::vacancy(&store.governors(), "core").reseating,
            None,
            "no claim: nothing is in flight"
        );
        assert!(
            why(&store).ends_with("daemon down") || why(&store).ends_with("counting"),
            "a live daemon with no claim is counting towards a threshold it has not reached — not waiting for a governor that is not coming this tick: {}",
            why(&store)
        );

        // Claimed, and the reseat under it failed: nothing is on its way, and
        // the next attempt is up to SEAT_CLAIMED_FOR away.
        crate::cmd_govern::claim_seat(&store, "core");
        assert!(why(&store).ends_with("reseating"), "{}", why(&store));
        crate::cmd_govern::reseat_failed(&store, "core");
        assert!(why(&store).ends_with("retrying"), "{}", why(&store));

        // **And a seat somebody closed is a decision, not a measurement.** The
        // marker made it true in the store while this sentence went on saying
        // `counting` — 201 held items on `tooling`, read as a seat the machine
        // was working towards filling.
        crate::cmd_govern::mark_stood_down(&store, "core");
        assert!(why(&store).contains("stood down at"), "{}", why(&store));
        assert!(
            !why(&store).ends_with("counting"),
            "and never both: {}",
            why(&store)
        );

        let _ = std::fs::remove_dir_all(&env.state());
    }

    /// **The gate is not "is a turn in flight", and this is the state that
    /// proved it.**
    ///
    /// `cmd_agent::tell` has refused a `Blocked` pane since robustness-083 and
    /// says why: a permission dialog holds the keyboard, so the text does not
    /// queue behind anything — it is typed *at the dialog*, where a sentence
    /// about what to do next can select an answer nobody chose. This path asked
    /// [`crate::place::State::turn_in_flight`], which is `Working` and nothing
    /// else, so a wake would have gone straight into that dialog.
    ///
    /// Asserted through [`held_because`] because that is the whole of what
    /// [`Tell::deliver`] asks — there is no second branch — so this is the gate
    /// and not a copy of it.
    #[test]
    fn a_wake_is_never_typed_at_a_seat_that_will_not_take_a_prompt() {
        use crate::place::State;

        // Exactly one state takes a wake, and it is the one every caller that
        // used to spell this `state == "idle"` was asking about.
        for state in [State::Empty, State::Starting, State::Working, State::Blocked, State::Gone, State::Unknown] {
            assert!(!state.will_take_a_prompt(), "{state:?}");
            let why = held_because(state).unwrap_or_else(|| panic!("{state:?} has no sentence"));
            assert_ne!(why, NOT_YET, "{state:?} is a refusal, not the ordinary state");
        }
        assert_eq!(held_because(State::Idle), None);

        // The two the old gate let through, named so a revert has to argue with
        // this rather than with a list.
        assert!(!State::Blocked.turn_in_flight(), "which is why the old gate delivered into a modal");
        assert!(!State::Unknown.turn_in_flight(), "and into a pane herdr could not be asked about");

        // And each refusal reads as its own repair: a person at a keyboard, a
        // spawn, and a reporter that could not be asked are three different
        // errands.
        let seen: std::collections::BTreeSet<&str> =
            [State::Working, State::Blocked, State::Starting, State::Empty, State::Unknown]
                .into_iter()
                .filter_map(held_because)
                .collect();
        assert_eq!(seen.len(), 5, "one sentence per repair: {seen:?}");
    }

    /// **A governor is told what woke it, and it is told on the wake.**
    ///
    /// `core-019` question 3. A wake arrives through `Kind::tell` — herdr
    /// typing at the pane — so it is indistinguishable from a person at a
    /// keyboard, and nothing in the custodian work order, the brief or the
    /// handbook says a governor's session may receive prompts nobody typed.
    /// The clause rides the wake rather than the work order because a `/clear`
    /// ed governor is still in the slot, still addressed and still woken, and
    /// the work order it read is gone.
    #[test]
    fn a_told_wake_says_what_it_is_and_who_it_is_for() {
        let said = preamble("core");

        assert!(said.contains("nobody typed this"), "{said}");
        assert!(said.contains("`core`"), "it names the scope this seat holds: {said}");
        assert!(said.contains("No reply is expected"), "a wake is not a question: {said}");
        // The off switch, named where a governor reliably reads. `core-019`
        // question 2: the seat is the switch, and a switch nothing points at is
        // one nobody finds — which is how `wsp watch` itself came to be started
        // by nobody.
        assert!(said.contains("wsp govern core --clear"), "{said}");
        assert!(said.contains("wsp watch --drain"), "and what else is being held: {said}");
    }

    /// **Painting is a fact about the destination.** The far end of this sink
    /// is a composer, and `Stream` used to build one `Paint::new()` for every
    /// sink — a question about *this* process's stdout. `wsp doctor` tells a
    /// person to run `wsp daemon` by hand when the daemon has wedged, and a
    /// daemon started that way has a tty, so every wake would have arrived as
    /// escape codes in a governor's prompt.
    ///
    /// That a `Stream` asks its sink at all is asserted next door, against a
    /// sink that definitely paints — see
    /// `cmd_watch::tests::a_stream_is_painted_for_its_sink_and_not_for_this_process`.
    /// This is the half that says which answer *this* sink gives.
    #[test]
    fn a_wake_is_never_painted_for_a_terminal_that_is_not_there() {
        let env = util::isolated("wake-paint");
        let store = Store::at(env.home(), env.state());
        let tell = Tell::new(&store, &Gone, "core");

        assert!(!Sink::paint(&tell).on(), "a composer cannot read escape codes");
    }

    /// The other half: a seat holding nothing costs nothing. The sweep above
    /// visits every seat with a record, and a record rewritten every twenty
    /// seconds for ever is a store write per seat per tick for no reason.
    #[test]
    fn a_seat_at_rest_is_not_rewritten_every_tick() {
        let env = util::isolated("wake-rest");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        store.set_watch(&key_for("core"), json!({ "scope": "core", "wake": true, "delivered": 3, "spool": [] }));

        let before = store.watches().get(&key_for("core")).cloned();
        wake(&store, &[], 60);
        assert_eq!(store.watches().get(&key_for("core")).cloned(), before, "nothing to say, nothing written");
    }

    /// A wake held is not a wake lost, and the two ticks that hold it must not
    /// each add a copy of what the first one was already holding.
    #[test]
    fn holding_a_wake_does_not_multiply_it() {
        let env = util::isolated("wake-nodup");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();

        wake(&store, &[emit(Kind::Review, "core", "a-1")], 0);
        for tick in 1..5 {
            wake(&store, &[], tick * 60);
        }
        assert_eq!(spool_of(&store, "core").depth(), 1, "one fact, however many ticks failed to deliver it");
    }

    // ---- one path to a governor, and a turn for a receipt --------------------

    /// A store with one task in one project and a seat on that project, and the
    /// fake herdr behind it.
    ///
    /// **The project is what the seat is taken on and not the worklist**, because
    /// `cycle`'s walk falls back to the first member's project and that is the
    /// case worth driving: a run whose governor is the one above its work rather
    /// than its own.
    fn a_governor(tag: &str, state: crate::place::State) -> (util::Isolated, Store, crate::fake::Fake) {
        use crate::fake::{Fake, Spot, Stage};
        let env = util::isolated(tag);
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let mut t = crate::model::Task::new("wsp-146 one path to a governor", "wsp-146");
        t.project = Some("demo".into());
        store.save_task(&t).unwrap();
        // The asker is an agent, not a person at a shell, and the pane it is in
        // is named rather than left to the environment: `wsp ask` refuses a
        // question that does not say who is waiting on it, and `my_pane()` reads
        // `HERDR_PANE_ID`, which every other test in this binary also writes.
        // A member holding the task is the whole of what makes this a member's
        // question — an answer goes home to the asker's task, so this is what
        // the return path is built on.
        std::env::set_var("HERDR_PANE_ID", "w1:p2");
        store.set_binding("w1:p2", json!({ "pane": "w1:p2", "task_id": "wsp-146" }));

        let mut spot = Spot::agent("w1:p1", "claude", "demo", state);
        spot.space = "w1".into();
        let mut stage = Stage::new();
        stage.put(spot);
        cmd_govern::take(&store, "demo", "w1", "w1:p1").unwrap();
        let fake = Fake::bind(env.path("herdr.sock"), stage).expect("a socket");
        let (k, v) = fake.socket_env();
        std::env::set_var(k, v);
        (env, store, fake)
    }

    fn told_to(fake: &crate::fake::Fake) -> Vec<String> {
        fake.asked()
            .into_iter()
            .filter(|a| a.verb == crate::fake::Verb::Tell)
            .map(|a| a.said)
            .collect()
    }

    fn told_count(fake: &crate::fake::Fake) -> usize {
        fake.asked().iter().filter(|a| a.verb == crate::fake::Verb::Tell).count()
    }

    fn delivered_to(store: &Store, scope: &str) -> usize {
        store
            .watches()
            .get(&key_for(scope))
            .and_then(|v| v.get("delivered"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as usize
    }

    /// **The row's whole claim, in one drive.** A governor mid-turn, a member
    /// asking it a question, a run telling it a verdict — and nothing refused,
    /// nothing lost and nothing counted as arrived.
    ///
    /// This is the acceptance test `wsp-146` names, and every word in it is a
    /// failure that used to happen: `wsp ask` reached the seat by asking the
    /// attention pass to derive `unanswered` a minute later, `cycle`'s verdict
    /// went through `govern --tell`, which **refuses a `Working` seat** — so the
    /// verdict came back as a hand raised on a member of the run — and the
    /// entry that reached a seat cleared on the *send* rather than on anything
    /// the seat did.
    #[test]
    fn a_question_and_a_verdict_reach_a_busy_governor_on_its_next_idle_and_only_a_turn_closes_them() {
        use crate::place::State;

        let (_env, store, fake) = a_governor("wake-asked", State::Working);

        // The member's question, through the verb an agent is told to use.
        let asked = Args::parse(vec![
            "wsp".into(),
            "wsp-146".into(),
            "may I land this, or does the barrier want a verifier first?".into(),
        ]);
        assert_eq!(crate::cmd_message::ask(&store, &asked), 0, "the question is raised");

        // The run's verdict, through the half of `cycle::tell` that decides
        // where a verdict goes — a two-line wrapper the test cannot reach
        // through `tell` itself, which stops at recording the sentence.
        let mut w = crate::model::Worklist::new("run", "tonight's run");
        w.set_status(crate::model::WorklistStatus::Running);
        w.set_groups(&[crate::model::Group {
            members: vec!["wsp-146".into()],
            ..crate::model::Group::default()
        }]);
        store.save_worklist(&w).unwrap();
        assert!(
            crate::cycle::hand_it_to_the_governor(&store, &w, "group 1 is at its barrier — go on or hold"),
            "the run's own governor took the verdict, having found the seat above its project"
        );

        // **Nothing was refused and nothing was typed.** The seat is mid-turn,
        // and mid-turn is the state this row exists for: `cycle` used to be told
        // *not ready — working* here and answered by raising a hand on `wsp-146`.
        assert!(told_to(&fake).is_empty(), "a busy seat is not typed at");
        assert_eq!(spool_of(&store, "demo").depth(), 2, "both sentences are owed, and the record is the record");
        assert_eq!(delivered_to(&store, "demo"), 0, "and nothing has arrived");
        assert_eq!(
            crate::message::raised(&store).len(),
            1,
            "one record: the question. The verdict has no record and is not owed an answer"
        );

        // The seat comes free. The next pass is what delivers, and it delivers
        // both in one typing — the flush is a batch, and a governor is not made
        // to pay two context reads for one barrier.
        fake.moves(&crate::place::Seat::new("w1:p1"), State::Idle);
        wake(&store, &[], util::epoch_secs());

        let said = told_to(&fake);
        assert_eq!(said.len(), 1, "one typing, carrying everything held: {said:?}");
        assert!(said[0].contains("may I land this"), "the question is in it: {}", said[0]);
        assert!(said[0].contains("at its barrier"), "and so is the verdict: {}", said[0]);
        // The return path rides the message rather than being something the
        // governor has to know by convention — `worklist-013` cost 2h14m for
        // exactly that omission.
        assert!(said[0].contains("wsp answer m-"), "and it says how to answer it: {}", said[0]);
        assert_eq!(spool_of(&store, "demo").depth(), 0, "a turn started, so both are delivered");
        assert_eq!(delivered_to(&store, "demo"), 2, "and the count says what the seat did, not what wsp sent");
    }

    /// **`wsp-170`: a question arrives whole, or says how to read it whole.**
    ///
    /// Two multi-paragraph `wsp ask`s reached the governor seat as their first
    /// paragraph and a `wsp answer` hint; the choice they asked about stayed in
    /// the asker's `--from` file. The cut was `Message::title` in the line typed
    /// at the seat, and a seat must never have to read the asker's screen to
    /// learn what it is being asked.
    #[test]
    fn a_three_paragraph_question_reaches_the_seat_with_its_choice_or_the_verb_that_shows_it() {
        use crate::place::State;

        let (_env, store, fake) = a_governor("wake-whole", State::Idle);
        let question = "cpd-251 wants to widen the scope of wsp-146 to the cycle verb.\n\n\
             The change touches cycle.rs and wake.rs, and a second lane is already in wake.rs.\n\n\
             Same change or separate claims? Reply A for one claim, B for two.";
        let asked = Args::parse(vec!["wsp".into(), "wsp-146".into(), question.into()]);
        assert_eq!(crate::cmd_message::ask(&store, &asked), 0, "the question is raised");

        let said = told_to(&fake);
        assert_eq!(said.len(), 1, "and typed at the idle seat once: {said:?}");
        assert!(said[0].contains("Same change or separate claims? Reply A for one claim, B for two."),
            "the choice, which is in the last paragraph, is what the seat reads: {}", said[0]);

        // And past the length where it is cut, the choice is still the end the
        // seat reads, and the verb that prints the middle is named.
        let long = format!("{}\n\n{}\n\nA or B: one claim or two?", "opening ".repeat(40), "middle ".repeat(400));
        use crate::message::{About, Message, Party, Waiting};
        let q = Message::question(Party::seat("cpd-251"), crate::message::Kind::Note, &long, Waiting::new("", "wsp-146"))
            .about(About::Task("wsp-146".into()));
        let typed = crate::cmd_message::for_a_governor(&q);
        assert!(typed.contains("A or B: one claim or two?"), "the question line survives the cut: {typed}");
        assert!(typed.contains(&format!("wsp ask {}", q.id)), "and so does the verb that shows it whole: {typed}");
        assert!(!typed.contains(&"middle ".repeat(100)), "the middle is what was cut");
    }

    /// **`wsp-166` point 2: a governor is typed decisions and nothing else.**
    ///
    /// A member at review inside a running list, a question that was answered, a
    /// row handed to another seat — each was typed at a governor and each cost a
    /// context read to learn nothing. They stay in the spool, so the drain and
    /// the logs still have them; the seat hears the hold, and what is blocked.
    #[test]
    fn a_governor_is_typed_the_hold_and_the_blocked_and_not_what_a_run_or_an_answer_already_settled() {
        use crate::place::State;

        let (_env, store, fake) = a_governor("wake-decisions", State::Idle);
        let mut w = crate::model::Worklist::new("run", "tonight's run");
        w.set_status(crate::model::WorklistStatus::Running);
        w.set_groups(&[crate::model::Group {
            members: vec!["wsp-146".into()],
            ..crate::model::Group::default()
        }]);
        store.save_worklist(&w).unwrap();

        let mut answered = emit(Kind::Unanswered, "demo", "wsp-146");
        answered.edge = Edge::Down;
        let mut handed = emit(Kind::Blocked, "demo", "wsp-146");
        handed.edge = Edge::Left;
        let now = util::epoch_secs();
        wake(&store, &[emit(Kind::Review, "demo", "wsp-146"), answered, handed], now);

        assert!(told_to(&fake).is_empty(), "a member's review, an answer and a hand-off type nothing: {:?}", told_to(&fake));
        assert_eq!(spool_of(&store, "demo").depth(), 3, "and all three are still there for the drain");
        assert_eq!(owed_to_a_seat(&store, &key_for("demo")), 0, "none of them is owed");

        let held = say(&store, "demo", "group 1 is at its barrier — hold", None);
        assert!(held.arrived(), "the hold went: {held:?}");
        let said = told_to(&fake);
        assert_eq!(said.len(), 1, "one typing: {said:?}");
        assert!(said[0].contains("hold"), "{}", said[0]);
        assert!(!said[0].contains("review") && !said[0].contains("cleared"), "and nothing the run settled rode along: {}", said[0]);
        assert_eq!(spool_of(&store, "demo").depth(), 3, "the withheld three outlive the typing");

        // And a review outside any run is still the governor's own work.
        fake.moves(&crate::place::Seat::new("w1:p1"), State::Idle);
        wake(&store, &[emit(Kind::Review, "demo", "loose-1"), emit(Kind::Blocked, "demo", "wsp-146")], now + 1);
        let said = told_to(&fake);
        assert_eq!(said.len(), 2, "one more typing: {said:?}");
        assert!(said[1].contains("loose-1") && said[1].contains("blocked"), "{}", said[1]);
    }

    /// **A type that starts no turn is still owed, and it is not typed again.**
    ///
    /// The `robustness-093` case — the text is in the composer and nobody pressed
    /// return — which for this path used to be the end of the story: the entry
    /// cleared, the line said *delivered, no turn seen*, and a governor that had
    /// read nothing was counted as having been told. `takes = false` on the fake
    /// is herdr's `agent_prompt_stalled`, which is the only answer a backend that
    /// watched can give about it.
    #[test]
    fn a_type_that_starts_no_turn_is_still_owed_and_is_not_typed_again_inside_the_window() {
        use crate::place::State;

        let (_env, store, fake) = a_governor("wake-unread", State::Idle);
        // The seat takes the sentence and does nothing with it.
        let mut next = fake.stage();
        next.takes = false;
        fake.restage(next);

        let report = say(&store, "demo", "the barrier is open — start group 2", None);
        assert!(!report.arrived(), "nothing read it, so nothing arrived: {report:?}");
        assert_eq!(report.held, 1, "and it is still owed");
        assert!(told_count(&fake) == 1, "it was typed once");

        // Two more ticks, seconds apart. Each one *could* type it again, and each
        // one must not: the text is sitting in a composer, and typing at it again
        // appends to what is there. That is `worklist-010`'s harm — one
        // paragraph, delivered three times — arriving through the retry this row
        // introduced.
        for tick in 1..3 {
            wake(&store, &[], util::epoch_secs() + tick);
        }
        assert_eq!(told_count(&fake), 1, "one typing, however many ticks have gone by");
        assert_eq!(spool_of(&store, "demo").depth(), 1, "still owed");
        assert_eq!(delivered_to(&store, "demo"), 0, "and nothing has been counted as delivered");
        assert!(holding(&store, "demo") == UNREAD, "and the record says which state it is in: {}", holding(&store, "demo"));

        // A turn at last. It confirms the sentence that is already there, so it
        // clears the entry **without typing anything**.
        fake.moves(&crate::place::Seat::new("w1:p1"), State::Working);
        wake(&store, &[], util::epoch_secs() + 10);

        assert_eq!(told_count(&fake), 1, "a turn is not a second sentence");
        assert_eq!(spool_of(&store, "demo").depth(), 0, "and the governor has it");
        assert_eq!(delivered_to(&store, "demo"), 1, "delivered, on the turn and not on the type");
    }

    /// **A withheld entry's stale stamp must not swallow the next sentence
    /// (`wsp-180`).** A batch typed with no turn is stamped — every entry in the
    /// spool, withheld ones included — and a withheld entry is never settled, so
    /// its stamp outlived the batch for good. Every later message then read
    /// *a turn began since the last type*, was reported delivered, and was never
    /// typed: three verifier blocks in two days, each answered `nothing worth a
    /// wake yet · 0 held`, and a governor that was never told.
    #[test]
    fn a_stamp_on_something_withheld_does_not_stand_for_a_sentence_never_typed() {
        use crate::place::State;

        let (_env, store, fake) = a_governor("wake-stale-stamp", State::Idle);
        let mut next = fake.stage();
        next.takes = false;
        fake.restage(next);

        // Something the seat is not to be typed, held in the spool.
        let mut answered = emit(Kind::Unanswered, "demo", "wsp-146");
        answered.edge = Edge::Down;
        wake(&store, &[answered], util::epoch_secs());
        assert_eq!(told_count(&fake), 0, "withheld, so not typed");

        // A sentence is typed and no turn comes of it, which stamps the batch.
        say(&store, "demo", "the first block", None);
        assert_eq!(told_count(&fake), 1);
        // The governor reads it: a turn, which clears what was typed.
        fake.moves(&crate::place::Seat::new("w1:p1"), State::Working);
        wake(&store, &[], util::epoch_secs() + 5);
        assert_eq!(spool_of(&store, "demo").depth(), 1, "only the withheld one is left");
        assert_eq!(told_count(&fake), 1);

        // A later block, to a seat that is free again.
        let mut next = fake.stage();
        next.takes = true;
        fake.restage(next);
        fake.moves(&crate::place::Seat::new("w1:p1"), State::Idle);
        let report = say(&store, "demo", "a verifier found a problem: wsp-179 is blocked", None);

        let said = told_to(&fake);
        assert_eq!(said.len(), 2, "the second sentence was typed and not counted as read: {said:?} {report:?}");
        assert!(said[1].contains("wsp-179 is blocked"), "{}", said[1]);
    }

    /// **A turn confirms what was typed, and not a sentence raised after it
    /// (`wsp-180`).** The acknowledgement cleared the whole batch, so a message
    /// that arrived while the seat was mid-turn on the last one was counted as
    /// read without ever being at the seat.
    #[test]
    fn a_turn_confirms_what_was_typed_and_not_what_arrived_after_it() {
        use crate::place::State;

        let (_env, store, fake) = a_governor("wake-ack-fresh", State::Idle);
        let mut next = fake.stage();
        next.takes = false;
        fake.restage(next);

        say(&store, "demo", "the first block", None);
        assert_eq!(told_count(&fake), 1);
        fake.moves(&crate::place::Seat::new("w1:p1"), State::Working);

        let report = say(&store, "demo", "the second block", None);
        assert_eq!(delivered_to(&store, "demo"), 0, "the second was never at the seat: {report:?}");
        assert_eq!(spool_of(&store, "demo").depth(), 2, "so both are still owed, held for a seat that is mid-turn");

        assert_eq!(told_count(&fake), 1, "and nothing was typed into a seat that is mid-turn");
    }

    /// **The daemon may die between the type and the turn, and the sentence goes
    /// out once.**
    ///
    /// The brief's live case, as a test: the only thing that crosses a restart is
    /// the record, so what matters is that the record says *typed, no turn yet* and
    /// that the next process reads it rather than typing again. The second
    /// `Store` is the restarted daemon — same directory, no memory of the type.
    #[test]
    fn a_daemon_that_dies_between_the_type_and_the_turn_does_not_send_the_sentence_twice() {
        use crate::place::State;

        let (_env, store, fake) = a_governor("wake-restart", State::Idle);
        let mut next = fake.stage();
        next.takes = false;
        fake.restage(next);

        say(&store, "demo", "group 1 is at its barrier", None);
        assert_eq!(told_count(&fake), 1);
        fake.forget();

        // The process goes. Nothing but `watches.json` survives.
        let restarted = Store::at(_env.home(), _env.state());
        assert_eq!(spool_of(&restarted, "demo").depth(), 1, "the sentence is on disk, owed");
        wake(&restarted, &[], util::epoch_secs());
        assert_eq!(told_count(&fake), 0, "nothing re-typed: the record said it was already at the seat");

        fake.moves(&crate::place::Seat::new("w1:p1"), State::Working);
        wake(&restarted, &[], util::epoch_secs());
        assert_eq!(told_count(&fake), 0, "and the turn confirmed it rather than repeating it");
        assert_eq!(spool_of(&restarted, "demo").depth(), 0, "delivered exactly once, to a seat that read it");
        assert_eq!(delivered_to(&restarted, "demo"), 1);
    }

    /// **The pass does not announce to a seat that was already handed the
    /// record.**
    ///
    /// `wsp ask` puts the question in the seat's spool itself; the attention pass
    /// derives `unanswered` for the same record a minute later. Without this the
    /// governor is told twice, and every wake costs it 208k — measured on the seat
    /// that filed `core-014`, for the same price as a question from Ed. The second
    /// half is the half that matters most: the level still goes to a seat that was
    /// *not* handed it, which is what happens when the addressee stands down and
    /// the routing walks up a level.
    #[test]
    fn a_record_the_seat_was_handed_is_not_announced_to_it_again_but_still_goes_to_whoever_else() {
        use crate::fake::Spot;
        use crate::place::State;

        let (_env, store, fake) = a_governor("wake-handed", State::Working);
        // A second post, and a second seat, so "where else it still goes" is one
        // assertion rather than an argument.
        let mut other = Spot::agent("w2:p1", "claude", "other", State::Working);
        other.space = "w2".into();
        let mut stage = fake.stage();
        stage.put(other);
        fake.restage(stage);
        cmd_govern::take(&store, "other", "w2", "w2:p1").unwrap();

        let asked = Args::parse(vec!["wsp".into(), "wsp-146".into(), "which one?".into()]);
        assert_eq!(crate::cmd_message::ask(&store, &asked), 0);
        let record = crate::message::raised(&store)
            .into_iter()
            .find(|m| m.handed_to("demo"))
            .expect("the record remembers the seat it was handed to")
            .id;
        assert_eq!(spool_of(&store, "demo").depth(), 1, "the question itself");

        // The pass, a minute later, deriving the level for the same record — to
        // the seat that was handed it, and to one that was not.
        let level = crate::cmd_watch::Signal::new(Kind::Unanswered, "wsp-146", "w1:p1 waiting").of(&record);
        let mine = Emit { edge: crate::cmd_watch::Edge::Up, signal: level.clone(), held: 0, to: "demo".into() };
        let theirs = Emit { edge: crate::cmd_watch::Edge::Up, signal: level, held: 0, to: "other".into() };
        wake(&store, &[mine, theirs], util::epoch_secs());

        assert_eq!(spool_of(&store, "demo").depth(), 1, "the seat that has it is not told again");
        assert_eq!(spool_of(&store, "other").depth(), 1, "and the level still reaches everybody else");
        // And the edge that *goes down* is not suppressed: the governor's cue to
        // stop watching for the question is worth more than the saving.
        let down = Emit {
            edge: crate::cmd_watch::Edge::Down,
            signal: crate::cmd_watch::Signal::new(Kind::Unanswered, "wsp-146", "answered").of(&record),
            held: 30,
            to: "demo".into(),
        };
        wake(&store, &[down], util::epoch_secs() + 60);
        assert_eq!(spool_of(&store, "demo").depth(), 2, "the question closing is still news");
    }

    /// **With no seat on the scope there is no spool, and the caller keeps its own
    /// way.**
    ///
    /// The brief's fourth item, and the reason `say` returns an `Option`: a run
    /// with nobody governing it must still put its verdict where a person will
    /// see it, and that is a hand on the run's first member — which is what
    /// `cycle::tell` has always done and what it fell back to *for the wrong
    /// reason* before this row.
    #[test]
    fn with_no_post_on_the_scope_there_is_no_spool_to_write_to() {
        let env = util::isolated("wake-nopost");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();

        // **No post is not no spool any more**, and this is the sentence that has
        // to say so. `wsp-174`'s item 2: a scope nobody has a seat on is a place
        // sentences are held, because a post created tonight is told what it
        // missed. What it is *not* is a seat somebody is in.
        let report = say(&store, "nobody", "a verdict", None);
        assert_eq!(spool_of(&store, "nobody").depth(), 1, "held, not dropped");
        assert!(report.why.contains("no seat on this scope"), "{}", report.why);
        assert!(
            !store.governors().contains_key("nobody"),
            "and still no governor invented for one: {:?}",
            store.governors()
        );

        // The run's own answer, which is the one the brief keeps: the hand on
        // the member a person looking at the run is looking at.
        let mut t = crate::model::Task::new("a member", "m-1");
        t.project = Some("demo".into());
        store.save_task(&t).unwrap();
        let mut w = crate::model::Worklist::new("run", "run");
        w.set_status(crate::model::WorklistStatus::Running);
        w.set_groups(&[crate::model::Group { members: vec!["m-1".into()], ..crate::model::Group::default() }]);
        store.save_worklist(&w).unwrap();
        assert!(
            !crate::cycle::hand_it_to_the_governor(&store, &w, "group 1 is at its barrier"),
            "so the caller is told to find its own way, and does"
        );
    }

    /// **A list wsp runs is told on its own scope, with its project's seat
    /// filled and its own still empty** — `wsp-202`, and the wsp seat's second
    /// edge: what was already held for the project stays there, and only what is
    /// said from now on goes to the list. The list's post is about to be seated
    /// by the reconciler's next tick, so empty is later and not nowhere. A list
    /// somebody stood down is nowhere, and falls back to a hand as before.
    #[test]
    fn a_list_wsp_runs_is_told_on_its_own_scope_and_not_on_its_projects_seat() {
        use crate::place::State;

        let (_env, store, _fake) = a_governor("wake-own-post", State::Working);
        say(&store, "demo", "a question already held for the project", None);
        let mut w = crate::model::Worklist::new("run", "run");
        w.set_status(crate::model::WorklistStatus::Running);
        w.set_groups(&[crate::model::Group {
            members: vec!["wsp-146".into()],
            agent: "claude".into(),
            ..crate::model::Group::default()
        }]);
        store.save_worklist(&w).unwrap();

        assert!(crate::cycle::hand_it_to_the_governor(&store, &w, "group 1 is at its barrier"));
        assert_eq!(spool_of(&store, "run").depth(), 1, "the verdict waits for the list's own governor");
        assert_eq!(spool_of(&store, "demo").depth(), 1, "and what the project already held stays where it was");

        store.set_governor("run", json!({ "host": util::hostname(), cmd_govern::STOOD_DOWN: util::now_iso() }));
        assert!(
            !crate::cycle::hand_it_to_the_governor(&store, &w, "group 1 is at its barrier"),
            "a stood-down list has nobody coming, and the project is not its answer either"
        );
    }

    /// **What a verb is told, and what `wsp-146` made it say.**
    ///
    /// Two things a receipt has never had to carry. *Where it went* — a verb
    /// whose whole effect is on somebody else's screen must name the screen. And
    /// *when it will arrive*, which for the first time can honestly be "later":
    /// an agent that cannot tell later from never is how a question sits open for
    /// a night, and `worklist-013`'s 2h14m is the measurement of what that costs.
    #[test]
    fn a_receipt_names_the_post_and_says_whether_the_seat_has_it_yet() {
        use crate::place::State;

        let (_env, store, fake) = a_governor("wake-receipt", State::Working);
        let report = say(&store, "demo", "a question for you", None);

        let busy = report.said("the demo seat", &util::Paint::plain());
        assert!(busy.contains("the demo seat"), "it names the post: {busy}");
        assert!(busy.contains("1 still owed"), "and how much: {busy}");
        assert!(busy.contains("mid-turn"), "and why not yet, in the words doctor prints: {busy}");
        assert!(busy.contains("wsp watch --drain"), "and what else is held for it: {busy}");

        fake.moves(&crate::place::Seat::new("w1:p1"), State::Idle);
        let mut next = fake.stage();
        next.takes = false;
        fake.restage(next);
        let report = say(&store, "demo", "and another", None);
        let typed = report.said("the demo seat", &util::Paint::plain());
        assert!(typed.contains("typed"), "a seat that took it and read nothing says so: {typed}");
        assert!(!typed.contains("still owed 0"), "and it is still owed: {typed}");
    }
}
