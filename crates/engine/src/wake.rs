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
//! *into the dialog*, an agent still coming up refuses it, and a pane backend
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
pub use crate::cycle::Report;
pub use crate::waiting::held_because;
use crate::cmd_watch::{Emit, Line, Sink, Spec, Spool, Spooled, Stream, EVERYONE};
use crate::place::Seat as Pane;
use crate::refusals::{accepted, refuse};
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
pub fn key_for(scope: &str) -> String {
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
pub fn scopes_owed(store: &Store) -> Vec<(String, usize)> {
    let prefix = "wake:";
    store
        .watch_keys(prefix)
        .into_iter()
        .filter_map(|k| {
            let owed = owed_to_a_seat(store, &k);
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
pub fn wake(store: &Store, emits: &[Emit], at: i64) {
    wake_by(store, emits, at, &Primaries::real())
}

/// [`wake`], with where a user's primary is said — so a test can say who is
/// logged in here and what the hub does with a message.
fn wake_by(store: &Store, emits: &[Emit], at: i64, primaries: &Primaries) {
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
    for key in store.watch_keys("wake:") {
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
        // **`primary:<user>` is two places** (`wsp-418`): this spoke's own
        // primary when the user is the one logged in here, spooled and ordered
        // like a governor's under its slot; another user's, a message through
        // the hub, once per level.
        if let Some(user) = crate::governors::user_of_primary(&e.to) {
            if !primaries.is_here(user) {
                if e.edge == crate::cmd_watch::Edge::Up {
                    let text = crate::cmd_watch::said_plain(e, at);
                    let id = e.signal.record.clone().unwrap_or_else(|| format!("{}:{}", e.signal.kind.word(), e.signal.subject));
                    route_to_primary(store, primaries, user, &e.to, &text, e.signal.record.as_deref(), Some(&id));
                }
                continue;
            }
            mine.entry(crate::governors::PRIMARY.to_string()).or_default().push(e);
            continue;
        }
        mine.entry(e.to.clone()).or_default().push(e);
    }
    for (scope, theirs) in mine {
        deliver_to(store, crate::cycle::seats(), &scope, &theirs, at);
    }
    // And the tells held for seats that could not be reached, now that the pass
    // has looked at the fleet once more.
    flush_held_tells_now(store);
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
    //
    // Nothing to append is nothing to make durable, and no write (`wsp-320`):
    // a seat owed a backlog and visited with no news is the idle pass's whole
    // workload, so this is read and the one write below is the only one.
    let mut spool = match emits.is_empty() {
        true => load(store, &key),
        false => store.update_watch(&key, |rec| {
            let fresh: Vec<Spooled> =
                emits.iter().map(|e| Spooled::of(at, Line::News((*e).clone()))).collect();
            let s = Spool::append(rec, fresh);
            stamp(rec, scope, delivered, NOT_YET, &s);
            s
        }),
    };

    // **`wsp-694`: what is about something closed, or a repeat, goes** —
    // before it can be typed, and each one said in `cycle.log`.
    if let Some(left) = drop_stale(store, scope, &spool, at) {
        spool = left;
    }

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
    tell.batch = typed_batch.clone();
    let (sent, folded): (Vec<u64>, Vec<u64>) = {
        let mut stream = Stream::new(&spec, &mut tell);
        let sent = stream.tick(at, &mut spool).iter().map(|h| h.seq).collect();
        (sent, std::mem::take(&mut stream.folded))
    };
    let written = sent.len();
    if !folded.is_empty() {
        crate::store::log_line(store, &format!(
            "told {scope}: {} held entries were more than one delivery carries — typed a summary and the newest {written}, \
             folded {} for `wsp watch --drain`",
            written + folded.len(),
            folded.len()
        ));
    }
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
        if written > 0 || !folded.is_empty() {
            Spool::settle_these(rec, &sent);
            Spool::fold(rec, at, &folded);
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
    Report { scope: scope.to_string(), settled: written + folded.len(), held: left, typed_at: tell.typed_at, why, routed_to: None }
}

/// Take off a scope's spool what [`Spool::stale`] names, inside the lock, and
/// say each in `cycle.log` (`wsp-694`). Hands back what the record holds after,
/// or `None` when nothing went.
///
/// **Judged outside the lock, removed by identity inside it**, so the store
/// reads `closed` makes — a worklist, a row — never run under the watch lock,
/// and an entry a `--drain` took first is simply not there to remove.
fn drop_stale(store: &Store, scope: &str, spool: &Spool, at: i64) -> Option<Spool> {
    if spool.held.is_empty() {
        return None;
    }
    // One read per name: a backlog names the same row hundreds of times.
    let known = std::cell::RefCell::new(BTreeMap::<String, bool>::new());
    let closed = |name: &str| {
        if let Some(c) = known.borrow().get(name) {
            return *c;
        }
        let c = match store.worklist(name) {
            Some(w) => w.status() == crate::model::WorklistStatus::Done,
            None => store.task(name).is_some_and(|t| !t.status().is_open()),
        };
        known.borrow_mut().insert(name.to_string(), c);
        c
    };
    let gone = spool.stale(at, &closed);
    if gone.is_empty() {
        return None;
    }
    let seqs: Vec<u64> = gone.iter().map(|(q, _)| *q).collect();
    let left = store.update_watch(&key_for(scope), |rec| {
        Spool::settle_these(rec, &seqs);
        Spool::of_json(rec.get("spool").unwrap_or(&Value::Null))
    });
    for (seq, why) in &gone {
        crate::store::log_line(store, &format!("told {scope}: dropped held entry {seq} unread — {why}"));
    }
    Some(left)
}

/// A seat ended for refusing is not the last to be typed its payload (`wsp-694`):
/// what was typed at it and never confirmed is folded, so its successor's first
/// wake is what arrived since and a count of these, not the same text again.
/// Returns how many were folded.
pub fn fold_what_was_typed(store: &Store, scope: &str) -> usize {
    let at = util::epoch_secs();
    let n = store.update_watch(&key_for(scope), |rec| {
        let held = Spool::of_json(rec.get("spool").unwrap_or(&Value::Null));
        let seqs: Vec<u64> = held.held.iter().filter(|h| h.typed > 0 && h.folded == 0).map(|h| h.seq).collect();
        Spool::fold(rec, at, &seqs);
        seqs.len()
    });
    if n > 0 {
        crate::store::log_line(store, &format!(
            "told {scope}: folded the {n} held entries typed at the refusing seat, so its successor is not handed the same text — `wsp watch --drain` reads them"
        ));
    }
    n
}

/// [`Report`] is in [`crate::cycle`] since wsp-438, the type the port's
/// [`Seats::say`] answers with; the spool's at-rest reading stays here.
impl Report {
    fn at_rest() -> Report {
        Report { scope: String::new(), settled: 0, held: 0, typed_at: None, why: NOT_YET.into(), routed_to: None }
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
/// [`crate::governors::seat_of_scope`], which stops at the top level of a record — so a
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
pub fn say(store: &Store, scope: &str, text: &str, record: Option<&str>) -> Report {
    say_through(store, crate::cycle::seats(), scope, text, record)
}

/// [`say`], with the port that reads the seat named — so a test can say what the
/// seat is doing rather than inherit whatever a machine with no backend says.
fn say_through(store: &Store, seats: &dyn Seats, scope: &str, text: &str, record: Option<&str>) -> Report {
    say_by(store, seats, scope, text, record, &Primaries::real())
}

/// [`say_through`], with where a user's primary is said.
fn say_by(store: &Store, seats: &dyn Seats, scope: &str, text: &str, record: Option<&str>, primaries: &Primaries) -> Report {
    let at = util::epoch_secs();
    // **A sentence for `primary:<user>`** (`wsp-418`): through the hub when it is
    // another user's, else this spoke's own primary slot, the same spool and
    // order path as any governor. The record's hop names the scope it was asked
    // for, which is the one the attention pass looks for.
    let slot;
    let (spool_scope, key) = match crate::governors::user_of_primary(scope) {
        Some(user) if !primaries.is_here(user) => {
            return route_to_primary(store, primaries, user, scope, text, record, record);
        }
        Some(_) => {
            slot = crate::governors::PRIMARY;
            (slot, key_for(slot))
        }
        None => (scope, key_for(scope)),
    };
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
    deliver_to(store, seats, spool_scope, &[], at)
}

/// Where a user's primary is, and how a sentence for another user's leaves this
/// spoke (`wsp-418`).
///
/// `primary:<user>` is this spoke's own primary slot when `<user>` is the user
/// logged in here (or nobody is: a spoke with no login has one primary, and it
/// is that), and another spoke's otherwise — which only the hub reaches.
struct Primaries<'a> {
    /// The user logged in here.
    me: Option<String>,
    /// A message for `primary:<user>` through the hub: the hub's id for it.
    route: &'a dyn Fn(&Store, &str, &str) -> Result<String, String>,
}

impl Primaries<'static> {
    fn real() -> Primaries<'static> {
        Primaries { me: crate::address::logged_in_user(), route: &through_the_hub }
    }
}

impl Primaries<'_> {
    fn is_here(&self, user: &str) -> bool {
        self.me.as_deref().map(str::trim).is_none_or(|me| me == user.trim())
    }
}

/// `text` to `to` (`primary:<user>`) as a routed message of the hub's
/// (`wsp-470`): the hub queues it for an offline spoke and tells it once.
fn through_the_hub(store: &Store, to: &str, text: &str) -> Result<String, String> {
    let home = wsp_client::hub_home().ok_or("no HOME")?;
    if !home.join(wsp_client::HUBS_FILE).exists() {
        return Err("not logged in to a hub: `wsp login <url>`".into());
    }
    let book = crate::address::Book::read(store);
    match crate::cmd_message::hub_tell(store, &home, &book, to, text, None, "wsp", None) {
        Some(Ok((id, ..))) => Ok(id),
        Some(Err(why)) => Err(why),
        None => Err(format!("`{to}` is not another user's primary")),
    }
}

/// A sentence for another user's primary, sent through the hub **once**: `once`
/// names it (a record's id, or a level's key), and a name sent before is not
/// sent again however many passes ask. What was sent is the record's `Sent` hop
/// to the scope, so the attention pass does not derive it again either.
///
/// A refusal is not remembered, so the next pass tries again: the sentence is
/// held on the record, and the report says why it has not gone.
fn route_to_primary(
    store: &Store,
    primaries: &Primaries,
    user: &str,
    scope: &str,
    text: &str,
    record: Option<&str>,
    once: Option<&str>,
) -> Report {
    let report = |settled: usize, why: String| Report {
        scope: scope.to_string(),
        settled,
        held: 0,
        typed_at: None,
        why,
        routed_to: Some(user.to_string()),
    };
    let sent_key = format!("routed:{scope}");
    if let Some(id) = once {
        let before = store.watches().get(&sent_key).and_then(|r| r.get(id)).is_some();
        if before {
            return report(0, "already sent through the hub".into());
        }
    }
    match (primaries.route)(store, scope, text) {
        Ok(hub_id) => {
            if let Some(id) = once {
                store.update_watch(&sent_key, |rec| {
                    if !rec.is_object() {
                        *rec = serde_json::json!({});
                    }
                    rec[id] = serde_json::Value::from(hub_id.clone());
                });
            }
            if let Some(id) = record {
                let _ = crate::message::sent(store, id, scope);
            }
            crate::store::log_line(store, &format!("told {scope}: sent to {user}'s primary through the hub ({hub_id})"));
            report(1, hub_id)
        }
        Err(why) => report(0, format!("the hub did not take it: {why}")),
    }
}

/// How a seat that has tells held for it reads right now.
pub enum Seen {
    /// Every backend answered and none of them has the seat: there is nobody to
    /// tell, now or later.
    Missing,
    /// The seat is there, in this state.
    Is(crate::place::State),
}

fn held_key(seat: &str) -> String {
    format!("{HELD_TELL}{seat}")
}

/// The spool a `wsp tell` to a seat on a silent machine waits in. Not `wake:`,
/// whose scopes are governors' and which the reconciler reseats (`scopes_owed`):
/// a held tell is addressed to a seat, and a seat is not a scope.
const HELD_TELL: &str = "held-tell:";

/// Hold a tell for a seat that cannot be reached, in the wake spool's own
/// record (`docs/remote-agents.md` section 3): written before anything else, so
/// a crash after the answer loses nothing. Returns how many tells the seat now
/// has held. **Only for a seat that exists** — `wsp tell` to one that does not is
/// refused before it comes here, and [`flush_held_tells`] drops what is left for
/// a seat that has gone for good.
pub fn hold_tell(store: &Store, seat: &str, text: &str) -> usize {
    hold_tell_for(store, seat, text, None)
}

/// [`hold_tell`], naming the row the seat holds (`wsp-659`): a seat that ends
/// with tells still held for it has them offered to the row's next seat, and
/// the row is how that seat is found once the old one's binding is gone.
pub fn hold_tell_for(store: &Store, seat: &str, text: &str, row: Option<&str>) -> usize {
    let at = util::epoch_secs();
    store.update_watch(&held_key(seat), |rec| {
        Spool::append(rec, vec![Spooled::of(at, Line::Note(crate::cmd_watch::Class::Message, text.to_string()))]);
        if let (Some(row), Some(o)) = (row.filter(|r| !r.is_empty()), rec.as_object_mut()) {
            o.insert("row".into(), json!(row));
        }
    });
    held_tells(store, seat).len()
}

/// How many tells each seat has held for it, queued for its next idle or for
/// its link (`wsp-659`): what `wsp wip` draws as `· N tells queued`. Seats with
/// none are left out.
pub fn queued_tells(store: &Store) -> BTreeMap<String, usize> {
    store
        .watch_keys(HELD_TELL)
        .into_iter()
        .filter_map(|key| {
            let seat = key[HELD_TELL.len()..].to_string();
            let n = held_tells(store, &seat).len();
            (n > 0).then_some((seat, n))
        })
        .collect()
}

/// The row a seat's held tells were for, if the tell named one.
fn held_row(store: &Store, seat: &str) -> Option<String> {
    store
        .watch(&held_key(seat))
        .and_then(|rec| rec.get("row").and_then(Value::as_str).map(str::to_string))
        .filter(|r| !r.is_empty())
}

/// What is held for this seat, oldest first.
pub fn held_tells(store: &Store, seat: &str) -> Vec<(u64, String)> {
    load(store, &held_key(seat))
        .held
        .iter()
        .filter_map(|h| h.message().map(|text| (h.seq, text.to_string())))
        .collect()
}

/// The pass that lets held tells go: each seat with any is read, and
///
/// - an idle seat is sent them, oldest first, each settled the moment `send` says
///   it left (a failed send stops the seat's batch, so order holds);
/// - a seat that is [`Seen::Missing`] loses them — nobody is coming;
/// - any other reading, **`Unreachable` included**, leaves them held.
///
/// Returns `(sent, dropped)`. `read` and `send` are the seams: the daemon brings
/// the backends, a test brings a script.
pub fn flush_held_tells(
    store: &Store,
    read: &dyn Fn(&str) -> Seen,
    send: &mut dyn FnMut(&str, &str) -> bool,
) -> (usize, usize) {
    let (mut sent, mut dropped) = (0, 0);
    for key in store.watch_keys(HELD_TELL) {
        let seat = key[HELD_TELL.len()..].to_string();
        let held = held_tells(store, &seat);
        if held.is_empty() {
            continue;
        }
        match read(&seat) {
            Seen::Missing => {
                // **Not dropped in silence** (`wsp-659`): the seat ended with
                // these unread, so each is offered to the row's next seat if one
                // is up, and said back to whoever sent it either way.
                let row = held_row(store, &seat);
                let next = row.as_deref().and_then(|r| store.panes_for_task(r).into_iter().find(|p| *p != seat));
                settle_key(store, &key, &held.iter().map(|(q, _)| *q).collect::<Vec<_>>());
                for (_, text) in &held {
                    if let Some(next) = &next {
                        hold_tell_for(store, next, text, row.as_deref());
                    }
                    store.log_event(
                        "agent-told-settled",
                        json!({ "pane": seat, "how": "dropped", "offered": next, "at": util::epoch_secs() }),
                    );
                    undelivered(store, &seat, row.as_deref(), next.as_deref(), text);
                }
                dropped += held.len();
            }
            Seen::Is(state) if state.will_take_a_prompt() => {
                for (seq, text) in &held {
                    if !send(&seat, text) {
                        break;
                    }
                    settle_key(store, &key, &[*seq]);
                    store.log_event("agent-told-settled", json!({ "pane": seat, "how": "delivered", "at": util::epoch_secs() }));
                    sent += 1;
                }
            }
            Seen::Is(_) => {}
        }
    }
    (sent, dropped)
}

/// A held tell whose seat ended before it was read, said back to its sender
/// (`wsp-659`): a line in `cycle.log`, and the sentence to the sender's own
/// post — the primary's or a governor's spool, a seat's held tells, or the
/// person's list — so nothing the person's seat sends is lost without a word.
fn undelivered(store: &Store, seat: &str, row: Option<&str>, next: Option<&str>, text: &str) {
    let from = crate::cmd_agent::speaker(text).unwrap_or("person").to_string();
    let words = util::truncate(crate::cmd_agent::undressed(text), 120);
    let whose = row.map(|r| format!(" ({r})")).unwrap_or_default();
    let fate = match (next, row) {
        (Some(n), _) => format!("offered to the row's next seat {n}"),
        (None, Some(r)) => format!("no seat holds {r} now — `wsp tell seat:{r} -` sends it again"),
        (None, None) => "nobody is left to read it".to_string(),
    };
    let said = format!("your tell to {seat}{whose} was not delivered — the seat ended with it unread; {fate}. It said: {words}");
    crate::store::log_line(store, &format!("told {seat}: undelivered, the seat ended with it held — {fate}; said back to {from}"));
    let at = util::epoch_secs();
    let spool = |scope: &str| {
        store.update_watch(&key_for(scope), |rec| {
            Spool::append(rec, vec![Spooled::of(at, Line::Note(crate::cmd_watch::Class::Message, format!("wsp says: {said}")))]);
        });
    };
    match crate::address::parse(&from) {
        Some(crate::address::Address::Primary) => spool(crate::governors::PRIMARY),
        Some(crate::address::Address::Governor(scope)) => spool(&scope),
        Some(crate::address::Address::Seat(r)) if store.panes_for_task(&r).first().is_some() => {
            let pane = store.panes_for_task(&r).remove(0);
            hold_tell_for(store, &pane, &format!("wsp says: {said}"), Some(&r));
        }
        _ => {
            use crate::message::{self, About, Kind, Message, Party};
            let about = row.map(|r| About::Task(r.to_string())).unwrap_or(About::Nothing);
            let _ = message::raise(store, &Message::new(Party::wsp(), Kind::Note, &said).about(about));
        }
    }
}

fn settle_key(store: &Store, key: &str, seqs: &[u64]) {
    store.update_watch(key, |rec| Spool::settle_these(rec, seqs));
}

/// [`flush_held_tells`] against the machine's own backends: the daemon's call.
pub fn flush_held_tells_now(store: &Store) -> (usize, usize) {
    if store.watch_keys(HELD_TELL).is_empty() {
        return (0, 0);
    }
    // Under `WSP_HOST=manager` a manager that does not answer leaves them all held.
    let Ok(backends) = crate::cmd_spawn::told_backends(false) else {
        return (0, 0);
    };
    let read = |seat: &str| {
        let mut unsure = false;
        for b in backends.iter() {
            match b.census() {
                Ok(c) => {
                    if let Some(r) = c.seats().find(|r| r.seat.as_str() == seat) {
                        return Seen::Is(r.state);
                    }
                }
                Err(_) => unsure = true,
            }
        }
        // A backend that could not be asked may be the one that has it.
        match unsure {
            true => Seen::Is(crate::place::State::Unknown),
            false => Seen::Missing,
        }
    };
    let mut send = |seat: &str, text: &str| match crate::cmd_agent::locate_seat(&backends, seat) {
        Some((place, row)) => agent_commands::of(&row.agent.kind).tell(place.as_ref(), &Pane::new(seat), text).is_ok(),
        None => false,
    };
    flush_held_tells(store, &read, &mut send)
}

/// The sentences held on a scope's spool, oldest first, as `(seq, at, text)`
/// — messages only, not the news beside them. See [`crate::cmd_watch::Spooled::message`].
pub fn messages_held(store: &Store, scope: &str) -> Vec<(u64, i64, String)> {
    load(store, &key_for(scope))
        .held
        .iter()
        .filter_map(|h| h.message().map(|text| (h.seq, h.at, text.to_string())))
        .collect()
}

/// Take these entries off a scope's spool, by identity, inside the lock — for a
/// caller that has put them somewhere else.
pub fn settle_held(store: &Store, scope: &str, seqs: &[u64]) {
    if seqs.is_empty() {
        return;
    }
    store.update_watch(&key_for(scope), |rec| Spool::settle_these(rec, seqs));
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
pub struct Tell<'a> {
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
    pub typed: bool,
    /// When that happened, carried out of the sink so a receipt can say how long
    /// the seat has been sitting on it.
    pub typed_at: Option<i64>,
    /// The spool entries this attempt says, by `seq`: under the orders machine
    /// the order's key, so the same emit set on the next tick is the same order
    /// (wsp-125).
    batch: Vec<u64>,
}

impl<'a> Tell<'a> {
    /// A `failed` from the seat's supervisor (wsp-477): a note raised on the
    /// scope once while the last is open, and the `cycle.log` line with it — a
    /// failure that goes on failing is said once, not every tick.
    fn escalate(&self, why: &str) {
        use crate::message::{self, About, Kind, Message, Party};
        let about = About::Scope(self.scope.clone());
        let open = message::raised(self.store)
            .iter()
            .any(|m| m.about == about && m.text.starts_with(FAILED_DELIVERY));
        if !open {
            crate::store::log_line(self.store, &format!("told {}: delivery failed — {why}", self.scope));
            let text = format!(
                "{FAILED_DELIVERY}{why}\n\nThe seat's supervisor (`compound-sup deliver`) answered `failed` \
                 after its own retries; wsp holds the sentence and does not retype it at once."
            );
            let _ = message::raise(self.store, &Message::new(Party::wsp(), Kind::Note, &text).about(about));
        }
    }

    fn new(store: &'a Store, seats: &'a dyn Seats, scope: &str) -> Tell<'a> {
        Tell { store, seats, scope: scope.to_string(), why: NOT_YET.into(), typed: false, typed_at: None, batch: Vec::new() }
    }
}

/// Nothing here is worth a context read on its own. The ordinary state, and
/// not a fault: it is what `core-017`'s table is for.
const NOT_YET: &str = "nothing worth a wake yet";

/// A scope nobody has ever governed: no record, so nothing to fill and nobody
/// filling it. The one case with genuinely no addressee — see [`nobody_there`].
/// What a raised `failed` delivery begins with, so a second tick finding the
/// first still open raises nothing more.
const FAILED_DELIVERY: &str = "delivery to the seat failed: ";

const NO_SEAT: &str = "no seat on this scope";

/// An order is out for the seat and it has not acknowledged it (wsp-125).
const ORDERED: &str = "sent to the seat as an order, not yet acknowledged";

/// A `failed` reason that says nothing reached the seat, so the sentence is
/// still wholly owed and trying again is safe (wsp-477).
fn took_nothing(why: &str) -> bool {
    ["no_composer", "no composer", "session_gone", "session gone", "refused", "refusing"].iter().any(|r| why.contains(r))
}

/// A refusal whose seat is holding a draft of its own in the input box — named
/// by the backend (`place_compound::DRAFT_STUCK`), so nothing was taken *and*
/// typing again would not be either.
fn draft_stuck(why: &str) -> bool {
    why.contains(crate::place_compound::DRAFT_STUCK)
}

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
    let v = crate::governors::vacancy(&store.governors(), scope);
    let up = store
        .daemon_holder()
        .map(|(pid, _)| pid)
        // The daemon asking about itself needs no `ps` (`wsp-320`): this runs
        // once per vacant seat on every pass, and that was a fork each.
        .is_some_and(|pid| pid == std::process::id() || crate::host::Host::alive(&crate::host::LOCAL, &[pid]).contains(&pid));
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
    //
    // A failed reseat says why and when the next try is (`wsp-177`). Bare
    // `retrying` was all a reader got while the reason sat in undated stderr.
    let tail = match (v.reseating, v.failed, up) {
        (Some(at), Some(_), _) => format!(
            "reseat failed · {} · next try {}",
            cmd_govern::reseat_why(&store.governors(), scope).unwrap_or_else(|| "no reason recorded".into()),
            util::local_hm(at + crate::governors::SEAT_CLAIMED_FOR)
        ),
        (Some(_), None, _) => "reseating".to_string(),
        (None, _, false) => "daemon down".to_string(),
        (None, _, true) => "counting".to_string(),
    };
    // A kind compound cannot run is a seat nothing will fill, and the one of
    // these a person has to answer: `counting` there would be a promise.
    let kind = crate::cmd_spawn::seat_kind(&store.governors(), scope);
    let tail = match (crate::place_compound::reads_kind(&kind), crate::repair::will_seat(store, scope)) {
        (false, _) => format!("a {kind} seat compound cannot run"),
        // A list that is not running is never reseated, owed or not
        // (`wsp-177`), so its sentence names the status and not a count.
        (true, false) => match store.worklist(scope).map(|w| w.status()).filter(|s| !s.is_running()) {
            Some(status) => format!("the list is {}, so nobody is coming", status.as_str()),
            // Nothing owed and no running list: nothing will fill it, and nothing
            // needs to until something is owed.
            None => "nothing owed, so nobody is coming".to_string(),
        },
        (true, true) => tail,
    };
    let tail = match crate::governors::stood_at(&store.governors(), scope) {
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
/// typed, and a wake arrives through `Kind::tell` — backend typing at the pane —
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
        // **Under the orders machine the spool is an order** (wsp-125): sent,
        // not typed, and confirmed by the order reaching `acknowledged` rather
        // than by a turn seen since. The delivery worker owns the gate, the
        // typing and the retries.
        if let Some(door) = crate::refusals::door(self.store) {
            return self.order(&door, &seat.pane, said);
        }
        let backends = crate::place::local_backends();
        let Some((place, found)) = cmd_govern::occupant(self.store, &backends, &seat) else {
            self.why = NOT_FOUND.into();
            return false;
        };
        let how = agent_commands::of(&found.agent.kind);
        // The gate: one state read, two questions, and both of them are about
        // a transport that delivers by *typing at a pane*.
        // [`agent_commands::Kind::queue_is_the_agents`] is that transport
        // named — its docs open on `Place::tell`, backend typing — so a kind
        // carrying a queue of its own pays for neither question.
        //
        // **Mid-turn is the durability half.** `core-021` d2: not corruption —
        // Claude Code queues a mid-turn prompt and answers it at the boundary —
        // but durability, because that queue is the agent's and dies with it
        // while the spool does not.
        //
        // **Asked of `agent.get` and not of the census row `occupant` hands
        // back.** `backend::panes` is `pane.list`, and the whole reason
        // `Backend::census` makes two calls is that `pane.list` alone cannot tell
        // a starting agent from an idle one — so a status read off that row is
        // not a reading of whether a turn is in flight. Driven 2026-08-21: with
        // the row's status the gate never fired once, and four wakes were
        // delivered into an agent that `agent.get` reported as `working`
        // throughout. `place_backend::turning` carries the same warning for the
        // same reason.
        //
        // **And where the keystrokes land is the other half**, which
        // [`crate::place::State::turn_in_flight`] does not answer: it is
        // `Working` and nothing else, so this path would type into a permission
        // dialog, into an agent still coming up, and into a pane backend could
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
        let folded = owed.folded();
        owed.withhold_for_a_seat(self.store);
        if let Some(at) = owed.typed_at() {
            // **Or a turn that began and ended since the type**, which a sample
            // cannot see: `wsp-166` measured cpd-250's replies at one to two
            // seconds against a twenty-second tick, so most were never seen
            // and the batch was typed again every [`RETYPED`] all night.
            let read = state.turn_in_flight() || crate::fold::turn_began_since(self.store, addressee.as_str(), at) == Some(true);
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
        // is that transport named — its docs open on `Place::tell`, backend typing
        // — so a kind carrying a queue of its own pays for neither question.
        //
        // **Mid-turn is the durability half.** `core-021` d2: not corruption —
        // Claude Code queues a mid-turn prompt and answers it at the boundary —
        // but durability, because that queue is the agent's and dies with it
        // while the spool does not.
        //
        // **Asked of `agent.get` and not of the census row `occupant` hands
        // back.** `backend::panes` is `pane.list`, and the whole reason
        // `Backend::census` makes two calls is that `pane.list` alone cannot tell
        // a starting agent from an idle one — so a status read off that row is
        // not a reading of whether a turn is in flight. Driven 2026-08-21: with
        // the row's status the gate never fired once, and four wakes were
        // delivered into an agent that `agent.get` reported as `working`
        // throughout. `place_backend::turning` carries the same warning for the
        // same reason.
        //
        // **And where the keystrokes land is the other half**, which
        // [`crate::place::State::turn_in_flight`] does not answer: it is
        // `Working` and nothing else, so this path would type into a permission
        // dialog, into an agent still coming up, and into a pane backend could
        // not be asked about — the three `cmd_agent::tell` has refused since
        // robustness-083. The module docs carry it; the question is
        // [`crate::place::State::will_take_a_prompt`], and asking the wider one
        // costs nothing this path was not already paying, because refusing is
        // the ordinary answer here.
        if how.queue_is_the_agents() {
            // `State::Unknown` when backend could not be asked, which
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

        let mut text = format!("{}\n{}", preamble(&self.scope), said.join("\n"));
        // What was folded is still this seat's, and a seat is told it is there.
        if folded > 0 {
            text.push_str(&format!(
                "\n· {folded} earlier entr{} folded on this seat and not typed — `wsp watch --drain` prints them",
                if folded == 1 { "y is" } else { "ies are" }
            ));
        }
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
        let outcome = scripted_answer().unwrap_or_else(|| how.tell(place.as_ref(), &addressee, &text));
        self.settle(outcome, before)
    }
}

/// What the seat's supervisor answers to the next tell, scripted by a test
/// (`wsp-634`): the one thing a fake backend cannot say, because `failed refused`
/// is `compound-sup deliver`'s own word. Empty in every other test, and in the
/// binary there is nothing here at all.
#[cfg(test)]
thread_local! {
    pub static SUPERVISOR: std::cell::RefCell<std::collections::VecDeque<crate::place::Result<crate::place::Delivery>>> =
        const { std::cell::RefCell::new(std::collections::VecDeque::new()) };
}

#[cfg(test)]
fn scripted_answer() -> Option<crate::place::Result<crate::place::Delivery>> {
    SUPERVISOR.with(|s| s.borrow_mut().pop_front())
}

#[cfg(not(test))]
fn scripted_answer() -> Option<crate::place::Result<crate::place::Delivery>> {
    None
}

impl Tell<'_> {
    /// The order's key for this batch at `seat`: per scope and emit set, and per
    /// seat, so a successor in the seat is sent its own order and not handed back
    /// the one its predecessor failed.
    fn order_key(&self, seat: &str) -> String {
        let seqs: Vec<String> = self.batch.iter().map(u64::to_string).collect();
        format!("wake:{}:{seat}:{}", self.scope, seqs.join(","))
    }

    /// The batch as an order (wsp-125): [`crate::refusals::Door::send`] under
    /// [`Tell::order_key`], so the tick that sent it and every tick after it with
    /// the same emits hold one order, and then that order's state read without
    /// waiting. `acknowledged` (or `done`, past it) clears the batch; `failed` is
    /// the supervisor's final answer, raised once, and the seat is ended as
    /// deaf by [`crate::repair::refusal_tick`] from its failed orders — nothing
    /// is counted in `refused-deliveries.json`.
    fn order(&mut self, door: &crate::refusals::Door, seat: &str, said: &[String]) -> bool {
        use wsp_store_contract::OrderState as S;
        let text = format!("{}\n{}", preamble(&self.scope), said.join("\n"));
        let sent = match door.send(seat, &text, &self.order_key(seat)) {
            Ok(sent) => sent,
            Err(e) => {
                self.why = format!("the orders machine did not take it: {e}");
                return false;
            }
        };
        if sent.fresh {
            crate::store::log_line(self.store, &format!("told {}: order {} queued for {seat}", self.scope, sent.id.0));
        }
        let Ok(Some(order)) = door.order(&sent.id) else {
            self.why = format!("order {} could not be read", sent.id.0);
            return false;
        };
        match order.state {
            S::Acknowledged | S::Done => {
                crate::store::log_line(
                    self.store,
                    &format!("told {}: delivered, order {} acknowledged by the seat", self.scope, sent.id.0),
                );
                true
            }
            S::Failed => {
                let why = order.why.unwrap_or_else(|| "its attempts ran out".into());
                self.why = format!("the seat's supervisor failed to deliver it: {why}");
                self.escalate(&why);
                false
            }
            S::Cancelled => {
                self.why = format!("order {} was cancelled", sent.id.0);
                false
            }
            S::Queued | S::Typed | S::Delivered => {
                self.why = format!("{ORDERED} (order {}, {})", sent.id.0, format!("{:?}", order.state).to_lowercase());
                false
            }
        }
    }

    /// What became of a typing, as the batch's fate: `true` clears it.
    fn settle(&mut self, outcome: crate::place::Result<crate::place::Delivery>, before: i64) -> bool {
        match outcome {
            Ok(crate::place::Delivery::Started) => {
                accepted(self.store, &self.scope);
                true
            }
            // The seat's supervisor acknowledged it (wsp-477): delivered
            // exactly once, nothing owed, no turn to wait for.
            Ok(crate::place::Delivery::Acked) => {
                accepted(self.store, &self.scope);
                crate::store::log_line(self.store, &format!("told {}: delivered, acknowledged by the seat", self.scope));
                true
            }
            // The supervisor owns the retry, so `failed` is final and the one
            // thing wsp escalates: raised once on the scope for a person.
            // Unless it names a seat that took nothing (`no_composer`,
            // `session_gone`), the supervisor has typed the sentence up to four
            // times, so it is stamped as typed and held for [`RETYPED`] rather
            // than typed again under a new id on every tick.
            Err(crate::place::Refusal::Failed(why)) => {
                self.why = format!("the seat's supervisor failed to deliver it: {why}");
                self.escalate(&why);
                // Counted, and the reason kept: a retry on the next tick says what
                // it is retrying after, and a seat that goes on refusing is
                // reseated by [`crate::repair::refusal_tick`] (wsp-634).
                let (n, last) = refuse(self.store, &self.scope, &why);
                if let (true, Some(last)) = (n > 1 && took_nothing(&why), last) {
                    crate::store::log_line(
                        self.store,
                        &format!("told {}: refused {n} ticks running — retrying; the last answer was `{last}`", self.scope),
                    );
                }
                // **`wsp-694`: a draft stuck in the input box is not typed at
                // again.** `no composer` with text in the box is the seat that
                // wedged on 2026-11-19: retrying every tick only offers the same
                // text to a box that will not take it. Held as typed, for the
                // retype window, and the refusal names the draft.
                if draft_stuck(&why) {
                    self.why = format!("held, not typed again — {why}");
                }
                if !took_nothing(&why) || draft_stuck(&why) {
                    self.typed = true;
                    self.typed_at = Some(before);
                }
                false
            }
            Ok(crate::place::Delivery::Unconfirmed)
            | Err(crate::place::Refusal::NotTaken)
            | Err(crate::place::Refusal::Unsubmitted(_)) => {
                // Typed, and nothing read it. Returning false puts the whole
                // batch back, and the stamp is what tells the next tick to look
                // for a turn rather than to type again.
                accepted(self.store, &self.scope);
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
    let rec = store.watch(key).unwrap_or(Value::Null);
    let n = rec.get("delivered").and_then(Value::as_u64).unwrap_or(0) as usize;
    (n, rec)
}

/// What [`scopes_owed`] counts for one scope, without visiting the others.
pub fn owed_to(store: &Store, scope: &str) -> usize {
    owed_to_a_seat(store, &key_for(scope))
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
    Spool::of_json(store.watch(key).as_ref().and_then(|v| v.get("spool")).unwrap_or(&Value::Null))
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
    let was = rec.clone();
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
    // **`tick` moves only with something else** (`wsp-320`). It is when this
    // record last said something new: a wake record is not a reporter, so
    // `doctor` reads no liveness off it (`cmd_watch::health` skips it). A stamp
    // that only moved the clock made every pass rewrite every held seat's
    // record, and `update_watch` writes nothing for a record that is unchanged.
    if let (Some(old), Some(o)) = (was.as_object(), rec.as_object_mut()) {
        if let Some(t) = old.get("tick") {
            let same = o.iter().all(|(k, v)| k == "tick" || old.get(k) == Some(v)) && old.len() == o.len();
            if same {
                o.insert("tick".into(), t.clone());
            }
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::refusals::refusing;
    use crate::cmd_watch::{Edge, Kind, Signal};
    use crate::base::args::Args;

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
        crate::cycle::install_seats(&crate::cmd_spawn::Fleet);
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
    /// There is no backend in a test, so nothing can be delivered: what is left
    /// on disk is exactly what would survive the process dying mid-send.
    #[test]
    fn what_the_pass_produced_is_on_disk_before_anything_is_attempted() {
        crate::cycle::install_seats(&crate::cmd_spawn::Fleet);
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
        crate::cycle::install_seats(&crate::cmd_spawn::Fleet);
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
        crate::cycle::install_seats(&crate::cmd_spawn::Fleet);
        let env = util::isolated("wake-twice");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let text = "a-1  finished and waiting on you";

        // What the person's verb would say about sending this a second time.
        let args = crate::base::args::Args::parse(vec!["wsp".into(), "tell".into()]);
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

    /// What the hub was sent, for a test that has no hub.
    fn counted() -> (std::cell::RefCell<Vec<(String, String)>>,) {
        (std::cell::RefCell::new(Vec::new()),)
    }

    fn an_ask_about(store: &Store, row: &str) -> crate::message::Message {
        let mut t = crate::model::Task::new("the work", row);
        t.project = Some("core".into());
        store.save_task(&t).unwrap();
        let q = crate::message::Message::question(
            crate::message::Party::seat("core"),
            crate::message::Kind::Note,
            "which way?",
            crate::message::Waiting::new("w1:p1", row),
        )
        .about(crate::message::About::Task(row.to_string()));
        crate::message::raise(store, &q).unwrap();
        q
    }

    /// **`wsp-418`: on jesper's spoke, an ask for a row allocated to ed is one
    /// routed message to `primary:ed`, not one per tick.** The pass says the same
    /// level every tick it stands, and a message already routed is not routed
    /// again — by the record's hop, and by what was sent for a level with no
    /// record. Nothing is spooled on this spoke for a seat that is not here.
    #[test]
    fn an_ask_for_another_users_primary_is_one_routed_message_not_one_per_tick() {
        crate::cycle::install_seats(&crate::cmd_spawn::Fleet);
        let env = util::isolated("wake-primary-other");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let q = an_ask_about(&store, "e-1");
        let (sent,) = counted();
        let route = |_: &Store, to: &str, text: &str| -> Result<String, String> {
            sent.borrow_mut().push((to.to_string(), text.to_string()));
            Ok(format!("hub-{}", sent.borrow().len()))
        };
        let primaries = Primaries { me: Some("jesper".into()), route: &route };

        let mut up = emit(Kind::Unanswered, "primary:ed", "e-1");
        up.signal.record = Some(q.id.clone());
        for tick in 0..4 {
            wake_by(&store, &[up.clone()], tick * 20, &primaries);
        }
        let told = sent.borrow().clone();
        assert_eq!(told.len(), 1, "one message however many passes: {told:?}");
        assert_eq!(told[0].0, "primary:ed", "to ed's primary");
        assert_eq!(spool_of(&store, "primary:ed").depth(), 0, "and nothing is spooled here for a seat that is not here");
        assert_eq!(spool_of(&store, "primary").depth(), 0, "jesper's own primary is not told ed's ask");

        // The same words said by `wsp ask` itself, twice: still the one.
        let first = say_by(&store, crate::cycle::seats(), "primary:ed", "which way?", Some(&q.id), &primaries);
        assert_eq!(sent.borrow().len(), 1, "already sent: {}", first.why);
        assert!(first.said("ed's primary", &util::Paint::plain()).contains("ed's primary"), "{}", first.said("x", &util::Paint::plain()));

        // A level with no record is one send too, and a down edge is not sent.
        let bare = emit(Kind::NeedsAPerson, "primary:ed", "e-2");
        wake_by(&store, &[bare.clone()], 100, &primaries);
        wake_by(&store, &[bare.clone()], 120, &primaries);
        let mut down = bare;
        down.edge = Edge::Down;
        wake_by(&store, &[down], 140, &primaries);
        assert_eq!(sent.borrow().len(), 2, "the second level once, its clearing not at all");

        // A hub that does not take it is tried again on the next pass.
        let refuse = |_: &Store, _: &str, _: &str| -> Result<String, String> { Err("no hub".into()) };
        let down_hub = Primaries { me: Some("jesper".into()), route: &refuse };
        let r = say_by(&store, crate::cycle::seats(), "primary:ed", "again", None, &down_hub);
        assert_eq!(r.settled, 0);
        assert!(r.why.contains("no hub"), "{}", r.why);
    }

    /// **`wsp-418`: the same ask on ed's own spoke is spooled for his primary**, the
    /// slot under the one name every governor spool has, and routed nowhere.
    #[test]
    fn an_ask_for_this_users_primary_is_spooled_for_the_local_primary_slot() {
        crate::cycle::install_seats(&crate::cmd_spawn::Fleet);
        let env = util::isolated("wake-primary-own");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let q = an_ask_about(&store, "e-1");
        let route = |_: &Store, to: &str, _: &str| -> Result<String, String> { panic!("{to} is here: nothing goes to the hub") };
        let primaries = Primaries { me: Some("ed".into()), route: &route };

        wake_by(&store, &[emit(Kind::Unanswered, "primary:ed", "e-1")], 0, &primaries);
        assert_eq!(spool_of(&store, "primary").depth(), 1, "held for the primary slot, as a governor's is");
        assert_eq!(spool_of(&store, "primary:ed").depth(), 0);

        say_by(&store, crate::cycle::seats(), "primary:ed", "which way?", Some(&q.id), &primaries);
        assert_eq!(spool_of(&store, "primary").depth(), 2, "a said sentence goes to the same spool");
        assert!(store.message(&q.id).unwrap().handed_to("primary:ed"), "and the record says the primary was handed it");
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
        crate::cycle::install_seats(&crate::cmd_spawn::Fleet);
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
    crate::cycle::install_seats(&crate::cmd_spawn::Fleet);
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
        crate::cycle::install_seats(&crate::cmd_spawn::Fleet);
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
        crate::cycle::install_seats(&crate::cmd_spawn::Fleet);
        let (_env, store, _key) = withheld("wakes-unowed");
        vacated(&store);
        assert!(!crate::repair::will_seat(&store, "core"), "the fixture is the case: nothing owed, no list");
        let said = unseated(&store, "core");
        assert!(said.ends_with("nothing owed, so nobody is coming"), "{said}");
    }

    /// **`wsp-320`: a seat that is owed and that nobody is in costs no write on
    /// a pass with no news.** Thirty such seats were the live store's idle
    /// state, each rewritten twice a minute — a megabyte and an `fsync` each —
    /// to move nothing but its `tick`.
    #[test]
    fn an_owed_vacant_seat_is_not_rewritten_by_a_pass_with_no_news() {
        crate::cycle::install_seats(&crate::cmd_spawn::Fleet);
        let env = util::isolated("wakes-owed-idle");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let key = key_for("core");
        let e = Emit {
            edge: Edge::Up,
            signal: Signal::new(Kind::Review, "m-1", "a verdict nobody has read").to("core"),
            held: 0,
            to: "core".into(),
        };
        store.update_watch(&key, |rec| {
            Spool::append(rec, vec![Spooled::of(0, Line::News(e))]);
        });
        assert!(owed_to_a_seat(&store, &key) > 0, "the fixture is the case: owed");
        vacated(&store);

        deliver_to(&store, &Gone, "core", &[], 0);
        mark(&store, &key);
        deliver_to(&store, &Gone, "core", &[], 0);
        assert!(!written(&store, &key), "a pass that changed nothing wrote the record anyway");
        assert_eq!(load(&store, &key).depth(), 1, "and what is owed is still held");
        drop(env);
    }


    /// **`wsp-179`'s third item: a whole spool withheld *and* an empty seat,
    /// asserting the sentence.** The at-rest gate returned before the only writer
    /// of `holding`, so this scope kept whatever it last said for ever — and the
    /// defect hid behind every fixture that appended a message, because a message
    /// is owed and goes the other way. Through `deliver_to` with no emits, which
    /// is the pass's own call for a scope with nothing new.
    #[test]
    fn a_scope_whose_whole_spool_is_withheld_and_whose_seat_is_empty_says_unseated() {
        crate::cycle::install_seats(&crate::cmd_spawn::Fleet);
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
        crate::cycle::install_seats(&crate::cmd_spawn::Fleet);
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
        crate::cycle::install_seats(&crate::cmd_spawn::Fleet);
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

    /// **wsp-477: what the seat's supervisor answered is the batch's fate.**
    /// `delivered` clears it and is said in `cycle.log`; `failed` raises the row
    /// once and, because the supervisor has already typed it, stamps it typed so
    /// no tick types it again under a new id — unless the reason is one where
    /// nothing reached the seat.
    #[test]
    fn a_supervisors_delivered_clears_and_is_said_and_its_failed_raises_once_and_is_not_retyped() {
        use crate::place::{Delivery, Refusal};
        let env = util::isolated("wake-acked");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let log = || std::fs::read_to_string(store.state_file("cycle.log")).unwrap_or_default();
        let raised = || {
            crate::message::raised(&store).into_iter().filter(|m| m.text.starts_with(FAILED_DELIVERY)).count()
        };
        let before = util::epoch_secs();

        let mut tell = Tell::new(&store, &Working, "demo");
        assert!(tell.settle(Ok(Delivery::Acked), before), "delivered clears the batch");
        assert!(!tell.typed && tell.typed_at.is_none());
        assert!(log().contains("told demo: delivered, acknowledged"), "and cycle.log says so: {}", log());
        assert_eq!(raised(), 0);

        // A failed after the supervisor's own retries: raised, and held as typed.
        let mut tell = Tell::new(&store, &Working, "demo");
        assert!(!tell.settle(Err(Refusal::Failed("cpd-1: no turn after 4 tries".into())), before));
        assert!(tell.typed && tell.typed_at == Some(before), "stamped, so the next tick does not type it again");
        assert_eq!(raised(), 1, "the row is raised");
        assert!(log().contains("delivery failed — cpd-1: no turn after 4 tries"), "{}", log());

        // The same failure on the next tick raises nothing more and says nothing more.
        let lines = log().lines().count();
        let mut tell = Tell::new(&store, &Working, "demo");
        assert!(!tell.settle(Err(Refusal::Failed("cpd-1: no turn after 4 tries".into())), before + 20));
        assert_eq!(raised(), 1, "raised once while the last is open");
        assert_eq!(log().lines().count(), lines, "and cycle.log does not repeat it every tick");

        // Nothing reached the seat: still wholly owed, and safe to try again.
        let mut tell = Tell::new(&store, &Working, "demo");
        assert!(!tell.settle(Err(Refusal::Failed("cpd-1: no_composer".into())), before));
        assert!(!tell.typed && tell.typed_at.is_none(), "not stamped: nothing was typed");
        let _ = std::fs::remove_dir_all(&env.state());
    }

    /// **`wsp-694`: a draft stuck in the input box is not typed at again.** A
    /// bare `no composer` took nothing and is retried next tick; the same
    /// refusal naming a draft in the box is held as typed for the retype
    /// window, and the reason `--status` prints names the draft.
    #[test]
    fn a_no_composer_refusal_naming_a_stuck_draft_is_held_and_not_retyped() {
        use crate::place::Refusal;
        let env = util::isolated("wake-draft");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let before = util::epoch_secs();
        let why = format!("cpd-6187: no composer — {} (81814 chars: \"wsp · nobody typed this…\")", crate::place_compound::DRAFT_STUCK);

        let mut tell = Tell::new(&store, &Working, "demo");
        assert!(!tell.settle(Err(Refusal::Failed(why.clone())), before));
        assert!(tell.typed && tell.typed_at == Some(before), "stamped, so the next tick does not offer it again");
        assert!(tell.why.contains("held, not typed again") && tell.why.contains("81814 chars"), "{}", tell.why);

        let mut tell = Tell::new(&store, &Working, "demo");
        assert!(!tell.settle(Err(Refusal::Failed("cpd-6187: no composer".into())), before));
        assert!(!tell.typed, "a box that is merely refusing is still asked again");
        let _ = std::fs::remove_dir_all(&env.state());
    }

    /// **`wsp-694`: a seat ended for refusing does not hand its successor the
    /// payload it refused.** What was typed at it is folded — withheld, and kept
    /// for `--drain` — and what was never typed is still owed.
    #[test]
    fn a_reseat_folds_what_the_refusing_seat_was_typed() {
        let env = util::isolated("wake-fold");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let key = key_for("demo");
        let line = |t: &str| Spooled::of(0, Line::Note(crate::cmd_watch::Class::Message, t.into()));
        let typed = store.update_watch(&key, |rec| {
            let s = Spool::append(rec, vec![line("the 81 KB one")]);
            let seqs: Vec<u64> = s.held.iter().map(|h| h.seq).collect();
            Spool::stamp_typed(rec, 100, &seqs);
            Spool::append(rec, vec![line("raised since")]);
            seqs
        });
        assert_eq!(fold_what_was_typed(&store, "demo"), typed.len());
        let held = load(&store, &key);
        assert_eq!((held.depth(), held.folded()), (2, 1), "kept for the drain");
        assert_eq!(owed_to(&store, "demo"), 1, "and only what arrived since is owed");
        let log = std::fs::read_to_string(store.state_file("cycle.log")).unwrap_or_default();
        assert!(log.contains("folded the 1 held entries typed at the refusing seat"), "{log}");
        assert_eq!(fold_what_was_typed(&store, "demo"), 0, "once");
        let _ = std::fs::remove_dir_all(&env.state());
    }

    /// **wsp-634: a refusal is not a typing.** The supervisor answers `failed …
    /// refused` for an idle seat with an empty composer (the hook never fired, so
    /// it reads the seat as holding other text). Nothing was typed, so the batch
    /// is not stamped — the next tick takes it again, under a new id — and the
    /// refusals are counted, with the reason, until the seat takes one.
    #[test]
    fn a_refusal_types_nothing_is_retried_and_counted_until_the_seat_takes_a_tell() {
        use crate::place::{Delivery, Refusal};
        let env = util::isolated("wake-refused");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let log = || std::fs::read_to_string(store.state_file("cycle.log")).unwrap_or_default();
        let why = "cpd-1: refused";
        let before = util::epoch_secs();

        let mut tell = Tell::new(&store, &Working, "demo");
        assert!(!tell.settle(Err(Refusal::Failed(why.into())), before));
        assert!(!tell.typed && tell.typed_at.is_none(), "nothing was typed, so it is not `typed, not sent`");
        assert_eq!(refusing(&store).get("demo"), Some(&(1, why.to_string())));

        let mut tell = Tell::new(&store, &Working, "demo");
        assert!(!tell.settle(Err(Refusal::Failed(why.into())), before + 20));
        assert!(!tell.typed, "and not held for a retype window: the next tick asks again");
        assert_eq!(refusing(&store).get("demo"), Some(&(2, why.to_string())));
        assert!(log().contains("refused 2 ticks running — retrying; the last answer was `cpd-1: refused`"), "{}", log());

        let mut tell = Tell::new(&store, &Working, "demo");
        assert!(tell.settle(Ok(Delivery::Acked), before + 40));
        assert!(refusing(&store).is_empty(), "a seat that takes a tell is not refusing");
        let _ = std::fs::remove_dir_all(&env.state());
    }

    /// **wsp-125: under the orders machine a governor's spool is an order.** One
    /// order for one emit set however many ticks hold it, cleared on the tick
    /// that reads it `acknowledged` — no turn asked after — and nothing counted
    /// in `refused-deliveries.json` on the way.
    #[test]
    fn under_the_orders_machine_a_spool_is_one_order_per_emit_set_and_clears_when_acknowledged() {
        use wsp_store_contract::SeatId;
        let env = util::isolated("wake-orders");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        cmd_govern::take(&store, "demo", "w1", "w1:p1").unwrap();
        let opened = crate::refusals::tests::open(&store, &["w1:p1"]);
        let orders = || opened.0.orders_for(&SeatId("w1:p1".into()), &[]).unwrap();

        let e = emit(Kind::NeedsAPerson, "demo", "a-1");
        let first = deliver_to(&store, &Idle, "demo", &[&e], 60);
        assert_eq!((first.settled, first.held), (0, 1), "sent, not yet acknowledged: {first:?}");
        assert!(first.why.contains(ORDERED), "{}", first.why);
        let second = deliver_to(&store, &Idle, "demo", &[], 80);
        assert_eq!((second.settled, second.held), (0, 1));
        let out = orders();
        assert_eq!(out.len(), 1, "one order across two ticks: {out:?}");
        assert!(out[0].text.contains("a-1") && out[0].text.contains("nobody typed this"), "{}", out[0].text);

        crate::refusals::tests::ack(&opened.0, &out[0].id);
        let third = deliver_to(&store, &Idle, "demo", &[], 100);
        assert!(third.arrived(), "acknowledged is the confirmation: {third:?}");
        assert_eq!(third.held, 0);
        assert_eq!(orders().len(), 1, "and confirming it sent nothing more");
        assert!(refusing(&store).is_empty());
        assert!(!store.state.join("refused-deliveries.json").exists());
    }

    /// **wsp-125: a failed order is the supervisor's final answer**: held,
    /// raised once on the scope, and not counted in `refused-deliveries.json` —
    /// the reconciler reads the failed order itself.
    #[test]
    fn under_the_orders_machine_a_failed_order_is_held_and_raised_and_counted_nowhere() {
        use wsp_store_contract::SeatId;
        let env = util::isolated("wake-orders-failed");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        cmd_govern::take(&store, "demo", "w1", "w1:p1").unwrap();
        let opened = crate::refusals::tests::open(&store, &["w1:p1"]);
        let e = emit(Kind::NeedsAPerson, "demo", "a-1");
        deliver_to(&store, &Idle, "demo", &[&e], 60);
        let out = opened.0.orders_for(&SeatId("w1:p1".into()), &[]).unwrap();
        crate::refusals::tests::fail(&opened.0, &out[0].id, "refused");
        let r = deliver_to(&store, &Idle, "demo", &[], 80);
        assert_eq!((r.settled, r.held), (0, 1));
        assert!(r.why.contains("failed to deliver it: refused"), "{}", r.why);
        deliver_to(&store, &Idle, "demo", &[], 100);
        let raised = crate::message::raised(&store).iter().filter(|m| m.text.starts_with(FAILED_DELIVERY)).count();
        assert_eq!(raised, 1, "raised once");
        assert!(!store.state.join("refused-deliveries.json").exists());
    }

    /// The three readings a seat can give [`crate::repair::reading`], as fakes.
    struct Gone;
    struct Idle;
    struct Working;
    struct Silent;

    impl crate::cycle::Seats for Gone {
        fn state(&self, _: &str) -> Option<crate::place::State> {
            Some(crate::place::State::Gone)
        }
    }
    impl crate::cycle::Seats for Idle {
        fn state(&self, _: &str) -> Option<crate::place::State> {
            Some(crate::place::State::Idle)
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
        crate::cycle::install_seats(&crate::cmd_spawn::Fleet);
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
            crate::governors::vacancy(&store.governors(), "core").reseating,
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
        crate::cmd_govern::reseat_failed(&store, "core", "the agent never came up");
        assert!(
            why(&store).contains("reseat failed · the agent never came up · next try "),
            "the seat says it failed, why, and when it is tried again: {}",
            why(&store)
        );

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
        assert!(!State::Unknown.turn_in_flight(), "and into a pane backend could not be asked about");

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
    /// `core-019` question 3. A wake arrives through `Kind::tell` — backend
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
        crate::cycle::install_seats(&crate::cmd_spawn::Fleet);
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
        crate::cycle::install_seats(&crate::cmd_spawn::Fleet);
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
    /// fake backend behind it.
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
        // `WSP_BACKEND_PANE_ID`, which every other test in this binary also writes.
        // A member holding the task is the whole of what makes this a member's
        // question — an answer goes home to the asker's task, so this is what
        // the return path is built on.
        std::env::set_var("WSP_BACKEND_PANE_ID", "w1:p2");
        store.set_binding("w1:p2", json!({ "pane": "w1:p2", "task_id": "wsp-146" }));

        let mut spot = Spot::agent("w1:p1", "claude", "demo", state);
        spot.space = "w1".into();
        let mut stage = Stage::new();
        stage.put(spot);
        cmd_govern::take(&store, "demo", "w1", "w1:p1").unwrap();
        let fake = Fake::bind(env.path("backend.sock"), stage).expect("a socket");
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

    /// A compound seat the census reads as `state`, holding `task`: the double
    /// `cmd_watch`'s tests use for a seat in the current census (a real seat
    /// of the in-process host, its record naming an agent and this process as
    /// the pid, and the state its last hook said). The first of the pair ends
    /// the supervisor `open` started when it drops.
    fn compound_seat_in(
        store: &Store,
        task: &str,
        state: crate::place::State,
    ) -> (String, crate::place_compound::EndsOnDrop) {
        use crate::place::Place;
        let compound = crate::place_compound::Compound::new();
        let seat = compound.open(&crate::place::Order::default()).expect("a compound seat");
        let dir = compound.dir_of(&seat).unwrap();
        let mut rec: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("seat.json")).unwrap()).unwrap();
        let ends = crate::place_compound::EndsOnDrop(rec["pid"].as_u64().expect("a live pid") as u32);
        rec["pid"] = json!(std::process::id());
        rec["agent"] = json!({ "kind": "claude", "name": "t-1", "args": [] });
        std::fs::write(dir.join("seat.json"), rec.to_string()).unwrap();
        let hook = if state == crate::place::State::Working { "UserPromptSubmit" } else { "Stop" };
        compound.heard(&seat, hook, state, &json!({}));
        assert_eq!(compound.state(&seat).unwrap(), state);
        store.save_task(&crate::model::Task::new(&format!("{task} a row mid-turn"), task)).unwrap();
        store.set_binding(seat.as_str(), json!({ "pane": seat.as_str(), "task_id": task }));
        (seat.as_str().to_string(), ends)
    }

    fn delivered_to(store: &Store, scope: &str) -> usize {
        store
            .watches()
            .get(&key_for(scope))
            .and_then(|v| v.get("delivered"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as usize
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
        crate::cycle::install_seats(&crate::cmd_spawn::Fleet);
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
        crate::cycle::install_seats(&crate::cmd_spawn::Fleet);
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

        store.set_governor("run", json!({ "host": util::hostname(), crate::governors::STOOD_DOWN: util::now_iso() }));
        assert!(
            !crate::cycle::hand_it_to_the_governor(&store, &w, "group 1 is at its barrier"),
            "a stood-down list has nobody coming, and the project is not its answer either"
        );
    }

    /// A list `demo`'s members stand in, run by wsp, with its own seat filled in
    /// `w2:p1` — the shape `wsp-263` puts under `demo`'s governor.
    fn a_list_under_demo(store: &Store) -> crate::model::Worklist {
        let mut w = crate::model::Worklist::new("run", "run");
        w.set_status(crate::model::WorklistStatus::Running);
        w.set_groups(&[crate::model::Group {
            members: vec!["wsp-146".into()],
            agent: "claude".into(),
            ..crate::model::Group::default()
        }]);
        store.save_worklist(&w).unwrap();
        store.set_governor("run", json!({ "workspace": "w2", "pane": "w2:p1" }));
        w
    }

    /// **The project's governor hears its lists** — the half of
    /// [`crate::cycle::tell_above`] that decides, driven: a line in `demo`'s
    /// spool and none in the list's. And not twice for a list run by hand from
    /// `demo`'s own seat, which `cycle::tell` has already told in full.
    #[test]
    fn a_lists_news_goes_to_the_governor_above_it_and_not_to_the_one_already_told() {
        crate::cycle::install_seats(&crate::cmd_spawn::Fleet);
        use crate::place::State;

        let (_env, store, _fake) = a_governor("wake-list-heard", State::Working);
        let w = a_list_under_demo(&store);
        assert_eq!(crate::cycle::hand_it_up(&store, &w, "The run list passed barrier 1 of 1").as_deref(), Some("demo"));
        assert_eq!(spool_of(&store, "demo").depth(), 1);
        assert_eq!(spool_of(&store, "run").depth(), 0);

        let mut by_hand = crate::model::Worklist::new("byhand", "by hand");
        by_hand.set_status(crate::model::WorklistStatus::Running);
        by_hand.set_groups(&[crate::model::Group { members: vec!["wsp-146".into()], ..crate::model::Group::default() }]);
        store.save_worklist(&by_hand).unwrap();
        assert_eq!(crate::cycle::hand_it_up(&store, &by_hand, "held"), None, "demo governs it, and heard it from `tell`");
        assert_eq!(spool_of(&store, "demo").depth(), 1);
    }

    /// **A tell to a seat on a silent machine is held and delivered on
    /// reconnect; a tell to a seat that does not exist is not held at all, and
    /// what was held for one that has gone is dropped** (`docs/remote-agents.md`
    /// section 3). The two are told apart by the reading, not by the spool: the
    /// spool takes whatever it is handed, so the tell verb must hand it only a
    /// seat that is there.
    #[test]
    fn a_tell_to_an_unreachable_seat_is_held_until_the_link_returns_and_one_to_a_missing_seat_is_dropped() {
        use crate::place::State;
        use std::cell::Cell;
        let env = util::isolated("wake-held-tell");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();

        // The verb: a seat nothing holds is refused and nothing is kept for it.
        let args = crate::base::args::Args::synth("tell", &["cpd-999", "are you there"], &[]);
        assert_eq!(crate::cmd_agent::tell(&store, &args), 1, "nothing holds it");
        assert!(held_tells(&store, "cpd-999").is_empty(), "so nothing is held for it");

        assert_eq!(hold_tell(&store, "cpd-7", "carry on with the second half"), 1);
        assert_eq!(hold_tell(&store, "cpd-7", "and run the tests"), 2);

        let sent = std::cell::RefCell::new(Vec::<String>::new());
        let reading = Cell::new(State::Unreachable);
        let read = |_: &str| Seen::Is(reading.get());
        let mut send = |seat: &str, text: &str| {
            sent.borrow_mut().push(format!("{seat}: {text}"));
            true
        };

        // Silent, working, blocked, starting or unreadable: held, not sent, not lost.
        for state in [State::Unreachable, State::Working, State::Blocked, State::Starting, State::Unknown] {
            reading.set(state);
            assert_eq!(flush_held_tells(&store, &read, &mut send), (0, 0), "{state:?}");
        }
        assert_eq!(held_tells(&store, "cpd-7").len(), 2);

        // The link returns and the seat is at its prompt: both go, in order.
        reading.set(State::Idle);
        assert_eq!(flush_held_tells(&store, &read, &mut send), (2, 0));
        assert_eq!(*sent.borrow(), ["cpd-7: carry on with the second half", "cpd-7: and run the tests"]);
        assert!(held_tells(&store, "cpd-7").is_empty(), "and are not sent twice");
        assert_eq!(flush_held_tells(&store, &read, &mut send), (0, 0));

        // A seat that has gone for good loses what was held for it.
        hold_tell(&store, "cpd-8", "hello?");
        let gone = |_: &str| Seen::Missing;
        assert_eq!(flush_held_tells(&store, &gone, &mut send), (0, 1));
        assert!(held_tells(&store, "cpd-8").is_empty());
        assert_eq!(sent.borrow().len(), 2, "nothing went to it");

        // A send that fails stops the batch and keeps the rest, in order.
        hold_tell(&store, "cpd-9", "one");
        hold_tell(&store, "cpd-9", "two");
        let mut fails = |_: &str, _: &str| false;
        assert_eq!(flush_held_tells(&store, &read, &mut fails), (0, 0));
        assert_eq!(held_tells(&store, "cpd-9").iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>(), ["one", "two"]);
    }

    /// `wsp-659` item 1: a tell to a seat mid-turn was refused ("look first,
    /// then `--anyway`") and nothing was kept, so the primary's sentence was lost
    /// unless retried by hand. Now it is queued on the seat's held tells, the
    /// sender is told so, nothing is typed into the turn, and the seat's next
    /// idle takes it — with a second tell queued behind it, in order.
    #[test]
    fn a_tell_to_a_working_seat_is_queued_and_typed_at_its_next_idle_in_order() {
        use crate::place::State;
        let env = util::isolated("wake-queued-tell");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let (seat, _ends) = compound_seat_in(&store, "wsp-700", State::Working);
        // The nothing-typed check is backed by the command's exit code and the held count, not by watching what was typed.
        let delivered = || store.events_of("agent-told").iter().filter(|e| e["queued"] != json!(true)).count();
        assert_eq!(delivered(), 0);

        let first = crate::base::args::Args::synth("tell", &["wsp-700", "the ninth finding: the bar is too tall"], &[]);
        assert_eq!(crate::cmd_agent::tell(&store, &first), 0, "queued is a success the sender is told about");
        let second = crate::base::args::Args::synth("tell", &["wsp-700", "and the tenth"], &[]);
        assert_eq!(crate::cmd_agent::tell(&store, &second), 0);
        assert_eq!(delivered(), 0, "nothing typed into the turn");

        let held = held_tells(&store, &seat);
        assert_eq!(held.len(), 2, "{held:?}");
        assert!(held[0].1.contains("the ninth finding") && held[1].1.contains("the tenth"), "in the order sent: {held:?}");
        assert!(held[0].1.contains(" says: "), "dressed, so the seat reads who speaks: {}", held[0].1);
        assert_eq!(held_row(&store, &seat).as_deref(), Some("wsp-700"), "the row is kept for the seat's successor");
        let told = store.events_of("agent-told");
        assert!(told.iter().all(|e| e["queued"] == json!(true)), "{told:?}");
        assert_eq!(queued_tells(&store).get(seat.as_str()), Some(&2), "what `wsp wip` draws as queued");
        let log = std::fs::read_to_string(store.state_file("cycle.log")).unwrap_or_default();
        assert!(log.contains("queued for its next idle"), "{log}");

        let sent = std::cell::RefCell::new(Vec::<String>::new());
        let mut send = |_: &str, text: &str| {
            sent.borrow_mut().push(text.to_string());
            true
        };
        assert_eq!(flush_held_tells(&store, &|_| Seen::Is(State::Working), &mut send), (0, 0), "still mid-turn");
        assert_eq!(flush_held_tells(&store, &|_| Seen::Is(State::Idle), &mut send), (2, 0), "its next idle");
        assert!(sent.borrow()[0].contains("ninth") && sent.borrow()[1].contains("tenth"));
        assert!(held_tells(&store, &seat).is_empty());
        assert!(queued_tells(&store).is_empty(), "and wip draws nothing queued");
    }

    /// `wsp-659`, the verifier's note: the queue path returns before
    /// `already_sent`, so the same tell sent twice to a working seat was queued
    /// twice and typed twice. It is held once; `--again` holds it again, and a
    /// different sentence still queues behind it.
    #[test]
    fn the_same_tell_sent_twice_to_a_working_seat_is_queued_once() {
        use crate::place::State;
        let env = util::isolated("wake-queued-twice");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let (seat, _ends) = compound_seat_in(&store, "wsp-701", State::Working);

        let tell = |words: &str, flags: &[(&str, &str)]| {
            crate::cmd_agent::tell(&store, &crate::base::args::Args::synth("tell", &["wsp-701", words], flags))
        };
        assert_eq!(tell("rebase onto the trunk", &[]), 0);
        assert_eq!(tell("rebase onto the trunk", &[]), 0, "a repeat is no failure");
        assert_eq!(held_tells(&store, &seat).len(), 1, "but it is held once");
        assert_eq!(tell("and then rerun the checks", &[]), 0);
        assert_eq!(held_tells(&store, &seat).len(), 2, "a different sentence still queues");
        assert_eq!(tell("rebase onto the trunk", &[("again", "")]), 0);
        assert_eq!(held_tells(&store, &seat).len(), 3, "`--again` queues it anyway");
    }

    /// `wsp-659` item 2: the primary's tell to compounding-698's seat was
    /// pending when the seat was ended, and was lost with nothing said. A held
    /// tell whose seat has gone is said back to its sender — the primary's spool,
    /// a governor's, the person's list — and offered to the row's next seat
    /// when one is up.
    #[test]
    fn a_tell_held_for_a_seat_that_ends_is_said_back_to_its_sender_and_offered_to_the_rows_next_seat() {
        let env = util::isolated("wake-held-ended");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let from_primary = crate::cmd_agent::dressed("primary", "m-1", "rebase onto the trunk first", true);
        hold_tell_for(&store, "cpd-5808", &from_primary, Some("compounding-698"));
        let mut send = |_: &str, _: &str| true;
        let gone = |_: &str| Seen::Missing;
        assert_eq!(flush_held_tells(&store, &gone, &mut send), (0, 1));

        let owed = messages_held(&store, crate::governors::PRIMARY);
        assert_eq!(owed.len(), 1, "the primary is told: {owed:?}");
        assert!(owed[0].2.contains("not delivered") && owed[0].2.contains("cpd-5808"), "{}", owed[0].2);
        assert!(owed[0].2.contains("rebase onto the trunk first"), "and what it said: {}", owed[0].2);
        assert!(!owed[0].2.contains("reply: wsp tell"), "the footer is not said back: {}", owed[0].2);
        assert!(owed[0].2.contains("wsp tell seat:compounding-698"), "and how to send it again: {}", owed[0].2);
        let log = std::fs::read_to_string(store.state_file("cycle.log")).unwrap_or_default();
        assert!(log.contains("told cpd-5808: undelivered") && log.contains("said back to primary"), "{log}");

        // The row has a seat again: the tell is offered to it, and the sender told so.
        store.set_binding("cpd-5900", json!({ "pane": "cpd-5900", "task_id": "compounding-698" }));
        let from_gov = crate::cmd_agent::dressed("governor:compounding", "m-2", "land it after the check", true);
        hold_tell_for(&store, "cpd-5809", &from_gov, Some("compounding-698"));
        assert_eq!(flush_held_tells(&store, &gone, &mut send), (0, 1));
        let offered = held_tells(&store, "cpd-5900");
        assert_eq!(offered.len(), 1, "{offered:?}");
        assert!(offered[0].1.contains("land it after the check"));
        let gov = messages_held(&store, "compounding");
        assert!(gov.iter().any(|(_, _, t)| t.contains("offered to the row's next seat cpd-5900")), "{gov:?}");

        // From a shell, the person's: an item on their list.
        hold_tell(&store, "cpd-5810", &crate::cmd_agent::dressed("person", "m-3", "stop for the night", false));
        let still = |seat: &str| if seat == "cpd-5900" { Seen::Is(crate::place::State::Working) } else { Seen::Missing };
        assert_eq!(flush_held_tells(&store, &still, &mut send), (0, 1));
        let items = crate::message::raised(&store);
        assert!(items.iter().any(|m| m.text.contains("cpd-5810") && m.text.contains("stop for the night")), "{items:?}");
    }
}

/// Moved from `cycle`'s tests (wsp-543): they call seats code, above the engine.
#[cfg(test)]
mod from_cycle {
    use crate::cycle::*;
    use crate::cycle::tests::*;
    pub use crate::store::SAID;
    use crate::model::{Group, Worklist, WorklistStatus};
    use crate::util;

    /// `wsp-216`. `wsp-acceptance-fixes` closed past its last barrier with its
    /// post holding a failed reseat's record and nobody in it. The close
    /// returned at the seat lookup, so the post was never stood down; the
    /// closing verdict was then held on the list's own scope, with nobody coming
    /// to it, and reached nobody.
    ///
    /// Now the empty post is stood down at the close, and the verdict goes on a
    /// member, whose notice reaches the project's seat — saying which list's it
    /// was and why the project has it.
    #[test]
    fn a_close_over_a_post_nobody_is_in_stands_it_down_and_the_verdict_reaches_the_seat_above() {
        crate::cycle::install_seats(&crate::cmd_spawn::Fleet);
        let (_env, store, w) = passed_with_seats("emptypost");
        a_failed_reseat(&store, "run");
        let _ = drained(&SAID);

        after_pass(&store, &Exits, &w, 1);

        let governors = store.governors();
        assert!(crate::governors::stood_at(&governors, "run").is_some(), "the empty post is stood down: {governors:?}");
        let said = drained(&SAID);
        assert!(
            said.iter().any(|l| l == "run is closing — stood its own seat down; nobody was sitting in it"),
            "and says so: {said:?}"
        );
        assert!(drained(&ENDED).iter().all(|s| s != "cpd-7"), "there was nobody in it to end");

        let now = store.worklist("run").unwrap();
        assert_eq!(nobody_coming(&store, "run").as_deref(), Some("the list is done"));
        assert!(!hand_it_to_the_governor(&store, &now, "Barrier 1 of run passed."), "not held on its own scope");
        assert!(tell_now(&store, &now, "Barrier 1 of run passed."), "somebody has it");
        assert!(crate::wake::messages_held(&store, "run").is_empty(), "nothing waits on the list's post");
        let to_p = crate::wake::messages_held(&store, "p");
        assert!(
            to_p.iter().any(|(_, _, t)| t.starts_with("Meant for the run list's own seat, and nobody is coming to it: the list is done.")
                && t.contains("Barrier 1 of run passed.")),
            "the project's seat has it, and is told whose it was: {to_p:?}"
        );
    }

    /// `wsp-216`'s live repair: what was already held on a done list's post
    /// when it closed — `wsp-acceptance-fixes`' two, the barrier-3 verdict among
    /// them — is handed on by a tick, and taken off the post. A running list's
    /// held sentences are its own, and stay.
    #[test]
    fn a_tick_hands_on_what_a_done_lists_post_was_holding() {
        crate::cycle::install_seats(&crate::cmd_spawn::Fleet);
        let (_env, store, _w) = passed_with_seats("handon");
        a_failed_reseat(&store, "run");
        crate::wake::say(&store, "run", "m-1 is at doing with nothing working on it", None);
        crate::wake::say(&store, "run", "Barrier 1 of run passed.", None);
        let mut w = store.worklist("run").unwrap();
        w.set_status(WorklistStatus::Done);
        store.save_worklist(&w).unwrap();
        let mut other = Worklist::new("other", "other");
        other.set_status(WorklistStatus::Running);
        other.set_groups(&[Group { members: vec!["m-2".into()], agent: "claude".into(), ..Group::default() }]);
        store.save_worklist(&other).unwrap();
        crate::wake::say(&store, "other", "for a list still running", None);
        let _ = drained(&SAID);

        hand_on_what_done_lists_held(&store);

        assert!(crate::wake::messages_held(&store, "run").is_empty(), "taken off the done list's post");
        assert_eq!(crate::wake::messages_held(&store, "other").len(), 1, "a running list keeps its own");
        let to_p: Vec<String> = crate::wake::messages_held(&store, "p").into_iter().map(|(_, _, t)| t).collect();
        assert_eq!(to_p.len(), 2, "{to_p:?}");
        let today = util::local_ymd(&util::now_iso());
        assert!(
            to_p.iter().all(|t| t.contains("Meant for the run list's own seat") && t.contains(&format!("(Held on its scope since {today} "))),
            "dated, since a done list's backlog is days old as often as not: {to_p:?}"
        );
        assert!(to_p[1].contains("Barrier 1 of run passed."), "{to_p:?}");
        let said = drained(&SAID);
        assert!(said.iter().any(|l| l.contains("handed on 2 of the 2")), "{said:?}");

        hand_on_what_done_lists_held(&store);
        assert_eq!(crate::wake::messages_held(&store, "p").len(), 2, "once");
    }

    /// `wsp-151`. cr-linux closed on a machine whose project had no seat, and
    /// all 26 sentences held on its post went to the person as flags —
    /// started, parked, resumed, verified, none asking anything. With nobody
    /// above to keep current, a sentence that asks nothing is let go; one that
    /// asks still reaches the person.
    #[test]
    fn a_done_list_with_no_seat_above_lets_go_what_asks_nothing() {
        crate::cycle::install_seats(&crate::cmd_spawn::Fleet);
        let (_env, store, _w) = passed_with_seats("letgo");
        a_failed_reseat(&store, "run");
        assert!(store.clear_governor("p"), "nobody above the list");
        crate::wake::say(&store, "run", "The run list was parked: nothing starts until it is resumed · wsp worklist show run", None);
        crate::wake::say(&store, "run", "The run list was resumed where it was parked · wsp worklist show run", None);
        crate::wake::say(&store, "run", "m-1 has been blocked for 40 min and waits on: does the schema keep its old column?", None);
        let mut w = store.worklist("run").unwrap();
        w.set_status(WorklistStatus::Done);
        store.save_worklist(&w).unwrap();
        let before: Vec<String> = crate::message::all(&store).into_iter().map(|m| m.id).collect();
        let _ = drained(&SAID);

        hand_on_what_done_lists_held(&store);

        assert!(crate::wake::messages_held(&store, "run").is_empty(), "nothing is left on the done list's post");
        let raised: Vec<String> =
            crate::message::all(&store).into_iter().filter(|m| !before.contains(&m.id)).map(|m| m.text).collect();
        assert_eq!(raised.len(), 1, "only the one that asks: {raised:?}");
        assert!(raised[0].contains("does the schema keep its old column?"), "{raised:?}");
        let said = drained(&SAID);
        assert!(
            said.iter().any(|l| l.contains("handed on 1 of the 3") && l.contains("let go 2 that asked nothing")),
            "{said:?}"
        );

        hand_on_what_done_lists_held(&store);
        assert_eq!(crate::message::all(&store).len(), before.len() + 1, "once");
    }
}

/// Moved from `repair`'s tests (wsp-543): they call seats code, above the engine.
#[cfg(test)]
mod from_repair {
    use crate::repair::*;
    use crate::repair::tests::*;
    use crate::place::State;
    use crate::store::Store;

    /// `n` lines sitting in `scope`'s wake spool — somebody has written to it and
    /// nobody has taken it, which is the fact the second trigger reads.
    fn held_for(store: &Store, scope: &str, n: usize) {
        store.update_watch(&crate::wake::key_for(scope), |rec| {
            crate::cmd_watch::Spool::append(
                rec,
                (0..n)
                    .map(|i| {
                        crate::cmd_watch::Spooled::of(
                            0,
                            crate::cmd_watch::Line::Note(
                                crate::cmd_watch::Class::Message,
                                format!("owed {i}"),
                            ),
                        )
                    })
                    .collect(),
            );
        });
    }

    /// **A scope that holds three lines and owes a seat none of them.**
    ///
    /// `wsp-178`'s live case: `tokenhub-spec-sync` is a `done` list, all three of its
    /// spool entries are `edge: left`, and `wsp-166` withholds those from a governor
    /// seat — so it owes nothing, while the spool's depth says it is holding three.
    /// `wsp-148`'s second trigger asked about depth and read that as a scope owing an
    /// answer, and reseated it every twenty minutes for hours on a compound agent
    /// that never comes up (`unseated: 250`).
    ///
    /// **Built with `edge: left` and not with `held_for`.** The helper appends a
    /// `Class::Message`, and a message always counts as owed — so a fixture written
    /// with it passes against the bug, which is the fourth time on this row that a
    /// fixture's own convenience was the thing hiding the defect. This is
    /// `tokenhub-spec-sync`'s shape exactly: a level that moved on, whose lines stay
    /// in the record and are not typed.
    #[test]
    fn a_scope_holding_only_withheld_lines_owes_nothing_and_is_never_reseated() {
        crate::cycle::install_seats(&crate::cmd_spawn::Fleet);
        let (_env, store) = in_flight("seat-withheld");
        seated(&store, "run", "w1", "cpd-1", "", "");
        // The other half of `tokenhub-spec-sync`: a `done` list, so nothing in this
        // scope can arrive by the running-list trigger either.
        let mut w = store.worklist("run").unwrap();
        w.set_status(crate::model::WorklistStatus::Done);
        store.save_worklist(&w).unwrap();

        store.update_watch(&crate::wake::key_for("run"), |rec| {
            crate::cmd_watch::Spool::append(
                rec,
                (0..3)
                    .map(|i| {
                        let mut e = crate::cmd_watch::Emit {
                            edge: crate::cmd_watch::Edge::Left,
                            signal: crate::cmd_watch::Signal::new(
                                crate::cmd_watch::Kind::Review,
                                "m-1",
                                "the level moved on",
                            )
                            .to("run"),
                            held: 0,
                            to: "run".into(),
                        };
                        e.held = i;
                        crate::cmd_watch::Spooled::of(0, crate::cmd_watch::Line::News(e))
                    })
                    .collect(),
            );
        });

        let held = store
            .watches()
            .get(&crate::wake::key_for("run"))
            .and_then(|v| v.get("spool"))
            .and_then(|s| s.as_array())
            .map(Vec::len)
            .unwrap_or(0);
        assert_eq!(held, 3, "the spool really is holding three: the fixture has to be this shape");
        assert!(
            !crate::wake::scopes_owed(&store).iter().any(|(s, _)| s == "run"),
            "and it owes a seat none of them: {:?}",
            crate::wake::scopes_owed(&store)
        );

        let gone = Absent(vec!["cpd-1".to_string()]);
        for _ in 0..(EMPTY_TICKS + 3) {
            a_pass(&store, &gone);
        }

        assert!(
            reseated().is_empty(),
            "a scope nobody is owed anything to is not a vacancy: {:?}",
            reseated()
        );
        let _ = std::fs::remove_dir_all(&store.root);
    }

    /// **And the other side of the same line, so the fixture cannot be satisfied by a
    /// trigger that never fires.** One `edge: up` on a running list is owed, and a
    /// scope owing something with an empty seat is exactly what the second trigger is
    /// for — asserted here because the test above passes against a trigger that
    /// returns nothing at all, which is the same way the withholding fix could have
    /// been shipped broken.
    #[test]
    fn a_scope_owing_one_line_is_still_reseated_when_its_seat_is_empty() {
        crate::cycle::install_seats(&crate::cmd_spawn::Fleet);
        let (_env, store) = in_flight("seat-owed");
        seated(&store, "run", "w1", "cpd-1", "", "");
        held_for(&store, "run", 1);
        assert!(
            crate::wake::scopes_owed(&store).iter().any(|(s, _)| s == "run"),
            "a message counts as owed, which is the half the other test cannot see"
        );

        let gone = Fake::new(&[("cpd-1", Some(State::Gone))]);
        a_pass(&store, &gone);
        a_pass(&store, &gone);
        assert_eq!(reseated(), vec!["run".to_string()], "so the trigger is still live");
        let _ = std::fs::remove_dir_all(&store.root);
    }
}
