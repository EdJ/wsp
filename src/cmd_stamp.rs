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
//! moves a few times an hour, the agent census is two socket calls and moves at
//! every turn boundary of every agent. Mixed into one token, the cheap frequent
//! change pays for the expensive rare one on every occurrence. The panel is the
//! proof — it keeps its own gates apart, at 5/s for attention and 30s for the
//! record, precisely so a raised hand does not cost a `readdir` of the store.
//!
//! A client is still never asked to *combine* anything. Each stamp gates one
//! refetch, and a client that only wants "did anything move" compares three
//! strings for equality instead of one. There is no arithmetic to get wrong,
//! which is the failure a single token is usually chosen to prevent.
//!
//! ## The agents stamp is taken from herdr and not from the state directory
//!
//! `fingerprint`'s doc leaves ephemeral state out on the grounds that a raised
//! hand is not a change to the work and has a stamp of its own where it needs
//! one. That is right, and it leaves a hole a polling client falls straight
//! into: the whole agents half of a surface — the strip, the four waiting
//! states, `wsp wip` — would sit stale for ever behind a record stamp.
//!
//! The hole is not filled by a third stamp over `~/.local/state/wsp/`, and that
//! is the thing worth writing down, because a list of state files is the
//! obvious answer. **The census is not in those files.** `turning` and the
//! waiting states are read by [`crate::place_herdr::state_of_agent`] out of
//! `agent_status`, `interactive_ready` and `launch_pending`, which are herdr's
//! fields and reach wsp only over the socket. A stamp over the state directory
//! cannot see an agent begin a turn, finish one, or die.
//!
//! The seat facts wsp *does* keep — a binding, a claim — need no stamp of their
//! own either, because the verbs that write them write a task as well:
//! `claim` ends in `save_task`, so it has already moved the record stamp. What
//! is left is the state a pane exiting leaves behind, and a pane exiting is the
//! most visible thing there is in herdr's own answer.
//!
//! So the agents stamp is a digest of `herdr::panes()` — the same call the
//! panel's own status poll makes, over the same fan-out, so a far machine's
//! agents are in it and a partition is not read as everybody stopping.
//!
//! **Widening [`Store::attention_stamp`] was the other candidate and is worse.**
//! Its argument is that it is two `stat`s and can therefore sit on the fastest
//! tick there is, five times a second across twenty-two panels; a socket
//! round-trip cannot go there. And it is addressed-to-somebody-*now* by
//! definition — a hand, a question — while an agent quietly finishing a turn is
//! news without being addressed to anyone. Folding the two would make the gate
//! that exists for questions fire on every `wsp say`.
//!
//! ## What is in the digest, and why the rest is not
//!
//! `pane_id`, `workspace_id`, `agent`, `agent_name`, `agent_status`,
//! `session_id`, `label`, `interactive_ready`, `launch_pending` — identity,
//! where it stands, what is running in it, what state that is in, and the
//! sentence it is wearing, which is what `wsp say` publishes.
//!
//! `title`, `cwd` and `focused` are deliberately out. A terminal rewrites its
//! own title on every prompt and `focused` moves whenever somebody looks at
//! another window: a stamp that moves when nothing a census draws has moved
//! costs a full refetch every time a person types, which is the same bill as
//! not having a stamp at all.
//!
//! Sorted by pane id before hashing, because herdr's ordering is herdr's and a
//! reordering is not news. Counted as well as hashed for the reason
//! [`Store::fingerprint`] counts files — but here every pane's id is in the
//! hash, so a pane leaving is already visible and the count is the hash's own.
//!
//! ## Absence is not news
//!
//! `herdr::panes()` degrades to an error here and to an empty list on a far
//! machine that said nothing, and the panel's rule about that is explicit: an
//! empty list is herdr not answering rather than everybody finishing at once,
//! and reading it as news clears the dock every time the socket hiccups. A CLI
//! process has no previous answer to fall back on, so it says so instead — the
//! stamp is `null` in JSON and `-` in the text form, and **a client treats that
//! as "no news", never as a change.** Two absences in a row are not a change
//! either, which is the point: only a value that differs from a previous
//! *value* is.
//!
//! ## Cost, measured rather than assumed
//!
//! It is on a client's interval, so the whole verb is the bill. Against the
//! live store on 2026-08-25, release build, the mean of 50 runs: **10.3ms** end
//! to end, of which 5.2ms is process start-up — `wsp --version` on the same
//! machine — and 4.09ms is [`Store::fingerprint`]'s own measured walk over 468
//! tasks. The two `stat`s and the socket round-trip together are the rest, and
//! a run with the socket removed measures the same to within the noise: the
//! census is not what this costs.
//!
//! What it replaces is `wsp ls --json --all`, 48.2ms on the same store, times
//! the four calls it takes to cover the record — and it replaces them on every
//! interval on which the answer is "nothing changed".
//!
//! ## Opaque
//
//!
//! Sixteen hex digits, and hex rather than a number so that nothing can subtract
//! them without first deciding to. Compared for equality and for nothing else:
//! neither stamp orders, and [`Store::fingerprint`]'s own doc records that a
//! collision is silent. A client that reads one as a version, a time or a size
//! is a client wsp will break.

use serde_json::json;

use crate::herdr;
use crate::store::Store;
use crate::util::Paint;
use crate::Args;

/// The three stamps, as this verb answers them.
struct Stamps {
    record: u64,
    attention: u64,
    /// `None` is herdr not answering. See the module doc: absence is not news.
    agents: Option<u64>,
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
/// A separate function from the socket call so that what is *in* the stamp is
/// testable without a herdr — the fields are the whole argument of this file
/// and a test that had to spawn a terminal to check them would not be written.
fn census(panes: &[herdr::Pane]) -> u64 {
    let mut ids: Vec<&herdr::Pane> = panes.iter().collect();
    ids.sort_by(|a, b| a.pane_id.cmp(&b.pane_id));
    let mut buf = String::new();
    for p in ids {
        // `\u{1f}` between fields so that two fields cannot be slid past each
        // other — a label ending in the next pane's id would otherwise hash the
        // same as the pair the other way round.
        for f in [
            &p.pane_id,
            &p.workspace_id,
            &p.agent,
            &p.agent_name,
            &p.agent_status,
            &p.session_id,
            &p.label,
        ] {
            buf.push_str(f);
            buf.push('\u{1f}');
        }
        // Three states and not two: herdr never sends `false`, so absence and
        // `false` mean different things — see [`herdr::Pane::interactive_ready`].
        for f in [p.interactive_ready, p.launch_pending] {
            buf.push(match f {
                Some(true) => 'y',
                Some(false) => 'n',
                None => '?',
            });
            buf.push('\u{1f}');
        }
        buf.push('\u{1e}');
    }
    digest(buf.as_bytes())
}

/// What herdr says the census is, or nothing at all.
///
/// An `Err` is this machine's socket not answering. An `Ok` that is empty is
/// the same thing one layer up — herdr returns an empty list rather than an
/// error when the connection is refused mid-fan-out — and the panel already
/// refuses to read that as everybody finishing at once.
fn agents_now() -> Option<u64> {
    if !herdr::available() {
        return None;
    }
    match herdr::panes() {
        Ok(p) if !p.is_empty() => Some(census(&p)),
        _ => None,
    }
}

fn take(store: &Store) -> Stamps {
    Stamps { record: store.fingerprint(), attention: store.attention_stamp(), agents: agents_now() }
}

/// The answer as a client reads it. `agents` is `null` and not missing: a key
/// that comes and goes is a key a client forgets to look for, and the whole
/// contract here is that the absence is *read* — as no news — rather than
/// skipped over.
fn document(s: &Stamps) -> serde_json::Value {
    json!({
        "record": hex(s.record),
        "attention": hex(s.attention),
        "agents": s.agents.map(hex),
    })
}

/// The same three, for a person. Names on the left in a fixed column so the
/// tokens line up under each other, which is the only way two of these are
/// ever compared by eye.
fn lines(s: &Stamps, p: &Paint) -> Vec<String> {
    let row = |name: &str, v: String| format!("{} {}", p.dim(&format!("{name:<9}")), v);
    vec![
        row("record", hex(s.record)),
        row("attention", hex(s.attention)),
        row("agents", s.agents.map(hex).unwrap_or_else(|| "-".into())),
    ]
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

    fn pane(id: &str) -> herdr::Pane {
        herdr::Pane {
            pane_id: id.into(),
            workspace_id: "w1".into(),
            agent: "claude".into(),
            agent_status: "idle".into(),
            ..Default::default()
        }
    }

    /// The four waiting states are read off these three fields and nowhere
    /// else, so a stamp that misses any of them leaves a surface drawing an
    /// agent that stopped an hour ago. This is the whole reason the agents
    /// stamp is taken from herdr rather than from the state directory: none of
    /// these has a file.
    #[test]
    fn an_agent_changing_state_moves_the_census() {
        let idle = vec![pane("p1")];
        let mut turning = idle.clone();
        turning[0].agent_status = "running".into();
        assert_ne!(census(&idle), census(&turning), "a turn starting was invisible");

        let mut ready = idle.clone();
        ready[0].interactive_ready = Some(true);
        assert_ne!(census(&idle), census(&ready), "interactive_ready was invisible");

        let mut launching = idle.clone();
        launching[0].launch_pending = Some(true);
        assert_ne!(census(&idle), census(&launching), "launch_pending was invisible");
    }

    /// Absence and `false` are two different answers from herdr — it never
    /// sends `false` — and a stamp that flattened them would move once, on the
    /// day herdr starts sending it, and never explain why.
    #[test]
    fn an_unset_field_is_not_the_same_as_a_false_one() {
        let mut absent = vec![pane("p1")];
        absent[0].interactive_ready = None;
        let mut no = absent.clone();
        no[0].interactive_ready = Some(false);
        assert_ne!(census(&absent), census(&no));
    }

    /// `wsp say` publishes its sentence as the pane's label and keeps nothing
    /// in the store unless the label had to be cut — so the label is where a
    /// status line changing is visible, and a census that skipped it would draw
    /// yesterday's sentence.
    #[test]
    fn a_pane_saying_something_new_moves_the_census() {
        let before = vec![pane("p1")];
        let mut after = before.clone();
        after[0].label = "landed the doorbell fix".into();
        assert_ne!(census(&before), census(&after));
    }

    /// A pane arriving or leaving is the loudest thing in a census, and it is
    /// what the state directory cannot see: a pane exiting clears its binding
    /// only once a daemon tick has reconciled it.
    #[test]
    fn a_pane_arriving_or_leaving_moves_the_census() {
        let one = vec![pane("p1")];
        let two = vec![pane("p1"), pane("p2")];
        assert_ne!(census(&one), census(&two), "a second agent was invisible");
        assert_ne!(census(&two), census(&vec![pane("p2")]), "a departure was invisible");
    }

    /// herdr's ordering is herdr's own and a reordering is not news. Without
    /// the sort this would refetch the whole store on whatever order a
    /// `pane.list` happened to come back in.
    #[test]
    fn the_same_panes_in_another_order_are_not_news() {
        let a = vec![pane("p1"), pane("p2"), pane("p3")];
        let b = vec![pane("p3"), pane("p1"), pane("p2")];
        assert_eq!(census(&a), census(&b));
    }

    /// The exclusions are the half of the argument a test usually loses. A
    /// terminal rewrites its title on every prompt and `focused` moves whenever
    /// somebody looks at another window; either one in the stamp is a full
    /// refetch every time a person types.
    #[test]
    fn a_title_or_a_focus_change_is_not_census_news() {
        let before = vec![pane("p1")];
        let mut typing = before.clone();
        typing[0].title = "~/claude/wsp — vim src/store.rs".into();
        assert_eq!(census(&before), census(&typing), "a terminal title moved the stamp");

        let mut looked_at = before.clone();
        looked_at[0].focused = true;
        assert_eq!(census(&before), census(&looked_at), "switching windows moved the stamp");
    }

    /// Fields are separated rather than run together, so no pane can be slid
    /// past its neighbour into the same digest.
    #[test]
    fn two_panes_cannot_be_confused_for_one_by_running_their_fields_together() {
        let mut a = vec![pane("p1")];
        a[0].agent = "claude".into();
        a[0].agent_name = "wsp-100".into();
        let mut b = vec![pane("p1")];
        b[0].agent = "claudewsp".into();
        b[0].agent_name = "-100".into();
        assert_ne!(census(&a), census(&b));
    }

    /// A client polling this cannot ask again for a value it did not get, so
    /// silence has to be *in* the answer rather than inferred from a missing
    /// key. Both spellings are contract: `null`, and `-` for the person.
    #[test]
    fn herdr_not_answering_is_said_rather_than_left_out() {
        let quiet = Stamps { record: 1, attention: 2, agents: None };
        let doc = document(&quiet);
        assert!(doc.get("agents").is_some(), "the key went missing instead of saying nothing");
        assert!(doc["agents"].is_null(), "silence came back as a value: {}", doc["agents"]);
        let drawn = lines(&quiet, &Paint::plain());
        assert!(
            drawn.iter().any(|l| l.split_whitespace().eq(["agents", "-"])),
            "no row said the census was unanswered: {drawn:?}"
        );

        let live = Stamps { record: 1, attention: 2, agents: Some(3) };
        assert_eq!(document(&live)["agents"], json!(hex(3)));
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
