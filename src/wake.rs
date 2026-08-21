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

use crate::agent_commands;
use crate::cmd_govern;
use crate::cmd_watch::{Emit, Line, Sink, Spec, Spool, Spooled, Stream, EVERYONE};
use crate::place::Seat as Pane;
use crate::store::Store;
use crate::util;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// How a seat's wake spool is named in the register.
///
/// Beside `wsp watch`'s own records and in the same file, so `--status` and
/// `doctor` read one place. The prefix is what tells the two apart: a watch key
/// is a pane and this is a scope, and they share a key space.
pub(crate) fn key_for(scope: &str) -> String {
    format!("wake:{scope}")
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
        mine.entry(e.to.clone()).or_default().push(e);
    }
    for (scope, theirs) in mine {
        deliver_to(store, &scope, &theirs, at);
    }
}

/// One seat's turn: spool everything, write it down, then see whether it is
/// owed a wake.
fn deliver_to(store: &Store, scope: &str, emits: &[&Emit], at: i64) {
    let key = key_for(scope);
    let spec = Spec::for_wake(scope);
    let delivered = record(store, &key).0;
    // Nothing new and nothing held is nothing to do. Said here rather than at
    // the caller because the caller visits every seat with a record, and a seat
    // at rest must not cost a store write every twenty seconds for ever.
    if emits.is_empty() && load(store, &key).depth() == 0 {
        return;
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

    // Nothing is offered as *hot*: everything this pass produced is already in
    // the spool by the line above, so the only question left is whether that
    // spool owes somebody a context read. `Stream::tick` answers it with the
    // same table `wsp watch --wake` uses, and returns what actually left —
    // which is empty unless the sink took it.
    // Taken before the flush, because a flush empties the copy in hand and the
    // number is what says *everything up to here has gone*.
    let watermark = Spool::watermark(&spool.held);
    let mut tell = Tell::new(store, scope);
    let written = Stream::new(&spec, &mut tell).tick(at, &mut spool).len();
    store.update_watch(&key, |rec| {
        // Delivered, so gone — by identity, so a drain that took them first is
        // not undone and anything appended since is not swept away with them.
        if written > 0 {
            Spool::settle(rec, watermark);
        }
        let now = Spool::of_json(rec.get("spool").unwrap_or(&serde_json::Value::Null));
        stamp(rec, scope, delivered + written, tell.why, &now);
    });
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
    scope: String,
    /// Why the last attempt did not land, in the words `--status` prints.
    ///
    /// **A delivery path nobody can inspect is the seventh way silence lies**,
    /// and this field is that lesson learned the hard way twice in one hour:
    /// driving this row, a wake sat at `0 delivered · holding 2` and the record
    /// could not say whether nobody held the seat, the agent was mid-turn, or
    /// the table simply had not judged anything worth a wake. Those want three
    /// different repairs and the first two are faults.
    why: &'static str,
}

impl<'a> Tell<'a> {
    fn new(store: &'a Store, scope: &str) -> Tell<'a> {
        Tell { store, scope: scope.to_string(), why: NOT_YET }
    }
}

/// Nothing here is worth a context read on its own. The ordinary state, and
/// not a fault: it is what `core-017`'s table is for.
const NOT_YET: &str = "nothing worth a wake yet";

/// Why a pane would not take a wake, in the words `--status` and `doctor`
/// print.
///
/// **One sentence per state rather than one for all of them**, because they
/// want four different repairs and a reader who cannot tell them apart goes
/// looking in the wrong place: mid-turn clears itself and is not a fault at
/// all, `Blocked` needs a person at a keyboard, `Starting` clears itself in
/// seconds, and `Unknown` means herdr could not be asked — which is a fault
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
        State::Unknown => Some("herdr cannot say what the seat is doing"),
    }
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
        let governors = self.store.governors();
        let Some(seat) = cmd_govern::seat_of_scope(&self.scope, &governors) else {
            // Nobody holds the post. Not a failure and not a drop: the spool
            // keeps it, and a seat filled tomorrow morning is told what it
            // missed. A wake with no addressee is the one case where holding
            // is obviously right.
            self.why = "no seat on this scope";
            return false;
        };
        let Some(pane) = cmd_govern::occupant(&seat) else {
            self.why = "the seat is empty";
            return false;
        };
        let how = agent_commands::of(&pane.agent);
        let place = crate::place_herdr::Herdr::new();
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
        let addressee = Pane::new(&pane.pane_id);
        if how.queue_is_the_agents() {
            // `State::Unknown` when herdr could not be asked, which
            // [`held_because`] refuses — an absence is not a fact, least of all
            // the fact that somebody is there to read this.
            let state = crate::place::Place::state(&place, &addressee).unwrap_or_default();
            if let Some(why) = held_because(state) {
                self.why = why;
                return false;
            }
        }
        let text = format!("{}\n{}", preamble(&self.scope), said.join("\n"));
        // No `Sent`, no `already_sent`, no `twice` — see this type's docs.
        match how.tell(&place, &addressee, &text) {
            Ok(_) => true,
            Err(_) => {
                self.why = "the seat would not take it";
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
            cmd_govern::take(&store, scope, &space, &pane);
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
        for (state, scope) in states.iter().skip(1) {
            assert_eq!(spool_of(&store, scope).depth(), 1, "{scope} is still owed it");
            assert_eq!(
                holding(&store, scope),
                held_because(*state).expect("a refusal has a sentence"),
                "{scope} says why",
            );
        }
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
        let tell = Tell::new(&store, "core");

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
}
