//! One sentence on its way to one pane, and what became of it (wsp-438):
//! [`Sent`], the repeat it refuses ([`twice`]), the honest report and the
//! `agent-told` event ([`delivered`]), and which backend answers for a seat
//! ([`locate_seat`]).
//!
//! Moved down out of `cmd_agent` so an answer typed at its asker
//! (`cmd_message`) is reported by the same code as `wsp tell` without reaching
//! up into the seats; the typing itself is the seats', through
//! [`crate::cycle::Seats::tell_as`]. `cmd_agent` re-exports each.

use serde_json::{json, Value};

use crate::base::args::Args;
use crate::place::{Delivery, Place, Refusal};
use crate::store::Store;
use crate::util::{self, Paint};

/// One sentence on its way to one pane: everything both `tell` verbs need to
/// recognise it, report it and record it.
///
/// It exists because there are two of them. `wsp tell <task>` and `wsp govern
/// <scope> --tell` end at the same [`crate::agent_commands::Kind::tell`] and
/// used to disagree about everything after it: one wrote an event and the other
/// wrote none, one read [`Refusal::NotTaken`] as a rescue and the other as a
/// failure, and only one of them was ever right. A governor's seat is the
/// address that has no task behind it, so the verb a governor is *obliged* to
/// use was the one with less of the machinery.
pub struct Sent {
    /// The addressee as an id: a task, or the scope a seat governs. What the
    /// JSON and the event carry.
    pub target: String,
    /// The same addressee as a person would say it — `worklist-002`, or `the
    /// wsp seat` — which is what makes a refusal read as a sentence.
    pub what: String,
    pub pane: String,
    /// What to type after `wsp peek` when the sentence needs rescuing.
    pub peek: String,
    pub chars: usize,
    /// The sentence itself, for the event `wsp tell --log --full` reads.
    pub text: String,
    /// This exact sentence to this exact pane, as a short handle.
    ///
    /// **Derived from the message and not minted**, which is the whole of its
    /// value: a resend of the same paragraph to the same seat carries the same
    /// id, so a retry is recognisable as a repeat by anything that has seen the
    /// first — [`Sent::already_sent`] below, and a person reading two lines of
    /// output. An id from a counter would be unique per attempt and would
    /// answer a question nobody asked.
    pub id: String,
    /// Who spoke, as an address, and which message this answers (`wsp-521`).
    /// Empty and `None` for the paths that do not say, which write the event as
    /// they always did.
    pub from: String,
    pub re: Option<String>,
    pub reply: bool,
    /// Set when this is one seat of a `list:` fan-out (`wsp-523`): the fan-out's
    /// key and how many seats it went to, which the event carries.
    pub fan: Option<(String, usize)>,
    pub json: bool,
}

/// How long a sentence is still the same sentence.
///
/// Two minutes, and both bounds are the recorded harm rather than taste.
/// worklist-010's retries came seconds after a delivery falsely reported as a
/// failure — that is the whole population this is for, and it is measured in
/// seconds. Anything much longer starts refusing the deliberate repeat: a
/// governor typing `continue` at an agent twice in an evening means it twice,
/// and a channel that silently swallowed the second would stall a night to
/// prevent a duplicate paragraph.
pub const SAME_BREATH: i64 = 120;

impl Sent {
    pub fn new(target: &str, what: &str, pane: &str, peek: &str, text: &str, args: &Args) -> Sent {
        Sent {
            target: target.into(),
            what: what.into(),
            pane: pane.into(),
            peek: peek.into(),
            chars: text.len(),
            text: text.to_string(),
            id: message_id(pane, text),
            from: String::new(),
            re: None,
            reply: false,
            fan: None,
            json: args.json(),
        }
    }

    pub fn in_fan(mut self, fan: Option<(&str, usize)>) -> Sent {
        self.fan = fan.map(|(k, n)| (k.to_string(), n));
        self
    }

    /// Who said it, what it answers, and whether it carried a reply footer.
    pub fn by(mut self, from: &str, re: Option<&str>, reply: bool) -> Sent {
        self.from = from.into();
        self.re = re.map(str::to_string);
        self.reply = reply;
        self
    }

    /// This same sentence, to this same pane, within the last [`SAME_BREATH`].
    ///
    /// The idempotence half of worklist-010, and it is the half that addresses
    /// the *harm* rather than the report. A verb that says "was not told" about
    /// a message it delivered turns one paragraph into three, and the fix for
    /// the report — a backend `tell` that no longer lies — cannot undo a
    /// retry that has already been typed. This can: the second send of the same
    /// text is recognised and refused, whatever made somebody type it.
    ///
    /// Off the event log, which is where the first send wrote itself. That is
    /// the only durable record either verb has ever left, and it is why
    /// [`delivered`] writes one from both paths rather than from one.
    pub fn already_sent(&self, store: &Store) -> Option<i64> {
        let now = util::epoch_secs();
        store
            .events_of("agent-told")
            .iter()
            .rev()
            .filter(|d| d.get("id").and_then(Value::as_str) == Some(&self.id))
            .filter_map(|d| d.get("at").and_then(Value::as_i64))
            .map(|at| now - at)
            .find(|ago| *ago <= SAME_BREATH)
    }
}

/// A sentence's short handle: the pane and the text, and nothing that changes
/// between two attempts to send it.
///
/// FNV-1a, written out rather than reached for, because `DefaultHasher` is
/// SipHash with keys that Rust does not promise across versions — an id that
/// changed under a compiler upgrade would silently stop recognising repeats,
/// which is the one thing this is for. Sixteen hex characters of a 64-bit
/// digest is not a claim about adversaries; two sentences would have to collide
/// *and* arrive at the same pane within two minutes to be confused.
pub fn message_id(pane: &str, text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in pane.as_bytes().iter().chain(b"\0").chain(text.as_bytes()) {
        h ^= *b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    format!("m-{h:016x}")
}

/// Refuse a sentence that has just been sent to this pane, and say so.
///
/// `--again` is the escape and it is deliberately a word rather than a
/// timeout: the case where somebody means it twice exists, and the case where
/// they are retrying a failure that never happened is the one worth an extra
/// keystroke.
pub fn twice(sent: &Sent, ago: i64, p: &Paint) -> i32 {
    if sent.json {
        println!(
            "{}",
            json!({ "target": sent.target, "pane": sent.pane, "id": sent.id, "told": false, "already": ago })
        );
    } else {
        println!(
            "{} {}  {}",
            p.dim("·"),
            p.bold(&sent.id),
            p.dim(&format!(
                "the same sentence reached {} {} ago — `--again` sends it anyway",
                sent.what,
                util::duration_human(ago)
            )),
        );
    }
    0
}

/// What became of a sentence, said honestly, recorded once, from both verbs.
///
/// **The rule this enforces is worklist-010's and it is a constraint rather
/// than a preference: never report failure for a send that happened.** The old
/// path printed `the wsp seat was not told` on a backend wait timing out, and the
/// seat it was addressed to had already read the message — so the governor
/// retried, and one paragraph was delivered three times. `delivered, no turn
/// seen` is a true sentence about the same event, and it is what
/// [`Delivery::Unconfirmed`] prints.
///
/// The event is the other half. `wsp tell` has always written `agent-told` and
/// `govern --tell` wrote nothing at all, so the channel a governor is obliged
/// to use was the one channel with no forensic trace — and worklist-010 was
/// investigated for a day out of terminal scrollback because of it. Both write
/// it now, with the message's id in it, which is what makes a repeat
/// recognisable at all.
pub fn delivered(store: &Store, outcome: crate::place::Result<Delivery>, sent: &Sent) -> i32 {
    let p = Paint::new();
    let Sent { target, what, pane, peek, chars, text, id, from, re, reply, fan, json } = sent;
    match outcome {
        Ok(how) => {
            // Recorded as an event and not on the task's log, deliberately. The
            // log is injected into every future spawn on that task, so a line
            // per sentence would be paid for by every session afterwards; this
            // is forensics — who said what to whom, when — and belongs where
            // the claims and the releases already are.
            store.log_event(
                "agent-told",
                json!({
                    "target": target, "pane": pane, "chars": chars, "id": id,
                    "from": from, "re": re, "reply": reply, "text": text,
                    "fan": fan.as_ref().map(|f| f.0.as_str()), "fan_of": fan.as_ref().map(|f| f.1),
                    "at": util::epoch_secs(),
                    "turn": matches!(how, Delivery::Started | Delivery::Acked),
                }),
            );
            // The seat's supervisor acknowledged it (wsp-477): the same line the
            // wake path writes for a governor scope, so a direct tell to a
            // compound seat leaves its acknowledgement in cycle.log too.
            if matches!(how, Delivery::Acked) {
                crate::store::log_line(store, &format!("told {target}: delivered, acknowledged by the seat"));
            }
            if *json {
                println!(
                    "{}",
                    json!({ "target": target, "pane": pane, "id": id, "told": true, "from": from, "re": re,
                            "turn": matches!(how, Delivery::Started | Delivery::Acked) })
                );
            } else {
                // The tail is on the unconfirmed one only, and it is not a
                // hedge: a sentence queued behind a turn has not been read yet,
                // which is a different thing to know at a barrier than a turn
                // that started on it.
                let tail = match how {
                    Delivery::Started => String::new(),
                    Delivery::Acked => format!(" · {}", p.dim("delivered")),
                    Delivery::Unconfirmed => format!(" · {}", p.dim("delivered, no turn seen")),
                };
                println!("{}{}", p.dim(&format!("→ {what} · {pane}")), tail);
            }
            0
        }
        // `NotTaken` is delivered-and-unmoved, which for this verb is the
        // interesting answer rather than an error. Where the text now sits is
        // said as the unknown it is: the keystrokes were written at the pane,
        // and whether the TUI is still holding them in the composer or dropped
        // them is inside the application, which wsp cannot read. The first
        // wording asserted "sitting in the composer" flat out; the spawn
        // failures of 2026-08-25 showed composers empty after deliveries that
        // had been reported the same way, so the assertion was a guess that
        // had already been wrong. The rescue differs by half, and peek tells
        // the halves apart — which is why it is named before the keystroke:
        // resending blind would duplicate a composer still holding the text.
        Err(Refusal::NotTaken) => {
            eprintln!("wsp: {what} took the text and started nothing — it may be sitting in the composer or may have been dropped");
            eprintln!("     `wsp peek {peek}` shows which; if it is sitting there, a return in the pane sends it");
            1
        }
        // Read off the screen, so said flat: it is there (`wsp-252`). The
        // return is the repair; a second `wsp tell` also works, and leaves
        // the sentence in the prompt twice.
        Err(Refusal::Unsubmitted(_)) => {
            eprintln!("wsp: {what} has the text typed in its prompt and not submitted — wsp pressed submit again and it stayed");
            eprintln!("     a return in {pane} sends it; `wsp peek {peek}` shows it waiting");
            1
        }
        Err(e) => {
            // The supervisor's own answer after its retries: the wake path's
            // line for it, so a failed direct tell is not only on stderr.
            if let Refusal::Failed(why) = &e {
                crate::store::log_line(store, &format!("told {target}: delivery failed — {why}"));
            }
            eprintln!("wsp: {what} was not told: {e}");
            1
        }
    }
}

/// The pane holding an agent that a needle names: a pane id, or a task.
///
/// A workspace is not accepted, and that is a refusal rather than an omission.
/// A workspace can hold several panes with agents in them, and picking one for
/// the caller is how a sentence meant for one agent reaches its neighbour —
/// `govern --tell` may do it because a governorship *is* the workspace's, and a
/// task is held by a pane.
/// Which of `backends` answers for `seat`, and the row it answered with —
/// `wsp tell`'s own join, pulled out so it can be proven without a real
/// `Place::tell` to receive anything (`compound-077`). The same "ask every
/// backend, take whichever one's census names this seat" `Wip::live` already
/// does, one seat at a time instead of the whole list.
pub fn locate_seat<'a>(
    backends: &'a [Box<dyn Place>; 1],
    seat: &str,
) -> Option<(&'a Box<dyn Place>, crate::place::Seated)> {
    backends.iter().find_map(|b| {
        b.census().ok().and_then(|c| c.seats().find(|s| s.seat.as_str() == seat).cloned()).map(|row| (b, row))
    })
}
