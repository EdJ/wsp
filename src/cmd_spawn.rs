//! `wsp spawn` — put a terminal, or an agent, on a piece of work.
//!
//! The panel could always open a workspace for a task and claim it there, and
//! then it stopped: somebody still had to walk over to the new pane and type
//! `claude`. And the whole gesture existed only as a key, so an agent could not
//! hand work to a new agent and neither could a script.
//!
//! One verb does both halves, and the panel's `O` and `S` run it rather than
//! keeping a second copy — the rule [`crate::panel::Effect::Run`] already
//! follows for every other command the panel issues.
//!
//! The order matters and is the whole design: **workspace, claim, agent,
//! sentence**. The claim has to land before the agent starts, because a Claude
//! Code session runs `wsp brief --session` from its `SessionStart` hook and
//! reads the claim on the way in. Started first, it would open knowing nothing
//! and the sentence would be the only thing it ever heard about the work.
//!
//! That order is also what lets the sentence stop asking for a brief: see
//! [`Handover`]. *What* the agent is started with, and how the sentence reaches
//! it, are facts about the agent rather than about placing work, and live in
//! [`crate::agent_commands`].
//!
//! [`despawn`] is the other end of it, and its order is the reverse: the seat
//! goes first and the claim last, for the reason `place.rs` gives.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::json;

use crate::agent_commands;
use crate::cmd_agent;
use crate::cmd_checkout::{self, Tree};
use crate::cmd_govern;
use crate::place::{Agent, Order, Place, Refusal, Seat, State};
use crate::place_herdr::Herdr;
use crate::resolve::Index;
use crate::store::Store;
use crate::util::{self, Clock, Paint};
use crate::Args;

/// How an agent came by the work it is being told about — and it is the one
/// thing that changes what the work order should say.
///
/// [`Handover::Spawned`] is `spawn`'s case. The order is workspace, claim,
/// agent, sentence, so by the time this is said the agent's `SessionStart` hook
/// has already run `wsp brief --session` *with the claim in place*: the task,
/// what binds it, and what to read are sitting at the top of its context. A
/// sentence asking it to fetch that again costs a round-trip, and a round-trip
/// at request 1 is a full context re-read — measured at ~35K on
/// t-260816-096, against ~700 for the duplicated text itself.
///
/// [`Handover::Running`] is the panel's. That agent's session began before the
/// claim existed, so its brief is a brief about holding nothing. It has to
/// fetch, and one `--session` call is the whole payload in one round-trip
/// rather than the dozen `wsp show` calls it would otherwise make.
///
/// The duplication is therefore disposed of by construction rather than by
/// remembering — the caller that knows the hook has run is the caller that
/// stops asking.
///
/// [`Handover::Custodian`] is the third, and it is a different *job* rather
/// than a different route to the same one. By the decision of 2026-08-17 on
/// t-260817-021 an agent can be assigned to a **project**, which is an edge the
/// model did not have — every other assignment in wsp is agent-to-task — and
/// what arrives in that slot is not a claimant with a piece of work to finish.
/// It sequences, directs, reviews and holds the record for everything beneath
/// it, and stands down when a person says so rather than when something is
/// done. This reverses the line that used to sit at the one call site below:
/// *"only a task gives an agent something to be told; a project workspace is a
/// place to work, not an instruction"*. Under a slot, a project workspace **is**
/// an instruction, and this is it.
///
/// It carries no fetch/no-fetch pair, because a custodian is spawned into its
/// slot and its `SessionStart` hook has therefore already run with the slot in
/// place — [`crate::cmd_brief`] draws the custodial brief off the same record.
/// A *running* agent handed a slot would be the fourth case and would have to
/// fetch; nothing in wsp does that yet, and inventing the sentence for it here
/// would be inventing the flow.
#[derive(Clone, Copy)]
pub enum Handover {
    Spawned,
    Running,
    Custodian,
}

/// What an agent is told about work it has just been handed.
///
/// One definition, three cases: the panel says this to an agent it claims a
/// task onto, `spawn` says it to the agent it just started, and `spawn
/// --govern` says the custodial one to an agent it has just put in a project's
/// slot. Wordings written in three places would be three contracts.
///
/// `subject` is a task for the first two and a **project** for the third, which
/// is the whole of what the new edge comes to.
pub fn work_order(subject: &str, how: Handover) -> String {
    match how {
        Handover::Spawned => format!(
            "You have been claimed onto {subject}. Your brief is already above: the task, \
             what binds it, and what to read. Begin work when you're ready."
        ),
        Handover::Running => format!(
            "You have been claimed onto {subject}. Please run `wsp brief --session`, then begin \
             work on the task when you're ready."
        ),
        // The four things the seat did on the night this was written from, in
        // the order they were done, and the one thing it must not become. No
        // task is named because there is not one: what is above this sentence
        // is the project's brief — what is open beneath it and who is standing
        // in it — and picking one of those up itself is the failure mode, not
        // the job.
        Handover::Custodian => format!(
            "You are the custodian of the {subject} project. You have not been claimed onto a \
             task and you should not claim one. Your brief is already above: what {subject} is \
             for, what is open beneath it, and who else is standing in it. The job is to \
             sequence what runs next and what waits, to write the direction an arriving agent \
             needs and no more, to review finished work against the code rather than against \
             the agent's report, and to hold the record: decisions, corrections, and what must \
             not close with the task that found it. `wsp flag --seat` is your inbox and `wsp \
             spawn` puts agents under you. You coordinate rather than authorise, so nothing \
             waits on your permission. Say what you are doing with `wsp say`, and begin by \
             reading what is open."
        ),
    }
}

/// The order `spawn` places: what to call the seat, where the work lives, and
/// what whatever runs there should know without having to infer it.
///
/// `WSP_PROJECT` and `WSP_TASK` go into the environment, so every pane inside
/// the seat knows what it is for without anyone having to read it off a path.
/// herdr does not persist env across a restart, which is why the durable answer
/// is a claim rather than this — but for the life of the session it is exact,
/// and exactness is what the cwd heuristic lacks.
///
/// It also *strips*, which is the same job and was the missing half of it: a
/// spawned agent is a new session and must inherit none of the spawning
/// session's identity. [`crate::place::shed_env`] says which names and why, and why an
/// empty value is the only strip there is to make. Nothing else in wsp is
/// positioned to do it — this is the one function that decides what a seat's
/// occupant finds — and it fails silently when it is not done: the agent runs
/// fine and saves no transcript.
///
/// A pure function of what `spawn` resolved, so what an agent is handed can be
/// asserted without a backend to hand it to.
fn order(work: &Work, cwd: Option<&str>, on: Option<&str>, show: bool) -> Order {
    Order {
        label: work.label.clone(),
        cwd: cwd.map(|c| c.to_string()),
        env: seat_env(work.project.as_deref(), work.task.as_deref()),
        on: on.map(|m| m.to_string()),
        show,
    }
}

/// What any seat wsp opens is given, whatever opened it.
///
/// Here rather than inlined above because `wsp resume` opens seats too, and a
/// resumed agent that inherited the caller's session identity — or missed the
/// store it is supposed to be reading — would be a second, quieter copy of
/// every failure [`crate::place::shed`] was written for.
pub(crate) fn seat_env(project: Option<&str>, task: Option<&str>) -> BTreeMap<String, String> {
    // Shed first: everything below is something this seat is *for*, and none of
    // it collides with a name the caller's Claude Code set.
    let mut env = crate::place::shed_env();
    // The store next, then what this seat is for — the latter wins if someone
    // has both, which is right: it is more specific.
    env.extend(
        util::store_env().into_iter().filter_map(|(k, v)| v.as_str().map(|v| (k, v.to_string()))),
    );
    if let Some(p) = project {
        env.insert("WSP_PROJECT".into(), p.to_string());
    }
    if let Some(t) = task {
        env.insert("WSP_TASK".into(), t.to_string());
    }
    env
}

/// Where a spawn is going: this machine unless `--on` says otherwise.
///
/// Asked, never inferred. There is no scheduler and no load model here on
/// purpose — auto-placement hides the thing you most want to see — and the
/// default is this machine so every existing caller and script keeps exactly
/// the behaviour it had.
///
/// A machine that is not in the store is a typo, and worth saying so rather
/// than letting it become a socket error about a path nobody typed. A machine
/// that is in the store but not answering is a different sentence, and carries
/// what the daemon last saw, because "why can I not spawn on mb2" is answered
/// by that line and nothing else.
fn placement(store: &Store, args: &Args) -> Result<Option<String>, String> {
    let Some(name) = args.get("on") else { return Ok(None) };
    let Some(m) = store.machine(&name) else {
        let known: Vec<String> = store.machines().into_iter().map(|m| m.name).collect();
        return Err(match known.is_empty() {
            true => format!("no machine `{name}` — this seat has none. wsp machine add <name> <ssh-target>"),
            false => format!("no machine `{name}` — there is {}", known.join(", ")),
        });
    };
    if !m.is_active() {
        return Err(format!("`{name}` is retired — wsp machine set {name} status=active"));
    }
    match store.machine_live(&name) {
        Some(l) if l.reachable => Ok(Some(m.name)),
        Some(l) if !l.error.is_empty() => Err(format!("`{name}` is not answering — {}", l.error)),
        Some(_) => Err(format!("`{name}` is not answering yet")),
        None => Err(format!(
            "`{name}` has no tunnel — is `wsp daemon` running? Nothing has reported on it"
        )),
    }
}

/// Which tier the agent is to be started at, checked before anything is opened.
///
/// Asked and never inferred, like [`placement`] — and here that is a park
/// rather than a principle: the decision of 2026-08-17 stood a routing
/// heuristic down on the evidence of its own dry runs and left this flag as the
/// whole mechanism, so whoever spawns states the tier when they already know it.
/// [`agent_commands::Kind::tier`] holds why the words are checked at all, and
/// [`agent_commands::EFFORTS`] why `--effort` is the one to reach for first.
///
/// What is decided *here* is only where and whether:
///
/// - **Before `place.open`.** A tier caught after the workspace exists has
///   already cost a workspace, a claim and a worktree, and now needs a
///   `wsp despawn` before it can be retyped. Nothing above this line has
///   written anything down.
/// - **Refused without `--agent`, not dropped.** `--model haiku` on a bare
///   workspace is a sentence about an agent that is never started, and a flag
///   that does nothing in silence is the failure the checking is for.
/// - **Refused when nobody will be there to unblock it.** A tier
///   [`agent_commands::Kind::unattended`] names cannot be walked away from, and
///   the default spawn is a background one — so the flag is refused unless
///   `--focus` says you are going to the pane. That check is second because it
///   is about this *spawn* and the one above it is about the words; a typo
///   should learn it is a typo before it learns anything else.
///
/// `--on <machine>` is orthogonal and stays that way: the flag states the tier,
/// and that machine's `claude` has its own version and its own settings.
fn tier(args: &Args, kind: &str, agent: bool) -> Result<(Option<String>, Option<String>), String> {
    let model = args.get("model");
    let effort = args.get("effort");
    if model.is_none() && effort.is_none() {
        return Ok((None, None));
    }
    if !agent {
        return Err("--model and --effort say how to start an agent — add --agent".into());
    }
    let kind = agent_commands::of(kind);
    kind.tier(model.as_deref(), effort.as_deref())?;
    if !args.has("focus") {
        if let Some(why) = kind.unattended(model.as_deref()) {
            return Err(why);
        }
    }
    Ok((model, effort))
}

/// herdr's default when nobody says which agent. Every other kind it knows is
/// spelt the way its own CLI spells it and passed straight through — an
/// unknown one is refused by herdr with the whole catalogue in the message,
/// which is a better list than one kept here and left to go stale.
pub(crate) const DEFAULT_KIND: &str = "claude";

/// How long the caller is prepared to wait, in three numbers and a clock.
///
/// A struct rather than three constants for the reason `place_herdr::Herdr`
/// gives about its own: every one of these was set by a failure, and a test that
/// has to sit through two real seconds to check what happens after two seconds
/// is a test nobody runs. [`Patience::default`] is what the CLI uses.
struct Patience<'a> {
    /// How long to give the agent to become ready for input, having started.
    ///
    /// A cold Claude Code measured four seconds to readiness on this machine;
    /// herdr's own default for the same wait is thirty. What "started" means,
    /// what has to be retried to get there and how long any of it takes are the
    /// backend's, and live in `place_herdr` — this is only how long the caller
    /// is prepared to wait.
    ready: Duration,
    /// How long a turn has to start, once the work order has been sent.
    ///
    /// **Measured against a live herdr 0.8.0 and a live Claude Code on
    /// 2026-08-18**, three runs of the loop below in a workspace of its own: the
    /// prompt was accepted and left sitting in the composer every time, and a
    /// return pressed at 5.1s started the turn in **0.15s**. So the number this
    /// has to be bigger than is not how long an agent takes to answer, it is how
    /// long Claude Code takes to *draw the composer that can be submitted into*
    /// — pressed at 2.0s, before the TUI had replaced the shell on screen, the
    /// return was swallowed exactly as the first one had been, and the seat sat
    /// idle for the twenty seconds that followed.
    ///
    /// Five seconds is above the widest reading of that window and below the
    /// point where a spawn feels stuck.
    ///
    /// It used to be paid on every spawn, which on that evening was the whole
    /// cost of the defect. It is now paid by a spawn that has already been told
    /// the turn did not start and is confirming a submit, because a backend that
    /// reports [`Refusal::NotTaken`] has done this wait already and
    /// [`hand_over`] does not repeat it. A healthy handover through herdr pays
    /// one `agent.get` and no sleep at all.
    taken: Duration,
    /// How many times to press submit again on a work order that arrived and
    /// was not taken.
    ///
    /// Bounded because pressing it is not free — a submit into an agent that
    /// *is* working goes in as an empty line — and because a second failure is
    /// evidence about the seat rather than about the keystroke. Two because a
    /// press can be lost the same way the first submit was: one is what the
    /// measurement above needed, and the spare is for the case where the
    /// composer was still a tenth of a second away.
    nudges: u32,
    /// How often to ask.
    poll: Duration,
    /// How long a seat has to go on looking empty before that is a death rather
    /// than a gap in what the backend can see.
    ///
    /// Two seconds is three times the longest gap measured — 620ms from
    /// `agent.start` to a named agent, recorded on 2026-08-17 — and it is only
    /// ever paid by a spawn that has genuinely failed, because one reading of a
    /// live agent clears it.
    gone: Duration,
    /// What time it is, and how to wait for the next look.
    ///
    /// Handed in rather than read, and this line was written by a flaky test
    /// rather than by taste. The throttle below is *per elapsed second* — ask
    /// the runtime once per [`Patience::gone`], because asking it is a
    /// subprocess — and a test that counts the asks in a fixed number of polls
    /// is measuring the machine's load, not the rule. It passed alone and failed
    /// under the full suite, which is the worst way for a test to be wrong:
    /// three agents building beside it stretched two hundred polls over enough
    /// wall-clock for a ninth ask.
    ///
    /// So the clock is a parameter and the test drives it, which makes the bound
    /// exact arithmetic instead of a guess about scheduling. The same seam the
    /// handbook asks for everywhere else: what value would have to be passed in
    /// for this to be testable?
    ///
    /// It carries the sleep as well as the reading, which is robustness-054's
    /// correction: a test that drove the clock and left the sleep behind still
    /// had to set `poll` to zero to be fast, so the loop under test was not the
    /// loop that runs. [`util::Clock`] holds the whole argument, and
    /// `place_herdr::Herdr` waits through the same one.
    clock: &'a dyn Clock,
}

impl Default for Patience<'static> {
    fn default() -> Patience<'static> {
        Patience {
            ready: Duration::from_millis(30_000),
            taken: Duration::from_millis(5_000),
            nudges: 2,
            poll: Duration::from_millis(150),
            gone: Duration::from_millis(2_000),
            clock: &util::Wall,
        }
    }
}

/// Wait until the agent in a seat will take a sentence.
///
/// **Not until it is idle**, which is the first trap this walked into: herdr
/// reports `agent_status: idle` while an agent is still drawing its banner, and
/// refuses a prompt in that window. `will_take_a_prompt` is the port's single
/// answer to the question every `state == "idle"` caller is actually asking, so
/// this is the one reading and there is nothing left here to get wrong.
///
/// **An empty seat is not a death, which is the second, and it cost a night.**
/// This used to return on the first empty reading, arguing that the agent
/// existed when `start` returned so nothing in it now meant it had stopped. Both
/// halves of that were wrong at once: `start` was coming back inside the launch
/// window (see `place_herdr::start`), and even with that fixed, one blind read
/// from one backend is a thin thing to end somebody's spawn on. It failed open
/// every time — a claimed task, a live agent, and no work order — and it was
/// only ever caught by a person watching the pane.
///
/// So an absence has to persist for [`Patience::gone`], and the kind gets a
/// veto. The
/// asymmetry in [`agent_commands::Kind::running`] is the point: it can say *this
/// agent is alive, keep waiting* and it cannot say *it is dead, stop now*,
/// because its registry is written a moment after the agent starts and "not yet"
/// and "never" look identical there. What that buys is a spawn that survives a
/// backend which cannot see, and still fails in two seconds rather than thirty
/// when the start really did fail.
fn wait_ready(
    place: &dyn Place,
    how: &dyn agent_commands::Kind,
    spawn: &agent_commands::Spawn,
    kind: &str,
    wait: &Patience,
) -> Result<(), String> {
    let seat = spawn.seat;
    let now = || wait.clock.now();
    let deadline = now() + wait.ready;
    let mut empty_since: Option<Instant> = None;
    loop {
        match place.state(seat) {
            Ok(s) if s.will_take_a_prompt() => return Ok(()),
            Ok(State::Empty) | Ok(State::Gone) => {
                let since = *empty_since.get_or_insert_with(now);
                if now().saturating_duration_since(since) >= wait.gone {
                    match how.running(spawn) {
                        // Alive, and the backend simply cannot see it. Start the
                        // clock again rather than clearing it, so the runtime is
                        // asked once per `gone` and not once per poll.
                        Some(true) => empty_since = Some(now()),
                        _ => return Err(format!("{kind} started and then stopped in {seat}")),
                    }
                }
            }
            // The seat itself, rather than what is in it. Nothing is coming back
            // from a pane that has been closed.
            Err(Refusal::NoSeat(_)) => return Err(format!("{seat} is gone")),
            // A refusal is not a verdict while there is time left: a backend that
            // did not answer this poll may answer the next.
            Ok(_) | Err(_) => empty_since = None,
        }
        if now() >= deadline {
            return Err(format!("{kind} started but never became ready for input"));
        }
        wait.clock.rest(wait.poll);
    }
}

/// Hand over the work order, and come back only when a turn has started on it.
///
/// **The defect this is the whole of, and it fails open, which is why it was
/// expensive.** `tell` returns `Ok` when the backend accepted the call, and
/// `spawn` reported success on the send. What was actually happening on a
/// terminal backend is that the sentence arrived in Claude Code's composer and
/// the Enter after it was swallowed: an agent sitting idle in front of a work
/// order it has never been shown, holding a claim, and reported as a healthy
/// spawn. Ed found it by watching a pane; nothing else could have. Four of six
/// agents in one burst on 2026-08-17, three of three in a quiet moment that
/// night with the machine idle and nobody at the keyboard, after herdr 0.8.0
/// had already put a wait between its own text and its own Enter.
///
/// **It is not wsp's, and that is why nothing wsp could do to its own call
/// fixed it.** Reproduced on 2026-08-18 with no wsp in the picture at all — a
/// workspace, `agent.start`, `agent.prompt` on the raw socket — three runs,
/// three sentences left sitting in the composer with the agent idle, read back
/// off the screen. What wsp owns is the half after that: noticing, and saying
/// so.
///
/// So the readiness wait is not the fix and was never missing: [`wait_ready`]
/// runs first and asks `will_take_a_prompt`, which is herdr's optimistic
/// signal and goes true before the composer can take a submission. Waiting
/// longer only widens the window it is wrong in. **The only thing that settles
/// it is the agent leaving idle**, which is a fact about the agent rather than
/// a promise about the call.
///
/// **Half of that reading has since moved to the server, and the half that
/// stayed is the interesting one.** `agent.prompt` will now hold its reply until
/// the agent's status moves and refuse when it does not
/// (`place_herdr::Herdr::tell`), so on that backend the looking below is no
/// longer how the defect is caught — the answer arrives already knowing. What it
/// cannot answer is *why* nothing moved. herdr sees a prompt delivered to an
/// agent that is genuinely idle, and that is the same picture whether the
/// composer was a moment from ready or a folder-trust modal had the keyboard
/// when the text arrived. Only the second is a thing a keystroke fixes and only
/// wsp is in a position to try, so the submit stays and this function still
/// exists. What it no longer does is *discover* the failure by waiting five
/// seconds for it.
///
/// Then the loop is what was run by hand: press submit again, look again,
/// bounded, and fail loudly rather than print a success. `Unsupported` from
/// [`Place::nudge`] ends it at once — a backend with nothing to press has
/// nothing to try — and is still a failure, because the turn did not start
/// either way. That case is `wsp spawn --on <machine>` into a session nobody
/// had logged in, which reported three lines of success on 2026-08-17 for an
/// agent that could never take a turn.
///
/// [`State::turn_in_flight`] and nothing softer is what counts as taken, and
/// robustness-083 gave that reading a name without widening it. The one that
/// asks to be let in is [`State::Blocked`] — an agent that answers a work order
/// by asking for a permission has plainly taken it — and it must stay out,
/// because the *other* thing that reads `blocked` is a folder-trust modal
/// holding the keyboard with the order still unsent behind it. Those two are
/// the same status and opposite outcomes, and the second Enter below is right
/// for one of them, so this stays strict and lets the press decide.
///
/// The cost of that strictness is the one false alarm available here: an agent
/// that takes the order, finishes the whole turn inside [`Patience::taken`] and
/// is idle again by the first look. A work order whose first act is reading a
/// brief does not, and a spawn wrongly called failed is recoverable in a way
/// that a spawn wrongly called succeeded is not.
fn hand_over(
    place: &dyn Place,
    how: &dyn agent_commands::Kind,
    spawn: &agent_commands::Spawn,
    text: &str,
    wait: &Patience,
) -> Result<(), String> {
    let seat = spawn.seat;
    // What was removed here, and it is a reading rather than a line: a backend
    // that answers [`Refusal::NotTaken`] has just spent its own timeout watching
    // for the turn this used to go and look for, so the first look is skipped and
    // the submit is pressed at once. herdr does that watching now
    // (`place_herdr::tell`), which is why the five seconds below are no longer
    // the thing that catches the failure — they are what confirms the rescue.
    //
    // The refusal is not an error. A caller that propagated it would report the
    // one failure this function exists to recover from, and `?` here was the
    // whole of that mistake.
    let mut look = match how.tell(place, seat, text) {
        // Both deliveries look the same from here, and deliberately.
        // `Delivery::Started` is a status change of *some* kind and this
        // function is strict about which — see the paragraph above on
        // `Blocked` — so the confirmation stays wsp's own.
        Ok(_) => true,
        Err(Refusal::NotTaken) => false,
        Err(e) => return Err(e.to_string()),
    };
    for pressed in 0..=wait.nudges {
        if look && took_it(place, seat, wait) {
            return Ok(());
        }
        // Only the first look can be skipped. After a submit nobody has watched
        // anything, and the confirmation is wsp's again.
        look = true;
        if pressed == wait.nudges {
            break;
        }
        match place.nudge(seat) {
            Ok(()) => {}
            Err(Refusal::Unsupported(_)) => {
                return Err(format!("{seat} took the work order and started nothing"))
            }
            Err(e) => return Err(format!("{seat} did not take the work order: {e}")),
        }
    }
    Err(format!("the work order is sitting in {seat} unsent"))
}

/// Whether a turn started inside [`Patience::taken`].
///
/// The same shape as [`wait_ready`]'s loop and deliberately not folded into it:
/// that one waits for an agent to become *able* to be told something, this one
/// waits for evidence that it *was*. They read the same call and mean opposite
/// things by an idle agent.
fn took_it(place: &dyn Place, seat: &Seat, wait: &Patience) -> bool {
    let now = || wait.clock.now();
    let deadline = now() + wait.taken;
    loop {
        if place.state(seat).is_ok_and(|s| s.turn_in_flight()) {
            return true;
        }
        if now() >= deadline {
            return false;
        }
        wait.clock.rest(wait.poll);
    }
}

/// Say how to reach an agent a spawn could not, where the kind knows a way.
///
/// The failure this softens is measured rather than imagined: `spawn`'s work
/// order failed to arrive seven times in one night, and every one of those
/// agents was started, alive, and recovered by hand over its own session
/// channel. What that recovery cost each time was somebody listing every agent
/// on the machine and working out which name belonged to the pane that had just
/// failed. wsp is the only thing that already knows.
///
/// Best effort by construction, and never wrong about *whose* agent it names —
/// a work order sent to the handle here would go to whoever it names, so
/// `agent_commands::pick` refuses an ambiguous one and always will. What it is no
/// longer allowed to be is **silent**, which is what it was on all three of the
/// failures robustness-041 reproduced; it is asked the spawn rather than the seat
/// for the reason `agent_commands` measures, and it may hedge.
///
/// The order of the two lines at the call sites is load-bearing: the caller
/// prints *why* the spawn failed and then calls this, so the wait inside delays
/// the advice and never the diagnosis.
pub(crate) fn unreached(how: &dyn agent_commands::Kind, place: &dyn Place, spawn: &agent_commands::Spawn) {
    if let Some(line) = agent_commands::recovery(how.address(place, spawn)) {
        eprintln!("wsp: {line}");
    }
}

/// Wait for a just-started agent to be ready for input, on the CLI's patience.
///
/// The one thing a second caller of `place.start` needs and cannot have: the
/// two-question sequence — *does it exist*, *will it listen* — is this file's,
/// and [`Patience`] is deliberately private because its numbers were each set
/// by a failure and are not a knob. `wsp resume` starts an agent exactly the
/// way `spawn` does and waits exactly as long.
pub(crate) fn ready(
    place: &dyn Place,
    how: &dyn agent_commands::Kind,
    spawn: &agent_commands::Spawn,
    kind: &str,
) -> Result<(), String> {
    wait_ready(place, how, spawn, kind, &Patience::default())
}

/// What `spawn` resolved its argument to.
struct Work {
    task: Option<String>,
    project: Option<String>,
    /// The **worklist** this spawn is being seated on, when `--govern` named
    /// one rather than a project.
    ///
    /// A separate field rather than a scope in `project`, and that is the whole
    /// reason it exists: `project` is what the pane is *standing in* — it fills
    /// `WSP_PROJECT`, it resolves the cwd, and every reading of "where am I"
    /// walks it — and a worklist is none of those things. A slug written into
    /// it would give the custodian a brief about a project that does not exist,
    /// which is precisely the near miss the `--govern` guard below refuses to
    /// make.
    list: Option<String>,
    /// The workspace's opening name. A claim renames it after the task a
    /// moment later — to the same thing, for a task, so the window does not
    /// change its name under whoever was already looking at it. A project
    /// keeps this one.
    label: String,
}

/// A task, or a project, or nothing that resolves.
///
/// `-p` forces the project reading. Without it a task is tried first and a
/// project second, which is the order the ids themselves suggest: `t-260815-033`
/// can only be a task, and a project slug can only be a project, so the two
/// collide solely on a title substring — where the task is what was meant, that
/// being the thing you were just reading.
fn resolve(store: &Store, args: &Args, index: &Index) -> Result<Work, String> {
    if let Some(p) = args.get("project") {
        // A worklist, but only for `--govern`. A list is a thing to *run*, not
        // a place to work: it has no root to stand in and no backlog to claim
        // out of, so `-p <slug>` on its own would open a workspace that could
        // not answer the first question asked of it. Under `--govern` it is the
        // obvious thing and needs no flag of its own — the seat's key is a
        // scope, so this is the same command it always was pointed at the other
        // half of one key space.
        if args.has("govern") {
            if let Some(w) = store.worklist(&p) {
                return Ok(Work {
                    task: None,
                    project: None,
                    label: crate::cmd_govern::governor_of(&w.id),
                    list: Some(w.id),
                });
            }
        }
        let proj = index.find(&p).ok_or_else(|| format!("no project matching `{p}`"))?;
        return Ok(Work {
            task: None,
            project: Some(proj.id.clone()),
            label: proj.name.clone(),
            list: None,
        });
    }
    let needle = args
        .rest
        .first()
        .cloned()
        .ok_or_else(|| "usage: wsp spawn <task|-p project> [--agent]".to_string())?;
    if args.has("govern") {
        if let Some(w) = store.worklist(&needle) {
            return Ok(Work {
                task: None,
                project: None,
                label: crate::cmd_govern::governor_of(&w.id),
                list: Some(w.id),
            });
        }
    }
    if let Some(t) = store.find_task(&needle) {
        return Ok(Work {
            project: t.project.clone(),
            label: cmd_agent::task_label(&t).unwrap_or_else(|| t.title.clone()),
            task: Some(t.id),
            list: None,
        });
    }
    match index.find(&needle) {
        Some(proj) => Ok(Work {
            task: None,
            project: Some(proj.id.clone()),
            label: proj.name.clone(),
            list: None,
        }),
        None => Err(format!("no task or project matching `{needle}`")),
    }
}

pub fn spawn(store: &Store, args: &Args) -> i32 {
    // No pre-flight question about a socket. `herdr::available()` used to guard
    // this and printed a herdr sentence for a herdr fact; a backend that is not
    // answering now says so from the call that wanted it, which arrives at the
    // same moment and names what it stopped.
    place_work(backend(args).as_ref(), store, args)
}

/// Which backend places this work: a terminal, or a supervisor with none.
///
/// The flag is the whole of the choice and there is deliberately no inference
/// behind it. What `--headless` buys is an agent that can be started, told,
/// observed and stopped with no terminal anywhere; what it costs is the one
/// thing a supervisor cannot give you, which is an agent you can **sit down in
/// front of** — no permission prompts, no input box, no attaching. That is a
/// decision about how you mean to work with this agent rather than a detail of
/// where it runs, so it is asked rather than guessed.
///
/// Everything below this line is backend-agnostic already, which is the port
/// earning itself: `place_work` was written against `&dyn Place` and needed no
/// change to grow a second implementor.
fn backend(args: &Args) -> Box<dyn Place> {
    match args.has("headless") {
        true => Box::new(crate::place_super::Supervisor::new()),
        false => Box::new(Herdr::new()),
    }
}

fn place_work(place: &dyn Place, store: &Store, args: &Args) -> i32 {
    let p = Paint::new();
    let index = Index::new(store.projects());
    let work = match resolve(store, args, &index) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("wsp: {e}");
            return 2;
        }
    };

    let on = match placement(store, args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("wsp: {e}");
            return 2;
        }
    };

    // Both read before anything is opened, so a mistyped alias costs a line of
    // output instead of a workspace to tear down again.
    let kind = args.get("kind").unwrap_or_else(|| DEFAULT_KIND.to_string());
    let (model, effort) = match tier(args, &kind, args.has("agent") || args.has("govern")) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("wsp: {e}");
            return 2;
        }
    };

    // A slot is on a project, so `--govern` on a task is a sentence with no
    // meaning rather than a near miss — and the near miss it would otherwise
    // become is the expensive one: an agent claimed onto a task and told it is
    // the custodian of everything above it.
    if args.has("govern") && work.task.is_some() {
        eprintln!("wsp: --govern seats an agent on a project or a worklist — name one, or use -p");
        return 2;
    }

    // Still this machine's paths, deliberately. A project root is a path in the
    // store and `~` expands here, which is right while the machines mirror each
    // other and is exactly what the Linux box breaks; host-qualified roots are
    // t-260815-060 and are not smuggled in here.
    let cwd = args
        .get("cwd")
        .or_else(|| work.project.as_deref().and_then(|p| index.root_of(p)));

    // One tree per agent, which is the whole of t-260815-022 and is done here
    // rather than asked of the agent. Every softer version of it has been tried
    // in this repository and recorded on that task — naming paths, staging by
    // hunk, announcing first — and each failed the same way: an agent that has
    // to remember a rule is an agent that will be halfway through something
    // more interesting when it matters. An agent that is simply standing in a
    // checkout of its own cannot take anybody's work, because the work is not
    // there to take.
    //
    // Only a task gets one. A project seat is a place to work rather than a
    // piece of work, it has nothing to branch for and nothing to land, and it
    // is where a person reviewing the whole tree wants to be standing.
    // `--no-tree` for the case where you deliberately want the trunk, and a
    // spoken fallback when the tree cannot be made, because a spawn that fails
    // outright over this is worse than a spawn that says where it put you.
    let cwd = match (&work.task, &cwd) {
        (Some(task), Some(root)) if !args.has("no-tree") => {
            Some(crate::cmd_checkout::tree_for(root, task).unwrap_or_else(|| {
                eprintln!("wsp: no tree of its own for {task} — opening in {root}");
                root.clone()
            }))
        }
        _ => cwd,
    };

    // Focus is asked for, never assumed. This line used to read
    // `!args.has("no-focus")`, so every spawn dragged the screen onto the new
    // seat unless the caller thought to say otherwise — and `spawn` is run by
    // the queue at least as often as by hand, which meant a batch starting
    // overnight took the screen off whatever a person was reading. herdr's
    // `workspace.create` defaults `focus` to false and wsp was opting in; the
    // doc on [`Order::show`] already said `false` by default, so the port
    // stated the rule and this line contradicted it.
    //
    // `--no-focus` is still parsed (`main::BOOL_FLAGS`) and now asks for what
    // already happens: dropping it outright would make an old invocation swallow
    // the id after it rather than fail, which is a worse way to learn.
    //
    // The ask below is honoured and the screen still moves, which is not this
    // line's doing: herdr's `workspace.created` runs wsp's own plugin hook, the
    // hook installs the panel, and the `pane.swap` it needs focuses with no way
    // to decline. See `panel::install::install_one` and `fork-002`.
    let order = order(&work, cwd.as_deref(), on.as_deref(), args.has("focus"));
    let seat = match place.open(&order) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("wsp: {e}");
            return 1;
        }
    };

    // The claim, through the one implementation of it. It refuses on work that
    // is done, work that is blocked, and work a live agent is holding, and each
    // refusal is a reason not to start an agent here — a spawn onto a blocked
    // task is precisely what that guard exists to prevent. The workspace is
    // left standing either way: it is a terminal in the right tree, which is
    // what you would have opened by hand, and closing something that already
    // has a shell in it to tidy up after a refusal is a worse trade.
    let claimed = match &work.task {
        Some(t) => {
            // The seat, under the name the claim still calls it. `claim --pane`
            // is `cmd_agent`'s vocabulary and migrating it is its own task; the
            // string is the same string either way.
            let mut flags: Vec<(&str, &str)> = vec![("pane", seat.as_str())];
            // The tier travels with the claim, because the claim is what
            // writes the attempt down. `spawn` is the only caller that knows
            // it — an agent claiming at its own shell has no flag to pass and
            // leaves the clause off, which is the honest record of a claim
            // made with nothing stated.
            if let Some(m) = &model {
                flags.push(("model", m));
            }
            if let Some(e) = &effort {
                flags.push(("effort", e));
            }
            if args.has("force") {
                flags.push(("force", "true"));
            }
            if args.json() {
                flags.push(("json", "true"));
            }
            cmd_agent_claim(store, t, &flags) == 0
        }
        None => false,
    };
    if work.task.is_some() && !claimed {
        eprintln!("wsp: opened {seat} — but the claim was refused, so no agent was started");
        return 1;
    }

    // The other assignment: this workspace is the custodian of a project. Done
    // here, beside the claim and for the same reason it is here rather than
    // after the agent starts — the agent's `SessionStart` hook runs `wsp
    // brief`, and a slot recorded a second later is a custodian whose first
    // sight of itself is a brief about holding nothing.
    // The scope the slot is keyed on, which is a project id or a worklist slug
    // — see [`cmd_govern::seat_for`]. Everything below reads it as a name to
    // record a seat under and to tell an agent what it is custodian of, and
    // neither of those cares which of the two it is.
    let governing: Option<String> = match args.has("govern") {
        true => work.list.clone().or_else(|| work.project.clone()),
        false => None,
    };
    if let Some(project) = &governing {
        match workspace_of(&seat) {
            Some(ws) => {
                if let Some(was) = cmd_govern::take(store, project, &ws, seat.as_str()) {
                    println!("  {}", p.dim(&format!("{project} seat taken from {}", was.workspace)));
                }
            }
            // The workspace id is herdr's word and the port has none for it, so
            // this is the one fact `spawn` cannot get through `place`. Spoken
            // rather than fatal: the terminal is open and the agent is about to
            // be told what its job is, and the record is one command away from
            // inside it.
            None => eprintln!(
                "wsp: opened {seat} but could not record the {project} seat — \
                 run `wsp govern {project}` in it"
            ),
        }
    }

    // A slot is for an agent. `--govern` without `--agent` would record a
    // position and leave a shell sitting in it, which is a seat that answers
    // for raised hands and cannot read one.
    let mut started: Option<String> = None;
    let mut told = false;
    let mut ordered = false;
    if args.has("agent") || governing.is_some() {
        let name = work
            .task
            .clone()
            .or_else(|| work.list.clone())
            .or_else(|| work.project.clone())
            .unwrap_or_default();
        let how = agent_commands::of(&kind);
        // The seat is passed in because the agent is named after it, and it can
        // be passed in because `open` has happened above: the port splits opening
        // a seat from starting an agent in it, so wsp holds the seat before
        // there is anything sitting there to ask. That ordering is what makes
        // the handle knowable in advance rather than discovered afterwards.
        let spawn = agent_commands::Spawn {
            full: args.has("full"),
            name: &name,
            seat: &seat,
            model: model.as_deref(),
            effort: effort.as_deref(),
            resume: None,
        };
        let agent = Agent { kind: kind.clone(), name: name.clone(), args: how.args(&spawn) };
        // Two waits, and they are different questions: `start` comes back when
        // the agent exists, `wait_ready` when it will listen. Whatever a backend
        // has to do to make the first one true — retry a shell that is not ready,
        // clear a half-typed line — happens on its side of the seam now.
        match place.start(&seat, &agent).map_err(|e| e.to_string())
            .and_then(|()| wait_ready(place, how, &spawn, &kind, &Patience::default()))
        {
            Ok(()) => {
                started = Some(kind.clone());
                // A task gives an agent something to be told, and so — since
                // t-260817-021 — does a project it is being made custodian of.
                // A bare project workspace is still what the line here used to
                // say of all of them: a place to work rather than an
                // instruction, with `f` in the panel the key that turns one
                // into the other. What changed is that `--govern` is an
                // instruction, and it is the custodial one.
                let order = match (&work.task, &governing) {
                    (Some(t), _) => Some(work_order(t, Handover::Spawned)),
                    (None, Some(p)) => Some(work_order(p, Handover::Custodian)),
                    (None, None) => None,
                };
                if let Some(text) = order {
                    ordered = true;
                    // Through the kind rather than through the port. Readiness
                    // is established above, and *how* a sentence reaches an
                    // agent of this kind — its own channel, or the backend
                    // typing at it — is the one decision this file must not
                    // make; see `agent_commands`.
                    match hand_over(place, how, &spawn, &text, &Patience::default()) {
                        Ok(()) => told = true,
                        Err(e) => {
                            eprintln!("wsp: agent started but not working on it: {e}");
                            unreached(how, place, &spawn);
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("wsp: {kind} did not start in {seat}: {e}");
                unreached(how, place, &spawn);
            }
        }
    }

    // The session the agent is running under, recorded now that there is one.
    //
    // The claim above ran *before* `start` — that ordering is this file's and is
    // deliberate, because the agent reads the claim in its `SessionStart` brief
    // — so the binding it wrote could not carry a session, and until 2026-08-17
    // none ever did. This is the first moment anything can, and it is the moment
    // the value is cheapest: the backend has just been asked whether the agent
    // is ready, so it plainly has an opinion about what is in the seat.
    //
    // Best-effort, and silent. A backend that cannot say yet — herdr's detection
    // lags a launch by a second or so — leaves the field empty and the daemon's
    // next `sync` fills it, which is why nothing here waits or complains.
    if started.is_some() {
        if let Ok(rows) = place.census() {
            cmd_agent::learn_sessions(
                store,
                rows.seats()
                    .filter(|r| r.seat == seat)
                    .map(|r| (r.seat.as_str(), r.session.as_str())),
            );
        }
    }

    if args.json() {
        println!(
            "{}",
            json!({
                // One handle where there were two. A herdr seat is the pane id
                // this printed as `pane` before, so a caller doing
                // `wsp release --pane $(...)` reads the same string out of a
                // different key; what has gone is `workspace`, which was herdr's
                // second id and is not something the port hands back.
                "seat": seat.as_str(),
                "task": work.task,
                "project": work.project,
                "cwd": cwd,
                "agent": started,
                "told": told,
            })
        );
    } else {
        let what = match &started {
            Some(kind) => format!("{kind} in {seat}"),
            None => format!("a terminal in {seat}"),
        };
        println!("  {}", p.dim(&format!("opened {what}{}", match &cwd {
            Some(c) => format!(" · {}", util::contract(&util::expand(c))),
            None => String::new(),
        })));
        if told {
            println!("  {}", p.dim("told it what it is holding"));
        }
    }
    // An agent asked for and not started is a failure however well the seat
    // went: the caller wanted somebody working, and there is nobody.
    if (args.has("agent") || governing.is_some()) && started.is_none() {
        return 1;
    }
    // And a work order that started no turn is the same failure one step later,
    // which is robustness-035 and is the reason this line exists. The seat is
    // open, the claim is written and the agent is alive — all of it true, none
    // of it work — so an exit code is the only thing left that a queue spawning
    // unattended can read. Saying so in the status is what stops a governor
    // concluding the night is under way.
    if ordered && !told {
        return 1;
    }
    0
}

/// The workspace a pane is in, which is the id a slot is recorded against.
///
/// The one herdr-shaped question `spawn` asks outside the port. `place::Seat`
/// is a string wsp is forbidden to parse — the whole point of the handle — and
/// `place::Seated` has no word for a workspace, because starting and stopping
/// work never needed one. A slot does: it is keyed on the workspace for the
/// same reason a claim is, so that an agent cleared and restarted on a new pane
/// is the same custodian rather than a vacancy.
pub(crate) fn workspace_of(seat: &Seat) -> Option<String> {
    crate::herdr::panes()
        .ok()?
        .into_iter()
        .find(|p| p.pane_id == seat.as_str())
        .map(|p| p.workspace_id)
        .filter(|w| !w.is_empty())
}

/// `wsp despawn` — end the agent on a piece of work, and put the work down.
///
/// The other end of [`spawn`], and the reason the port has a `stop` at all: a
/// loop that starts agents one at a time and cannot end one makes despawning
/// part of the loop rather than an edge case.
///
/// # One verb, because the hand procedure was never finished
///
/// Ending an agent took three commands and nothing did all three:
///
///     wsp despawn <id>            ended the agent, released the claim
///     wsp checkout <id> --rm      removed the worktree
///     herdr workspace close <ws>  closed the workspace the pane left behind
///
/// Only the first is a wsp verb anybody reaches for, so the other two were done
/// by whoever noticed — which is why they were not done. What that cost is on
/// robustness-076 and is not small: eighteen worktrees and nineteen workspaces
/// left standing after one overnight batch, every one of them found by
/// accident. `wsp checkout --sweep` cannot help, because it only removes trees
/// whose task is *finished* and everything here sits at `review` by design.
///
/// So this verb does the whole ending. Two of the three are one call already —
/// closing the last pane of a workspace takes the workspace with it, measured
/// against herdr 0.8.0 on 2026-08-19 and argued in `place_herdr`'s module docs —
/// and the third is [`cmd_checkout::discard`], with `--rm`'s refusals and not
/// the sweep's. There is a fourth nobody had counted: a build tree under the
/// state directory is keyed on the workspace, so the workspace going takes its
/// last owner with it, and 9.6G of that residue had accumulated by 2026-08-17.
/// [`crate::cmd_verify::clear_build_key`] takes the one that has just been
/// orphaned.
///
/// **Stop first, release last** — the reverse of the list above, and the
/// argument for it is where the rule about the claim and the seat already
/// lives, in `place.rs`. This is the half of it that is code: the claim is
/// released only once the seat is gone, a seat that was *already* gone counts
/// as gone, and a backend that did not answer is neither. The tree comes after
/// both, because it is the only step that can be done later by hand.
///
/// # What it will not do
///
/// It is aimed by hand at a seat, and it stays that way. Nothing here reaps on
/// idleness: a task at `review` is not evidence its agent has stopped — `review`
/// is where everything sits, because `done` belongs to the person the work is
/// for — and an agent idle for ten minutes may be waiting on a person.
/// Robustness-051 was exactly that, and killing it would have destroyed work to
/// save a directory.
///
/// Every step reports what it *did* rather than what it attempted, including
/// the tree it decided to leave and why. That is the house fault this verb was
/// written against, and a cleanup that prints "removed" over a tree still on
/// disk is worse than one that never ran.
///
/// No guard on an agent that is busy, and that is a decision rather than an
/// omission. `claim`'s live-holder guard protects you from a *third party* you
/// may not have known was there; this verb is aimed at a seat by somebody who
/// knows what is in it. What ending it costs is the session, not the work — a
/// tree holding anything uncommitted is left exactly where it is, which is the
/// whole of what [`cmd_checkout::discard`] refuses on — and the state it would
/// refuse on is the one a wedged agent reads as, which is when you most want
/// this.
///
/// `--headless` names the backend, the same flag the spawn was placed with, and
/// **wsp does not know which one a seat belongs to** — a claim records a
/// workspace, a binding records a seat id, and neither says what issued it.
/// Asking is the honest version of that gap: the alternative is to try one
/// backend and then the other, which turns "no such seat" into "no such seat
/// anywhere" and makes a stopped agent indistinguishable from a mistyped id.
/// Which record should carry it is the state-model half of the translation
/// layer (the binds note on robustness-061), and is not decided here.
pub fn despawn(store: &Store, args: &Args) -> i32 {
    let keep = args.has("keep-tree");
    let tidy = |seat: &Seat, task: Option<&str>, ws: Option<&str>| swept_up(store, seat, task, ws, keep);
    end_work(backend(args).as_ref(), store, args, cmd_agent::my_pane().as_deref(), &tidy)
}

/// What the ending took away after the seat itself.
struct Leftovers {
    tree: Tree,
    /// Build trees that were keyed on the workspace the seat took with it.
    builds: Vec<String>,
}

/// The tree and the build trees, once the seat has gone.
///
/// Passed into [`end_work`] as a closure rather than called from inside it, for
/// the reason `me` is passed in below: this is the half of the verb that needs
/// a git repository and a live herdr under it, and a test of the *ordering* —
/// stop, release, then tidy — should not need either. It is also the half that
/// removes directories, and a unit test that reached the real one would be
/// running `git worktree remove` against whatever tree the test runner happens
/// to be standing in.
/// `keep` is `--keep-tree`, and it covers the *checkout* only. A build tree
/// keyed on a workspace that has gone has no owner left to keep it for, so
/// there is nothing for a flag to protect and no flag to remember.
fn swept_up(
    store: &Store,
    seat: &Seat,
    task: Option<&str>,
    workspace: Option<&str>,
    keep: bool,
) -> Leftovers {
    let cwd = std::env::current_dir().unwrap_or_default();
    let here = util::real(&cwd.display().to_string());
    // The seat this despawn has just closed is not somebody standing in the
    // tree. herdr can go on listing a pane for a moment after `pane.close`
    // returns, and reading that back as an occupant is how a cleanup verb comes
    // to refuse on the pane it removed itself.
    let seen: Result<Vec<crate::herdr::Pane>, String> = match crate::herdr::available() {
        // No herdr on this machine is not a pane in the tree: `place_super`
        // runs agents with no terminal at all, and a cleanup that refused
        // without one would remove nothing there, ever.
        false => Ok(Vec::new()),
        // A socket that is there and will not answer is a different fact, and
        // the direction to fail in is the one `sync.rs:41` already argues:
        // silence is not evidence that nobody is standing in the tree.
        true => crate::herdr::panes()
            .map(|ps| ps.into_iter().filter(|p| p.pane_id != seat.as_str()).collect())
            .map_err(|e| format!("herdr did not say who is standing in it: {e}")),
    };
    let standing = |dir: &std::path::Path| -> Option<String> {
        let dir = util::real(&dir.display().to_string());
        if here.starts_with(&dir) {
            return Some("you are standing in it".into());
        }
        let panes = match &seen {
            Ok(panes) => panes,
            Err(why) => return Some(why.clone()),
        };
        panes
            .iter()
            .find(|p| util::real(&p.cwd).starts_with(&dir))
            .map(|p| format!("{} is standing in it", p.pane_id))
    };

    let tree = match task.filter(|_| !keep) {
        Some(t) => cmd_checkout::discard(cmd_checkout::candidates(store, &cwd, t), t, &standing),
        // A seat holding nothing names no tree. There is no path from a seat to
        // a checkout except through the task, and guessing one from the pane's
        // cwd would remove a tree on the strength of where somebody stood.
        None => Tree::Absent,
    };

    // Only once herdr says the workspace has actually gone. A seat with
    // siblings leaves its workspace standing, and a build tree keyed on a live
    // workspace belongs to whoever is still in it.
    let gone = |ws: &str| crate::herdr::workspaces().map(|all| !all.iter().any(|w| w.id == ws));
    let builds = match workspace {
        Some(ws) if gone(ws).unwrap_or(false) => crate::cmd_verify::clear_build_key(store, ws),
        _ => Vec::new(),
    };
    Leftovers { tree, builds }
}

/// Which seat a despawn is about: the one named, or the one holding the task.
///
/// A task resolves through its *binding*, which is the only record that names a
/// seat — a claim names a workspace, and the port cannot turn one into a seat
/// (`place::Seated` does not say where a seat is). So a task whose binding has
/// been lost, which is what a herdr restart leaves behind, is refused with the
/// command that rebuilds it rather than a guess: `wsp reconcile` binds a claim
/// back to a pane by label, and despawn works again afterwards.
fn seat_of(store: &Store, args: &Args, index: &Index) -> Result<(Seat, Option<String>), String> {
    let task_of = |seat: &str| {
        store
            .bindings()
            .get(seat)
            .and_then(|b| b.get("task_id"))
            .and_then(|t| t.as_str())
            .map(String::from)
    };
    if let Some(given) = args.get("pane").or_else(|| args.get("seat")) {
        let task = task_of(&given);
        return Ok((Seat::new(given), task));
    }
    if args.rest.is_empty() {
        return Err("usage: wsp despawn <task> | wsp despawn --pane <seat>".into());
    }
    let work = resolve(store, args, index)?;
    let Some(task) = work.task else {
        return Err("despawn ends the agent on a task — a project is not a seat".into());
    };
    match store.panes_for_task(&task).first() {
        Some(seat) => Ok((Seat::new(seat.clone()), Some(task))),
        None => Err(match store.claims().contains_key(&task) {
            // The claim outlived the pane it was made in, which is what a herdr
            // restart does. Nothing here can say which seat holds it.
            true => format!(
                "{task} is claimed but no seat is bound to it — `wsp reconcile` to bind it again, \
                 or `wsp release --pane <seat>` if the agent is already gone"
            ),
            false => format!("nothing is working {task}"),
        }),
    }
}

/// `me` is which seat this process is standing in, passed in rather than read
/// here: the refusal below is the one behaviour of this verb that depends on the
/// environment, and a test that had to export `HERDR_PANE_ID` to reach it would
/// be changing a process-wide variable every other test can see — which is a
/// flake somebody else's test pays for. It has already happened once in this
/// tree, to `cmd_install`'s lock test, from the first draft of these tests.
fn end_work(
    place: &dyn Place,
    store: &Store,
    args: &Args,
    me: Option<&str>,
    tidy: &dyn Fn(&Seat, Option<&str>, Option<&str>) -> Leftovers,
) -> i32 {
    let p = Paint::new();
    let index = Index::new(store.projects());
    let (seat, task) = match seat_of(store, args, &index) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("wsp: {e}");
            return 2;
        }
    };

    // Ending the seat you are standing in kills this process partway through,
    // which is the one case where stopping first cannot work: there would be
    // nobody left to release the claim. Refused rather than reordered, because
    // an agent that wants to put its work down has a verb for that already, and
    // a loop that has resolved its own seat by accident wants to be told.
    if me == Some(seat.as_str()) {
        eprintln!("wsp: {seat} is this pane — `wsp release`, then leave");
        return 2;
    }

    // A governing pane is the one agent that cannot be restarted. Everything
    // else here is replaceable — despawn and respawn is the ordinary way to
    // clear a stuck agent — but a seat's whole value is the thread it has been
    // holding all session, and there is no verb that puts that back.
    //
    // A guard on this verb rather than a gate in front of the agents: nothing
    // under a seat waits on it, and this refusal costs only whoever pointed
    // despawn at the seat. Fail-open by construction — the record names the
    // pane the seat started in, so a seat whose agent has already been replaced
    // stops matching and this says nothing.
    if !args.has("force") {
        let governors = store.governors();
        let held: Vec<String> = governors
            .iter()
            .filter(|(_, rec)| rec.get("pane").and_then(|x| x.as_str()) == Some(seat.as_str()))
            .map(|(proj, _)| proj.clone())
            .collect();
        if !held.is_empty() {
            eprintln!("{} {seat} is the seat for {}", p.yellow("✗"), held.join(" "));
            eprintln!("  {}", p.dim("the thread it is holding does not come back — `wsp govern --clear` there first"));
            eprintln!("  {}", p.dim(&format!("wsp despawn --pane {seat} --force   to end it anyway")));
            return 1;
        }
    }

    // Asked before the seat is closed, because after it there is nothing left
    // to ask: the workspace is the id a build tree is keyed on, and a pane that
    // has gone cannot say which one it was in.
    let workspace = workspace_of(&seat);

    let closed = match place.stop(&seat) {
        Ok(()) => true,
        // Already gone. The first half of the verb is done, however it happened.
        Err(Refusal::NoSeat(_)) => false,
        Err(e) => {
            eprintln!("wsp: {seat} is still standing, so nothing was released: {e}");
            return 1;
        }
    };

    // The claim, through the one implementation of ending one. `release` writes
    // the `worked` record, the line in the task's log and the commit; a second
    // copy of that here would be a second contract.
    let (released, ended) = cmd_agent::release_pane(store, seat.as_str());
    // What the binding said, unless the release found something else there —
    // which it will not, and if it ever does, the release is the later reading.
    let task = ended.or(task);

    // Last, and only now: the tree is the one step a person can still do by
    // hand afterwards, so it must not be the step that stops the claim being
    // released. Everything before this line is what `despawn` has always done.
    let Leftovers { tree, builds } = tidy(&seat, task.as_deref(), workspace.as_deref());

    if args.json() {
        println!(
            "{}",
            json!({
                "seat": seat.as_str(),
                "closed": closed,
                "task": task,
                "released": released,
                "tree": match &tree {
                    Tree::Absent => serde_json::Value::Null,
                    Tree::Removed { path, branch_kept } =>
                        json!({ "removed": true, "path": path, "branch_kept": branch_kept }),
                    Tree::Kept { path, why } =>
                        json!({ "removed": false, "path": path, "why": why }),
                },
                "builds": builds,
            })
        );
    } else {
        println!("  {}", p.dim(&match closed {
            true => format!("ended {seat}"),
            false => format!("{seat} was already gone"),
        }));
        match (&task, released) {
            (Some(t), true) => println!("  {}", p.dim(&format!("released {t}"))),
            (Some(t), false) => println!("  {}", p.dim(&format!("{t} was not bound to it"))),
            (None, _) => println!("  {}", p.dim("it was holding nothing")),
        }
        match &tree {
            // Nothing at all, rather than "no tree": the common ending has no
            // checkout in it and a line saying so on every despawn is a line
            // every reader learns to skip, including on the despawns where the
            // next two matter.
            Tree::Absent => {}
            Tree::Removed { path, branch_kept } => {
                println!("  {}", p.dim(&format!("removed {path}")));
                if *branch_kept {
                    let name = task.as_deref().unwrap_or_default();
                    println!("  {}", p.yellow(&format!("branch {name} kept — it has commits the trunk has not")));
                }
            }
            // Not dim. This is the one line here that is somebody's to act on,
            // and the leak this verb exists to stop is exactly a cleanup step
            // that reported success it had not achieved.
            Tree::Kept { path, why } => println!("  {}", p.yellow(&format!("kept {path} — {why}"))),
        }
        if !builds.is_empty() {
            println!("  {}", p.dim(&format!("removed {} build tree(s) keyed on the workspace", builds.len())));
        }
    }
    0
}

/// `wsp claim <task> --pane <pane>`, called rather than shelled out to.
pub(crate) fn cmd_agent_claim(store: &Store, task: &str, flags: &[(&str, &str)]) -> i32 {
    crate::cmd_agent::claim(store, &Args::synth("claim", &[task], flags))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::place::Delivery;

    fn seat(tag: &str) -> Store {
        let root = std::env::temp_dir().join(format!("wsp-place-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = Store::at(root.clone(), root.join("state"));
        store.ensure_dirs().unwrap();
        store
    }

    /// Every way the tier can be wrong, and the fact that none of them costs a
    /// workspace.
    ///
    /// The three refusals are three different mistakes. A typo is the one the
    /// flag exists to catch early, because the alternative is an agent that
    /// starts, is refused its model by Claude Code in a pane nobody is watching
    /// and is reported here as never having become ready. A tier without
    /// `--agent` is a sentence about an agent that is never started, and
    /// dropping it silently is exactly the failure mode of the `--effort`
    /// warning this validation was written against. A tier on a kind wsp has no
    /// vocabulary for would be passed on by nobody: `Plain::args` sends
    /// nothing, so accepting it would be wsp saying it started `codex` on haiku
    /// and starting it on whatever codex defaults to.
    #[test]
    fn a_mistyped_tier_is_refused_before_anything_is_opened() {
        let none = |flags: &[(&str, &str)]| tier(&Args::synth("spawn", &["t-1"], flags), "claude", true);

        assert_eq!(none(&[]).unwrap(), (None, None), "no flag must send no argument at all");
        assert_eq!(
            none(&[("model", "opus[1m]"), ("effort", "low")]).unwrap(),
            (Some("opus[1m]".into()), Some("low".into())),
            "what was typed is what is passed on"
        );

        let err = none(&[("model", "opsu")]).unwrap_err();
        assert!(err.contains("opsu") && err.contains("sonnet"), "the typo and the list: {err}");
        let err = none(&[("model", "claude-opus-5")]).unwrap_err();
        assert!(err.contains("claude-opus-5"), "a full name is not an alias: {err}");
        let err = none(&[("effort", "hi")]).unwrap_err();
        assert!(err.contains("xhigh"), "an ignored effort is a session that lied: {err}");

        let err = tier(&Args::synth("spawn", &["t-1"], &[("model", "opus")]), "claude", false).unwrap_err();
        assert!(err.contains("--agent"), "a tier with nothing to start it: {err}");

        let err = tier(&Args::synth("spawn", &["t-1"], &[("model", "opus")]), "codex", true).unwrap_err();
        assert!(err.contains("claude"), "wsp does not know how codex spells a model: {err}");
    }

    /// A tier nobody can leave alone is refused for the spawn that would leave
    /// it alone, and allowed for the one that would not.
    ///
    /// Both halves, because a refusal with no way through is a dead end and the
    /// way through is the half that will be doubted. The measurement is on
    /// `Kind::unattended`; what is asserted here is only that a background spawn
    /// reads it and a focused one does not, and that the sentence a person gets
    /// names the tier they typed rather than the rule that caught it.
    #[test]
    fn a_tier_that_cannot_be_left_alone_is_refused_unless_somebody_is_going_to_the_pane() {
        let background = |m: &str| {
            tier(&Args::synth("spawn", &["t-1"], &[("model", m)]), "claude", true)
        };

        let err = background("haiku").unwrap_err();
        assert!(err.contains("haiku"), "the tier that was typed: {err}");
        assert!(err.contains("manual mode"), "and what is actually missing on it: {err}");
        assert!(err.contains("--focus") && err.contains("sonnet"), "and both ways on: {err}");

        assert!(background("haiku[1m]").is_err(), "a bigger window is the same tier");
        assert!(background("sonnet").is_ok(), "the tiers that hold their own are untouched");
        assert!(background("opus[1m]").is_ok(), "including with the suffix on");

        let flags = [("model", "haiku"), ("focus", "true")];
        let focused = tier(&Args::synth("spawn", &["t-1"], &flags), "claude", true);
        assert!(focused.is_ok(), "somebody is going to the pane: {focused:?}");
    }

    /// And the refusal happens before `place.open`, which is the whole reason
    /// it is read at the top of `place_work` rather than where the agent starts.
    ///
    /// A tier checked after the workspace exists has already cost a workspace, a
    /// claim and a worktree, and the person now has to `wsp despawn` before they
    /// can retype the word. `Opens` panics in `start`, so this also asserts
    /// nothing tried to run an agent at a tier that was refused.
    #[test]
    fn a_refused_tier_leaves_no_workspace_behind() {
        let _guard = no_backend();
        let store = seat("tier");
        store.save_project(&Project::new("robustness")).unwrap();

        let place = Opens(std::cell::RefCell::new(Vec::new()));
        let flags = [("project", "robustness"), ("agent", "true"), ("model", "opsu")];
        let code = place_work(&place, &store, &Args::synth("spawn", &[], &flags));

        assert_eq!(code, 2, "a mistyped tier is a usage error");
        assert!(place.0.borrow().is_empty(), "a workspace was opened for a spawn that cannot start");

        let _ = std::fs::remove_dir_all(&store.root);
    }

    /// A worklist is something to **govern** and not a place to work, and that
    /// is the whole of what `--govern` had to learn.
    ///
    /// `governors.json` is keyed on a scope now — a project id or a worklist
    /// slug — so `wsp spawn -p <slug> --govern` is the same command pointed at
    /// the other half of one key space, and it needs no flag of its own. What
    /// it must *not* do is write the slug into `project`: that field is what
    /// the pane is standing in, it fills `WSP_PROJECT` and resolves the cwd,
    /// and a custodian whose brief was about a project that does not exist is
    /// the near miss the guard above this refuses to make.
    #[test]
    fn a_worklist_is_something_to_govern_rather_than_a_place_to_work() {
        use crate::model::Worklist;
        let store = seat("govern-list");
        store.save_worklist(&Worklist::new("batch", "Overnight batch")).unwrap();
        store.save_project(&Project::new("robustness")).unwrap();
        let index = Index::new(store.projects());

        let named = |flags: &[(&str, &str)]| resolve(&store, &Args::synth("spawn", &[], flags), &index);
        let w = named(&[("project", "batch"), ("govern", "true")]).expect("a list is a scope");
        assert_eq!(w.list.as_deref(), Some("batch"));
        assert_eq!(w.project, None, "and not a project to stand in");

        // Bare, it is the same answer: the slug is a name `--govern` knows.
        let w = resolve(&store, &Args::synth("spawn", &["batch"], &[("govern", "true")]), &index)
            .expect("named without -p");
        assert_eq!(w.list.as_deref(), Some("batch"));

        // Without `--govern` it is not a spawn target at all, and the message
        // says so in the words of the thing that was asked for.
        let err = named(&[("project", "batch")]).err().expect("a list is not a place to work");
        assert!(err.contains("batch"), "{err}");
        // A project still resolves exactly as it did, `--govern` or not.
        for flags in [vec![("project", "robustness")], vec![("project", "robustness"), ("govern", "true")]] {
            let w = named(&flags).unwrap();
            assert_eq!(w.project.as_deref(), Some("robustness"));
            assert_eq!(w.list, None);
        }

        let _ = std::fs::remove_dir_all(&store.root);
    }

    /// Every way `--on` can be wrong, and the sentence each one earns.
    ///
    /// They are four different problems — a typo, a machine you retired, a
    /// machine that is down, and a daemon that is not running — and the fix for
    /// each is different, so one "cannot spawn on mb2" for all four would be
    /// the least useful thing this could say. The unreachable case carries what
    /// the daemon last saw, because that line is the whole answer to "why can I
    /// not spawn on mb2".
    #[test]
    fn saying_where_is_checked_before_anything_is_opened() {
        use crate::model::Machine;
        use crate::store::MachineLive;
        let store = seat("errs");
        let on = |v: &str| Args::synth("spawn", &["t-1"], &[("on", v)]);

        assert!(placement(&store, &Args::synth("spawn", &["t-1"], &[])).unwrap().is_none(),
            "no flag is this machine, which is what every existing caller passes");

        let err = placement(&store, &on("mb2")).unwrap_err();
        assert!(err.contains("this seat has none"), "{err}");

        store.save_machine(&Machine::new("mb2", "mb2")).unwrap();
        let err = placement(&store, &on("mb3")).unwrap_err();
        assert!(err.contains("there is mb2"), "a typo is told what there was: {err}");

        let err = placement(&store, &on("mb2")).unwrap_err();
        assert!(err.contains("is `wsp daemon` running"), "nothing has reported: {err}");

        store.set_machine_live("mb2", &MachineLive {
            reachable: false,
            error: "ssh: no route to host".into(),
            ..Default::default()
        });
        let err = placement(&store, &on("mb2")).unwrap_err();
        assert!(err.contains("no route to host"), "what the daemon saw: {err}");

        store.set_machine_live("mb2", &MachineLive { reachable: true, ..Default::default() });
        assert_eq!(placement(&store, &on("mb2")).unwrap().as_deref(), Some("mb2"));

        let mut retired = store.machine("mb2").unwrap();
        retired.status = "retired".into();
        store.save_machine(&retired).unwrap();
        let err = placement(&store, &on("mb2")).unwrap_err();
        assert!(err.contains("retired"), "{err}");

        let _ = std::fs::remove_dir_all(&store.root);
    }
    use crate::model::Project;

    /// What a seat is opened with, which is the whole of what `spawn` says to a
    /// backend before anything is running in it.
    ///
    /// `WSP_PROJECT` and `WSP_TASK` are the part that matters and the part
    /// nothing else can supply: herdr does not persist an environment across a
    /// restart, so this is exact for the life of the session and the claim is
    /// what is durable. A seat opened without them leaves every pane inside it
    /// inferring what it is for from a path.
    #[test]
    fn a_seat_is_opened_knowing_what_it_is_for() {
        let work = Work {
            task: Some("t-260817-004".into()),
            project: Some("robustness".into()),
            label: "robustness/004 · a title".into(),
            list: None,
        };
        let o = order(&work, Some("~/claude/wsp"), Some("mb2"), false);
        assert_eq!(o.label, "robustness/004 · a title");
        assert_eq!(o.cwd.as_deref(), Some("~/claude/wsp"), "expanded by the backend, not here");
        assert_eq!(o.on.as_deref(), Some("mb2"));
        // `show` is the one thing here that is scaffolding rather than shape:
        // until `arrange` has an implementor there is no spec to declare focus
        // into, so this is where `--focus` has to be said. See `Order::show`.
        assert!(!o.show, "--focus has nowhere else to be said yet");
        assert_eq!(o.env.get("WSP_TASK").map(String::as_str), Some("t-260817-004"));
        assert_eq!(o.env.get("WSP_PROJECT").map(String::as_str), Some("robustness"));

        // A project spawn has no task, and says so by absence rather than by an
        // empty string somebody downstream has to test for.
        let proj =
            Work { task: None, project: Some("robustness".into()), label: "robustness".into(), list: None };
        let o = order(&proj, None, None, true);
        assert!(o.env.get("WSP_TASK").is_none());
        assert!(o.on.is_none());
        assert!(o.show);
    }

    /// The other half of the same order: an agent spawned from inside an agent
    /// is a new session and inherits none of the spawning session's identity.
    ///
    /// Left in, `CLAUDE_CODE_CHILD_SESSION` tells the child it is somebody's
    /// sub-session and it saves no transcript — so it works, looks right, and
    /// leaves no record, which is worst exactly when somebody is measuring. The
    /// argument and the measurement are on `place::CHILD_MARKER`; what this
    /// asserts is that the strip is in the order rather than in a `env -u`
    /// incantation the caller has to remember.
    #[test]
    fn a_spawned_agent_is_not_handed_the_spawning_session() {
        let work = Work { task: None, project: None, label: "probe".into(), list: None };
        let o = order(&work, None, None, false);
        assert_eq!(
            o.env.get(crate::place::CHILD_MARKER).map(String::as_str),
            Some(""),
            "the spawned agent will save no transcript and say so in one truncated line"
        );
        for (k, v) in &o.env {
            assert!(
                !crate::place::shed(k) || v.is_empty(),
                "{k}={v} is the caller's session, handed to the seat"
            );
        }
    }

    /// The two sub-projects the backlog is split into have no checkout of
    /// their own — `wsp/render` and `wsp/data` are two halves of one tree. A
    /// spawn that read only a project's own `roots` put the agent wherever the
    /// caller happened to be standing, which for a panel is wherever it was
    /// installed.
    #[test]
    fn a_project_with_no_root_of_its_own_inherits_one() {
        let mut parent = Project::new("wsp");
        parent.roots = vec!["~/claude/wsp".into()];
        let mut child = Project::new("render");
        child.parent = Some("wsp".into());
        let mut orphan = Project::new("tooling");
        orphan.parent = Some("nowhere".into());

        let index = Index::new(vec![parent, child, orphan]);
        assert_eq!(index.root_of("wsp").as_deref(), Some("~/claude/wsp"));
        assert_eq!(index.root_of("render").as_deref(), Some("~/claude/wsp"));
        assert_eq!(index.root_of("tooling"), None, "a missing parent ends the walk");
        assert_eq!(index.root_of("nothing-here"), None);
    }

    /// A cycle in `parent` is a store anyone can write by hand, and `doctor`
    /// reports it rather than the walk hanging on it.
    #[test]
    fn a_parent_cycle_does_not_spin() {
        let mut a = Project::new("a");
        a.parent = Some("b".into());
        let mut b = Project::new("b");
        b.parent = Some("a".into());
        assert_eq!(Index::new(vec![a, b]).root_of("a"), None);
    }

    /// The sentence an agent is handed work with has one definition and two
    /// cases, and the case is decided by whether the hook has already run with
    /// this claim in place.
    ///
    /// A spawned agent's has: `spawn` claims before it starts the agent, so the
    /// payload is at the top of its context and asking for it again is a wasted
    /// round-trip — which at request 1 is a full context re-read, ~35K on the
    /// measurement that prompted this, against ~700 for the duplicated text.
    /// An agent the panel hands work to has been running since before the claim
    /// existed, so it genuinely has to fetch, and `--session` makes that one
    /// call rather than a dozen `wsp show`s.
    #[test]
    fn only_the_agent_whose_hook_missed_the_claim_is_asked_to_fetch() {
        let spawned = work_order("t-260815-033", Handover::Spawned);
        assert!(spawned.contains("t-260815-033"));
        assert!(!spawned.contains("wsp brief"), "the hook has already injected it: {spawned}");

        let running = work_order("t-260815-033", Handover::Running);
        assert!(running.contains("t-260815-033"));
        assert!(running.contains("wsp brief --session"), "the whole payload in one call: {running}");
    }

    /// The third case, and it is a different job rather than a different route
    /// to the same one.
    ///
    /// What it must not say is the thing every other work order says: pick up
    /// this task and finish it. A custodian that claims work is the failure the
    /// position was built out of — the seat on the night this came from
    /// borrowed t-260816-078 to have somewhere to stand, and every surface in
    /// wsp then described it as an agent working that task.
    #[test]
    fn a_custodian_is_told_the_job_rather_than_handed_a_task() {
        let text = work_order("robustness", Handover::Custodian);
        assert!(text.contains("custodian of the robustness project"), "{text}");
        assert!(!text.starts_with("You have been claimed"), "that is the other job: {text}");
        assert!(text.contains("should not claim"), "and it has to be said: {text}");
        // The four things the seat actually did, and the one it must not
        // become. A gate is the failure mode with the widest blast radius —
        // every agent under it waiting on a round-trip — so it is named.
        for owed in ["sequence", "direction", "review", "record", "authorise"] {
            assert!(text.contains(owed), "the work order drops `{owed}`: {text}");
        }
        // No fetch. `--govern` records the slot before the agent starts, so its
        // `SessionStart` hook has already run `wsp brief` with the slot in
        // place — asking again at request 1 is a whole context re-read.
        assert!(!text.contains("wsp brief"), "the hook has already injected it: {text}");
    }

    /// The work order is ASCII, and it is not a style rule.
    ///
    /// t-260817-004: a spawned agent sat with its work order typed into the
    /// input box and never submitted. The pane had the text and the agent had
    /// nothing to do, so `wsp wip` showed a healthy spawn doing no work — it
    /// fails open, which is the worst way for the loop's start verb to fail.
    /// The one thing that sentence had which every working one before it did
    /// not was an em-dash.
    ///
    /// Asserted over both cases and over the whole string rather than the one
    /// character, because the next well-meant curly quote or ellipsis costs
    /// another unattended spawn to find. Ed's call, 2026-08-17: do not use them.
    #[test]
    fn the_work_order_is_ascii_because_a_spawn_once_hung_on_one_character() {
        for how in [Handover::Spawned, Handover::Running, Handover::Custodian] {
            let text = work_order("t-260815-033", how);
            assert!(
                text.is_ascii(),
                "non-ASCII in a work order, which is what t-260817-004 was: {text}"
            );
        }
    }

    /// A backend reading off a script, one answer per poll, holding the last one
    /// for ever after.
    ///
    /// The script is the whole point: what killed t-260817-010 was a *sequence*
    /// of readings rather than any single one, and a fake that can only be put
    /// in a state cannot express "empty, and then not".
    ///
    /// It holds no clock. Time is [`util::Dial`]'s, wound by the wait's own
    /// `rest` and by nothing else — which is the honest model, because waiting
    /// is the only thing in a poll loop that takes any. [`STEP`] is what one
    /// poll costs.
    struct Reads {
        script: std::cell::RefCell<std::collections::VecDeque<crate::place::Result<State>>>,
        last: std::cell::RefCell<crate::place::Result<State>>,
    }

    /// What one poll costs on the tests' clock.
    const STEP: Duration = Duration::from_millis(1);

    impl Reads {
        fn of(script: Vec<crate::place::Result<State>>) -> Reads {
            Reads {
                last: std::cell::RefCell::new(
                    script.last().cloned().unwrap_or(Ok(State::Unknown)),
                ),
                script: std::cell::RefCell::new(script.into()),
            }
        }
    }

    impl Place for Reads {
        fn state(&self, _: &Seat) -> crate::place::Result<State> {
            match self.script.borrow_mut().pop_front() {
                Some(s) => s,
                None => self.last.borrow().clone(),
            }
        }
        fn open(&self, _: &Order) -> crate::place::Result<Seat> {
            panic!("waiting does not open seats")
        }
        fn start(&self, _: &Seat, _: &Agent) -> crate::place::Result<()> {
            panic!("waiting does not start agents")
        }
        fn tell(&self, _: &Seat, _: &str) -> crate::place::Result<Delivery> {
            panic!("waiting does not talk to agents")
        }
        fn stop(&self, _: &Seat) -> crate::place::Result<()> {
            panic!("waiting does not end seats")
        }
        fn census(&self) -> crate::place::Result<crate::place::Census> {
            panic!("waiting is about one seat")
        }
        fn watch(&self, _: &mut dyn FnMut(crate::place::Event) -> bool) -> crate::place::Result<()> {
            panic!("waiting does not subscribe")
        }
        fn here(&self) -> Option<Seat> {
            panic!("waiting is about a seat it was handed")
        }
    }

    /// A kind with a fixed answer about whether its agent is alive, which counts
    /// how often it was asked.
    ///
    /// Counted because the cost of asking is a subprocess: the rule is once per
    /// `gone` and not once per poll, and a rule about frequency is not checked
    /// by a test that only looks at the answer.
    struct Says(Option<bool>, std::cell::Cell<u32>);

    impl agent_commands::Kind for Says {
        fn running(&self, _: &agent_commands::Spawn) -> Option<bool> {
            self.1.set(self.1.get() + 1);
            self.0
        }
        fn args(&self, _: &agent_commands::Spawn) -> Vec<String> {
            Vec::new()
        }
        fn tier(&self, _: Option<&str>, _: Option<&str>) -> std::result::Result<(), String> {
            Ok(())
        }
        fn address(
            &self,
            _: &dyn Place,
            _: &agent_commands::Spawn,
        ) -> Option<agent_commands::Address> {
            None
        }
        fn tell(&self, _: &dyn Place, _: &Seat, _: &str) -> crate::place::Result<Delivery> {
            panic!("readiness does not deliver work orders")
        }
        fn ran(&self, _: &str, _: &str) -> Option<agent_commands::Ran> {
            None
        }
    }

    /// A grace of thirty polls and a deadline of two thousand, on a clock that
    /// only moves when the wait rests. Nothing here sleeps.
    const GRACE: Duration = Duration::from_millis(30);
    const READY: Duration = Duration::from_millis(2_000);
    /// Ten polls to start a turn in, and two presses. Small because the clock is
    /// the test's: what these buy is arithmetic a reader can check in their
    /// head, so a bound below is a statement about the loop rather than about
    /// the machine it ran on.
    const TAKEN: Duration = Duration::from_millis(10);
    const PRESSES: u32 = 2;

    fn waiting_on(
        place: &Reads,
        how: &dyn agent_commands::Kind,
        seat: &Seat,
        clock: &util::Dial,
    ) -> Result<(), String> {
        let spawn =
            agent_commands::Spawn { full: false, name: "t-260817-010", seat, model: None, effort: None, resume: None };
        let wait = Patience {
            ready: READY,
            taken: TAKEN,
            nudges: PRESSES,
            poll: STEP,
            gone: GRACE,
            clock,
        };
        wait_ready(place, how, &spawn, "claude", &wait)
    }

    /// **The failure this task is named for, at the moment it is decided.** A
    /// backend that cannot see a live agent must not end its spawn.
    ///
    /// The script is what was recorded on 2026-08-17: `start` returns, and the
    /// seat reads empty for a stretch because herdr's detection has not caught
    /// up with the agent sitting in it. The old rule returned on the first of
    /// those readings, so the work order was never sent and a claimed task sat
    /// in front of an idle agent until somebody noticed.
    ///
    /// Both halves of the new rule are asserted here: the wait survives the
    /// blind stretch, and the runtime is asked about it once per [`GRACE`] —
    /// not once per poll, because asking is a subprocess.
    ///
    /// The count is exact, and that it can be is the point. Asserted against the
    /// wall clock it was **flaky** — nine asks where eight were allowed,
    /// passing alone and failing under the full suite, because the throttle is
    /// per elapsed second and three other agents building beside it stretched
    /// two hundred polls over enough time for one more. The clock the wait reads
    /// is now a parameter and the wait's own `rest` drives it one [`STEP`] per
    /// poll, so the answer below is arithmetic: two hundred polls of one
    /// millisecond, asked every thirty, is six.
    #[test]
    fn a_seat_the_backend_cannot_see_does_not_end_a_spawn_whose_agent_is_alive() {
        let alive = Says(Some(true), std::cell::Cell::new(0));
        let dial = util::Dial::new();
        let place = Reads::of(
            std::iter::repeat_with(|| Ok(State::Empty))
                .take(200)
                .chain([Ok(State::Starting), Ok(State::Idle)])
                .collect(),
        );
        assert_eq!(waiting_on(&place, &alive, &Seat::new("w2C:p1"), &dial), Ok(()));
        assert_eq!(alive.1.get(), 6, "once per grace over two hundred polls, and no oftener");
    }

    /// And the other side of it: a spawn that really did fail still fails, and
    /// does not wait out the deadline to say so.
    ///
    /// This is what the fast verdict was for and why it is kept rather than
    /// replaced by patience. Thirty seconds of silence in front of a person who
    /// has just typed `wsp spawn` is its own kind of wrong answer.
    #[test]
    fn an_agent_that_never_started_is_reported_without_waiting_out_the_deadline() {
        let dead = Says(Some(false), std::cell::Cell::new(0));
        let dial = util::Dial::new();
        let place = Reads::of(vec![Ok(State::Empty)]);
        assert_eq!(
            waiting_on(&place, &dead, &Seat::new("w2C:p1"), &dial),
            Err("claude started and then stopped in w2C:p1".into())
        );
        assert_eq!(dead.1.get(), 1, "one question, asked once the grace was up");
        assert!(dial.elapsed() < READY, "waited out the whole deadline to say so");
        assert!(dial.elapsed() >= GRACE, "gave up before a launch has had time to finish");
        // A kind with no runtime to ask is not thereby immortal: the seat has
        // still been empty for longer than a launch takes.
        let mute = Says(None, std::cell::Cell::new(0));
        let place = Reads::of(vec![Ok(State::Empty)]);
        assert!(waiting_on(&place, &mute, &Seat::new("w2C:p1"), &util::Dial::new()).is_err());
    }

    /// One empty reading between two live ones is a gap in what the backend can
    /// see, and nothing is asked about it.
    ///
    /// The cheap half of the rule, and the one that does the most work: a single
    /// dropout costs nothing at all, so the expensive question is only reached by
    /// a seat that has looked empty for two seconds together.
    #[test]
    fn a_single_dropout_between_two_live_readings_is_not_worth_asking_about() {
        let never = Says(Some(false), std::cell::Cell::new(0));
        let place = Reads::of(vec![
            Ok(State::Starting),
            Ok(State::Empty),
            Err(Refusal::Unreachable("socket".into())),
            Ok(State::Starting),
            Ok(State::Idle),
        ]);
        assert_eq!(waiting_on(&place, &never, &Seat::new("w2C:p1"), &util::Dial::new()), Ok(()));
        assert_eq!(never.1.get(), 0, "a subprocess was run for a single blink");
    }

    /// A seat that is gone is not a seat whose agent is quiet, and no amount of
    /// patience will make a closed pane answer.
    #[test]
    fn a_pane_that_has_been_closed_is_said_plainly_and_at_once() {
        let alive = Says(Some(true), std::cell::Cell::new(0));
        let place = Reads::of(vec![Err(Refusal::NoSeat(Seat::new("w2C:p1")))]);
        assert_eq!(
            waiting_on(&place, &alive, &Seat::new("w2C:p1"), &util::Dial::new()),
            Err("w2C:p1 is gone".into())
        );
    }

    /// A backend that takes a work order and then does what the night of
    /// 2026-08-17 did: holds it in the composer until somebody presses return.
    ///
    /// `starts_after` is the number of presses it takes to start a turn —
    /// `Some(0)` is a healthy handover, `None` is the seat nothing rescues, and
    /// `Some(1)` is every failure Ed recovered by hand. The counter is the
    /// assertion that matters twice over: a loop that presses when it did not
    /// need to is typing into somebody's work, and one that presses for ever is
    /// the silence this task was filed about wearing a different face.
    struct Composer {
        starts_after: Option<u32>,
        can_press: bool,
        /// Whether this backend *watched* for the turn and can say it never
        /// started, which is what herdr's `agent.prompt` wait answers and what
        /// the port spells [`Refusal::NotTaken`].
        ///
        /// `false` is every backend that cannot, and the composer's silence has
        /// to be found by looking. The two answers describe the same seat and
        /// the handover should reach the same end from both — what differs is
        /// how long it spends finding out, which is what the test below is for.
        watches: bool,
        pressed: std::cell::Cell<u32>,
        heard: std::cell::RefCell<Vec<String>>,
    }

    impl Composer {
        fn of(starts_after: Option<u32>, can_press: bool) -> Composer {
            Composer {
                starts_after,
                can_press,
                watches: false,
                pressed: std::cell::Cell::new(0),
                heard: std::cell::RefCell::new(Vec::new()),
            }
        }

        fn watching(starts_after: Option<u32>) -> Composer {
            Composer { watches: true, ..Composer::of(starts_after, true) }
        }
    }

    impl Place for Composer {
        fn tell(&self, _: &Seat, text: &str) -> crate::place::Result<Delivery> {
            self.heard.borrow_mut().push(text.to_string());
            match self.watches && self.starts_after != Some(0) {
                true => Err(Refusal::NotTaken),
                false => Ok(Delivery::Started),
            }
        }
        fn nudge(&self, _: &Seat) -> crate::place::Result<()> {
            if !self.can_press {
                return Err(Refusal::Unsupported("press submit"));
            }
            self.pressed.set(self.pressed.get() + 1);
            Ok(())
        }
        fn state(&self, _: &Seat) -> crate::place::Result<State> {
            Ok(match self.starts_after {
                Some(n) if self.pressed.get() >= n => State::Working,
                // Idle, which is the whole of the defect: the agent is well,
                // it is waiting for input, and what it is waiting on is the
                // work order it was already given.
                _ => State::Idle,
            })
        }
        fn open(&self, _: &Order) -> crate::place::Result<Seat> {
            panic!("a handover does not open seats")
        }
        fn start(&self, _: &Seat, _: &Agent) -> crate::place::Result<()> {
            panic!("the agent is already running")
        }
        fn stop(&self, _: &Seat) -> crate::place::Result<()> {
            panic!("a handover that fails leaves the agent standing")
        }
        fn census(&self) -> crate::place::Result<crate::place::Census> {
            panic!("a handover is about one seat")
        }
        fn watch(&self, _: &mut dyn FnMut(crate::place::Event) -> bool) -> crate::place::Result<()> {
            panic!("a handover does not subscribe")
        }
        fn here(&self) -> Option<Seat> {
            panic!("a handover is about a seat it was handed")
        }
    }

    /// A kind that says its sentence through the backend, which is what
    /// `agent_commands::Claude` does and what makes the port's `tell` the one
    /// under test.
    struct Speaks;

    impl agent_commands::Kind for Speaks {
        fn tell(&self, place: &dyn Place, seat: &Seat, text: &str) -> crate::place::Result<Delivery> {
            place.tell(seat, text)
        }
        fn args(&self, _: &agent_commands::Spawn) -> Vec<String> {
            Vec::new()
        }
        fn tier(&self, _: Option<&str>, _: Option<&str>) -> std::result::Result<(), String> {
            Ok(())
        }
        fn address(
            &self,
            _: &dyn Place,
            _: &agent_commands::Spawn,
        ) -> Option<agent_commands::Address> {
            None
        }
        fn running(&self, _: &agent_commands::Spawn) -> Option<bool> {
            None
        }
        fn ran(&self, _: &str, _: &str) -> Option<agent_commands::Ran> {
            None
        }
    }

    fn handing_over(place: &Composer, clock: &util::Dial) -> Result<(), String> {
        let seat = Seat::new("w3M:p1");
        let spawn = agent_commands::Spawn {
            full: false,
            name: "robustness-035",
            seat: &seat,
            model: None,
            effort: None,
            resume: None,
        };
        let wait = Patience {
            ready: READY,
            taken: TAKEN,
            nudges: PRESSES,
            poll: STEP,
            gone: GRACE,
            clock,
        };
        hand_over(place, &Speaks, &spawn, "you have been claimed onto robustness-035", &wait)
    }

    /// The healthy handover, and the assertion is that nothing else happens: an
    /// agent that starts a turn is not typed at, and the sentence is not paid
    /// for twice.
    #[test]
    fn a_work_order_the_agent_starts_on_is_not_pressed_again() {
        let place = Composer::of(Some(0), true);
        let dial = util::Dial::new();
        assert_eq!(handing_over(&place, &dial), Ok(()));
        assert_eq!(place.pressed.get(), 0, "return was pressed at an agent already working");
        assert_eq!(place.heard.borrow().len(), 1, "the work order was sent more than once");
        assert_eq!(dial.elapsed(), Duration::ZERO, "a spawn that went well waited for it");
    }

    /// **The failure this task is named for, and the recovery that closes it.**
    ///
    /// The sentence arrives, the submit is swallowed, the agent sits idle in
    /// front of it. One press starts the turn — which is exactly what
    /// `herdr agent send-keys <pane> enter` did by hand, four times in one
    /// burst on 2026-08-17.
    ///
    /// The second assertion is the one that would be easy to lose: the retry is
    /// a *submit* and never a second `tell`. Sending the work order again would
    /// leave two copies of it in the composer, and the agent would read the
    /// duplicate as something it had been told twice.
    #[test]
    fn a_work_order_left_sitting_in_the_composer_is_submitted_again() {
        let place = Composer::of(Some(1), true);
        let dial = util::Dial::new();
        assert_eq!(handing_over(&place, &dial), Ok(()));
        assert_eq!(place.pressed.get(), 1, "one press was enough by hand and should be here");
        assert_eq!(place.heard.borrow().len(), 1, "the work order was sent twice over");
        assert_eq!(dial.elapsed(), TAKEN, "one window of patience before pressing, and one only");
    }

    /// And when nothing rescues it, the spawn fails and says so.
    ///
    /// A claimed task in front of an idle agent is what this used to report as
    /// success, so the assertion is on the `Err` rather than on the recovery:
    /// the caller prints it, exits non-zero, and an unattended queue learns that
    /// nothing has started. The press count is bounded arithmetic — three
    /// windows and two presses — because a loop that goes on pressing is the
    /// same silence with more typing in it.
    #[test]
    fn an_agent_that_never_takes_the_work_order_fails_the_spawn_rather_than_reporting_one() {
        let place = Composer::of(None, true);
        let dial = util::Dial::new();
        assert_eq!(
            handing_over(&place, &dial),
            Err("the work order is sitting in w3M:p1 unsent".into())
        );
        assert_eq!(place.pressed.get(), PRESSES, "the press loop is not bounded");
        assert_eq!(dial.elapsed(), TAKEN * (PRESSES + 1), "the wait is not bounded either");
    }

    /// A backend with nothing to press does not get pressed, and the spawn
    /// still fails.
    ///
    /// The second instance from the same night: `wsp spawn --on` into a session
    /// that was never logged in reported every line of success it has. There is
    /// no keystroke that fixes that one, and the point of the answer is that the
    /// caller stops rather than that it recovers — a turn that has not started
    /// is a spawn that has not happened, whatever the backend can or cannot do
    /// about it.
    #[test]
    fn a_backend_with_nothing_to_press_says_the_turn_never_started() {
        let place = Composer::of(None, false);
        let dial = util::Dial::new();
        assert_eq!(
            handing_over(&place, &dial),
            Err("w3M:p1 took the work order and started nothing".into())
        );
        assert_eq!(place.pressed.get(), 0, "something was pressed on a backend with no keys");
        assert_eq!(dial.elapsed(), TAKEN, "a backend that cannot be pressed was waited on twice");
    }

    /// **A backend that watched the turn never start is believed rather than
    /// polled, and it reaches the same end.**
    ///
    /// herdr's `agent.prompt` now holds its reply until the agent's status moves
    /// and refuses when it does not, so the five seconds this used to spend
    /// discovering that were being spent twice — once by the server, then again
    /// by wsp asking a question it had just been answered. The submit is pressed
    /// at once instead, and the clock is the assertion: nothing is waited for
    /// before the press, and what is waited for after it is the rescue.
    ///
    /// The recovery itself is untouched, and that is the point of pairing this
    /// with the test above rather than replacing it. herdr can say a prompt was
    /// delivered and the agent unmoved; it cannot say why, and a folder-trust
    /// modal holding the keyboard looks from its side exactly like a composer
    /// that was not ready. Only one of those is a thing a keystroke fixes, so
    /// the keystroke stays.
    #[test]
    fn a_backend_that_says_the_turn_never_started_is_not_asked_again_before_pressing() {
        let place = Composer::watching(Some(1));
        let dial = util::Dial::new();
        assert_eq!(handing_over(&place, &dial), Ok(()));
        assert_eq!(place.pressed.get(), 1, "the work order still needed its submit");
        assert_eq!(place.heard.borrow().len(), 1, "the work order was sent more than once");
        assert_eq!(dial.elapsed(), Duration::ZERO, "wsp waited for what it had already been told");
    }

    /// The same backend on a healthy handover: `Ok` from something that watched
    /// is a turn that started, and it costs one look and no waiting.
    #[test]
    fn a_handover_a_backend_confirmed_itself_costs_no_waiting() {
        let place = Composer::watching(Some(0));
        let dial = util::Dial::new();
        assert_eq!(handing_over(&place, &dial), Ok(()));
        assert_eq!(place.pressed.get(), 0, "return was pressed at an agent already working");
        assert_eq!(dial.elapsed(), Duration::ZERO, "a spawn that went well waited for it");
    }

    /// A watching backend whose agent nothing rescues still fails, and the
    /// bound is the presses rather than the answer that started it.
    #[test]
    fn a_turn_no_submit_starts_fails_the_spawn_whoever_noticed_first() {
        let place = Composer::watching(None);
        let dial = util::Dial::new();
        assert_eq!(
            handing_over(&place, &dial),
            Err("the work order is sitting in w3M:p1 unsent".into())
        );
        assert_eq!(place.pressed.get(), PRESSES, "the press loop is not bounded");
        assert_eq!(dial.elapsed(), TAKEN * PRESSES, "the look it was spared was not spared");
    }

    /// A backend that only remembers what it was asked to open.
    struct Opens(std::cell::RefCell<Vec<Order>>);

    impl Place for Opens {
        fn open(&self, order: &Order) -> crate::place::Result<Seat> {
            self.0.borrow_mut().push(order.clone());
            Ok(Seat::new("w9:p1"))
        }
        fn stop(&self, _: &Seat) -> crate::place::Result<()> {
            panic!("spawn does not end seats")
        }
        fn start(&self, _: &Seat, _: &Agent) -> crate::place::Result<()> {
            panic!("no agent was asked for")
        }
        fn tell(&self, _: &Seat, _: &str) -> crate::place::Result<Delivery> {
            panic!("no agent was asked for")
        }
        fn state(&self, _: &Seat) -> crate::place::Result<State> {
            panic!("spawn does not ask how the work is going")
        }
        fn census(&self) -> crate::place::Result<crate::place::Census> {
            panic!("spawn is about one seat")
        }
        fn watch(&self, _: &mut dyn FnMut(crate::place::Event) -> bool) -> crate::place::Result<()> {
            panic!("spawn does not wait for anything")
        }
        fn here(&self) -> Option<Seat> {
            panic!("spawn opens a seat rather than asking which one it is in")
        }
    }

    /// The whole of t-260815-022, at the one line where it happens.
    ///
    /// Every softer answer to two agents in one checkout has been tried in this
    /// repository and each failed the same way — an agent that has to remember
    /// a rule is halfway past it when it matters. So the tree is not something
    /// an agent asks for, it is where `spawn` opens the pane; and the assertion
    /// that matters is that a task seat is *not* opened at the project root.
    ///
    /// A project seat still is. It has nothing to branch for and nothing to
    /// land, and it is where somebody reading the whole tree wants to stand.
    #[test]
    fn a_task_seat_is_opened_in_a_checkout_of_its_own_and_a_project_seat_is_not() {
        let _guard = no_backend();
        let store = seat("tree");
        let root = store.root.join("repo");
        std::fs::create_dir_all(&root).unwrap();
        for argv in [
            vec!["init", "--quiet", "-b", "master"],
            vec!["commit", "--quiet", "--allow-empty", "-m", "first"],
        ] {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(&argv)
                .env_remove("GIT_INDEX_FILE")
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        }
        let mut proj = Project::new("robustness");
        proj.roots = vec![root.display().to_string()];
        store.save_project(&proj).unwrap();
        let mut task = crate::model::Task::new("one tree each", "t-1");
        task.project = Some("robustness".into());
        store.save_task(&task).unwrap();

        let opened = |args: Args| {
            let place = Opens(std::cell::RefCell::new(Vec::new()));
            place_work(&place, &store, &args);
            let cwd = place.0.borrow().first().and_then(|o| o.cwd.clone()).unwrap_or_default();
            cwd
        };

        let cwd = opened(Args::synth("spawn", &["t-1"], &[]));
        assert!(
            cwd.ends_with(".worktrees/t-1"),
            "a task seat was opened in the shared trunk: {cwd}"
        );
        assert!(std::path::Path::new(&cwd).join(".git").exists(), "{cwd} is not a checkout");

        let cwd = opened(Args::synth("spawn", &[], &[("project", "robustness")]));
        assert_eq!(
            util::real(&cwd),
            util::real(&root.display().to_string()),
            "a project seat left the trunk"
        );

        // `--govern` seats an agent on a project, and a task is not one. The
        // near miss is the expensive one: a claim on a piece of work plus a
        // sentence telling the agent it is answerable for everything above it,
        // which is the confusion the slot exists to end. Refused before
        // anything is opened, so a typo costs nothing.
        let place = Opens(std::cell::RefCell::new(Vec::new()));
        let code = place_work(
            &place,
            &store,
            &Args::synth("spawn", &["t-1"], &[("govern", "true")]),
        );
        assert_eq!(code, 2, "--govern on a task was accepted");
        assert!(place.0.borrow().is_empty(), "it opened a workspace before refusing");

        let _ = std::fs::remove_dir_all(&store.root);
    }

    /// A spawn asks for the screen only when somebody asked for it.
    ///
    /// *Asks*, and the distinction is load-bearing: the ask is honoured all the
    /// way down and the screen moves anyway, because the panel installed into
    /// the new workspace swaps a pane and `pane.swap` focuses unconditionally.
    /// That is `fork-002` and it cannot be tested here — this test owns what
    /// wsp requests, which is the only half wsp decides.
    ///
    /// This line read `!args.has("no-focus")` until 2026-08-17, so the screen
    /// moved onto every new seat unless the caller remembered to say otherwise —
    /// and `spawn` is run by the queue as much as by hand, which made a batch
    /// started overnight a sequence of jumps away from whatever was being read.
    /// herdr's `workspace.create` defaults `focus` to false and [`Order::show`]
    /// says the same, so the flip is wsp stopping opting in rather than a new
    /// rule. `--no-focus` still parses, and now names the default.
    #[test]
    fn a_spawn_leaves_the_screen_where_it_was_unless_asked() {
        let _guard = no_backend();
        let store = seat("focus");
        store.save_project(&Project::new("robustness")).unwrap();

        let shown = |flags: &[(&str, &str)]| {
            let mut f = vec![("project", "robustness")];
            f.extend_from_slice(flags);
            let place = Opens(std::cell::RefCell::new(Vec::new()));
            place_work(&place, &store, &Args::synth("spawn", &[], &f));
            let show = place.0.borrow().first().map(|o| o.show);
            show.expect("nothing was opened")
        };

        assert!(!shown(&[]), "a spawn with no opinion took the screen");
        assert!(shown(&[("focus", "true")]), "--focus asked and was not obeyed");
        assert!(!shown(&[("no-focus", "true")]), "--no-focus asks for what already happens");

        let _ = std::fs::remove_dir_all(&store.root);
    }

    /// A backend that answers `stop` however the test needs, and nothing else.
    ///
    /// In-process rather than the fake behind a socket, and for the reason the
    /// fake's own docs record: `stop` and the arrange port's close are the same
    /// herdr method, so a socket can see that a pane was taken away and cannot
    /// see which verb meant it. What is under test here is the *order* — which
    /// half ran, and what survived the other half failing — and that is a
    /// question about wsp, not about a wire.
    struct Ends {
        snub: Option<Refusal>,
        asked: std::cell::RefCell<Vec<Seat>>,
    }

    impl Ends {
        fn ok() -> Ends {
            Ends { snub: None, asked: std::cell::RefCell::new(Vec::new()) }
        }
        fn refusing(snub: Refusal) -> Ends {
            Ends { snub: Some(snub), asked: std::cell::RefCell::new(Vec::new()) }
        }
    }

    impl Place for Ends {
        fn stop(&self, seat: &Seat) -> crate::place::Result<()> {
            self.asked.borrow_mut().push(seat.clone());
            match &self.snub {
                Some(r) => Err(r.clone()),
                None => Ok(()),
            }
        }
        // Loudly rather than politely: a despawn that opened a seat or told an
        // agent something would be a defect these tests exist to notice.
        fn open(&self, _: &Order) -> crate::place::Result<Seat> {
            panic!("despawn does not open seats")
        }
        fn start(&self, _: &Seat, _: &Agent) -> crate::place::Result<()> {
            panic!("despawn does not start agents")
        }
        fn tell(&self, _: &Seat, _: &str) -> crate::place::Result<Delivery> {
            panic!("despawn does not talk to agents")
        }
        fn state(&self, _: &Seat) -> crate::place::Result<State> {
            panic!("despawn does not ask how the work is going")
        }
        fn census(&self) -> crate::place::Result<crate::place::Census> {
            panic!("despawn is about one seat")
        }
        fn watch(&self, _: &mut dyn FnMut(crate::place::Event) -> bool) -> crate::place::Result<()> {
            panic!("despawn does not wait for anything")
        }
        // Which seat this is arrives as an argument to `end_work`, deliberately:
        // see `a_despawn_will_not_end_the_seat_it_is_running_in`.
        fn here(&self) -> Option<Seat> {
            panic!("despawn is told which seat it is running in")
        }
    }

    /// A task somebody is working: the task, the binding that names the seat, and
    /// the claim the binding stands for.
    fn working(store: &Store, task: &str, seat: &str) {
        let mut t = crate::model::Task::new("stop, and the claim with it", task);
        t.project = Some("robustness".into());
        t.status_raw = "doing".into();
        store.save_task(&t).unwrap();
        store.set_binding(seat, json!({ "task_id": task, "pane_id": seat, "workspace_id": "w1" }));
        store.set_claim(
            task,
            json!({ "workspace_id": "w1", "workspace_label": "robustness/095", "cwd": "/tmp" }),
        );
    }

    /// Nothing in these tests may reach a herdr, and one of them would: `release`
    /// re-syncs on its way out, and this machine has a live herdr with real
    /// workspaces in it. A socket path that answers nothing makes every call fail
    /// at once rather than pushing a fixture's metadata onto somebody's pane.
    /// An empty store comes with it, on the same argument one step further out:
    /// a store these tests did not write is a store somebody else did, and
    /// `place_work` reads one — see [`crate::util::isolated`].
    fn no_backend() -> util::Isolated {
        let env = util::isolated("spawn-no-backend");
        std::env::set_var("HERDR_SOCKET_PATH", "/nonexistent/wsp-despawn-tests.sock");
        env
    }

    /// The tidying half, stubbed, and what each test asserts instead.
    ///
    /// [`swept_up`] is the only part of `despawn` that needs a git repository
    /// and a live herdr, and the tests below are about the *ordering* — stop,
    /// release, then tidy — and about what is reported. Reaching the real one
    /// here would run `git worktree remove` in whatever tree the test runner is
    /// standing in, which is somebody's actual work. What it did is recorded so
    /// the one thing worth asserting about it can be: that it ran last, and only
    /// on an ending that got that far.
    #[derive(Default)]
    struct Tidied(std::cell::RefCell<Vec<(String, Option<String>)>>);

    impl Tidied {
        fn f(&self) -> impl Fn(&Seat, Option<&str>, Option<&str>) -> Leftovers + '_ {
            |seat: &Seat, task: Option<&str>, _ws: Option<&str>| {
                self.0.borrow_mut().push((seat.as_str().to_string(), task.map(String::from)));
                Leftovers { tree: Tree::Absent, builds: Vec::new() }
            }
        }
    }

    /// **The decision this task was opened for.** A seat that will not close
    /// keeps its claim.
    ///
    /// The two failures are not comparable, which is why the order is not a
    /// matter of taste: work that looks unowned while an agent is still standing
    /// in it is handed to a second agent by the next `claim` — that guard reads
    /// bindings, so releasing first blinds it — and two agents in one tree is the
    /// failure this whole store is arranged against. A claim left over a closed
    /// seat is residue `reconcile --reap` already sweeps.
    #[test]
    fn a_seat_that_will_not_close_keeps_its_claim() {
        let _env = no_backend();
        let store = seat("stop-refused");
        working(&store, "t-260816-095", "w1:p1");

        let place = Ends::refusing(Refusal::Backend("pane is not going anywhere".into()));
        let tidied = Tidied::default();
        let code = end_work(&place, &store, &Args::synth("despawn", &["095"], &[]), None, &tidied.f());

        assert_eq!(code, 1, "a despawn that ended nothing must not report success");
        assert_eq!(place.asked.borrow().len(), 1, "it did try");
        assert!(store.claims().contains_key("t-260816-095"), "the claim went with the agent still there");
        assert!(store.bindings().contains_key("w1:p1"), "and so did the binding");
        assert!(tidied.0.borrow().is_empty(), "a seat still standing keeps its tree too");

        let _ = std::fs::remove_dir_all(&store.root);
    }

    /// The other half of the direction's question: ending a seat ends the
    /// **claim**, not only the binding.
    ///
    /// A pane exiting is an accident of process lifetime and leaves the intent
    /// standing; this is a decision, so it ends the way `release` does and leaves
    /// the same record — a `worked` row saying who had it and for how long, and a
    /// line in the task's log. The status stays `doing`, because work with nobody
    /// on it is a true and useful state.
    #[test]
    fn ending_a_seat_ends_the_claim_and_leaves_the_record_a_release_leaves() {
        let _env = no_backend();
        let store = seat("stop-ok");
        working(&store, "t-260816-095", "w1:p1");

        let place = Ends::ok();
        let tidied = Tidied::default();
        let code = end_work(&place, &store, &Args::synth("despawn", &["095"], &[]), None, &tidied.f());

        assert_eq!(code, 0);
        assert_eq!(place.asked.borrow().as_slice(), &[Seat::new("w1:p1")], "the bound seat");
        assert!(!store.claims().contains_key("t-260816-095"), "the claim outlived the seat");
        assert!(!store.bindings().contains_key("w1:p1"));
        assert!(store.worked().contains_key("t-260816-095"), "no trace of who had it");
        let t = store.task("t-260816-095").expect("the task");
        assert_eq!(t.status_raw, "doing", "work nobody is on is still work");
        assert!(t.body.contains("released"), "the log should say it was put down:\n{}", t.body);
        assert_eq!(
            tidied.0.borrow().as_slice(),
            &[("w1:p1".to_string(), Some("t-260816-095".to_string()))],
            "the tree is tidied last, and named by the task the release ended"
        );

        let _ = std::fs::remove_dir_all(&store.root);
    }

    /// A seat that had already gone still ends the claim, and this is the case
    /// the verb is most needed for.
    ///
    /// An agent whose backend crashed under it leaves exactly this: no seat, and
    /// a claim. Treating `NoSeat` as a failure would leave the one command that
    /// ends a claim unable to end the claims that most need ending — and the
    /// residue would be swept by a reaper instead, which is a person noticing
    /// later rather than a verb doing what it was asked.
    #[test]
    fn a_seat_that_was_already_gone_still_ends_the_claim() {
        let _env = no_backend();
        let store = seat("stop-gone");
        working(&store, "t-260816-095", "w1:p1");

        let place = Ends::refusing(Refusal::NoSeat(Seat::new("w1:p1")));
        let tidied = Tidied::default();
        let code = end_work(&place, &store, &Args::synth("despawn", &["095"], &[]), None, &tidied.f());

        assert_eq!(code, 0);
        assert!(!store.claims().contains_key("t-260816-095"));

        // A backend that did not answer is not the same sentence, and must not
        // release: silence is not evidence that the seat is gone.
        working(&store, "t-260816-094", "w1:p2");
        let quiet = Ends::refusing(Refusal::Unreachable("no socket".into()));
        let untidied = Tidied::default();
        assert_eq!(end_work(&quiet, &store, &Args::synth("despawn", &["094"], &[]), None, &untidied.f()), 1);
        assert!(untidied.0.borrow().is_empty(), "an ending that released nothing must not remove a tree");
        assert!(store.claims().contains_key("t-260816-094"), "released on a backend's silence");

        let _ = std::fs::remove_dir_all(&store.root);
    }

    /// Which seat a despawn is about, and the two ways there is not one.
    ///
    /// A claim names a workspace and a binding names a seat, so a task whose
    /// binding was lost — a herdr restart, and the claim is the half that
    /// survives — cannot be resolved to a seat by anything in the port. Said
    /// with the command that rebuilds the binding, rather than guessed at.
    #[test]
    fn a_task_whose_seat_is_unknown_is_told_what_would_find_it() {
        let _env = no_backend();
        let store = seat("stop-unbound");
        let index = Index::new(store.projects());
        working(&store, "t-260816-095", "w1:p1");

        let of = |a: &Args| seat_of(&store, a, &index);
        assert_eq!(
            of(&Args::synth("despawn", &["095"], &[])).unwrap(),
            (Seat::new("w1:p1"), Some("t-260816-095".into()))
        );
        // Named directly, the seat is whatever was said — including one no
        // binding knows about, which is how a stray agent is ended.
        assert_eq!(
            of(&Args::synth("despawn", &[], &[("pane", "w9:p9")])).unwrap(),
            (Seat::new("w9:p9"), None)
        );

        store.clear_binding("w1:p1");
        let err = of(&Args::synth("despawn", &["095"], &[])).unwrap_err();
        assert!(err.contains("wsp reconcile"), "the way back is not named: {err}");

        store.clear_claim("t-260816-095");
        let err = of(&Args::synth("despawn", &["095"], &[])).unwrap_err();
        assert!(err.contains("nothing is working"), "{err}");

        let err = of(&Args::synth("despawn", &[], &[])).unwrap_err();
        assert!(err.contains("usage"), "{err}");

        let _ = std::fs::remove_dir_all(&store.root);
    }

    /// An agent cannot despawn itself, and the refusal is the ordering decision
    /// showing its edge.
    ///
    /// Stopping first means the process dies partway through, before the claim is
    /// released — so the one seat this verb must not touch is the one it is
    /// running in. `wsp release` is the verb for putting your own work down, and
    /// saying so is more use than saying no.
    ///
    /// Which seat this is arrives as an argument, so this asserts the behaviour
    /// without exporting `HERDR_PANE_ID` for every other test in the process to
    /// trip over; see [`end_work`].
    #[test]
    fn a_despawn_will_not_end_the_seat_it_is_running_in() {
        let _env = no_backend();
        let store = seat("stop-self");
        working(&store, "t-260816-095", "w1:p1");

        let place = Ends::ok();
        let tidied = Tidied::default();
        let code = end_work(&place, &store, &Args::synth("despawn", &["095"], &[]), Some("w1:p1"), &tidied.f());

        assert_eq!(code, 2);
        assert!(place.asked.borrow().is_empty(), "it asked the backend to end this pane");
        assert!(store.claims().contains_key("t-260816-095"), "and it dropped its own claim");

        let _ = std::fs::remove_dir_all(&store.root);
    }

}
