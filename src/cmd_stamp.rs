//! `wsp stamp` — has anything changed? — answered in one process start.
//!
//! [`Store::fingerprint`] and [`Store::attention_stamp`] have been `pub` all
//! along and reachable from outside the process never, because every consumer
//! so far held a `Store`: the panel and the daemon are wsp. A client that is a
//! *separate* process had two ways in and both are bad. Poll `wsp ls --json`
//! and its siblings — four invocations and ~150ms of parsing the whole store,
//! paid every interval, to discover that nothing changed, which is the exact
//! thing a fingerprint exists to avoid. Or walk `~/wsp` itself, which makes a
//! second reader of the on-disk shape; `worklist-009` is what that costs even
//! inside one repository, where `fingerprint` was written against the record
//! kinds its author knew about, `worklists/` arrived, and a worklist row sat
//! stale through a group finishing, a barrier opening and a status going to
//! `held`. The same walk copied into another repository is the same defect
//! with the divergence invisible from both ends.
//!
//! So: one verb, one process start, and it says nothing but the answer.
//!
//! ## Three stamps rather than one token
//!
//! A composite is the one shape that cannot be taken apart again, and taking
//! it apart is the client's whole decision. The two halves do not change at the
//! same rate and do not cost the same to re-read: the record is 468 files and
//! moves a few times an hour, the agent census is a backend call and moves at
//! every turn boundary of every agent. Mixed into one token, the cheap frequent
//! change pays for the expensive rare one on every occurrence. The panel is the
//! proof — it keeps its own gates apart, at 5/s for attention and 30s for the
//! record, precisely so a raised hand does not cost a `readdir` of the store.
//!
//! A client is still never asked to *combine* anything. Each stamp gates one
//! refetch, and a client that only wants "did anything move" compares three
//! values for equality instead of one. There is no arithmetic to get wrong,
//! which is the failure a single token is usually chosen to prevent.
//!
//! ## The census is not in the store, and it is not herdr's either
//!
//! `fingerprint`'s doc leaves ephemeral state out on the grounds that a raised
//! hand is not a change to the work and has a stamp of its own where it needs
//! one. That is right, and it leaves a hole a polling client falls straight
//! into: the whole agents half of a surface — the strip, the four waiting
//! states, `wsp wip` — would sit stale for ever behind a record stamp.
//!
//! The hole is not filled by a third stamp over `~/.local/state/wsp/`, and that
//! is the thing worth writing down, because a list of state files is the
//! obvious answer. **The census is not in those files.** Whether a turn is in
//! flight is [`State::turn_in_flight`], and no file in the store carries it: a
//! stamp over the state directory cannot see an agent begin a turn, finish one,
//! or die. The seat facts wsp *does* keep need no stamp of their own either,
//! because the verbs that write them write a task as well — `claim` ends in
//! `save_task`, so it has already moved the record stamp.
//!
//! **The other place is not herdr.** This file digested `herdr::panes()` and
//! read `agent_status`, `interactive_ready` and `launch_pending` for one day,
//! and that is a tie the rest of wsp spent [`crate::place`] removing — a port
//! written so that nothing in a signature names a pane, a window or a tab. Its
//! second implementor hosts agents with no terminal at all, and a surface that
//! polls this verb is a candidate third. A stamp wired to herdr's field names
//! would have to be unpicked again by exactly the migration this verb exists to
//! serve, so it asks [`Place::census`] and digests [`Seated`] — `seat`, `label`,
//! `agent`, `state`, `session`, in wsp's vocabulary — and [`State`] is already
//! backend-neutral.
//!
//! **Widening [`Store::attention_stamp`] was the other candidate and is worse.**
//! Its argument is that it is two `stat`s and can therefore sit on the fastest
//! tick there is, five times a second across twenty-two panels; a backend call
//! cannot go there. And it is addressed-to-somebody-*now* by definition — a
//! hand, a question — while an agent quietly finishing a turn is news without
//! being addressed to anyone. Folding the two would make the gate that exists
//! for questions fire on every `wsp say`.
//!
//! ## Silence is a state, and it is not "unchanged"
//!
//! **A `null` that means "I could not ask" compares equal to itself for ever.**
//! A client polling on an interval reads `null`, finds it equal to the `null`
//! before it, concludes nothing changed and goes on drawing the last census it
//! managed to fetch — live, beside a record half that is still moving. That is
//! a surface lying, and a client cannot tell the lie from this side of the
//! wire. It is `compound-062`.
//!
//! [`Census`] was built for this and says so: `heard` and `silent` are
//! different constructors, there is no `Default`, and *the cost of this bug is
//! a `Vec::new()` that looks like an answer*. So the answer here is never a
//! bare token. `agents` is an object carrying `heard`, a `stamp` and the
//! `silent` list, and **a client reads `heard` before it compares anything**:
//! `heard: false` is no signal, and the census it is holding is unknown rather
//! than unchanged.
//!
//! **A partial answer is an answer**, which is [`Census`]'s own rule — one
//! machine down out of four is three machines' worth of fact, and reading it as
//! a failure would empty the panel every time a laptop closed. So a silent
//! machine leaves `heard: true`, a stamp that still covers everybody who
//! answered, and its own row in `silent`.
//!
//! And the silent set is **in** the stamp, which is the half that is easy to
//! miss: when a far machine drops off, its seats simply stop appearing, and a
//! digest over the rows alone would move once and then read as *everybody there
//! stopped*. Hashing the set of silent machines makes the partition itself the
//! change, so a client refetches, finds them in `silent`, and draws them
//! unknown instead of empty. The *reason* is not hashed — a backend's error
//! text is a backend's error text and may differ run to run — so the stamp
//! moves when the set of silent machines changes and not when the wording does.
//!
//! ## What is in the digest, and why the rest is not
//!
//! `seat`, `label`, `agent.kind`, `agent.name`, `state`, `session` — identity,
//! what is running in it, what state that is in, and the sentence it is
//! wearing, which is what `wsp say` publishes into the label.
//!
//! `cwd` is deliberately out, and it is the same exclusion that kept herdr's
//! `title` and `focused` out before it: a stamp that moves when nothing a
//! census draws has moved costs a full refetch every time somebody types, which
//! is the same bill as not having a stamp at all. `agent.args` is out because
//! it is a fact about how a seat was *opened* rather than about what is in it,
//! and no census fills it in.
//!
//! Sorted by seat before hashing, because a backend's ordering is the
//! backend's and a reordering is not news.
//!
//! ## Which backend is asked
//!
//! **Every one `wsp wip` already asks, folded the same way — `compound-064`
//! item 2.** This used to be `cmd_spawn::backend(args)`, the one `--headless`
//! selects — singular, herdr by default — on the reasoning directly below,
//! now stale: `Census` keys by machine, not backend, so two backends folded
//! with [`Census::and`] file under `""` and `"compound"`
//! ([`crate::cmd_spawn::LOCAL_BACKEND_NAMES`]) rather than colliding on one
//! name, which is the fact that makes the fold safe. It had to change because
//! `Wip::live` went plural first (`compound-078`): `wsp wip --json` already
//! shows a `compound` session with no flag asked of its caller, while this
//! verb's token — the one thing that tells a polling surface *whether* to
//! reread `wip` at all — stayed singular and herdr-first. A change that lived
//! only in a `compound` session moved nothing here, so a host polling this
//! token alone never noticed (`compound-062`).
//!
//! **A whole backend answering nothing is not the fleet gone quiet.**
//! [`Census::was_heard`] already treats one silent MACHINE behind a single
//! backend as a partial answer — "a partial answer is an answer" is this
//! file's own rule, above — and folding a second backend in extends that
//! rule across the new axis rather than writing a second one: `heard` goes
//! false only when NOTHING answered, backend or machine, and a backend that
//! answered nothing folds in as a silent row exactly like a silent machine
//! does, under a name that cannot be mistaken for one. In practice
//! `Compound::census` almost never refuses at all — an empty run directory
//! is `Ok`, not `Err` — so what this mostly protects is the other direction:
//! a machine running `compound` sessions with no herdr installed must not
//! read as "no signal" the moment herdr's own call fails.
//!
//! ## Cost, measured rather than assumed
//!
//! It is on a client's interval, so the whole verb is the bill. Against the
//! live store on 2026-08-25, release build, the mean of 50 runs: **11.5ms** end
//! to end, of which 4.9ms is process start-up — `wsp --version` on the same
//! machine — and 4.09ms is [`Store::fingerprint`]'s own measured walk over 468
//! tasks. The rest is two `stat`s and the census — herdr's alone, at the time
//! this was measured; `compound-064` item 2 adds a second directory pass, on
//! `Compound::census`'s own reasoning that it costs no socket at all.
//!
//! **The port costs 1.2ms against the raw `herdr::panes()` this file digested
//! before it** — 11.5ms against 10.3ms, measured the same way — and the 1.2ms
//! buys the reading rather than only the decoupling: [`Place::census`] asks
//! `pane.list` *and* `agent.list`, which is the only way to tell a starting
//! agent from an idle one. A stamp blind to [`State::Starting`] would miss the
//! launch window, which is the window `agent.prompt` refuses in.
//!
//! What it replaces is `wsp ls --json --all`, 48.2ms on the same store, times
//! the four calls it takes to cover the record — and it replaces them on every
//! interval on which the answer is "nothing changed".
//!
//! ## Opaque
//!
//! Sixteen hex digits, and hex rather than a number so that nothing can subtract
//! them without first deciding to. Compared for equality and for nothing else:
//! neither stamp orders, and [`Store::fingerprint`]'s own doc records that a
//! collision is silent. A client that reads one as a version, a time or a size
//! is a client wsp will break.

use serde_json::{json, Value};

use crate::place::{Census, Seated};
#[cfg(test)]
use crate::place::Place;
use crate::store::Store;
use crate::util::Paint;
use crate::Args;

/// The three stamps, as this verb answers them.
struct Stamps {
    record: u64,
    attention: u64,
    agents: Agents,
}

/// The census half: a stamp when somebody answered, and who did not.
struct Agents {
    /// `None` is **nobody answered**, which is not the same fact as an empty
    /// census and must never be compared as one. See the module doc.
    stamp: Option<u64>,
    /// machine → why, for every backend that said nothing. `""` is this
    /// machine. Non-empty beside a `stamp` is a *partial* answer, which is
    /// still an answer.
    silent: Vec<(String, String)>,
}

impl Agents {
    /// Whether there is a census here at all. The first thing a client reads,
    /// and the thing a bare `null` could not say.
    fn heard(&self) -> bool {
        self.stamp.is_some()
    }
}

fn hex(v: u64) -> String {
    format!("{v:016x}")
}

/// FNV-1a over the bytes. Sixty-four bits, one pass, no dependency — the same
/// bargain the rest of this binary makes, and the stamp is compared for
/// equality only, so nothing here needs to be more than well mixed.
fn digest(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// The census, as one number.
///
/// Takes a [`Census`] and not a backend, which is the whole point: what is in
/// the stamp is a fact about the port's vocabulary, so it is testable against
/// any implementor — including one with no terminal — without a socket
/// anywhere near it.
fn census(c: &Census) -> u64 {
    let mut seats: Vec<&Seated> = c.seats().collect();
    seats.sort_by(|a, b| a.seat.as_str().cmp(b.seat.as_str()));
    let mut buf = String::new();
    // `\u{1f}` between fields so that two cannot be slid past each other — a
    // label ending in the next seat's id would otherwise hash the same as the
    // pair the other way round.
    fn field(buf: &mut String, s: &str) {
        buf.push_str(s);
        buf.push('\u{1f}');
    }
    for s in seats {
        field(&mut buf, s.seat.as_str());
        field(&mut buf, &s.label);
        field(&mut buf, &s.agent.kind);
        field(&mut buf, &s.agent.name);
        field(&mut buf, s.state.as_str());
        field(&mut buf, &s.session);
        buf.push('\u{1e}');
    }
    // The partition itself, so that a machine dropping off is a change rather
    // than its agents quietly ceasing to exist. The reason is left out on
    // purpose — see the module doc.
    let mut quiet: Vec<&str> = c.unheard().map(|(m, _)| m).collect();
    quiet.sort_unstable();
    for m in quiet {
        field(&mut buf, m);
        buf.push('\u{1d}');
    }
    digest(buf.as_bytes())
}

/// Who said nothing, and why, in the shape the answer publishes.
fn silences(c: &Census) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> =
        c.unheard().map(|(m, why)| (m.to_string(), why.to_string())).collect();
    out.sort();
    out
}

/// Keep the difference between an empty census and no census at all. Pure
/// over an already-taken [`Census`] — folded from every backend in
/// production ([`combined_census`]), a single one in the tests that want to
/// prove one backend's own seats reach the digest correctly without the
/// other backend's directory in the way.
fn agents_of(c: &Census) -> Agents {
    match c.was_heard() {
        true => Agents { stamp: Some(census(c)), silent: silences(c) },
        // Nobody answered — not one backend, not one machine behind any of
        // them. See [`Census::was_heard`].
        false => Agents { stamp: None, silent: silences(c) },
    }
}

/// One backend, asked and turned into [`Agents`] directly — what a caller
/// with its own `&dyn Place` in hand uses. Test-only: [`stamp`] asks every
/// backend through [`combined_census`] in production, and this is what
/// proves one backend's own seats reach the digest correctly in isolation.
#[cfg(test)]
fn agents_now(place: &dyn Place) -> Agents {
    agents_of(&place.census().unwrap_or_else(|why| Census::silent("", why)))
}

/// Ask every backend wsp can spawn onto and fold the answers into one
/// census — the module doc's "Which backend is asked" has the reasoning.
///
/// **The decision this function makes: a silent BACKEND reads the same way
/// a silent MACHINE already does.** A backend's own [`Refusal`] becomes
/// [`Census::silent`] under [`crate::cmd_spawn::LOCAL_BACKEND_NAMES`]'s name
/// for it, and [`Census::and`] folds it in exactly as it folds a far
/// machine's silence — one list, one digest, one `unheard` to read either
/// kind of gap off. No second rule: [`Census::was_heard`] is reused rather
/// than reinvented, so "a partial answer is an answer" — this file's
/// standing rule for one machine going quiet — now also covers a whole
/// backend nobody has installed folding in silent beside one that answered,
/// rather than reading as the fleet gone quiet. Proven against a real
/// compound seat with no herdr socket anywhere
/// (`a_compound_only_machine_is_heard_even_though_herdr_never_answers`):
/// `heard()` stays true, and the stamp still moves when that seat opens and
/// again when it closes.
fn combined_census() -> Census {
    let mut c: Option<Census> = None;
    for (name, backend) in crate::cmd_spawn::LOCAL_BACKEND_NAMES.iter().zip(crate::cmd_spawn::local_backends()) {
        let asked = backend.census().unwrap_or_else(|why| Census::silent(name, why));
        c = Some(match c {
            None => asked,
            Some(m) => m.and(asked),
        });
    }
    c.expect("local_backends() is never empty")
}

fn take(store: &Store) -> Stamps {
    Stamps {
        record: store.fingerprint(),
        attention: store.attention_stamp(),
        agents: agents_of(&combined_census()),
    }
}

/// A machine's name for a person. `""` is this one, and printing nothing there
/// puts a reason on the line with no subject.
fn named(machine: &str) -> &str {
    match machine.is_empty() {
        true => "this machine",
        false => machine,
    }
}

/// The answer as a client reads it.
///
/// `agents` is an object and never a bare token, so that "I could not ask"
/// cannot be reached by comparing one string to another — `heard` is read
/// first, and a client that skips it gets an object that still differs from the
/// one before it. See the module doc.
fn document(s: &Stamps) -> Value {
    json!({
        "record": hex(s.record),
        "attention": hex(s.attention),
        "agents": {
            "heard": s.agents.heard(),
            "stamp": s.agents.stamp.map(hex),
            "silent": s.agents.silent.iter()
                .map(|(m, why)| json!({ "machine": m, "why": why }))
                .collect::<Vec<_>>(),
        },
    })
}

/// The same three, for a person. Names on the left in a fixed column so the
/// tokens line up under each other, which is the only way two of these are ever
/// compared by eye.
fn lines(s: &Stamps, p: &Paint) -> Vec<String> {
    let row = |name: &str, v: String| format!("{} {}", p.dim(&format!("{name:<9}")), v);
    let mut out = vec![
        row("record", hex(s.record)),
        row("attention", hex(s.attention)),
        // "no signal" and not "-": a dash reads as a value that happens to be
        // empty, which is the reading this whole section exists to refuse.
        row("agents", s.agents.stamp.map(hex).unwrap_or_else(|| "no signal".into())),
    ];
    for (m, why) in &s.agents.silent {
        out.push(row("silent", format!("{} · {why}", named(m))));
    }
    out
}

pub fn stamp(store: &Store, args: &Args) -> i32 {
    let s = take(store);
    match args.json() {
        true => println!("{}", document(&s)),
        false => {
            for l in lines(&s, &Paint::new()) {
                println!("{l}");
            }
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::place::{Agent, Order, Refusal, Seat, State};
    use crate::util;

    fn seated(seat: &str) -> Seated {
        Seated {
            seat: Seat::new(seat),
            agent: Agent { kind: "claude".into(), ..Agent::default() },
            state: State::Idle,
            ..Seated::default()
        }
    }

    fn here(seats: Vec<Seated>) -> Census {
        Census::heard("", seats)
    }

    /// The reading no file in the store carries, and the reason the census is
    /// asked of a backend at all: an agent starting a turn, finishing one, or
    /// stopping in front of a permission prompt.
    #[test]
    fn an_agent_changing_state_moves_the_census() {
        let idle = here(vec![seated("p1")]);
        for other in [State::Working, State::Starting, State::Blocked, State::Gone] {
            let mut row = seated("p1");
            row.state = other;
            assert_ne!(
                census(&idle),
                census(&here(vec![row])),
                "{} was invisible beside idle",
                other.as_str()
            );
        }
    }

    /// `wsp say` publishes its sentence into the seat's label, so the label is
    /// where a status line changing is visible; a census that skipped it would
    /// draw yesterday's sentence.
    #[test]
    fn a_seat_saying_something_new_moves_the_census() {
        let before = here(vec![seated("p1")]);
        let mut row = seated("p1");
        row.label = "landed the doorbell fix".into();
        assert_ne!(census(&before), census(&here(vec![row])));
    }

    /// A seat arriving or leaving is the loudest thing in a census, and it is
    /// what the state directory cannot see: a seat ending clears its binding
    /// only once a daemon tick has reconciled it.
    #[test]
    fn a_seat_arriving_or_leaving_moves_the_census() {
        let one = here(vec![seated("p1")]);
        let two = here(vec![seated("p1"), seated("p2")]);
        assert_ne!(census(&one), census(&two), "a second agent was invisible");
        assert_ne!(census(&two), census(&here(vec![seated("p2")])), "a departure was invisible");
    }

    /// A backend's ordering is the backend's own. Without the sort this would
    /// refetch the whole store on whatever order a listing came back in.
    #[test]
    fn the_same_seats_in_another_order_are_not_news() {
        let a = here(vec![seated("p1"), seated("p2"), seated("p3")]);
        let b = here(vec![seated("p3"), seated("p1"), seated("p2")]);
        assert_eq!(census(&a), census(&b));
    }

    /// The exclusion is the half of an argument a test usually loses. A cwd
    /// moves whenever somebody cds, and a stamp that moves when nothing a
    /// census draws has moved costs a full refetch every time a person types.
    #[test]
    fn a_seat_changing_directory_is_not_census_news() {
        let before = here(vec![seated("p1")]);
        let mut row = seated("p1");
        row.cwd = "~/claude/wsp/.worktrees/wsp-100".into();
        assert_eq!(census(&before), census(&here(vec![row])), "a cwd moved the stamp");
    }

    /// Fields are separated rather than run together, so no seat can be slid
    /// past its neighbour into the same digest.
    #[test]
    fn two_seats_cannot_be_confused_for_one_by_running_their_fields_together() {
        let mut a = seated("p1");
        a.agent = Agent { kind: "claude".into(), name: "wsp-100".into(), args: Vec::new() };
        let mut b = seated("p1");
        b.agent = Agent { kind: "claudewsp".into(), name: "-100".into(), args: Vec::new() };
        assert_ne!(census(&here(vec![a])), census(&here(vec![b])));
    }

    /// **A machine dropping off is a change, not its agents ceasing to exist.**
    /// Its seats stop appearing in `seats()`, so a digest over the rows alone
    /// would move once and thereafter read as everybody there having stopped.
    #[test]
    fn a_machine_going_silent_is_a_change_and_not_an_empty_machine() {
        let both = here(vec![seated("p1")]).and(Census::heard("mb2", vec![seated("p9@mb2")]));
        let partitioned =
            here(vec![seated("p1")]).and(Census::silent("mb2", Refusal::Unreachable("gone".into())));
        assert_ne!(census(&both), census(&partitioned), "the partition was invisible");

        // …and it is not the same as mb2 answering that it holds nothing.
        let empty = here(vec![seated("p1")]).and(Census::heard("mb2", vec![]));
        assert_ne!(
            census(&empty),
            census(&partitioned),
            "a silent machine hashed the same as one holding no agents"
        );
    }

    /// The wording of a refusal is a backend's own and may differ run to run.
    /// A stamp that moved with it would refetch the store on the phrasing of an
    /// error message.
    #[test]
    fn the_wording_of_a_refusal_is_not_in_the_stamp() {
        let a = Census::silent("mb2", Refusal::Unreachable("connection refused".into()));
        let b = Census::silent("mb2", Refusal::Backend("no route to host".into()));
        assert_eq!(census(&here(vec![seated("p1")]).and(a)), census(&here(vec![seated("p1")]).and(b)));
    }

    /// `compound-062`. A client polling reads `heard` before it compares
    /// anything, because "I could not ask" compares equal to itself for ever
    /// and would otherwise be read as "nothing changed" — a census frozen
    /// beside a record half that is still moving.
    #[test]
    fn nobody_answering_is_said_rather_than_left_to_compare_equal() {
        let quiet = Stamps {
            record: 1,
            attention: 2,
            agents: Agents { stamp: None, silent: vec![(String::new(), "no herdr socket".into())] },
        };
        let doc = document(&quiet);
        assert_eq!(doc["agents"]["heard"], json!(false), "silence did not say so");
        assert!(doc["agents"]["stamp"].is_null(), "silence came back as a value");
        assert_eq!(doc["agents"]["silent"][0]["machine"], json!(""));
        assert_eq!(doc["agents"]["silent"][0]["why"], json!("no herdr socket"));

        let drawn = lines(&quiet, &Paint::plain());
        assert!(
            drawn.iter().any(|l| l.contains("no signal")),
            "no row said the census was unanswered: {drawn:?}"
        );
        assert!(drawn.iter().any(|l| l.contains("this machine")), "{drawn:?}");
    }

    /// The other side of it: a census that *was* heard and holds nothing is a
    /// fact, and it must not read as silence.
    #[test]
    fn a_backend_holding_nothing_is_an_answer_and_not_a_silence() {
        let heard_nothing = here(vec![]);
        let empty = Stamps {
            record: 1,
            attention: 2,
            agents: Agents { stamp: Some(census(&heard_nothing)), silent: Vec::new() },
        };
        let doc = document(&empty);
        assert_eq!(doc["agents"]["heard"], json!(true), "an empty census read as no answer");
        assert!(doc["agents"]["stamp"].is_string());
        assert_eq!(doc["agents"]["silent"], json!([]));
    }

    /// **A second backend, and it has no terminal at all.** The digest is a
    /// function of the port's vocabulary, so a supervisor's seats arrive in it
    /// on exactly the same terms as a pane's — which is the whole reason this
    /// file no longer reads `agent_status`.
    #[test]
    fn a_backend_with_no_terminal_answers_into_the_same_stamp() {
        let root = std::env::temp_dir()
            .join(format!("wsp-stamp-super-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&root);
        let place = crate::place_super::Supervisor::at(root.clone());

        let empty = agents_now(&place);
        assert!(empty.heard(), "a supervisor with no seats still answered");
        assert!(empty.silent.is_empty());

        let seat = place
            .open(&Order { label: "wsp-100 · stamp".into(), ..Order::default() })
            .expect("a seat");
        let opened = agents_now(&place);
        assert_ne!(opened.stamp, empty.stamp, "a seat opening was invisible to the stamp");

        // And the same rows, however they were come by, are the same stamp:
        // nothing in the digest knows which backend filled them in.
        let by_hand = Census::heard(
            "",
            place.census().unwrap().seats().cloned().collect::<Vec<_>>(),
        );
        assert_eq!(opened.stamp, Some(census(&by_hand)));

        place.stop(&seat).expect("the seat was there");
        assert_ne!(agents_now(&place).stamp, opened.stamp, "the seat ending was invisible");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `compound-064` item 2's own decision, proven against real backends:
    /// a machine with `compound` sessions and no herdr up at all is HEARD —
    /// herdr's own total refusal folds in as a silent row under `""`
    /// (`crate::cmd_spawn::LOCAL_BACKEND_NAMES`), never as the whole answer
    /// going to `None`. `stamp` (the CLI verb) is what this file used to ask
    /// a single, flag-selected backend for; asking `combined_census` proves
    /// the fold `stamp` now runs on rather than a hand-built `Agents`.
    #[test]
    fn a_compound_only_machine_is_heard_even_though_herdr_never_answers() {
        let _env = util::isolated("stamp-compound-only");
        let store = Store::open();
        // No herdr socket bound anywhere — `HERDR_SOCKET_PATH` names a file
        // that does not exist, `util::isolated`'s own doing.
        let compound = crate::place_compound::Compound::new();

        let before = take(&store);
        assert!(before.agents.heard(), "a real (if empty) compound census is still an answer");
        assert!(
            before.agents.silent.iter().any(|(m, _)| m == ""),
            "herdr's own refusal is on record: {:?}",
            before.agents.silent
        );
        assert!(
            !before.agents.silent.iter().any(|(m, _)| m == "compound"),
            "compound answered — it must not also read as silent: {:?}",
            before.agents.silent
        );

        let seat = compound.open(&Order::default()).expect("a compound seat");
        let opened = take(&store);
        assert_ne!(
            opened.agents.stamp, before.agents.stamp,
            "a seat that exists only in compound's own directory must move the token \
             `wsp stamp` publishes, or a host polling it never re-reads `wip` (compound-062)"
        );

        compound.stop(&seat).expect("the seat was there");
        let closed = take(&store);
        assert_ne!(closed.agents.stamp, opened.agents.stamp, "the seat ending was invisible");
    }

    /// Hex and not a number, so that a client wanting to subtract two stamps
    /// has to decide to. Sixteen digits always, so a token is a fixed-width
    /// string a client can store in a column.
    #[test]
    fn a_stamp_is_sixteen_hex_digits_and_never_a_number() {
        assert_eq!(hex(0), "0000000000000000");
        assert_eq!(hex(u64::MAX), "ffffffffffffffff");
        assert_eq!(hex(1).len(), 16);
    }
}
