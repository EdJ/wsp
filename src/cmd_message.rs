//! `wsp ask`, `wsp answer`, `wsp ack` — the return path.
//!
//! **The fault this closes, in one sentence Ed wrote on 2026-08-19:** *"the
//! response from the governor to you was not sent back here, it's just sat at
//! its tty output."*
//!
//! `wsp govern <scope> --tell` writes into a pane and reports `told: true`.
//! That is a delivery receipt and there is no return path of any kind — the
//! reply is whatever the receiving agent prints to a screen the sender cannot
//! read. So for the whole of 2026-08-19 **a person was the transport**: every
//! message the `wsp` seat got from the `worklist` seat arrived because Ed read
//! one pane and pasted it into another. A shared-stash repair, a protocol about
//! `panel/rows.rs`, three falsified hypotheses and a design correction, all
//! hand-carried.
//!
//! # A verb, and why it is not merely a rule
//!
//! `wsp-095` left this open: is the reply path a verb, or is it a *rule* — that
//! a seat answers on the task, never only in prose? The evidence says a verb,
//! and the evidence is that **the rule was already in force and was already
//! being followed when it failed.**
//!
//! `worklist-013` raised a flag asking whether it could land. The seat answered
//! within minutes, by `wsp tell`; the agent acted, finished, landed and was
//! despawned. **The flag stayed up for 2h14m** and came down by hand, while the
//! seat was clearing something else. Nothing linked the answer to the question.
//! The raiser was told and the record was not, and for two hours that seat's own
//! watch reported a hand up on work that was finished. The custodian that wrote
//! the rule broke it the same day, on its own instrument, and did not notice —
//! because the answer and the record were two acts and only one of them was on
//! the path of getting the work moving.
//!
//! So the thing being added is not a channel. It is that **the record happens
//! as a consequence of answering rather than as a second act somebody
//! remembers.** [`answer`] writes the task log first, closes the question, mints
//! the reply and delivers it, and there is no ordering in which a person can do
//! three of those and forget the fourth.
//!
//! # And why it is not a second message system
//!
//! `wsp-095` is explicit that a verb here must not become one. Four things it
//! does not add, each of which it would have to for that to be true:
//!
//! - **No transport.** Delivery is [`crate::agent_commands::Kind::tell`] through
//!   [`crate::cmd_agent::Sent`] and [`crate::cmd_agent::delivered`] — the exact
//!   path `wsp tell` and `wsp govern --tell` take, so there is one delivery,
//!   one idempotence window and one honest-reporting rule, not a fourth.
//! - **No address book.** The addressee is [`crate::message::Waiting`], which
//!   the asker wrote when it asked. Nobody chooses where a reply goes.
//! - **No record.** `messages.json` and the question lifecycle are
//!   `worklist-022`'s, landed and untouched here; this is its first consumer.
//! - **No inbox an agent has to remember to read.** The answer lands on the
//!   asker's task log, which every brief already injects. That is the one place
//!   in wsp where task-prose injection is a feature rather than a cost.
//!
//! # What happens to `--tell`, since the brief asks
//!
//! Nothing. It stays exactly as it is, and the line between them is not
//! politeness — it is **whether anything is owed back**. A governor telling an
//! agent to stop is not a conversation, and a one-directional push to a pane is
//! still right for direction, for a work order, and for prose. What changes is
//! that the case where something *is* owed back now has a verb of its own, so
//! `--tell` stops being the thing people reach for when they mean *answering*.
//! That was the whole of the `worklist-013` failure: the answer went down the
//! direction channel, which has no memory, and the question was left standing in
//! the one that does.
//!
//! # Level, edge, or both — stated rather than assumed
//!
//! All three, and each does a job the other two cannot:
//!
//! - **The open question is a LEVEL.** [`crate::message::open_questions`] is a
//!   full read, correct after any disconnection, and it self-clears when the
//!   question closes. That is what a watcher subscribes to, per `wsp-095`
//!   Part 13, and it is why the hand goes down here *because the question
//!   closed* rather than because somebody remembered to lower it. 2h14m is the
//!   measured cost of the other arrangement. The subscriber is
//!   [`crate::cmd_watch::Kind::Unanswered`], and the population it reads is
//!   [`crate::message::Message::wants_answering`] — the same one [`open`]
//!   lists, asked once, so the verb and the watch cannot come to disagree about
//!   whether the fleet is quiet.
//! - **The answer is an EDGE.** `message-answered` in `events.jsonl`, so
//!   `~/wsp/hooks/on-message-answered` is a desktop notification away and a
//!   watcher's liveness stream carries it. An answer is a moment and cannot be
//!   recomputed.
//! - **The line on the task is neither.** It is the record, and it is the only
//!   one of the three that outlives the worktree and the panes.
//!
//! # Delivery, against Part 5's hard refusal
//!
//! *Nothing may push text into a working agent's composer, except `stop`.* This
//! pushes, and it is inside the refusal rather than an exception to it, because
//! **the party being pushed to is the one that is sitting still** — that is what
//! [`crate::message::Waiting`] means and it is the whole cost of an open
//! question. The two states where a push is a hazard are checked exactly as
//! `wsp tell` checks them: a `blocked` pane has a dialog holding the keyboard
//! and is refused, and a pane mid-turn queues, which [`crate::cmd_agent::delivered`]
//! reports as *delivered, no turn seen* rather than as success. Either way the
//! answer is already on the task before a byte is sent, so a delivery that does
//! not land is late and never lost.

use serde_json::json;

use crate::herdr;
use crate::message::{self, About, Kind, Landed, Message, Party, Shape, Waiting};
use crate::store::Store;
use crate::util::{self, Paint};
use crate::Args;

/// `wsp ask <id> "the question"` — a question about one task, with a return
/// path.
///
/// Bare, it lists what is open, the way `wsp flag` does: the question a person
/// asks from a shell is the same one a panel answers by drawing the section, and
/// it should not need a second verb.
///
/// # Why the subject is required
///
/// `wsp flag <id>` and `wsp tell <id>` both take an id first and this matches
/// them, but the reason here is stronger than consistency: **the subject is
/// what gives the answer somewhere to live.** For an agent that is its own task
/// and the subject is a detail; for a seat, which holds no task at all, the
/// subject is the only durable home an answer has — see
/// [`crate::message::homes_to`]. A question wsp cannot land an answer on is
/// refused at the point of asking rather than discovered at the point of
/// answering, which is the difference between a sentence now and an agent
/// waiting for one.
///
/// If there is genuinely no task — if the thing being said is not owed an
/// answer at all — that is what `wsp tell` and `wsp govern --tell` are still
/// for. Prose with nothing owed back does not need a record.
pub fn ask(store: &Store, args: &Args) -> i32 {
    let Some(needle) = args.rest.first().cloned() else {
        return open(store, args);
    };
    // A message id and nothing to say is the verb a long question points at:
    // `for_a_governor` and the `unanswered` line both end `wsp ask <id>` when
    // they had to cut, so a seat never has to read the asker's screen or its
    // `--from` file to learn what it is being asked.
    if args.text(1).trim().is_empty() && args.get("from").is_none() && needle.starts_with("m-") {
        return whole(store, &needle, args);
    }
    let subject = match store.task_or_why(&needle) {
        Ok(t) => t,
        Err(why) => {
            eprintln!("{why}");
            eprintln!("     a question needs a task to land its answer on · `wsp tell` is for prose");
            return 1;
        }
    };
    let text = match crate::cmd_agent::prose(args, 1) {
        Ok(t) => t,
        Err(code) => return code,
    };

    let (me, pane) = whoami(store);
    let held = pane
        .as_deref()
        .and_then(|p| store.bindings().get(p).cloned())
        .and_then(|b| b.get("task_id").and_then(|t| t.as_str()).map(str::to_string))
        .filter(|id| store.task(id).is_some())
        .unwrap_or_default();

    let kind = match args.get("kind").as_deref().map(Kind::parse) {
        Some(Some(k)) => k,
        Some(None) => {
            eprintln!("wsp: --kind is one of stop, direction, note, fyi");
            return 2;
        }
        // A question's default is the quiet one, and `stop` must be typed:
        // `wsp-095`'s rule is that every verb defaults to the quietest kind it
        // can honestly be.
        None => Kind::Note,
    };

    let q = Message::question(
        me.clone(),
        kind,
        &text,
        Waiting::new(pane.as_deref().unwrap_or_default(), &held),
    )
    .about(About::Task(subject.id.clone()));

    // The guarantee this verb makes, checked before the record exists rather
    // than reported after it does: there is no path here by which an answer can
    // be written and not routed.
    if message::homes_to(store, &q).is_none() {
        eprintln!("wsp: an answer to that would have nowhere to live");
        eprintln!("     {} is not a task this store holds", subject.id);
        return 1;
    }

    if let Err(e) = message::raise(store, &q) {
        eprintln!("wsp: {e}");
        return 1;
    }

    // **The seat, now, and not on the next pass.** `wsp-146`: the record being
    // open was never the same thing as the governor having the question. The
    // attention pass derives `unanswered` for it within a minute and the wake
    // path delivers it, which is the whole mechanism — but it meant the words
    // sat in a file for up to a tick before they were anywhere, and this verb
    // is the one thing in the system whose entire purpose is getting a sentence
    // to a governor. So the question goes into that seat's spool here, inside
    // the lock, and the same [`crate::wake`] delivery the daemon uses is asked
    // once before the verb returns.
    //
    // With no seat above the task there is nobody to hold it and the receipt
    // says so, which is the one case where *asked of every panel* is the whole
    // answer — nothing is lost, because a level nobody is addressee of is the
    // hook's audience and every panel draws it.
    let seat = crate::cmd_govern::answering_seat(store, &subject);
    // `map`, not `and_then`: `wake::say` no longer answers "is there a seat
    // here" — it spools and reports, because a vacancy is later, not nowhere.
    let report = seat.as_ref().map(|s| {
        crate::wake::say(store, &s.scope, &for_a_governor(&q), Some(&q.id))
    });

    let home = message::homes_to(store, &q).unwrap_or_default();
    if args.json() {
        println!(
            "{}",
            json!({ "id": q.id, "about": subject.id, "waiting": held, "pane": pane, "lands_on": home,
                    "seat": seat.as_ref().map(|s| &s.scope),
                    "held": report.as_ref().map(|r| r.held), "why": report.as_ref().map(|r| r.why.clone()) })
        );
        return 0;
    }
    let p = Paint::new();
    println!("{} {}  {}", p.red("?"), p.bold(&q.id), q.title());
    println!("  {}", p.dim(&format!("about {} · answered onto {home}", subject.id)));
    println!("  {}", p.dim(&addressed(store, &subject)));
    println!("  {}", p.dim(&format!("wsp answer {} \"…\" closes it and comes back here", q.id)));
    // Where it went, and when it will arrive — `wsp-146`'s receipt. A verb whose
    // whole effect is on somebody else's screen has to name the screen; the part
    // that is new here is the *when*, because for the first time the honest
    // answer is sometimes "later", and an agent that cannot tell later from
    // never is how a question sits open for a night.
    if let Some(r) = &report {
        println!("  {}", r.said(&format!("the {} seat", r.scope), &p));
    }
    0
}

/// The question as it goes to the seat, and the return path with it.
///
/// **Composed here rather than left to the wake's own rendering**, because the
/// governor's line is the only place the answer will ever be offered from: an
/// `unanswered` level names `wsp answer <id>` in its detail, which is right for
/// a reader reading a column and is useless to an agent deciding whether it can
/// act in the next thirty seconds. So: who sent it, what it wants, and the two
/// commands that finish it.
///
/// **`wsp ask` and not `wsp flag` is named in the last line**, and that is the
/// row's third requirement — one verb, chosen out of what exists because it is
/// the one with a return path. Every other spelling still works and lands in the
/// same spool; this line is what a governor reads to learn which one to reach for.
pub(crate) fn for_a_governor(q: &Message) -> String {
    let about = q.about.task().unwrap_or("this work");
    format!(
        "{by} asked about {about}:\n\n{body}\n\n\
         answer with `wsp answer {id} \"…\"` — it lands on the asker's task and closes this\n\
         `wsp ask` with no id lists every question still open · `wsp watch --drain` shows what \
         this seat is owed",
        by = q.from.byline(),
        body = as_typed(q),
        id = q.id,
    )
}

/// How much of a question is typed at a seat before it is cut.
///
/// **A question arrives whole, or says it did not.** The cut used to be
/// [`Message::title`] — the first line — so a three-paragraph `wsp ask` whose
/// choice was in the last paragraph reached the governor as its opening and a
/// `wsp answer` hint, and the governor went to the asker's pane or `--from`
/// file to learn what it was being asked (`wsp-170`). Past this length a
/// governor's composer is better served by the verb that prints it, but the
/// question line is never what is lost.
const TYPED_WHOLE: usize = 1500;

/// Of each end of a cut question, how much is kept. A question's choice is
/// nearly always in its last paragraph and its subject in its first.
const KEPT: usize = 400;

/// The question as it is typed: whole when it is short, and otherwise its first
/// and last paragraphs with the verb that prints the rest. Never silently cut.
fn as_typed(q: &Message) -> String {
    let text = q.text.trim();
    if text.chars().count() <= TYPED_WHOLE {
        return text.to_string();
    }
    let paras: Vec<&str> = text.split("\n\n").map(str::trim).filter(|p| !p.is_empty()).collect();
    let (first, last) = (paras.first().copied().unwrap_or(text), paras.last().copied().unwrap_or(text));
    let kept = |p: &str, from_end: bool| -> String {
        let n = p.chars().count();
        match (n <= KEPT, from_end) {
            (true, _) => p.to_string(),
            (false, false) => util::truncate(p, KEPT),
            (false, true) => format!("…{}", p.chars().skip(n - KEPT + 1).collect::<String>()),
        }
    };
    let verb = format!("`wsp ask {}` prints the whole question", q.id);
    match paras.len() {
        0 | 1 => format!("{}\n[{verb}]", kept(first, false)),
        _ => format!("{}\n\n[… {verb} …]\n\n{}", kept(first, false), kept(last, true)),
    }
}

/// `wsp ask <message-id>` — one question, whole.
fn whole(store: &Store, needle: &str, args: &Args) -> i32 {
    let q = match find(store, needle) {
        Ok(m) => m,
        Err(code) => return code,
    };
    if args.json() {
        println!("{}", q.to_json());
        return 0;
    }
    let p = Paint::new();
    println!("{} {}", p.red("?"), p.bold(&q.id));
    println!("  {}", p.dim(&waiting_line(&q)));
    println!();
    println!("{}", q.text.trim());
    if q.is_open() {
        println!();
        println!("{}", p.dim(&format!("wsp answer {} \"…\" closes it and reaches whoever asked", q.id)));
    }
    0
}

/// `wsp answer <message-id> "the sentence"` — close a question, on the record
/// and at the asker, in that order.
///
/// **The order is the design.** `wsp-095` Part 7: the answer is appended to the
/// asker's task log *first* and delivered *second*, because the delivery is
/// best-effort and the record is not. Three things follow and the first is the
/// one that matters — there is no drop case to get wrong. The write happens
/// whether the asker is alive, stopped or gone; a respawned agent reads it in
/// its brief; and it outlives the worktree, which `worklist-012` found agent
/// memory does not.
///
/// `--abandon` is the other ending and takes the same road home, because an
/// asker that learns nothing from a question closing is an asker that sits
/// waiting for ever. Both require a sentence. Neither is a keystroke, and that
/// is the refusal `wsp worklist go` already makes at a barrier: *what wsp
/// contributes is the obligation to write a sentence, not the judgement.*
///
/// There is no third ending. Lowering a hand is not a disposition for a
/// question — `worklist-004`, where a seat answered by another route and then
/// cleared the flag, so **clearing looked like answering** and the asker learned
/// nothing from it.
pub fn answer(store: &Store, args: &Args) -> i32 {
    let Some(needle) = args.rest.first().cloned() else {
        eprintln!("usage: wsp answer <message-id> \"…\"   (or `-` to read it from stdin)");
        eprintln!("       wsp ask   lists what is open, with its id");
        return 2;
    };
    let q = match find(store, &needle) {
        Ok(m) => m,
        Err(code) => return code,
    };
    let text = match crate::cmd_agent::prose(args, 1) {
        Ok(t) => t,
        Err(code) => return code,
    };
    let giving_up = args.has("abandon");
    let (me, _) = whoami(store);

    let closed = match giving_up {
        false => message::answer(store, &q.id, &me, &text),
        true => message::abandon(store, &q.id, &me, &text),
    };
    let closed = match closed {
        Ok(c) => c,
        Err(e) => {
            eprintln!("wsp: {e}");
            return 1;
        }
    };

    // The record is written by here and nothing below can unwrite it, which is
    // why the exit code is 0 for every delivery outcome except a refusal a
    // person can act on. A verb that returned failure for an answer that landed
    // is `worklist-010` again, and a retry would be refused as a repeat anyway.
    let p = Paint::new();
    if !args.json() {
        let verb = match giving_up {
            false => "answered",
            true => "abandoned",
        };
        println!("{} {}  {}", p.dim("·"), p.bold(&closed.question.id), p.dim(verb));
        println!("  {}", p.dim(&landed_line(&closed.landed)));
    }
    deliver(store, &closed, args)
}

/// `wsp ack <message-id>` — *I have this and I am not passing it on.*
///
/// A notification's disposition, and it is a real act rather than a dismissal:
/// it is what makes the chain auditable, and `wsp-095` Part 6 is explicit that
/// it costs one keystroke and **is not an answer**. The shape refuses it on a
/// question, which is the whole of that Part in one call.
///
/// The population this is for today is replies: [`answer`] mints one per
/// question closed, addressed to whoever asked, and it stands open until the
/// party it was addressed to says it has read it. Nothing acknowledges on
/// somebody else's behalf — a delivery receipt is not a reading, and treating it
/// as one is the same substitution that made clearing look like answering.
pub fn ack(store: &Store, args: &Args) -> i32 {
    let Some(needle) = args.rest.first().cloned() else {
        eprintln!("usage: wsp ack <message-id>");
        return 2;
    };
    let m = match find(store, &needle) {
        Ok(m) => m,
        Err(code) => return code,
    };
    let (me, _) = whoami(store);
    match message::acknowledge(store, &m.id, &me) {
        Ok(m) => {
            if args.json() {
                println!("{}", json!({ "id": m.id, "state": m.state_raw }));
            } else {
                let p = Paint::new();
                println!("{} {}  {}", p.dim("·"), p.bold(&m.id), p.dim("acknowledged"));
            }
            0
        }
        Err(e) => {
            eprintln!("wsp: {e}");
            if m.shape() == Some(Shape::Question) {
                eprintln!("     `wsp answer {} \"…\"` is what closes a question", m.id);
            }
            1
        }
    }
}

// ---- what is up -----------------------------------------------------------

/// `wsp ask` with nothing to ask: the level, read whole.
///
/// A full read rather than a feed, for `robustness-075`'s reason restated in
/// `wsp-095` Part 13: **a poll is self-healing and a stream is not**, and a
/// level-set read answers *"nothing is up"* positively — which is the difference
/// between a watcher that is quiet and one that is broken.
fn open(store: &Store, args: &Args) -> i32 {
    let all = message::all(store);
    // The two halves of `Message::wants_answering`, which is the population
    // `wsp watch unanswered` reports off the same predicate — split here
    // because a person reads sections and a level set does not have any.
    let questions: Vec<&Message> = all
        .iter()
        .filter(|m| m.wants_answering())
        .filter(|m| m.shape() == Some(Shape::Question) && m.is_open())
        .collect();
    // Records this build cannot read. They are listed rather than counted and
    // rather than dropped: see `Message::needs_attention` — reporting "nothing
    // is up" when the honest answer is "there is something here I cannot read"
    // is silence wearing a safe-looking name, and the installed binary is
    // routinely not the tree.
    let unreadable: Vec<&Message> = all
        .iter()
        .filter(|m| m.wants_answering())
        .filter(|m| m.shape().is_none() || m.state().is_none())
        .collect();
    let replies: Vec<&Message> =
        all.iter().filter(|m| m.reply_to.is_some() && m.is_open()).collect();

    if args.json() {
        let out = |v: &Vec<&Message>| v.iter().map(|m| m.to_json()).collect::<Vec<_>>();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "open": out(&questions),
                "replies": out(&replies),
                "unreadable": out(&unreadable),
            }))
            .unwrap_or_default()
        );
        return 0;
    }

    let p = Paint::new();
    if questions.is_empty() && replies.is_empty() && unreadable.is_empty() {
        println!("{}", p.dim("nothing open"));
        return 0;
    }
    for m in &questions {
        println!("{} {}  {}", p.red("?"), p.bold(&m.id), m.title());
        // The rest of it, because a list that shows a question's first line is
        // a list that hides the choice it asks (`wsp-170`).
        for line in m.body().lines() {
            println!("  {line}");
        }
        println!("  {}", p.dim(&waiting_line(m)));
    }
    // Every reply, and not only the ones addressed to this caller. The level
    // set is *what is up on this machine*, and a read that answered differently
    // per caller would stop being the self-healing full read `wsp-095` Part 13
    // asks for — so the addressee is named on the row instead of filtered on.
    // Routing a per-seat view is `seats_for`, and it is not in this change.
    for m in &replies {
        println!("{} {}  {}", p.dim("←"), p.bold(&m.id), m.title());
        println!("  {}", p.dim(&waiting_line(m)));
        println!("  {}", p.dim(&format!("wsp ack {} when whoever asked has read it", m.id)));
    }
    for m in &unreadable {
        println!("{} {}  {}", p.red("!"), p.bold(&m.id), m.title());
        println!(
            "  {}",
            p.dim(&format!(
                "shape `{}` state `{}` — this build does not know those words; a newer one wrote them",
                m.shape_raw, m.state_raw
            ))
        );
    }
    if !questions.is_empty() {
        println!("{}", p.dim("wsp answer <id> \"…\" closes one and reaches whoever asked"));
    }
    0
}

/// Which pane the answer is typed into.
///
/// **The task first, and the pane it was asked from second**, which is the
/// whole reason [`crate::message::Waiting`] holds both: *the pane can go and
/// the task cannot.* A pane id is the most perishable identifier herdr has — it
/// dies with its workspace and one cascade of `pane.exited` once cleared every
/// binding on this machine at a stroke — so a question open long enough to be
/// worth answering is a question whose pane may have been reused. Reading the
/// pane straight off the record types the answer at whoever is standing there
/// now.
///
/// So the binding is asked first: whoever holds the work is who is sitting
/// still on it, including when that is a respawned agent rather than the one
/// that asked. It has already read the answer in its brief by then, because the
/// log line was written before this ran — this only saves it the poll.
///
/// The recorded pane is the fallback and not the other way round, because a
/// seat holds no task and its pane is the only address it has.
fn route(store: &Store, waiting: Option<&Waiting>) -> String {
    let Some(w) = waiting else { return String::new() };
    if !w.task.is_empty() {
        if let Some(pane) = store.panes_for_task(&w.task).into_iter().next() {
            return pane;
        }
    }
    w.pane.clone()
}

/// Who is sitting still, and for how long — the cost of an open question, which
/// is the thing that distinguishes it from a notification.
fn waiting_line(m: &Message) -> String {
    let mut parts = vec![format!("from {}", m.from.byline())];
    if let Some(w) = &m.waiting {
        match (w.pane.as_str(), w.task.as_str()) {
            ("", "") => {}
            ("", t) => parts.push(format!("waiting on {t}")),
            (pane, "") => parts.push(format!("waiting in {pane}")),
            (pane, t) => parts.push(format!("waiting in {pane} · {t}")),
        }
    }
    if let Some(t) = m.about.task() {
        parts.push(format!("about {t}"));
    }
    let held = util::since(&m.at);
    if held > 0 {
        parts.push(util::duration_human(held));
    }
    parts.join(" · ")
}

// ---- delivery -------------------------------------------------------------

/// Where the words went, as a sentence. A verb whose whole effect is on
/// somebody else's screen has to name the screen, or it reads as having done
/// nothing.
fn landed_line(landed: &Landed) -> String {
    match landed {
        Landed::Task(id) => format!("on {id}'s log"),
        Landed::RecordOnly(why) => format!("on the message record only — {why}"),
    }
}

/// Take the answer to whoever asked.
///
/// The reply carries its own byline, because the one channel wsp does not
/// control is a human keyboard: **attributed text is a seat or an agent and
/// unattributed text is a person**, and that inversion only works if every
/// message wsp sends says who sent it. The seat had already adopted the rule by
/// hand before the transport could carry it.
fn deliver(store: &Store, closed: &message::Closed, args: &Args) -> i32 {
    let p = Paint::new();
    let pane = route(store, closed.reply.waiting.as_ref());
    let receipt = |told: bool, why: &str| {
        if args.json() {
            println!(
                "{}",
                json!({
                    "id": closed.question.id,
                    "state": closed.question.state_raw,
                    "reply": closed.reply.id,
                    "landed": landed_line(&closed.landed),
                    "told": told,
                    "why": why,
                })
            );
        } else if !why.is_empty() {
            println!("  {}", p.dim(why));
        }
    };

    if pane.is_empty() {
        receipt(false, "nobody to tell — the answer is on the record and in the log");
        return 0;
    }
    // Asked of every backend wsp can spawn onto (`compound-091`), not herdr
    // alone — the same fold `wsp tell` makes since `compound-077` and for the
    // same reason: a seat is a herdr pane, a compound seat, or nothing this
    // machine knows, and only the census that made it can say which. This verb
    // used to gate on `herdr::available()` and then scan `herdr::panes()`, so
    // an answer addressed to a compound-hosted agent was filed as "no herdr
    // socket" and never delivered, on a machine where the agent was running.
    let backends = crate::cmd_spawn::local_backends();
    let Some((place, found)) = crate::cmd_agent::locate_seat(&backends, &pane) else {
        receipt(false, &format!("nothing answers for {pane} any more — the answer is written"));
        return 0;
    };
    if found.agent.kind.trim().is_empty() {
        receipt(false, &format!("{pane} holds no agent — the answer is written"));
        return 0;
    }
    // The one state a message must not be sent into, checked here for the same
    // reason `wsp tell` checks it: a blocked agent has a permission dialog
    // holding the keyboard, so the text is typed *at the dialog*, where a
    // sentence about what to do next can select an answer nobody chose. Asked
    // of the port's own `State` rather than herdr's screen-scraped one, so the
    // guard holds for whichever backend answered.
    if let Some(wait) = crate::waiting::on_screen(found.state) {
        receipt(
            false,
            &format!("{pane} is {} — the answer is written; `wsp peek {pane}` shows what it is asking", wait.sentence()),
        );
        return 0;
    }

    let text = wire(closed);
    let how = crate::agent_commands::of(&found.agent.kind);
    let sent = crate::cmd_agent::Sent::new(&pane, &whose(closed), &pane, &pane, &text, args);
    if let Some(ago) = sent.already_sent(store) {
        if !args.has("again") {
            return crate::cmd_agent::twice(&sent, ago, &p);
        }
    }
    let out = how.tell(place.as_ref(), &crate::place::Seat::new(&pane), &text);
    // `delivered` owns the honest-reporting rule and the `agent-told` event, so
    // this reuses it whole rather than reimplementing either. Its non-zero exit
    // on `NotTaken` is right for `wsp tell`, where the sentence is the only
    // thing that happened; here the record is already written, so a rescue is
    // worth printing and is not a failure of the verb.
    match crate::cmd_agent::delivered(store, out, &sent) {
        0 => 0,
        _ => {
            eprintln!("     the answer is on the record whatever the composer does");
            0
        }
    }
}

/// How the asker sees it, and the first line is the byline.
fn wire(closed: &message::Closed) -> String {
    let mut out = format!(
        "[{}, answering your question {}]\n\n{}",
        whose(closed),
        closed.question.id,
        closed.reply.text.trim()
    );
    out.push_str(&format!("\n\nyou asked: {}", closed.question.title()));
    if let Landed::Task(id) = &closed.landed {
        out.push_str(&format!("\nthis is also on {id}'s log, so it survives this pane"));
    }
    out.push_str(&format!("\n`wsp ack {}` when you have read it", closed.reply.id));
    out
}

fn whose(closed: &message::Closed) -> String {
    closed.reply.from.byline()
}

/// Who a question is addressed to, in one line of receipt.
///
/// The same walk `wsp flag` prints and deliberately **not** the same sentence.
/// A flag's line ends *"x there lowers it"*, which is the correct instruction
/// for a hand and the exact wrong one for a question: lowering is not a
/// disposition, and a surface that offered it would reproduce `worklist-004`
/// every time somebody reached for the keystroke. So the walk is shared and the
/// closing clause is not — the routing is one rule and what may close a record
/// is a property of its shape.
fn addressed(store: &Store, task: &crate::model::Task) -> String {
    let governors = store.governors();
    match crate::cmd_govern::answering_seat(store, task) {
        Some(s) => format!(
            "asked of the {} governor · {}",
            s.scope,
            crate::cmd_govern::room_of(&governors, &s.scope)
        ),
        None => "asked of every panel — no governor above it".into(),
    }
}

// ---- who am I, and which record did you mean -------------------------------

/// This caller as a [`Party`], and the pane it is in if it is in one.
///
/// A seat is a seat before it is a pane, because that is what the other end
/// needs to read: an answer from *the wsp seat* is a different thing to weigh
/// than one from a pane id nobody can place. The pane is still returned beside
/// it, because a question asked from a seat still has to be answered back into
/// the workspace the seat is sitting in.
fn whoami(store: &Store) -> (Party, Option<String>) {
    // Off `my_pane()` and not `HERDR_WORKSPACE_ID`: after `compound-092` there
    // is no workspace to ask, only the seat, and `my_pane()` is already the
    // one place that picks it — a supervisor's `WSP_SEAT_ID` first, herdr's
    // `HERDR_PANE_ID` after — so a caller outside herdr is not told it is
    // nobody just because this file used to reach for herdr's own variable.
    let pane = crate::cmd_agent::my_pane();
    let scope = pane
        .as_deref()
        .and_then(|p| crate::cmd_govern::governs(&store.governors(), &crate::place::Seat::new(p)));
    match (scope, &pane) {
        (Some(scope), _) => (Party::seat(&scope), pane.clone()),
        (None, Some(p)) => (
            // The workspace here is a byline's, for a panel to jump to — not a
            // decision, so herdr's own variable is still the honest source for
            // it, and empty (as it is under any other backend) is correct.
            Party::pane(p, herdr::Env::read().workspace_id.as_deref().unwrap_or_default()),
            pane.clone(),
        ),
        // Outside herdr entirely, which is a person at a shell. The fallback is
        // the honest one rather than a convenience: unattributed is a human.
        (None, None) => (Party::Human, None),
    }
}

/// The record a needle names: an id, or an unambiguous prefix of one.
///
/// A message id is minted from a clock and a pid and nobody types one whole.
/// The prefix is refused when it matches two, rather than resolved to the
/// first: answering the wrong question is the failure this file exists to stop,
/// and it would be silent.
fn find(store: &Store, needle: &str) -> Result<Message, i32> {
    if let Some(m) = store.message(needle) {
        return Ok(m);
    }
    let hits: Vec<Message> =
        message::all(store).into_iter().filter(|m| m.id.starts_with(needle)).collect();
    match hits.len() {
        1 => Ok(hits.into_iter().next().unwrap_or_else(|| unreachable!())),
        0 => {
            eprintln!("wsp: no message `{needle}` — `wsp ask` lists what is open");
            Err(1)
        }
        n => {
            eprintln!("wsp: `{needle}` names {n} messages:");
            for m in hits.iter().take(8) {
                eprintln!("     {}  {}", m.id, m.title());
            }
            Err(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Task;

    fn scratch(tag: &str) -> Store {
        let root = std::env::temp_dir().join(format!("wsp-ans-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = Store::at(root.clone(), root.join("state"));
        store.ensure_dirs().unwrap();
        store
    }

    fn task(store: &Store, id: &str) -> Task {
        let mut t = Task::new("the work", id);
        t.project = Some("worklist".into());
        t.body = String::from("## Overview\nthe brief\n\n## Log\n- 2026-08-19 claimed\n");
        store.save_task(&t).unwrap();
        t
    }

    fn a_question(store: &Store, about: &str) -> Message {
        let q = Message::question(
            Party::seat("worklist"),
            Kind::Note,
            "does the barrier reading hold?",
            Waiting::new("w3R:p1", ""),
        )
        .about(About::Task(about.to_string()));
        message::raise(store, &q).unwrap();
        q
    }

    /// A message id is minted from a clock and a pid and **nobody types one
    /// whole**, so a prefix has to resolve — and a prefix that names two has to
    /// refuse rather than pick. Answering the wrong question is the failure
    /// this file exists to stop, and resolving to the first match would do it
    /// silently: the asker of the other question goes on sitting still while
    /// its record says somebody dealt with it.
    #[test]
    fn a_prefix_naming_two_questions_is_refused_rather_than_resolved() {
        let store = scratch("prefix");
        task(&store, "wsp-095");
        let one = a_question(&store, "wsp-095");
        let two = a_question(&store, "wsp-095");
        assert_ne!(one.id, two.id, "two questions are two records, keyed by their own ids");

        assert_eq!(find(&store, &one.id).map(|m| m.id), Ok(one.id.clone()), "an id resolves whole");
        assert_eq!(find(&store, "m-").err(), Some(2), "a prefix that names both is refused");
        assert_eq!(find(&store, "m-nothing").err(), Some(1), "and one that names none says so");
    }

    /// **The inversion `wsp-095` Part 4 is for**: today, unattributed text
    /// reads as Ed, so attributed text has to be a seat or an agent and
    /// unattributed text a person. wsp cannot make herdr attribute a keystroke
    /// — that is herdr's — so what wsp can do is carry the sender *in the
    /// payload*, and this is the one place the payload is composed.
    ///
    /// The dated instance: `worklist-004`'s question was answered by text that
    /// appeared in its composer from an unattributed sender, and the answer was
    /// wrong — following it would have destroyed another lane's test fixture.
    /// The reader could not weigh it because it could not tell who was
    /// speaking.
    #[test]
    fn what_reaches_the_asker_opens_by_saying_who_is_speaking() {
        let store = scratch("wire");
        task(&store, "wsp-095");
        let q = a_question(&store, "wsp-095");
        let closed = message::answer(
            &store,
            &q.id,
            &Party::seat("wsp"),
            "it holds — cmd_checkout::ahead() reads the branch off the tree",
        )
        .unwrap();

        let text = wire(&closed);
        let first = text.lines().next().unwrap_or_default();
        assert!(first.contains("wsp seat"), "the byline is the first thing read: {first}");
        assert!(first.contains(&q.id), "…and it names the question, so the answer is not free-floating");
        assert!(text.contains("cmd_checkout::ahead()"), "the answer itself is in there");
        assert!(
            text.contains("wsp-095's log"),
            "and it says the words survive this pane, which is the property Ed was supplying by hand",
        );
    }

    /// `wsp-170`: the verb a cut question points at exists, takes the id the
    /// line printed, and a short question is never cut at all.
    #[test]
    fn a_question_is_typed_whole_when_short_and_a_message_id_prints_it_whole() {
        let store = scratch("whole");
        task(&store, "wsp-095");
        let text = "first paragraph\n\nsecond paragraph\n\nA or B?";
        let q = Message::question(Party::seat("worklist"), Kind::Note, text, Waiting::new("", "wsp-095"))
            .about(About::Task("wsp-095".into()));
        message::raise(&store, &q).unwrap();

        let typed = for_a_governor(&q);
        assert!(typed.contains(text), "a short question is typed as asked: {typed}");
        assert!(!typed.contains("prints the whole question"), "and says nothing was cut: {typed}");

        let args = crate::Args::parse(["ask", q.id.as_str()].iter().map(|s| s.to_string()).collect());
        assert_eq!(ask(&store, &args), 0, "a message id and nothing to say shows the question");
        let args = crate::Args::parse(["ask", "m-nothing"].iter().map(|s| s.to_string()).collect());
        assert_eq!(ask(&store, &args), 1, "an id that names nothing says so");
    }

    /// A question needs a subject **because the subject is what gives the
    /// answer somewhere to live**, and the refusal is at the point of asking
    /// rather than at the point of answering. Discovered late, this is an agent
    /// sitting still while somebody works out that its question has no return
    /// address; refused early, it is one sentence.
    #[test]
    fn a_question_about_nothing_is_refused_where_it_is_cheap() {
        let store = scratch("subject");
        let args = crate::Args::parse(
            ["ask", "nothing-095", "can I land this?"].iter().map(|s| s.to_string()).collect(),
        );
        assert_eq!(ask(&store, &args), 1, "no such task, so there is nowhere for an answer to go");
        assert!(message::all(&store).is_empty(), "and nothing was written on the way to finding out");
    }

    /// `Waiting` holds a pane **and** a task because *the pane can go and the
    /// task cannot*, and this is the one call that spends the second field. A
    /// pane id is the most perishable identifier herdr has — one cascade of
    /// `pane.exited` once cleared every binding on this machine at a stroke —
    /// so a question open long enough to be worth answering is one whose pane
    /// may since have been reused. Typing an answer at whoever is standing
    /// there now is a wrong delivery that reports success.
    #[test]
    fn an_answer_goes_to_whoever_holds_the_work_now_and_not_to_a_pane_that_moved() {
        let store = scratch("route");
        task(&store, "worklist-014");
        store.set_binding("w9Z:p1", serde_json::json!({ "task_id": "worklist-014" }));

        assert_eq!(
            route(&store, Some(&Waiting::new("w4N:p1", "worklist-014"))),
            "w9Z:p1",
            "the binding is asked first, because it is the field that survives a respawn",
        );
        assert_eq!(
            route(&store, Some(&Waiting::new("w3R:p1", ""))),
            "w3R:p1",
            "a seat holds no task, so its pane is the only address it has",
        );
        assert_eq!(
            route(&store, Some(&Waiting::new("w4N:p1", "worklist-020"))),
            "w4N:p1",
            "a task nothing is bound to falls back rather than answering nobody",
        );
        assert_eq!(route(&store, None), "", "and nothing named is nobody to tell");
    }
}
