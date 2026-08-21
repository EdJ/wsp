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
//! # The turn gate is for durability, not for corruption
//!
//! `core-021` d2, driven against a real Claude Code rather than a fake. A
//! prompt delivered mid-turn is *queued* by Claude Code and answered when the
//! turn ends; nothing is typed at a live composer. What is not durable is the
//! queue: it lives in the agent, so a wake handed to it and then `/clear`ed,
//! restarted or killed is a wake wsp has recorded as delivered and nobody ever
//! read. Holding it costs ninety seconds and keeps the spool as the record for
//! the whole window.
//!
//! Asked of the *kind* rather than written into this path, because it is a fact
//! about a transport and not about waking a governor — see
//! [`crate::agent_commands::Kind::queue_is_the_agents`].

use crate::agent_commands;
use crate::cmd_govern;
use crate::cmd_watch::{Emit, Line, Sink, Spec, Spool, Stream, EVERYONE};
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
    let mut spool = load(store, &key);
    let delivered = record(store, &key).0;
    // Nothing new and nothing held is nothing to do. Said here rather than at
    // the caller because the caller now visits every seat with a record, and a
    // seat at rest must not cost a store write every twenty seconds for ever.
    if emits.is_empty() && spool.depth() == 0 {
        return;
    }

    for e in emits {
        spool.put(at, Line::News((*e).clone()));
    }
    // Written down before anything is attempted — see the module docs. If this
    // process dies in the next line, the fact is on disk and the next tick
    // delivers it.
    save(store, &key, &spool, delivered, NOT_YET);

    // Nothing is offered as *hot*: everything this pass produced is already in
    // the spool by the line above, so the only question left is whether that
    // spool owes somebody a context read. `Stream::tick` answers it with the
    // same table `wsp watch --wake` uses, and returns what actually left —
    // which is empty unless the sink took it, so it is also the count.
    let mut tell = Tell::new(store, scope);
    let sent = Stream::new(&spec, &mut tell).tick(at, &mut spool).len();
    save(store, &key, &spool, delivered + sent, tell.why);
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

impl Sink for Tell<'_> {
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
        // The gate. `core-021` d2: not corruption — Claude Code queues a
        // mid-turn prompt and answers it at the boundary — but durability,
        // because that queue is the agent's and dies with it while the spool
        // does not.
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
        let addressee = Pane::new(&pane.pane_id);
        if how.queue_is_the_agents()
            && crate::place::Place::state(&place, &addressee).is_ok_and(|s| s.turn_in_flight())
        {
            self.why = "the seat is mid-turn";
            return false;
        }
        let text = said.join("\n");
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
fn save(store: &Store, key: &str, spool: &Spool, delivered: usize, why: &str) {
    store.set_watch(
        key,
        json!({
            "scope": key.strip_prefix("wake:").unwrap_or(key),
            "host": util::hostname(),
            // No process. This is a record of what the daemon owes a seat, not
            // of a reporter that could die — see `cmd_watch::Registered::watching`.
            "pid": 0,
            "tick": util::now_iso(),
            "wake": true,
            "delivered": delivered,
            // Why it is still holding whatever it is holding. Empty when it is
            // holding nothing, because a reason for a thing that is not
            // happening is noise — see [`Tell::why`].
            "holding": match spool.depth() {
                0 => String::new(),
                _ => why.to_string(),
            },
            "spool": spool.json(),
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
