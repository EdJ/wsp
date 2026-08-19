//! The unattended pass: the level set derived on the daemon's tick, and the
//! two edges of it that leave the process.
//!
//! `robustness-088`'s complaint in one line: **nothing notices a stopped agent
//! unless somebody runs a command.** `doctor`, `wip`, `overlap`, the panel and
//! `wsp watch` all state the fact correctly and all five of them state it to
//! whoever is looking. `cmd_agent::quiet_note`'s `State::Blocked` arm is the
//! sharpest instance — *"stopped on a prompt only a person can answer"*, and it
//! deliberately skips the hour on the ground that the repair is one keystroke —
//! and on 2026-08-19 the one event in a whole session that genuinely required
//! Ed was exactly that, with nothing raising a hand, because the agent was
//! blocked before it could raise one.
//!
//! The daemon is the only long-lived process wsp has. It is therefore the only
//! thing that can look when nobody asked.
//!
//! # What this is not, which is where the task was blocked
//!
//! It **raises no flag and writes no message.** That was the open question —
//! *"raising a flag is a store mutation on a timer, and that is a different
//! kind of thing to review"* — and `wsp-095` answered it by distinguishing a
//! level from an edge. A stopped agent is a [`crate::message::Shape::Signal`]:
//! derived, true-now, self-clearing, and never stored, because *a queued level
//! is a fact that has stopped being true sitting on a panel*, which is this
//! task's own stated fear. So [`crate::message::raise`] refuses one and so does
//! `Store::save_message`, and nothing here tries.
//!
//! Which disposes of all four questions the task could not answer:
//!
//! - *What clears it?* The predicate going false. There is nothing to clear.
//! - *What stops it re-raising every fifteen minutes?* It never re-raises,
//!   because it never arrives. [`Ledger`] emits on change and never on state.
//! - *Does it go to a governor that has itself stopped?* [`addressee`] walks
//!   [`cmd_govern::seat_for`], which terminates at *everyone*; and a stopped
//!   seat is a subject of the same predicate, so it raises its own hand.
//! - *Is an hour right unattended?* Not asked here. The pass reads
//!   [`cmd_watch::Poll`], which measures how long a level has actually held
//!   rather than guessing from a proxy.
//!
//! # One computation, and this is its second caller
//!
//! Every predicate is [`cmd_watch::Poll`]'s, unchanged and not re-derived. That
//! is the whole discipline of `robustness-083`'s lesson — `agent_status ==
//! "idle"` was written out four times in four censuses and three of them were
//! wrong — and of `wsp watch`'s own: four hand-built monitors re-derived
//! `needs_a_person` while standing in front of the field that already carried
//! it. A daemon pass that computed its own conjunction would be the fifth.
//!
//! What is new here is only what a *subscription* cannot supply:
//!
//! 1. **A scope nobody asked for.** [`cmd_watch::Scope::machine`] — every task
//!    on the box, because the pass is not answering for anybody.
//! 2. **An address per signal rather than per run.** A watch is one seat asking
//!    about its own scope, so `to` is a constant. Here every signal is routed
//!    on its own subject, and most of them are addressed somewhere different.
//! 3. **A ledger that survives the process.** See [`load`].
//!
//! # The sink, and its honest limit
//!
//! `events.jsonl` and `~/wsp/hooks/on-attention-raised`. `wsp-095` Part 5 is
//! right that the hook costs nothing to reuse and is the escape to anything
//! louder — *a desktop notification is already an executable file* — and it is
//! worth being plain that **wsp ships no hook**, so on a machine with an empty
//! `hooks/` this pass writes a line to a log and reaches nobody. `hooks/`
//! carries a sample that works on this machine; making it executable is the
//! whole installation, and it is deliberately a person's decision, because what
//! is allowed to interrupt Ed at 3am is not wsp's call to make.
//!
//! The one thing this must never become is a push into a working agent. That is
//! Part 5's hard refusal — `agent.prompt` to a working agent *queues*, which is
//! `robustness-093`'s mechanism with seven recorded instances — and nothing
//! here has an agent's pane in its hand at all.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::cmd_govern;
use crate::cmd_watch::{Edge, Emit, Kind, Ledger, Poll, Scope, Signal, Source};
use crate::resolve::Index;
use crate::store::Store;
use crate::util;
use crate::worklist;

/// How often the pass runs, in seconds.
///
/// Sixty, and it is `wsp watch`'s interval rather than the daemon's twenty for
/// a reason that is arithmetic and not taste. A tick is one store sweep and
/// three herdr calls; the daemon's own 20s tick is a debounce for herdr events,
/// which is a different job at a different price, and riding on it would have
/// tripled this one for news that moves in minutes.
///
/// The arithmetic, because a cost claim in this tree is measured rather than
/// asserted. 2026-08-19, this machine, the live store: 453 task files, four
/// agent panes, twenty level reads timed as a batch — **51ms wall and 47ms of
/// CPU each**, process start included and below the timer's resolution. At
/// sixty seconds that is 0.08% of one core; at twenty it would be 0.25%.
///
/// The number that would move it is the store sweep, not herdr: `Poll::sample`
/// reads every task file, which is what the panel and `sync` already do on
/// their own timers. If that ever stops being free it stops being free for all
/// three at once, and the fix belongs in `Store::tasks` rather than here.
///
/// Nothing else is tuned to it. Every threshold below is a duration against a
/// clock, so the interval changes how soon news arrives and changes nothing
/// about what counts as news.
const EVERY: i64 = 60;

/// How long a level must hold before it is said, for the levels that flap.
///
/// `wsp watch`'s five minutes, and the same argument: an agent between turns is
/// stopped for seconds and a turn in flight is not stopped at all, so five
/// minutes of genuinely no turn on a task that is still `doing` is not a gap
/// between turns. [`Signal::at_once`] carries the exception, and the reading
/// that earns it is herdr's `blocked` — a modal holding the keyboard, where
/// waiting five minutes to mention it would be waiting five minutes to say a
/// word.
const SETTLE: i64 = 5 * 60;

/// Where the ledger lives in the watch register.
///
/// A key and not a pane id, because there is one of these per store and it is
/// not in a pane. It sorts to the front of `wsp watch --status`, which is where
/// a person wants the machine's own reading.
pub(crate) const KEY: &str = "daemon";

/// Who a signal is for, or `everyone`.
///
/// Per signal, which is the one thing a subscription cannot do: `wsp watch`
/// asks one scope's question and every answer is that scope's, while the pass
/// asks the machine's and every answer goes somewhere different.
///
/// [`cmd_govern::seat_for`] and not a walk of our own, so a signal reaches
/// exactly the seat a hand raised on the same task would — including the
/// running worklist's seat, which is tried before the project chain. A subject
/// that is not a task at all (`herdr`, for [`Kind::Blind`]) has no chain to
/// walk and is everybody's by construction, which is correct: wsp being unable
/// to see the agents is not one seat's problem.
///
/// [`Store::task`] and deliberately not [`Store::task_now`], which is the
/// resolving read every other place that turns a *recorded* id into a task now
/// uses. A subject arrives here off the ledger, and the ledger persists in
/// `watches.json` — so this looked like the fourth instance of the raw-read
/// fault. It is not, because `watches.json` is in
/// [`Store::state_files_with_ids`]: the ledger's subjects are rewritten by a
/// renumbering, so an id that misses here is one that has genuinely gone, and a
/// falling edge on work nobody can find belongs to everybody.
fn addressee(
    store: &Store,
    index: &Index,
    governors: &BTreeMap<String, Value>,
    lists: &worklist::Running,
    subject: &str,
) -> String {
    let Some(task) = store.task(subject) else { return EVERYONE.into() };
    cmd_govern::seat_for(governors, index, lists.list_of(&task.id), task.project.as_deref())
        .map(|s| s.scope)
        .unwrap_or_else(|| EVERYONE.into())
}

/// The address of a signal nobody in particular answers for.
///
/// A word rather than an absent field, because the reader is a shell script:
/// `everyone` is a thing to test for and a null is a thing to forget to test
/// for. It is the panel's own last resort said out loud — `wsp flag`'s
/// *"raised on every panel"* — and it is the common case on a machine with no
/// seats, which is why `wsp-095` Part 9 asks that it be visible rather than
/// silently universal.
pub(crate) const EVERYONE: &str = "everyone";

/// The event kind an edge is logged and hooked under.
fn event_kind(edge: Edge) -> &'static str {
    match edge {
        Edge::Up => "attention-raised",
        Edge::Down => "attention-cleared",
    }
}

/// Read back what the last pass was holding.
///
/// **The ledger is on disk because the process is not durable and the levels
/// are.** The daemon `exec`s itself whenever an install lands underneath it
/// (`daemon::reload`), and herdr restarts it on its own restart. An in-memory
/// ledger would be empty on the far side of both, so every level standing at
/// that moment would be primed away — or, worse, announced again as new. Ed
/// installs several times a day; a pass that re-announced the machine's whole
/// standing set on each one is the panel-full-of-flags failure delivered by
/// phone.
///
/// [`Ledger::of_json`]'s own note is what makes this work at all: the whole
/// signal is stored and not just its timing, so a level that has *gone* while
/// the daemon was down is still there to be compared against and its clearing
/// is still reported.
///
/// The second half of the answer is [`cmd_watch::resume`], and it is the same
/// sentence taken one step further: the file this reads is routinely the
/// previous binary's, so what it holds may be keyed by rules this build does
/// not use. The flag it comes back with is *may this be diffed against*, and it
/// is false for both of the reasons a diff would lie — there is no ledger, or
/// there is one and another build wrote it.
fn load(store: &Store) -> Option<(Ledger, bool)> {
    let rec = store.watches().get(KEY).cloned()?;
    rec.get("ledger").is_some().then(|| crate::cmd_watch::resume(&rec))
}

/// Write the ledger back, with the fact that a reporter ticked.
///
/// `pid` is the daemon's own and is not zero, which is the difference between
/// this and `wsp watch --once`'s pull ledger: there **is** a process that
/// claimed to keep reporting, so `wsp doctor` may say when it stopped.
/// `daemon::health` reports a daemon that is *gone*; this reports one that is
/// alive and has stopped ticking, which is the wedged case and the one nothing
/// else in the tree could state — a hook that blocks is enough to cause it,
/// because `Store::run_hook` waits for its child.
fn save(store: &Store, ledger: &Ledger, ticks: u64) {
    store.set_watch(
        KEY,
        json!({
            "scope": Scope::machine().name,
            "seated": false,
            // What makes `doctor` offer the right repair. A stalled watch is
            // forgotten with `wsp watch --forget`; a stalled daemon is not, and
            // advice that does not work is worse than none.
            "daemon": true,
            "pid": std::process::id(),
            "host": util::hostname(),
            "tick": util::now_iso(),
            "every": EVERY,
            "ticks": ticks,
            "standing": ledger.standing(),
            "watching": Kind::every().iter().map(|k| k.word()).collect::<Vec<_>>(),
            // Whose key rules the ledger below is written by. See
            // [`cmd_watch::resume`] for what reads it and what it cost not to
            // have it.
            "build": crate::build_stamp(),
            "ledger": ledger.json(),
        }),
    );
}

/// The pass's state between ticks, which is only when it last ran.
///
/// The ledger itself deliberately is **not** held here. It is read from the
/// store and written back on every tick, so the daemon and anything else
/// reading the register see one answer, and so an `exec` in the middle of the
/// loop loses nothing. One file write a minute against a file the panel already
/// polls is not a cost worth holding state to avoid.
pub(crate) struct Pass {
    /// `None` until it has run once. An `Option` and not a zero, because a
    /// sentinel that means *never* only reads as never against a real clock —
    /// the first version was `0` and was correct for the daemon and wrong for
    /// every test that supplies its own time, which is the wrong way round.
    last: Option<i64>,
    ticks: u64,
}

impl Pass {
    pub(crate) fn new() -> Pass {
        Pass { last: None, ticks: 0 }
    }

    /// Whether it is time. Asked by the daemon's loop rather than slept on,
    /// because the loop belongs to the sync and this is a passenger on it.
    ///
    /// A fresh pass is due at once: a daemon started by hand is normally
    /// started because something is already wrong, and there is nothing to
    /// announce on the first tick anyway — see the priming in [`tick`].
    pub(crate) fn due(&self, at: i64) -> bool {
        self.last.is_none_or(|last| at - last >= EVERY)
    }
}

/// One tick: read the levels, diff them, deliver the edges.
///
/// The source is a parameter and not built here, which is the whole of what
/// makes this testable without a herdr. [`cmd_watch::Source`] already asks for
/// a level set rather than for news, so a fake answers it in three lines and
/// the settle rule, the addressing and the emission are all driven without a
/// socket. `wsp verify` runs on a machine where herdr may not be up at all.
pub(crate) fn tick(store: &Store, pass: &mut Pass, source: &mut dyn Source, at: i64) -> Vec<Emit> {
    pass.last = Some(at);
    // A store that is not there reads as a machine with no work on it, so every
    // level would clear at once and the whole standing set would arrive as news
    // saying it was over. `wsp watch` ends its run for this; the daemon has
    // other jobs, so it skips the pass and keeps its ledger, and the first tick
    // with a store to read carries on where it left off.
    if !store.exists() {
        return Vec::new();
    }
    pass.ticks += 1;
    let now = source.sample();
    let (mut ledger, known) = load(store).unwrap_or_default();
    // Priming on the first pass ever, for `Ledger::prime`'s reason: a
    // subscription is to changes from now, and a daemon started on a machine
    // with six standing stalls must not announce six things that have been true
    // for hours. `prime` lets `blind` through and only `blind`, because a
    // reader that cannot see the agents from its first tick must say so or
    // everything it goes on not to say is meaningless.
    //
    // And priming on the first pass after an install, which is the same
    // statement about a different discontinuity: `daemon::reload` `exec`s
    // within a tick of a build landing on our path, and what the new image
    // inherits is a file the old one keyed. See [`cmd_watch::resume`] — a diff
    // across that boundary reports one clearing that never happened and one
    // raising for a level that never went down, per standing hand, and the
    // whole of what makes this affordable is that `prime` keeps what it
    // recognises.
    let emits = match known {
        true => ledger.advance(&now, at, SETTLE),
        false => ledger.prime(&now, at),
    };
    // The ledger is written **before** anything is delivered, and that order is
    // a choice about which failure to have. A hook is somebody's executable: it
    // may block, and `Store::run_hook` waits for its child. Writing first means
    // a hook that dies takes its one notification with it; writing after would
    // mean a hook that dies gets the same notification again on every tick
    // until it stops dying. At-most-once for an interruption, and the fact
    // itself is never lost — it is a level, so it is still standing in the
    // register and in `wsp wip`, `doctor` and the panel.
    save(store, &ledger, pass.ticks);
    deliver(store, &emits);
    emits
}

/// Put each edge on the event log, where a hook may see it.
///
/// Separate from the diff so that what is *derived* and what is *sent* are two
/// readable halves — and because the second is the half with a side effect on
/// somebody's phone.
fn deliver(store: &Store, emits: &[Emit]) {
    if emits.is_empty() {
        return;
    }
    let index = Index::new(store.projects());
    let governors = store.governors();
    let lists = worklist::Running::read(store);
    let stamp = util::now_iso();
    for e in emits {
        let to = addressee(store, &index, &governors, &lists, &e.signal.subject);
        store.log_event(event_kind(e.edge), payload(&e.signal, e.edge, &to, &stamp, e.held));
    }
}

/// One edge, as `wsp-095` Part 4's envelope plus the two things an envelope has
/// no room for.
///
/// [`Signal::envelope`] is the shared shape and is asked rather than copied, so
/// what a hook receives and what `wsp watch --json` prints are one document. It
/// carries `shape: "signal"` — [`crate::message::Shape::Signal`] — which is the
/// word that tells a reader this is a level and not something anybody wrote:
/// there is no id to answer, no state to close and nobody to reply to, and a
/// consumer that treats it as a notification would be waiting for a disposition
/// that can never come.
///
/// `held` is the addition. A stall reported after settling has been true for
/// five minutes and one cleared after nine hours was ignored for nine hours,
/// and those are different pieces of news; the envelope has no field for it
/// because a level does not know its own age, only a diff does.
fn payload(signal: &Signal, edge: Edge, to: &str, at: &str, held: i64) -> Value {
    let mut v = signal.envelope(edge, to, at);
    if let Some(o) = v.as_object_mut() {
        o.insert("held".into(), json!(held));
        // Said rather than left to the reader's arithmetic, because the reader
        // is a shell script deciding whether to make a noise and `duration_human`
        // is not something it has.
        o.insert("held_human".into(), json!(util::duration_human(held)));
    }
    v
}

/// What is standing, by subject, for a surface that draws one word per row.
///
/// **Ed, 2026-08-17, and it is the half of `robustness-051` that is not a
/// delivery at all:** *"when the agent raises questions to the user, we don't
/// see a flag or any similar notification in the UI."* The tokens `sync`
/// publishes are `scope`, `task`, `taskid`, `tstatus` — four facts about which
/// work this is and none about whether it is waiting on somebody. So an agent
/// stopped with a question and an agent mid-turn drew identically in the one
/// surface that is on screen all the time, which is the whole reason a person
/// had to open a pane to find out.
///
/// **Read off the ledger and not recomputed**, which is the discipline the
/// fourth hand-built monitor broke while standing in front of the field that
/// answered it. This is the pass's third consumer — `wsp watch`'s stream, the
/// hook, and now the sidebar — and all three are looking at one derivation.
///
/// The ledger is the right source rather than a fresh [`Poll`] for a reason
/// beyond cost: it is the only one that knows how long a level has held, so a
/// stall inside its settle window is correctly absent here. And it cannot go
/// stale behind the sidebar, because the process that writes it is the process
/// that publishes the tokens — [`tick`] and `sync::sync` are two statements in
/// one loop, and a daemon that has stopped doing the first has stopped doing
/// the second.
///
/// One word per subject, and where two levels stand on one task it is the
/// louder — a question somebody wrote beats a stall wsp inferred, because it
/// has words in it and the person reading has to go and look either way.
pub(crate) fn standing(store: &Store) -> BTreeMap<String, &'static str> {
    // The stamp is not asked about here. Whether this build would key the set
    // the same way changes what may be *diffed*; it changes nothing about what
    // is standing, and a surface that drew nothing across an install would be
    // the same silence by another route.
    let Some((ledger, _)) = load(store) else { return BTreeMap::new() };
    let mut out: BTreeMap<String, (crate::message::Kind, &'static str)> = BTreeMap::new();
    for s in ledger.told() {
        let here = (s.loudness(), s.kind.word());
        out.entry(s.subject.clone())
            .and_modify(|held| {
                if here.0 < held.0 {
                    *held = here;
                }
            })
            .or_insert(here);
    }
    out.into_iter().map(|(k, (_, word))| (k, word)).collect()
}

/// A source over the whole machine, for the daemon.
pub(crate) fn machine_source(store: &Store) -> Poll<'_> {
    Poll::new(store, Scope::machine(), Kind::every().into_iter().collect(), None)
}

/// Whether the pass has anywhere to deliver to, for `wsp doctor`.
///
/// **A delivery path with nothing at the end of it is the failure this file
/// exists to remove, wearing the shape of the fix.** The pass can run
/// perfectly, derive the right level, write the right line, and reach nobody —
/// and every surface would report healthy, which is exactly the note-shaped
/// silence `robustness-083` cost a night to. So the one thing wsp can honestly
/// say about its own sink, it says.
///
/// A note and never a problem, and only once the pass has actually registered.
/// A person who reads the panel and wants nothing buzzing at them has a
/// correctly configured machine, and a check that called that broken would be
/// the false alarm that teaches people to skip this section.
///
/// The executable bit is part of the question because `Store::run_hook` tests
/// `is_file` and then spawns: a hook that is present and not executable fails
/// silently, one `chmod` away from working, with nothing anywhere saying so.
pub(crate) fn health(store: &Store, notes: &mut Vec<String>) {
    if store.watches().get(KEY).is_none() {
        return;
    }
    let hook = store.root.join("hooks").join("on-attention-raised");
    let runnable = std::fs::metadata(&hook).is_ok_and(|m| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            m.is_file() && m.permissions().mode() & 0o111 != 0
        }
        #[cfg(not(unix))]
        {
            m.is_file()
        }
    });
    if runnable {
        return;
    }
    let why = match hook.is_file() {
        true => format!("{} is not executable (`chmod +x`)", util::contract(&hook)),
        false => format!("there is no {}", util::contract(&hook)),
    };
    notes.push(format!(
        "the daemon derives attention signals and {why}, so nothing it notices reaches \
         anybody unattended. Whatever is there is handed the event as JSON on stdin — \
         `hooks/on-attention-raised.sample` in the wsp tree is a working one, and \
         `wsp watch` is the same facts with a person in front of them"
    ));
}

/// Stand the pass down, so nothing reads its last tick as a reporter that died.
///
/// Called where the daemon ends on purpose — and it **keeps the ledger** and
/// drops only the claim to be reporting. Clearing the record outright was the
/// first version and it was wrong in the one case it exists for: this runs when
/// another daemon has taken the store, so the successor would find no ledger,
/// [`tick`] would prime, and every level standing at the moment of the hand-over
/// would be swallowed silently and for ever. Handing over is precisely when
/// continuity matters most.
///
/// `pid: 0` is the same statement `wsp watch --once` makes — a ledger, not a
/// reporter — so `cmd_watch::health` stops expecting ticks from it without
/// anything having to remember to forget it.
///
/// Not called on the reload path. An `exec` is the same daemon continuing, and
/// it is still reporting.
pub(crate) fn stand_down(store: &Store) {
    let Some(mut rec) = store.watches().get(KEY).cloned() else { return };
    if let Some(o) = rec.as_object_mut() {
        o.insert("pid".into(), json!(0));
    }
    store.set_watch(KEY, rec);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Group, Task, Worklist, WorklistStatus};

    /// A source that answers with whatever the test is holding.
    ///
    /// Three lines, and that is the argument for [`Source`] asking for a level
    /// set rather than for news: the settle rule, the addressing, the ledger's
    /// survival and the hook are all driven here without a herdr socket, on a
    /// machine where `wsp verify` may be running with no herdr at all.
    struct Fake(Vec<Signal>);

    impl Source for Fake {
        fn sample(&mut self) -> Vec<Signal> {
            self.0.clone()
        }
    }

    fn store(tag: &str) -> (util::Isolated, Store) {
        let env = util::isolated(&format!("attention-{tag}"));
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        (env, store)
    }

    fn task(store: &Store, id: &str, project: Option<&str>) {
        let mut t = Task::new("a task", id);
        t.project = project.map(str::to_string);
        store.save_task(&t).unwrap();
    }

    fn stall(subject: &str) -> Signal {
        Signal::new(Kind::NeedsAPerson, subject, "w1:p1 · no turn running")
    }

    /// Every `attention-*` line in the log, oldest first.
    fn delivered(store: &Store) -> Vec<Value> {
        let mut out = store.events_of("attention-raised");
        out.extend(store.events_of("attention-cleared"));
        out
    }

    /// **The thing the task is about.** Nobody ran a command; the pass ran on a
    /// timer and the fact left the process.
    #[test]
    fn a_stall_nobody_asked_about_reaches_the_event_log() {
        let (_env, store) = store("reaches");
        task(&store, "a-1", None);
        let mut pass = Pass::new();

        // Primed on a quiet machine, so the first tick has nothing to say.
        assert!(tick(&store, &mut pass, &mut Fake(vec![]), 0).is_empty());

        let mut src = Fake(vec![stall("a-1")]);
        let out = tick(&store, &mut pass, &mut src, 60);
        assert_eq!(out.len(), 1, "the stall is news");
        let sent = delivered(&store);
        assert_eq!(sent.len(), 1, "and it was written where a hook can see it");
        assert_eq!(sent[0]["about"], "a-1");
        assert_eq!(sent[0]["shape"], "signal", "a level, so nothing is owed back");
        assert_eq!(sent[0]["edge"], "up");
    }

    /// Emit on change, never on state — the question this task blocked on, as
    /// an assertion. *"What stops it re-raising every fifteen minutes?"* It
    /// never raises at all; it is a level that is either up or down.
    #[test]
    fn a_level_that_stays_up_is_delivered_once_and_never_again() {
        let (_env, store) = store("once");
        task(&store, "a-1", None);
        let mut pass = Pass::new();
        tick(&store, &mut pass, &mut Fake(vec![]), 0);

        let mut src = Fake(vec![stall("a-1")]);
        assert_eq!(tick(&store, &mut pass, &mut src, 60).len(), 1);
        for t in 2..20 {
            assert!(tick(&store, &mut pass, &mut src, t * 60).is_empty(), "tick {t} said it again");
        }
        assert_eq!(delivered(&store).len(), 1, "one hook call for one fact");
    }

    /// **Ed's other half of `robustness-051`.** The pass reaching a hook wakes
    /// a person who is away; this is the surface that is on screen the whole
    /// time, and until now it carried four tokens about *which work* and none
    /// about whether the work was waiting on anybody.
    #[test]
    fn what_is_standing_is_readable_by_the_surface_that_is_always_on_screen() {
        let (_env, store) = store("sidebar");
        task(&store, "a-1", None);
        let mut pass = Pass::new();
        tick(&store, &mut pass, &mut Fake(vec![]), 0);
        assert!(standing(&store).is_empty(), "a quiet machine costs the sidebar nothing");

        tick(&store, &mut pass, &mut Fake(vec![stall("a-1")]), 60);
        assert_eq!(standing(&store).get("a-1").copied(), Some("needs-a-person"));

        // …and it goes away with the fact, because it is the same level.
        tick(&store, &mut pass, &mut Fake(vec![]), 120);
        assert!(standing(&store).get("a-1").is_none());
    }

    /// A level still inside its settle window is an agent between turns as far
    /// as anybody knows. Drawing it would put a word on a permanently visible
    /// surface for four minutes and take it off again, which is the flicker the
    /// settle exists to prevent.
    #[test]
    fn a_stall_that_has_not_settled_yet_is_not_drawn_anywhere() {
        let (_env, store) = store("settling");
        task(&store, "a-1", None);
        let mut pass = Pass::new();
        tick(&store, &mut pass, &mut Fake(vec![]), 0);

        let mut src = Fake(vec![Signal::new(Kind::NeedsAPerson, "a-1", "w1:p1 · no turn").settling()]);
        tick(&store, &mut pass, &mut src, 60);
        assert!(standing(&store).is_empty(), "one minute in, this is a gap between turns");

        tick(&store, &mut pass, &mut src, 60 + SETTLE);
        assert_eq!(standing(&store).get("a-1").copied(), Some("needs-a-person"));
    }

    /// Two levels on one task and one word to draw. The louder wins, and a
    /// question somebody wrote is louder than a stall wsp inferred — it has
    /// words in it, and the person reading has to go and look either way.
    #[test]
    fn where_two_levels_stand_on_one_task_the_sidebar_draws_the_louder() {
        let (_env, store) = store("louder");
        task(&store, "a-1", None);
        let mut pass = Pass::new();
        tick(&store, &mut pass, &mut Fake(vec![]), 0);

        let asked = Signal::new(Kind::Unanswered, "a-1", "w1:p1 waiting · may I land?").loud();
        tick(&store, &mut pass, &mut Fake(vec![stall("a-1"), asked]), 60);
        assert_eq!(standing(&store).get("a-1").copied(), Some("unanswered"));
    }

    /// A daemon that has just started draws the whole standing set at once,
    /// where it deliberately *announces* none of it. Priming is about what is
    /// news; the sidebar is about what is true, and a level standing across a
    /// restart is still true.
    #[test]
    fn a_primed_level_is_drawn_even_though_it_was_never_announced() {
        let (_env, store) = store("primed");
        task(&store, "a-1", None);
        let mut pass = Pass::new();
        assert!(tick(&store, &mut pass, &mut Fake(vec![stall("a-1")]), 0).is_empty());
        assert_eq!(standing(&store).get("a-1").copied(), Some("needs-a-person"));
    }

    /// The predicate going false is the only thing that clears it, and the
    /// clearing is itself news: *do not go and prod that agent.*
    #[test]
    fn the_predicate_going_false_is_what_lowers_it() {
        let (_env, store) = store("clears");
        task(&store, "a-1", None);
        let mut pass = Pass::new();
        tick(&store, &mut pass, &mut Fake(vec![]), 0);
        tick(&store, &mut pass, &mut Fake(vec![stall("a-1")]), 60);

        let out = tick(&store, &mut pass, &mut Fake(vec![]), 120);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].edge, Edge::Down);
        let sent = store.events_of("attention-cleared");
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["held"], 60, "and it says how long it was up");
    }

    /// **An install must not re-announce the machine.** The daemon `exec`s
    /// itself whenever one lands, so the in-memory half of this is gone several
    /// times a day; a ledger that did not survive would put every standing
    /// stall on somebody's phone on each one.
    #[test]
    fn a_daemon_that_restarts_does_not_announce_what_it_already_said() {
        let (_env, store) = store("restart");
        task(&store, "a-1", None);
        let mut pass = Pass::new();
        tick(&store, &mut pass, &mut Fake(vec![]), 0);
        assert_eq!(tick(&store, &mut pass, &mut Fake(vec![stall("a-1")]), 60).len(), 1);

        // The exec: a new process, a new `Pass`, the same store.
        let mut after = Pass::new();
        assert!(
            tick(&store, &mut after, &mut Fake(vec![stall("a-1")]), 120).is_empty(),
            "the same stall arrived twice"
        );
        // And the far side still knows enough to report it clearing, which is
        // the case a ledger holding only timings would have lost.
        assert_eq!(tick(&store, &mut after, &mut Fake(vec![]), 180).len(), 1);
    }

    /// Say the ledger on disk was written by some other build, which is what
    /// every ledger the daemon reads after an install is.
    fn written_by_another_build(store: &Store) {
        let mut rec = store.watches().get(KEY).cloned().expect("there is a ledger to restamp");
        rec.as_object_mut().unwrap().insert("build".into(), json!("another-build"));
        store.set_watch(KEY, rec);
    }

    /// **`worklist-034`, driven through the pass that produced it.** An install
    /// lands, `daemon::reload` `exec`s within the tick, and the ledger the new
    /// image inherits is keyed by the rules of the build that wrote it —
    /// `Signal::key` gained a `record` component in `34ae8a3`, so one raised
    /// hand is `flag:a-1` on one side of the install and `flag:a-1:m-1` on the
    /// other.
    ///
    /// Diffed, that is a clearing that never happened and a raising for a level
    /// that never went down, for every standing hand on the machine, at
    /// whatever hour the install lands. What it costs instead is one quiet
    /// tick.
    #[test]
    fn an_install_that_moved_a_key_costs_one_quiet_tick_and_not_two_false_edges() {
        let (_env, store) = store("rekeyed");
        task(&store, "a-1", None);
        let mut pass = Pass::new();
        tick(&store, &mut pass, &mut Fake(vec![]), 0);
        let hand = Signal::new(Kind::Flag, "a-1", "can I take this?");
        assert_eq!(tick(&store, &mut pass, &mut Fake(vec![hand]), 60).len(), 1);

        written_by_another_build(&store);
        let rekeyed = Signal::new(Kind::Flag, "a-1", "can I take this?").of("m-1");
        let mut after = Pass::new();
        let out = tick(&store, &mut after, &mut Fake(vec![rekeyed]), 120);
        assert!(out.is_empty(), "the hand never moved, so nothing is owed anybody: {out:?}");
        assert_eq!(delivered(&store).len(), 1, "one edge in the whole run, and it is the true one");
        assert_eq!(standing(&store).get("a-1").copied(), Some("flag"), "and it is still up on the sidebar");

        // The tick after is a diff again: the quiet is one tick, not a mode.
        let out = tick(&store, &mut after, &mut Fake(vec![]), 180);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].edge, Edge::Down);
    }

    /// The ledger remembers by id, so a renumbering has to reach it.
    ///
    /// Nothing here is emitted twice, and that is the whole assertion. Left
    /// behind, the ledger's copy of the old id makes the next tick read one
    /// standing level as two events — a fall under the name that has gone and
    /// a rise under the one that replaced it — so a person is interrupted
    /// again about a task they were already told about, and the falling half
    /// is addressed to everybody because nothing answers to its subject.
    ///
    /// Asserted through [`Store::state_files_with_ids`] rather than beside it:
    /// a test keeping its own copy of a hand-kept list is the same mistake with
    /// a green tick over it.
    #[test]
    fn a_level_standing_when_its_task_is_renumbered_is_not_reported_all_over_again() {
        assert!(
            Store::state_files_with_ids().contains(&"watches.json"),
            "the attention ledger is keyed by task id and lives in this file",
        );

        let (_env, store) = store("renumbered");
        task(&store, "t-260815-014", Some("worklist"));
        let mut pass = Pass::new();
        tick(&store, &mut pass, &mut Fake(vec![]), 0);
        tick(&store, &mut pass, &mut Fake(vec![stall("t-260815-014")]), 60);
        assert_eq!(store.events_of("attention-raised").len(), 1, "told once, as it should be");

        let map = std::collections::BTreeMap::from([(
            "t-260815-014".to_string(),
            "worklist-002".to_string(),
        )]);
        store.rename_tasks(&map).unwrap();

        // The source computes its subjects from the store, so after the rename
        // it says the same thing under the new name.
        let emits = tick(&store, &mut pass, &mut Fake(vec![stall("worklist-002")]), 120);
        assert!(
            emits.is_empty(),
            "the same level under its new name is not a change: {emits:?}",
        );
        assert_eq!(store.events_of("attention-raised").len(), 1, "and nobody is told twice");
        assert!(
            store.events_of("attention-cleared").is_empty(),
            "least of all that the work stopped needing them",
        );
    }

    /// A signal is derived, so it is **never a record**. `wsp-095`'s answer to
    /// this task's own stated fear: a queued level is a fact that has stopped
    /// being true sitting on a panel.
    #[test]
    fn nothing_the_pass_derives_is_ever_stored_as_a_message_or_a_flag() {
        let (_env, store) = store("nostore");
        task(&store, "a-1", None);
        let mut pass = Pass::new();
        tick(&store, &mut pass, &mut Fake(vec![]), 0);
        tick(&store, &mut pass, &mut Fake(vec![stall("a-1")]), 60);

        assert!(store.messages().is_empty(), "a level is not a message");
        assert!(store.flags().is_empty(), "and it is not a hand somebody raised");
    }

    /// Addressed per signal by the walk a raised hand uses, so a governor is
    /// told about its own scope and nobody is told about everything.
    #[test]
    fn a_signal_is_addressed_to_the_seat_that_answers_for_its_subject() {
        let (_env, store) = store("address");
        task(&store, "a-1", Some("robustness"));
        task(&store, "b-1", None);
        store.set_governor(
            "robustness",
            json!({ "workspace": "w9", "pane": "w9:p1", "host": util::hostname() }),
        );
        let mut pass = Pass::new();
        tick(&store, &mut pass, &mut Fake(vec![]), 0);
        tick(&store, &mut pass, &mut Fake(vec![stall("a-1"), stall("b-1")]), 60);

        let sent = store.events_of("attention-raised");
        let to = |about: &str| {
            sent.iter()
                .find(|v| v["about"] == about)
                .map(|v| v["to"].as_str().unwrap_or_default().to_string())
                .expect("delivered")
        };
        assert_eq!(to("a-1"), "robustness", "the seat that governs it");
        assert_eq!(to("b-1"), EVERYONE, "and no seat above it means a person, not nowhere");
    }

    /// The routing rule a worklist adds, on the surface that runs unattended.
    ///
    /// `worklist-005`: a member of tonight's run is answered for by whoever is
    /// running it, not by whoever governs the project it happens to live in.
    /// Asserted here because the live check of it on 2026-08-19 could not
    /// exercise it — the running list had no seat, so the flag fell through to
    /// the project chain, correctly and without testing anything.
    #[test]
    fn a_member_of_a_running_worklist_is_answered_for_by_the_list() {
        let (_env, store) = store("worklist");
        task(&store, "a-1", Some("robustness"));
        store.set_governor(
            "robustness",
            json!({ "workspace": "w9", "pane": "w9:p1", "host": util::hostname() }),
        );
        store.set_governor(
            "tonight",
            json!({ "workspace": "w3", "pane": "w3:p1", "host": util::hostname() }),
        );
        let mut w = Worklist::new("tonight", "tonight");
        w.set_status(WorklistStatus::Running);
        w.set_groups(&[Group { members: vec!["a-1".into()], ..Default::default() }]);
        store.save_worklist(&w).unwrap();

        let mut pass = Pass::new();
        tick(&store, &mut pass, &mut Fake(vec![]), 0);
        tick(&store, &mut pass, &mut Fake(vec![stall("a-1")]), 60);
        assert_eq!(store.events_of("attention-raised")[0]["to"], "tonight");
    }

    /// **The sink, driven rather than described.** `wsp-095` Part 5 says the
    /// hook is the escape to anything louder because a desktop notification is
    /// already an executable file. This is that claim executed: a script in
    /// `hooks/`, a tick, and the payload on its stdin.
    #[test]
    fn the_hook_is_run_with_the_signal_on_its_stdin() {
        let (env, store) = store("hook");
        task(&store, "a-1", None);
        let landed = env.path("landed");
        let hook = env.home().join("hooks/on-attention-raised");
        std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
        std::fs::write(&hook, format!("#!/bin/sh\ncat > {}\n", landed.display())).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let mut pass = Pass::new();
        tick(&store, &mut pass, &mut Fake(vec![]), 0);
        tick(&store, &mut pass, &mut Fake(vec![stall("a-1")]), 60);

        let got = std::fs::read_to_string(&landed).expect("the hook ran");
        let v: Value = serde_json::from_str(&got).expect("and was handed JSON");
        assert_eq!(v["kind"], "attention-raised");
        assert_eq!(v["data"]["about"], "a-1");
        assert_eq!(v["data"]["to"], EVERYONE);
    }

    /// The loud reading, carried all the way to the sink.
    ///
    /// This is the whole reason the task exists: `quiet_note`'s `State::Blocked`
    /// arm is the one signal wsp computes that genuinely needs a person, it
    /// deliberately skips every threshold, and until now nothing carried it
    /// anywhere. A hook reads `kind` to decide whether to make a noise, so the
    /// promotion has to survive the envelope.
    #[test]
    fn a_modal_holding_the_keyboard_arrives_as_direction_and_not_as_a_note() {
        let (_env, store) = store("loud");
        task(&store, "a-1", None);
        let blocked = Signal::new(
            Kind::NeedsAPerson,
            "a-1",
            "w1:p1 · stopped on a prompt only a person can answer",
        )
        .loud();
        let mut pass = Pass::new();
        tick(&store, &mut pass, &mut Fake(vec![]), 0);
        tick(&store, &mut pass, &mut Fake(vec![blocked]), 60);

        let sent = store.events_of("attention-raised");
        assert_eq!(sent[0]["kind"], "direction", "the only kind that may reach a person at once");
        assert_eq!(sent[0]["shape"], "signal");
    }

    /// The daemon's ledger is a reporter's, not a pull caller's, so `doctor`
    /// may say when it stopped — and it is marked, so `doctor` offers the
    /// repair that works.
    #[test]
    fn the_pass_registers_as_something_that_can_be_reported_as_having_stopped() {
        let (_env, store) = store("register");
        let mut pass = Pass::new();
        tick(&store, &mut pass, &mut Fake(vec![]), 0);

        let rec = store.watches().get(KEY).cloned().expect("registered");
        assert_eq!(rec["daemon"], true);
        assert_eq!(rec["pid"], std::process::id(), "a process claimed to keep looking");

        stand_down(&store);
        let after = store.watches().get(KEY).cloned().expect("the ledger stays");
        assert_eq!(after["pid"], 0, "a clean exit takes the claim back");
        assert!(after.get("ledger").is_some());
    }

    /// **A hand-over must not swallow the machine.** Standing down used to clear
    /// the record, so the daemon taking over found no ledger, primed, and lost
    /// every level that was standing at that moment — silently, and for ever,
    /// which is the exact failure this file was written to remove.
    #[test]
    fn handing_the_store_to_another_daemon_carries_the_ledger_across() {
        let (_env, store) = store("handover");
        task(&store, "a-1", None);
        let mut pass = Pass::new();
        tick(&store, &mut pass, &mut Fake(vec![]), 0);
        tick(&store, &mut pass, &mut Fake(vec![stall("a-1")]), 60);
        stand_down(&store);

        // The successor. It knows the stall was already said, and it still
        // knows enough to report it clearing.
        let mut next = Pass::new();
        assert!(tick(&store, &mut next, &mut Fake(vec![stall("a-1")]), 120).is_empty());
        assert_eq!(tick(&store, &mut next, &mut Fake(vec![]), 180).len(), 1, "the clearing was lost");
    }

    /// It is a passenger on the daemon's loop and keeps its own time, so a
    /// busy fleet waking the daemon every second does not turn a minute's job
    /// into a per-event one.
    #[test]
    fn the_pass_keeps_its_own_interval_rather_than_the_loops() {
        let (_env, store) = store("due");
        let mut pass = Pass::new();
        assert!(pass.due(0), "a fresh daemon looks at once, whatever the clock says");
        tick(&store, &mut pass, &mut Fake(vec![]), 1_000);
        assert!(!pass.due(1_030));
        assert!(pass.due(1_000 + EVERY));
    }
}
