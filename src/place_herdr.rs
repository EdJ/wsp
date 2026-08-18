//! herdr, behind the place-work port: the first backend, and the second
//! implementor is the fake.
//!
//! `place.rs` says what wsp needs from whatever runs its agents. This says how
//! herdr supplies it, and everything herdr makes hard lives in here rather than
//! at a call site — the shell race, the retype, the launch window, the two ids
//! of two lifetimes, the `@machine` suffix. A call site that used to know all of
//! that now says `place.start(&seat, &agent)`.
//!
//! # The seat is the pane id, and that is a debt rather than a design
//!
//! [`Place::open`] returns one handle and herdr has two ids: a workspace and a
//! pane. This adapter answers with the **pane id**, because that is what every
//! verb underneath takes — `agent.start` takes a `pane_id`, `agent.prompt` a
//! `target`, and `wsp claim --pane` takes the same string. A workspace-id seat
//! would have to be resolved back to its root pane on every call, and the claim
//! would have to be handed something else.
//!
//! **Both ids are durable, and that is not the reassurance it sounds like.**
//! Measured against upstream 0.8.0 on 2026-08-19 (`robustness-062`): across
//! three server restarts in one day herdr reissued neither. `session.json`
//! persists a workspace's `id` and its `public_pane_numbers` behind a
//! `next_public_pane_number` that only advances, and `w1:p6` has been `w1:p6`
//! since 2026-08-15. What *is* reissued is herdr's internal pane id, which
//! nothing here ever sees. So the older note in this file — that a pane id is
//! reissued on every restart — was wrong, and so is anything that inherited it.
//!
//! The catch is the reason [`Seat`] still wants corroborating rather than
//! trusting. The same restart **kills every agent process**: eleven panes for
//! eleven, respawned as fresh shells, each handed back its transcript by
//! `claude --resume`. So the handle outlives the thing behind it, and a seat id
//! that still resolves is not evidence that it names the same agent. The
//! durability [`Seat`] promises is therefore about corroboration and not about
//! the handle — `robustness-058`'s design, reached for the opposite reason from
//! the one written down here before.
//!
//! # herdr's name for the seat an occupant is in is `HERDR_PANE_ID`
//!
//! [`Place::here`] is the port's *which seat is this process in*, and this
//! adapter's whole answer is `HERDR_PANE_ID`. herdr sets it per pane, every
//! agent-side verb has always read it, and this is now the one place that knows
//! that — which is the point: the reading was at a call site
//! (`cmd_agent::my_pane`) and is behind the port.
//!
//! It reads rather than asks, and the alternative was measured rather than
//! assumed. `pane.current` is the only method that could answer *who am I*, and
//! it answers *what is focused*: called with no params from a shell in `w31:p1`
//! on 2026-08-17 against herdr 0.7.5, it replied `w1:pJ` — another workspace,
//! somebody else's work. Its one parameter is `caller_pane_id`, which is the
//! answer being asked for, so a socket connection carries no identity here at
//! all. `place.rs` has the rest of the argument, including why an id wsp minted
//! for itself would have been a translation table rather than a handle.
//!
//! **The `@machine` suffix is not added here and that is deliberate.** An id
//! arriving from an executor is already qualified: `executor/wsp` rewrites
//! `HERDR_PANE_ID` to `w0:p3@mb2` before forwarding it over ssh, because that
//! shim is the only thing that knows which machine it is standing on. A seat read
//! out of the environment on the seat machine itself is a local pane and bare,
//! exactly as [`Herdr::open`] returns one. So this is the same string wsp has
//! always used, and the suffix goes on staying `Order::on`'s business at `open`
//! and the shim's everywhere else.
//!
//! # The three things this fixes rather than ports
//!
//! Recorded against a live herdr 0.7.5 and a real Claude Code on 2026-08-17
//! (t-260816-080), and all three were latent because nothing called `place.rs`:
//!
//! 1. **`interactive_ready` is never `false`.** The port decided an agent was
//!    starting from `ready == Some(false)`, a reading herdr does not send: for
//!    3.3 seconds after `agent.start` the answer is `agent_status: "idle"` with
//!    `launch_pending: true` and *no* `interactive_ready` field, and then the
//!    two swap — and `launch_pending` goes *absent* rather than `false`, so the
//!    pair swaps by presence and not by value. Confirmed unchanged on upstream
//!    0.8.0, 2026-08-19: 199 samples over 12s, the swap at 3.0s, never `false`.
//!    Absence is the signal in both directions. [`state_of_agent`]
//!    reads what is sent, and takes a reply rather than three loose arguments so
//!    that no test can pin a shape herdr has never produced — which is how this
//!    survived being written down twice.
//! 2. **A listing can tell starting from idle, if it is the right listing.**
//!    `pane.list` rows have no readiness field in herdr's schema; `agent.list`
//!    carries the same `AgentInfo` as `agent.get` and was recorded carrying
//!    `interactive_ready: true`. So [`Herdr::census`] reads its seats from
//!    `pane.list` and its *states* from `agent.list`, and the port's asymmetry
//!    is between panes and agents rather than between one and many.
//! 3. **`agent.start` returns before the agent exists.** Its reply carries
//!    `launch_pending: true` and `agent_status: "unknown"`, and the pane does
//!    not yet report an agent *kind*. `Place::start` promises the caller that
//!    the agent exists when it returns, so [`Herdr::start`] waits for the kind
//!    — and that wait is where the retype lives.
//!
//!    Re-measured against upstream 0.8.0 on 2026-08-19 (`robustness-062`), and
//!    one half of the 0.7.5 reading no longer holds: the **name** is in
//!    `agent.start`'s own reply, immediately, so it is not the thing to wait
//!    for. What still arrives late is the detected kind, about 180ms behind.
//!    [`look`] reads `"agent"` and not `"name"`, so the loop below was already
//!    waiting on the right field; only the sentence describing it was wrong.
//!
//! # Ending a seat is `pane.close`, and the workspace goes with it
//!
//! herdr has no verb for *stop this agent*. Its eighty-nine methods can start
//! one, prompt one, rename one and release wsp's authority over one, and the
//! only thing that ends the process is taking its pane away. So
//! [`Place::stop`] is `pane.close` on the seat — the one call, on the one id the
//! port already holds, with no workspace to resolve first.
//!
//! Which is *not* what the two-command hand procedure did (`herdr workspace
//! close w26`), and the difference is measured rather than argued. Recorded
//! against a live herdr 0.7.5 in a sandbox session on 2026-08-17:
//!
//! - closing a pane kills what was running in it — a backgrounded child of the
//!   pane's shell went with it, so the whole group goes;
//! - closing the **last** pane of a workspace takes the workspace with it:
//!   `workspace.list` came back empty. A seat wsp opened is a workspace's root
//!   pane, so one `pane.close` is the whole of what two commands did by hand;
//! - closing a pane that has siblings leaves the workspace and the siblings
//!   standing. This is why the verb is not `workspace.close`: a seat is one
//!   place an agent runs, and a backend asked to end one must not take down the
//!   pane somebody else is reading beside it.
//!
//! `workspace_not_found` is therefore a code this file never has to read, and
//! [`refusal`] does not know it: the id in hand is always a pane's.
//!
//! # What the caller stopped having to know
//!
//! - **A brand-new pane has no shell.** `agent.start` ten milliseconds after
//!   `workspace.create` is refused with `agent_pane_busy`, "not an available
//!   shell", so the launch retries while that is the refusal and gives up on any
//!   other.
//! - **A shell that is not quite at a prompt eats what is typed at it.** The
//!   observed failure was ` mclaude`, `command not found`, and a minute of
//!   waiting for an agent that was never going to exist. So `ctrl-u` and one
//!   retype — but only while herdr can see no agent in the pane at all, because
//!   typing `claude` at a Claude Code that is merely still booting leaves the
//!   word in its input box for somebody to find later.
//! - **`available()` is gone.** A pre-flight question about a socket becomes
//!   [`Refusal::Unreachable`] from the call that wanted it, which arrives at the
//!   same moment the guard would have and says which call it stopped.
//!
//! # What is still herdr's shape and has not moved
//!
//! The `@machine` suffix. `place.rs` gives it to a `Remote` decorator that does
//! not exist yet, so [`Herdr::open`] qualifies the id it gets back exactly as
//! `cmd_spawn::open_workspace` used to, and every later call routes itself off
//! that suffix inside `herdr::call`. When `Remote` arrives, [`Order::on`] and
//! the two lines below it are the whole of what moves.

// [`Herdr::census`] and [`Herdr::watch`] have no caller yet — `sync` and the
// daemon are the next slice — so what only they use reads as dead. They are
// implemented and tested rather than left to the migration that needs them,
// because a trait method a backend has never had to answer is a guess.
#![allow(dead_code)]

use std::time::Duration;

use serde_json::{json, Value};

use crate::herdr;
use crate::place::{Agent, Event, Order, Place, Refusal, Result, Seat, Seated, State};
use crate::util::{self, Clock};

/// herdr as a place to put work.
///
/// The four durations are fields rather than constants because they are the
/// only interesting thing about `start`: every one of them was set by a failure,
/// and a test that had to sit through five real seconds to check the shell race
/// is a test nobody runs. The clock beside them is why a test no longer has to
/// sit through any of it. [`Herdr::new`] is what the CLI uses.
pub struct Herdr<'a> {
    /// How long a brand-new pane gets to grow a shell before `agent_pane_busy`
    /// stops being a race and starts being a failure.
    pub shell: Duration,
    /// How long to wait for the agent to appear at all, having typed its name.
    pub appear: Duration,
    /// When to decide the typing was eaten, clear the line and type again. Once.
    pub retype: Duration,
    /// How often to look while waiting.
    pub poll: Duration,
    /// What time it is, and how to wait for the next look.
    ///
    /// Shortening the four durations above made the tests fast; it did not make
    /// them honest, and `a_pane_with_no_shell_yet_is_retried_and_nothing_else_is`
    /// failed once in four `wsp verify` runs on a busy machine because a 400ms
    /// window and an 80ms sleep on another thread were racing the scheduler.
    /// [`util::Clock`] carries the argument; here it is enough that every wait
    /// below goes through this and nothing in this file reads the machine.
    pub clock: &'a dyn Clock,
}

impl Default for Herdr<'static> {
    fn default() -> Herdr<'static> {
        Herdr {
            // A cold Claude Code measured four seconds to readiness on this
            // machine; herdr's own default for the same wait is thirty.
            shell: Duration::from_millis(5_000),
            appear: Duration::from_millis(30_000),
            retype: Duration::from_millis(6_000),
            poll: Duration::from_millis(150),
            clock: &util::Wall,
        }
    }
}

impl Herdr<'static> {
    pub fn new() -> Herdr<'static> {
        Herdr::default()
    }
}

/// Where this machine's herdr listens, as the default for a machine whose
/// record does not say.
///
/// The mirrored-path assumption, and this is the one place it is made. It is
/// right for a Mac driving a Mac with the same username — which is what exists
/// — and wrong for the Linux box, which is why `Machine::backend_at` is
/// writable and why being wrong shows up as a tunnel that will not come up
/// rather than as anything subtler.
///
/// It lives here because interpreting a machine's backend address is the
/// adapter's job and nobody else's: `place.rs` carries the string, the model
/// stores it, and only this file knows that herdr's is a filesystem path and
/// that a second machine's is likely to be the same one as ours. It was in
/// `model.rs` until t-260816-064, where it was the durable entities reaching
/// for a concrete backend.
pub fn mirrored_socket() -> String {
    herdr::socket_path().to_string_lossy().into_owned()
}

/// The calls that wait on something outside herdr get longer than [`herdr::call`]'s
/// three seconds: `workspace.create` starts a shell, and `agent.prompt` is
/// answered by the agent rather than by the server.
const SLOW: Duration = Duration::from_secs(10);

/// The five herdr error codes wsp has ever had to tell apart.
///
/// Written down once and matched in this file only. `spawn` matching on
/// `agent_pane_busy` at a call site was the coupling that survived every
/// previous attempt at a seam: a caller that knows a backend's error strings has
/// not been decoupled from it whatever the signatures say.
const NOT_READY: &str = "agent_not_ready";
const NO_AGENT: &str = "agent_not_found";
const NO_PANE: &str = "pane_not_found";
const PANE_BUSY: &str = "agent_pane_busy";
const NOT_TAKEN: &str = "agent_prompt_stalled";

/// Every status herdr has a word for, which is how [`Herdr::tell`] asks its wait
/// for the one thing it wants and nothing more.
///
/// herdr's wait is two waits (`api/wait.rs:222`). The first is the one worth
/// having: it holds the reply until the agent's `state_change_seq` moves, and
/// that counter only moves on a real status change (`app/actions.rs:2973`), so
/// it is evidence that the agent *left idle* rather than that keystrokes were
/// written. The second then waits for a named status, and asking for one is how
/// you get a false failure — a Claude Code that answers a work order by asking
/// for a permission is `blocked` and never `working`, and a wait told to hold
/// out for `working` would report a prompt that was plainly taken as stalled.
/// Naming every status is what makes the second wait match at once and leaves
/// the first as the whole of it.
const ANY_STATUS: [&str; 5] = ["idle", "working", "blocked", "done", "unknown"];

/// How long herdr has to watch a prompt take effect before it says it did not.
///
/// Bounded on both sides and neither bound is taste. Above herdr's own 5s cap on
/// that watch, because the answer that tells a stalled prompt from an ordinary
/// timeout is only produced when this is above it (`api/wait.rs:238`) — asking
/// for five seconds gets the same wait and a reply wsp cannot read. Below
/// [`SLOW`], because wsp's socket read has to outlast the answer it is waiting
/// for, or a prompt herdr is about to report on comes back as no backend at all.
const TAKEN: u64 = 8_000;

/// herdr's failure, in wsp's words.
fn refusal(seat: &Seat, e: &std::io::Error) -> Refusal {
    let msg = e.to_string();
    if !herdr::answered(e) {
        return Refusal::Unreachable(msg);
    }
    if msg.contains(NOT_READY) {
        // herdr's own word for the launch window, so the state it names is the
        // one the window is.
        return Refusal::NotReady(State::Starting);
    }
    if msg.contains(NO_AGENT) || msg.contains(NO_PANE) {
        return Refusal::NoSeat(seat.clone());
    }
    if msg.contains(NOT_TAKEN) {
        // Only `agent.prompt` can answer this, and only when it was asked to
        // watch. It is herdr reporting the sentence delivered and the agent
        // unmoved, which is what the port calls not taken.
        return Refusal::NotTaken;
    }
    Refusal::Backend(msg)
}

/// The status alone, which is everything a reading without readiness has.
///
/// An empty `agent` is a seat with nothing in it — recorded, a pane with no
/// agent carries no `agent` field at all and answers `agent_status: "unknown"`.
/// herdr cannot distinguish that from an agent which has exited, which is why
/// [`State::Gone`] is raised from the event stream instead of from a reading.
fn of_status(agent: &str, status: &str) -> State {
    if agent.trim().is_empty() {
        return State::Empty;
    }
    match status.trim() {
        "working" => State::Working,
        "idle" => State::Idle,
        _ => State::Unknown,
    }
}

/// What one `AgentInfo` — `agent.get`, or a row of `agent.list` — says.
///
/// Takes the parsed record rather than three loose arguments, because the bug
/// this replaces was a function that could be handed a reading herdr never
/// sends, and was, by its own tests, twice. The readiness arms:
///
/// - `interactive_ready: true` — the agent will take a prompt and the status is
///   worth reading;
/// - `interactive_ready: false` — herdr has never been seen to send this. If it
///   ever does, false means not ready, which is [`State::Starting`];
/// - absent, with `launch_pending: true` — the launch window: three seconds of
///   looking exactly like an idle agent;
/// - absent, with no `launch_pending` either — a plugin-reported agent answers
///   this way, so the absence is not evidence of readiness in either direction.
///   [`State::Unknown`], and `will_take_a_prompt` says no to it, because an
///   absence is not a fact.
pub fn state_of_agent(p: &herdr::Pane) -> State {
    match of_status(&p.agent, &p.agent_status) {
        State::Empty => State::Empty,
        said => match (p.interactive_ready, p.launch_pending) {
            (Some(true), _) => said,
            (Some(false), _) | (None, Some(true)) => State::Starting,
            (None, _) => State::Unknown,
        },
    }
}

/// The same question of a row that carries no readiness — `pane.list`, and the
/// `pane` inside an event.
///
/// **This one cannot tell a starting agent from an idle one.** A pane row has no
/// `interactive_ready` in herdr's schema, and for the three seconds a Claude
/// Code takes to come up it reads `idle` like any other. That is `place.rs`'s
/// listing asymmetry, corrected: it is between *panes and agents*, not between
/// one row and many. `Working` off one of these is exact; `Idle` means "idle or
/// still coming up, and this row cannot say which", so the caller that must not
/// be lied to asks [`Place::state`] about the one seat.
fn state_of_pane(p: &herdr::Pane) -> State {
    of_status(&p.agent, &p.agent_status)
}

/// A `herdr::Pane` as a census row.
fn seated(p: &herdr::Pane, state: State) -> Seated {
    Seated {
        seat: Seat::new(&p.pane_id),
        label: p.label.clone(),
        cwd: p.cwd.clone(),
        agent: Agent { kind: p.agent.clone(), name: p.agent_name.clone(), args: Vec::new() },
        state,
        session: p.session_id.clone(),
    }
}

/// What `agent.get` says about a seat, or `None` where herdr says there is no
/// agent.
///
/// Recorded: a pane with no agent answers `agent_not_found`, and so does a pane
/// that does not exist. A caller cannot tell an empty seat from a missing one
/// this way, which is what [`Herdr::state`] spends a second call on.
fn look(seat: &Seat) -> Result<Option<herdr::Pane>> {
    match herdr::call("agent.get", json!({ "target": seat.as_str() })) {
        Ok(v) => Ok(v.get("agent").map(herdr::parse_pane)),
        Err(e) => match refusal(seat, &e) {
            Refusal::NoSeat(_) => Ok(None),
            other => Err(other),
        },
    }
}

/// Whether the seat itself is there, asked only when there is no agent in it.
fn exists(seat: &Seat) -> Result<bool> {
    match herdr::call("pane.get", json!({ "pane_id": seat.as_str() })) {
        Ok(_) => Ok(true),
        Err(e) => match refusal(seat, &e) {
            Refusal::NoSeat(_) => Ok(false),
            other => Err(other),
        },
    }
}

/// Type the agent's name at the pane's shell prompt, retrying the one refusal
/// that is a race rather than a failure.
fn launch(place: &Herdr, seat: &Seat, agent: &Agent) -> Result<()> {
    let mut params = json!({ "pane_id": seat.as_str(), "kind": agent.kind, "name": agent.name });
    if !agent.args.is_empty() {
        // Omitted entirely when there is nothing to say, so an untrimmed start
        // puts exactly the bytes on the socket that it always did.
        params["args"] = json!(agent.args);
    }
    let deadline = place.clock.now() + place.shell;
    loop {
        match herdr::call_for("agent.start", params.clone(), SLOW) {
            Ok(_) => return Ok(()),
            Err(e) => {
                if !e.to_string().contains(PANE_BUSY) || place.clock.now() >= deadline {
                    return Err(refusal(seat, &e));
                }
                place.clock.rest(place.poll);
            }
        }
    }
}

/// The event types this backend has to hear to keep "tell me when it stops".
///
/// `pane.agent_status_changed` is not here and must not be: it is per-pane, its
/// request requires a `pane_id`, and one entry asking for it globally refuses
/// the entire list. `pane.updated` is the only global carrier of a status
/// change and is also the noisiest thing herdr sends, which is a cost the daemon
/// declines today and this port cannot — [`Event::Moved`] has no other source.
///
/// `pane.exited` is deliberately absent: it is the pane's process ending rather
/// than the agent's, and the agent stopping already arrives exactly, as a
/// `pane.agent_detected` with its agent released.
const EVENTS: &[&str] = &["pane.created", "pane.closed", "pane.updated", "pane.agent_detected"];

/// One herdr event, as none, one or more port events.
fn events_of(name: &str, data: &Value) -> Vec<Event> {
    let pane = data.get("pane");
    let seat = |key: &str| -> Option<Seat> {
        let id = data
            .get(key)
            .and_then(|v| v.as_str())
            .or_else(|| pane?.get(key)?.as_str())
            .unwrap_or("");
        match id.is_empty() {
            true => None,
            false => Some(Seat::new(id)),
        }
    };
    let Some(seat) = seat("pane_id") else { return Vec::new() };
    match name {
        "pane_created" => vec![Event::Opened(seat)],
        "pane_closed" => vec![Event::Closed(seat)],
        // The agent field carries the answer in both directions: a name is an
        // agent that has appeared, and a null one is the agent that was there
        // having gone. This is the only reading that can say `Stopped` at all —
        // from a listing, an agent that has exited leaves a pane that looks like
        // a shell somebody opened.
        "pane_agent_detected" => {
            let gone = data.get("agent").map(|a| a.is_null()).unwrap_or(true)
                || data.get("released").and_then(|r| r.as_bool()).unwrap_or(false);
            vec![match gone {
                true => Event::Stopped(seat),
                false => Event::Started(seat),
            }]
        }
        "pane_updated" => {
            // A shell's output changing is not a state change. What this can say
            // about one that is, it says through a pane row — so `Idle` here
            // means idle or still coming up; see [`state_of_pane`].
            let Some(p) = pane.map(herdr::parse_pane) else { return Vec::new() };
            match p.agent.trim().is_empty() {
                true => Vec::new(),
                false => vec![Event::Moved(seat, state_of_pane(&p))],
            }
        }
        _ => Vec::new(),
    }
}

impl Place for Herdr<'_> {
    fn open(&self, order: &Order) -> Result<Seat> {
        let mut env = serde_json::Map::new();
        for (k, v) in &order.env {
            env.insert(k.clone(), Value::String(v.clone()));
        }
        let mut params = json!({ "label": order.label, "env": env, "focus": order.show });
        if let Some(c) = &order.cwd {
            // Expanded against *this* machine's home even when the work is
            // going somewhere else, which is deliberate and is t-260815-060's:
            // the machines mirror each other, and host-qualified roots are not
            // smuggled in here.
            params["cwd"] = json!(util::expand(c).display().to_string());
        }
        // The one call with nothing in it to route on — a seat that does not
        // exist yet has no id to say where it should be — so the machine is
        // named, and the id comes back qualified. An id that went into a claim
        // bare would name *this* machine's pane for as long as the claim lived.
        let r = herdr::call_on(order.on.as_deref(), "workspace.create", params, SLOW)
            .map_err(|e| refusal(&Seat::default(), &e))?;
        let bare = r
            .get("root_pane")
            .and_then(|p| p.get("pane_id"))
            .and_then(|x| x.as_str())
            .ok_or_else(|| Refusal::Backend("workspace.create returned no pane_id".into()))?;
        Ok(Seat::new(match &order.on {
            Some(m) => format!("{bare}@{m}"),
            None => bare.to_string(),
        }))
    }

    /// `HERDR_PANE_ID`, which herdr put there when it made the pane.
    ///
    /// Through [`herdr::Env`] rather than [`std::env`] because that is where the
    /// plugin fallback lives: a wsp run as a herdr plugin command has no pane
    /// variable and gets its pane out of `HERDR_PLUGIN_EVENT_JSON` instead. Both
    /// are herdr telling us where we are stood, and the difference is herdr's.
    ///
    /// [`crate::place::seat_from_env`] is not consulted, even as a fallback. Two
    /// answers to *which seat is this* is a way to be in two seats: under herdr a
    /// stale `WSP_SEAT_ID` inherited from somewhere would name a pane this
    /// backend never issued, and `wsp claim` would bind a task to it in silence.
    fn here(&self) -> Option<Seat> {
        herdr::Env::read().pane_id.filter(|s| !s.is_empty()).map(Seat::new)
    }

    /// Returns when the agent exists, which herdr does not tell us and has to be
    /// waited for. Not when it will take a prompt: those are different moments,
    /// three seconds apart, and conflating them is the `agent_not_ready` bug.
    ///
    /// **What "exists" means is the whole of t-260817-010, and it was wrong
    /// here.** This waited for `agent.get` to answer with a record at all, and
    /// during the launch window it answers with a record that names nothing:
    /// `launch_pending: true`, `agent_status: "unknown"`, and no `agent` field —
    /// recorded 70ms after `agent.start` in a sandbox on 2026-08-17, with the
    /// name arriving at 620ms. `parse_pane` reads the missing field as `""`, so
    /// [`state_of_agent`] calls that record [`State::Empty`], and a caller that
    /// polls readiness the moment this returns is told the agent it just started
    /// has stopped. Three of three sandbox spawns failed that way in under two
    /// and a half seconds, with a live Claude Code in the pane.
    ///
    /// So the wait is for a **named** agent, which is what the promise above
    /// always meant. The distinction is not decoration: an empty `agent` field is
    /// also what a plain shell looks like, and the two are told apart by nothing
    /// but `launch_pending`.
    ///
    /// Which is also why the retype is now held off while that flag is set.
    /// Before, this returned before the retype could ever apply to the launch
    /// window; now that it waits through it, clearing the line and typing
    /// `claude` again at six seconds would be typing into a Claude Code that
    /// herdr has told us it is in the middle of starting — the exact harm the
    /// retype's own rule was written to avoid. `launch_pending` is herdr saying
    /// *I have one coming up*, so the eaten-keystroke case it exists for is the
    /// case where herdr says nothing of the sort.
    fn start(&self, seat: &Seat, agent: &Agent) -> Result<()> {
        launch(self, seat, agent)?;
        let began = self.clock.now();
        let mut retyped = false;
        loop {
            let seen = look(seat)?;
            if seen.as_ref().is_some_and(|p| !p.agent.trim().is_empty()) {
                return Ok(());
            }
            let waited = self.clock.now().saturating_duration_since(began);
            if waited >= self.appear {
                return Err(Refusal::Backend(format!(
                    "nothing that looks like {} appeared in {seat}",
                    agent.kind
                )));
            }
            let coming_up = seen.and_then(|p| p.launch_pending).unwrap_or(false);
            if !retyped && !coming_up && waited >= self.retype {
                retyped = true;
                let _ = herdr::call(
                    "pane.send_text",
                    json!({ "pane_id": seat.as_str(), "text": "\x15" }),
                );
                launch(self, seat, agent)?;
            }
            self.clock.rest(self.poll);
        }
    }

    /// `agent.prompt`, and — unless the agent is already working — herdr's own
    /// `wait` on it.
    ///
    /// **The field closes robustness-035 at the only place it could ever have
    /// been closed.** Without it `agent.prompt` writes the text, schedules the
    /// Enter 300ms later and answers `ok` before that Enter exists
    /// (`app/api/agents.rs:105`), so `Ok` meant the server had taken the call and
    /// nothing about the agent. With it the server holds the reply until the
    /// agent's status moves, and says `agent_prompt_stalled` when it does not.
    /// That is the reading wsp used to take for itself, taken by the only
    /// process that can take it without a race.
    ///
    /// **The gate is not an optimisation.** An agent that is already working is
    /// the case herdr's wait cannot answer: it skips the effect watch entirely
    /// and goes to the second wait, which demands a status change — and a
    /// sentence queued behind a turn in progress causes none, so the reply would
    /// be held for the whole of [`TAKEN`] and then report a failure for a message
    /// that was delivered. That is not a corner: it is `wsp govern --tell`, which
    /// exists to speak to a governor in the middle of a night's sequencing, and
    /// an unconditional wait would turn every one of those into eight seconds of
    /// silence ending in a lie. So the wait is asked for only where its evidence
    /// means something — a prompt into an agent that was not doing anything, on
    /// the grounds that such an agent starting to do something is the proof.
    ///
    /// The extra `agent.get` that gate costs is one round trip on a local socket
    /// against a call that is about to wait seconds, and there is no way to ask
    /// for the first wait without it: what to send depends on the state, and only
    /// herdr knows the state.
    fn tell(&self, seat: &Seat, text: &str) -> Result<()> {
        let mut params = json!({ "target": seat.as_str(), "text": text });
        if !matches!(self.state(seat), Ok(State::Working)) {
            params["wait"] = json!({ "until": ANY_STATUS, "timeout_ms": TAKEN });
        }
        herdr::call_for("agent.prompt", params, SLOW).map(|_| ()).map_err(|e| refusal(seat, &e))
    }

    /// `agent.send_keys enter`, which is the recovery that was run by hand every
    /// time this failed.
    ///
    /// Addressed to the agent rather than to the pane, and that is the whole
    /// choice here: `pane.send_text "\r"` would type a return at whatever is in
    /// the pane now, and the case this is reached in is one where wsp's picture
    /// of the seat has already been shown to be optimistic. An agent target that
    /// no longer names an agent is refused by herdr, which is the answer wanted.
    fn nudge(&self, seat: &Seat) -> Result<()> {
        herdr::call("agent.send_keys", json!({ "target": seat.as_str(), "keys": ["enter"] }))
            .map(|_| ())
            .map_err(|e| refusal(seat, &e))
    }

    /// Take the seat's pane away, which is the only thing herdr has that ends
    /// an agent — and, when the seat is a workspace's last pane, ends the
    /// workspace too. See the module docs for what was measured.
    ///
    /// [`herdr::call`]'s three seconds rather than [`SLOW`]: the two calls that
    /// wait longer wait on something outside herdr — a shell coming up, an agent
    /// answering a prompt — and this waits on herdr killing a process it owns.
    /// Recorded answering immediately.
    fn stop(&self, seat: &Seat) -> Result<()> {
        herdr::call("pane.close", json!({ "pane_id": seat.as_str() }))
            .map(|_| ())
            .map_err(|e| refusal(seat, &e))
    }

    fn state(&self, seat: &Seat) -> Result<State> {
        match look(seat)? {
            Some(a) => Ok(state_of_agent(&a)),
            // No agent, which herdr spells the same way as no pane. The second
            // call is what tells a seat somebody is sitting in from one that has
            // been closed under us.
            None => match exists(seat)? {
                true => Ok(State::Empty),
                false => Err(Refusal::NoSeat(seat.clone())),
            },
        }
    }

    /// Seats from `pane.list`, states from `agent.list`.
    ///
    /// Two calls because neither answers the question alone: `pane.list` is the
    /// only one that knows about a seat with nobody in it, and `agent.list` is
    /// the only one that can tell a starting agent from an idle one. Both fan
    /// out across machines already, so this is a census of everywhere.
    ///
    /// An error is not an empty list, and this returns the error — the rule is
    /// older than the port (`sync.rs:41`) and outlives it.
    fn census(&self) -> Result<Vec<Seated>> {
        let panes = herdr::panes().map_err(|e| refusal(&Seat::default(), &e))?;
        let agents = herdr::agents().map_err(|e| refusal(&Seat::default(), &e))?;
        Ok(panes
            .iter()
            .map(|p| match agents.iter().find(|a| a.pane_id == p.pane_id) {
                Some(a) => seated(a, state_of_agent(a)),
                None => seated(p, state_of_pane(p)),
            })
            .collect())
    }

    /// This machine's stream. A `Remote` decorator is what will hold one per
    /// machine, exactly as the daemon does today — herdr has no concept of a
    /// host, so there is no fan-out to do here.
    fn watch(&self, f: &mut dyn FnMut(Event) -> bool) -> Result<()> {
        herdr::subscribe(EVENTS, |name, data| {
            events_of(name, data).into_iter().all(|e| f(e))
        })
        .map_err(|e| refusal(&Seat::default(), &e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{Fake, Quiet, Snub, Spot, Stage, Verb};
    use crate::util::Dial;

    /// A fake bound at the socket wsp's own client reads, so every test below
    /// goes over the wire rather than round it — and an empty store beside it,
    /// because `census` goes through `herdr::panes`, which fans out over the
    /// machines the store names. Held for as long as the guard is: see
    /// [`crate::util::isolated`].
    fn bound(name: &str, stage: Stage) -> (Fake, util::Isolated) {
        let env = util::isolated(&format!("adapter-{name}"));
        let fake = Fake::bind(env.path("herdr.sock"), stage).unwrap();
        let (k, v) = fake.socket_env();
        std::env::set_var(k, v);
        (fake, env)
    }

    /// The adapter with its four windows shortened and a clock the test winds.
    ///
    /// The numbers are a hundredth of the real ones because the durations they
    /// stand in for were chosen against a live herdr, and reading `400` next to
    /// `5_000` says *this is that window, scaled* in a way a made-up `40` would
    /// not. They cost nothing either way now: nothing below sleeps, so these are
    /// arithmetic rather than a bill.
    fn brisk<'a, 'b: 'a>(clock: &'a Dial<'b>) -> Herdr<'a> {
        Herdr { shell: Duration::from_millis(400), retype: Duration::from_millis(150),
                appear: Duration::from_millis(800), poll: Duration::from_millis(20),
                clock }
    }

    /// How many polls a window of this length allows, and therefore how many
    /// asks it costs: the first one is free, and each one after it is bought
    /// with a poll's wait.
    fn asks_in(window: Duration, poll: Duration) -> usize {
        1 + (window.as_millis() / poll.as_millis()) as usize
    }

    /// How often the fake was asked to do something.
    fn times(fake: &Fake, verb: Verb) -> usize {
        fake.verbs().iter().filter(|v| **v == verb).count()
    }

    /// How long to wait for an event that has already been pushed down a socket
    /// on this machine.
    ///
    /// A hang-guard rather than a measurement, and generous on purpose: it is
    /// only ever paid by a test that is already failing, so there is nothing to
    /// buy by keeping it tight and a flake to buy by it (robustness-054).
    const DELIVERY: Duration = Duration::from_secs(10);

    /// `spawn`'s order, driven end to end through the port: open a seat, start
    /// an agent, find it will not take a prompt yet, and only then tell it
    /// something.
    ///
    /// The recording is the assertion and it is in wsp's words, which is the
    /// whole argument for the seam: this test says nothing about
    /// `workspace.create` and would go on passing against a backend that has
    /// never heard of a workspace.
    #[test]
    fn the_order_a_spawn_places_is_open_start_wait_tell() {
        let mut stage = Stage::new();
        // The window a live herdr passes through in half a second whether you
        // wanted it to or not.
        stage.settle = false;
        let (fake, _env) = bound("order", stage);
        let dial = Dial::new();
        let place = brisk(&dial);

        let seat = place
            .open(&Order {
                label: "robustness/061".into(),
                cwd: Some("/tmp".into()),
                show: true,
                ..Order::default()
            })
            .expect("a seat");
        place.start(&seat, &Agent { kind: "claude".into(), name: "t-1".into(), args: Vec::new() })
            .expect("an agent");

        assert_eq!(place.state(&seat).unwrap(), State::Starting, "the launch window");
        assert!(!State::Starting.will_take_a_prompt());
        assert_eq!(
            place.tell(&seat, "go"),
            Err(Refusal::NotReady(State::Starting)),
            "herdr's agent_not_ready, in the port's words"
        );

        fake.moves(&seat, State::Idle);
        assert_eq!(place.state(&seat).unwrap(), State::Idle);
        place.tell(&seat, "go").expect("now it takes one");

        let told = fake.asked().into_iter().find(|a| a.verb == Verb::Tell && a.said == "go");
        assert_eq!(told.map(|a| a.seat), Some(Some(seat.clone())), "the sentence reached the seat");
        assert!(fake.verbs().starts_with(&[Verb::Open, Verb::Start]), "{:?}", fake.verbs());

    }

    /// A seat with an idle agent in it, which is what both tests below start
    /// from and neither is about arranging.
    fn seated(fake: &Fake, place: &Herdr) -> Seat {
        let seat = place.open(&Order { label: "fork/015".into(), ..Order::default() })
            .expect("a seat");
        place.start(&seat, &Agent { kind: "claude".into(), name: "t-1".into(), args: Vec::new() })
            .expect("an agent");
        fake.moves(&seat, State::Idle);
        seat
    }

    /// **The trap in asking herdr to wait, and the one path it would have taken
    /// down every night.**
    ///
    /// `wsp govern --tell` speaks to a governor in the middle of a night's
    /// sequencing, and a sentence given to an agent that is already working is
    /// queued behind the turn in progress. It is delivered, it is correct, and
    /// nothing about the agent changes — so a wait that is watching for a change
    /// watches for something that cannot happen, holds the reply for its whole
    /// timeout, and then reports a failure for a message that arrived. Eight
    /// seconds of silence ending in a lie, on the wire every agent on the machine
    /// is directed with.
    ///
    /// The fake refuses that combination outright, so this passes only while wsp
    /// keeps the wait for the case whose evidence means something: an agent that
    /// was not doing anything and starts.
    #[test]
    fn a_message_to_an_agent_that_is_already_working_is_not_waited_on() {
        let (fake, _env) = bound("busy", Stage::default());
        let dial = Dial::new();
        let place = brisk(&dial);
        let seat = seated(&fake, &place);

        fake.moves(&seat, State::Working);
        assert_eq!(place.state(&seat).unwrap(), State::Working, "mid-turn, as a governor is");

        place.tell(&seat, "stand down at eight").expect("a queued sentence is delivered");
        let told = fake.asked().into_iter().find(|a| a.verb == Verb::Tell);
        assert_eq!(told.map(|a| a.said), Some("stand down at eight".into()), "it reached the seat");
    }

    /// The other half: a prompt into an idle agent *is* waited on, and a wait
    /// that comes back empty is [`Refusal::NotTaken`] rather than a success.
    ///
    /// This is robustness-035 arriving as an answer. The same stage — a socket
    /// that accepts the call and leaves the sentence in the composer — used to be
    /// indistinguishable from a healthy handover at this layer, and every reading
    /// that found it out had to be taken by wsp afterwards.
    #[test]
    fn a_prompt_the_agent_never_acts_on_comes_back_as_not_taken() {
        let mut stage = Stage::default();
        stage.takes = false;
        let (fake, _env) = bound("unsent", stage);
        let dial = Dial::new();
        let place = brisk(&dial);
        let seat = seated(&fake, &place);

        assert_eq!(place.tell(&seat, "go"), Err(Refusal::NotTaken));
        assert_eq!(place.state(&seat).unwrap(), State::Idle, "idle in front of its own work order");
    }

    /// **t-260817-010.** An agent that has been started and not yet detected is
    /// not a seat that emptied, and `start` must not come back until the
    /// difference can be seen.
    ///
    /// The failure this pins was not subtle once it was found and was invisible
    /// until then, because both halves are honest on their own: `agent.get`
    /// answers with a record the moment the launch begins, and that record names
    /// nothing. `start` waited for *a record*, so it returned inside the window;
    /// `cmd_spawn::wait_ready` then read the same record as [`State::Empty`] and
    /// reported that a live Claude Code had started and stopped. Three of three
    /// sandbox spawns died that way in under two and a half seconds on
    /// 2026-08-17, with the agent sitting idle in the pane the whole time.
    ///
    /// So the assertion is the one thing the caller is entitled to: from the
    /// moment `start` returns, the seat never reads empty. `Stage::unnamed`
    /// carries the recording that makes this a real window rather than a
    /// hypothesis — and with it at nought, which is what the fake used to do,
    /// this test passes against the broken code.
    #[test]
    fn an_agent_that_is_not_detected_yet_is_never_read_as_a_seat_that_emptied() {
        let mut stage = Stage::new();
        stage.settle = false;
        let (fake, _env) = bound("launching", stage);
        let dial = Dial::new();
        let place = brisk(&dial);

        let seat = place.open(&Order { label: "robustness/010".into(), ..Order::default() })
            .expect("a seat");
        // While the launch window is open the seat reads empty, and that is
        // herdr's answer rather than the fake's invention: a record with no
        // agent named in it.
        place.start(&seat, &Agent { kind: "claude".into(), name: "t-1".into(), args: Vec::new() })
            .expect("an agent");
        assert_eq!(
            place.state(&seat).unwrap(),
            State::Starting,
            "start came back before the agent was detectable"
        );
        assert!(fake.stage().unnamed > 0, "a stage that skips the window cannot catch this");

    }

    /// Ending a seat, and ending one that has already gone.
    ///
    /// The verb is one call and the interesting half is the second one: an agent
    /// whose backend crashed under it is the ordinary case for `stop`, and the
    /// caller has to be able to tell "there was nothing there" from "the backend
    /// said no" — because it releases the claim on the first and must not on the
    /// second. herdr spells both `pane_not_found`, which is the word [`refusal`]
    /// already turns into [`Refusal::NoSeat`].
    ///
    /// What a socket cannot check is asserted in prose instead: that closing a
    /// pane kills what was running in it, and takes the workspace with it when it
    /// was the only pane, is recorded in the module docs from a live herdr — the
    /// fake has no processes to kill.
    #[test]
    fn a_seat_that_is_ended_is_gone_and_ending_it_twice_is_not_a_failure() {
        let (fake, _env) = bound(
            "stop",
            Stage::of(vec![
                Spot::agent("w1:p1", "claude", "t-1", State::Idle).labelled("robustness/095"),
                Spot::agent("w2:p1", "claude", "t-2", State::Working).labelled("robustness/094"),
            ]),
        );
        let dial = Dial::new();
        let place = brisk(&dial);
        let seat = Seat::new("w1:p1");

        place.stop(&seat).expect("the seat was there");
        assert!(fake.stage().find(&seat).is_none(), "the seat outlived its stop");
        // Somebody else's seat is not swept up with it. On herdr this is why the
        // verb is `pane.close` and not `workspace.close`.
        assert_eq!(place.state(&Seat::new("w2:p1")).unwrap(), State::Working);

        assert_eq!(place.stop(&seat), Err(Refusal::NoSeat(seat.clone())), "already gone");
        assert_eq!(place.state(&seat), Err(Refusal::NoSeat(seat)));

    }

    /// The bug this adapter exists to stop porting faithfully.
    ///
    /// herdr says `agent_status: "idle"` for the three seconds it takes a Claude
    /// Code to draw its banner, and marks the window by an *absence* —
    /// `launch_pending` present, `interactive_ready` gone. The port used to read
    /// `Some(false)`, which herdr has never sent, so the launch window came back
    /// as `Idle` and `will_take_a_prompt` said yes.
    ///
    /// Asserted against the fake's dialect rather than a hand-written reply, so
    /// it cannot pass on a reading herdr does not produce.
    #[test]
    fn the_launch_window_is_starting_and_not_idle() {
        let read = |v: Value| state_of_agent(&herdr::parse_pane(&v));

        let coming_up = json!({ "agent": "claude", "agent_status": "idle", "launch_pending": true });
        assert_eq!(read(coming_up.clone()), State::Starting);
        assert!(!read(coming_up).will_take_a_prompt());

        let ready = json!({ "agent": "claude", "agent_status": "idle", "interactive_ready": true });
        assert_eq!(read(ready.clone()), State::Idle);
        assert!(read(ready).will_take_a_prompt());

        // A plugin-reported agent carries neither field, so its readiness is not
        // known rather than established.
        let plugin = json!({ "agent": "claude", "agent_status": "idle" });
        assert_eq!(read(plugin.clone()), State::Unknown);
        assert!(!read(plugin).will_take_a_prompt());

        // And a pane with nobody in it says so with no `agent` field at all.
        assert_eq!(read(json!({ "agent_status": "unknown" })), State::Empty);
        assert_eq!(read(json!({ "agent": "", "agent_status": "idle" })), State::Empty);

        // The same reading off a pane row, which is the one that cannot tell the
        // first two apart — and must not pretend to.
        let row = |v: Value| state_of_pane(&herdr::parse_pane(&v));
        assert_eq!(row(json!({ "agent": "claude", "agent_status": "idle" })), State::Idle);
        assert_eq!(row(json!({ "agent": "claude", "agent_status": "working" })), State::Working);
    }

    /// The shell race. `agent.start` into a pane ten milliseconds old is refused
    /// with `agent_pane_busy` as a matter of course, so the refusal is retried
    /// while it is that one — and giving up on it is a failure with herdr's own
    /// words in it rather than a hang.
    ///
    /// **This is the test robustness-054 is named for**, and what it used to do
    /// was measure the machine: `Instant::now()` around the call, a `sleep(80ms)`
    /// on a scoped thread standing in for the shell arriving, and a 400ms window
    /// racing both. It failed once in four consecutive `wsp verify` runs on
    /// 2026-08-17 with three other agents building — and `wsp install` is gated
    /// on `wsp verify --release`, so it blocked the install exactly when the
    /// machine was busy, which is exactly when several agents are working.
    ///
    /// The three cases below are now arithmetic, because the adapter's clock is
    /// a parameter and [`Dial`] is what winds it. The shell arriving is a thing
    /// that happens at a time rather than a thread that sleeps for one, so
    /// nothing is racing anything: the socket is still real and only the clock
    /// is not. Each case gets a dial of its own, since what is being asserted
    /// about each is how long *that* wait was.
    #[test]
    fn a_pane_with_no_shell_yet_is_retried_and_nothing_else_is() {
        let (fake, _env) = bound("busy", Stage::new());
        let agent = Agent { kind: "claude".into(), name: "t-1".into(), args: Vec::new() };

        // Opening a seat waits for nothing, so this dial never moves.
        let dial = Dial::new();
        let seat = brisk(&dial).open(&Order::default()).expect("a seat");
        fake.refuses(Verb::Start, Snub::Busy);

        // A shell that never arrives. The window is spent to the millisecond and
        // then the refusal is passed on in herdr's own words.
        let dial = Dial::new();
        let place = brisk(&dial);
        fake.forget();
        let err = place.start(&seat, &agent).expect_err("a shell that never arrived");
        assert!(matches!(&err, Refusal::Backend(m) if m.contains("agent_pane_busy")), "{err}");
        assert_eq!(dial.elapsed(), place.shell, "it gave up somewhere other than the window's end");
        assert_eq!(
            times(&fake, Verb::Start),
            asks_in(place.shell, place.poll),
            "one ask, and then one per poll for as long as the window was open"
        );

        // The same refusal, relenting inside the window, is the case this is
        // for: a brand-new pane whose shell turns up.
        const ARRIVES: Duration = Duration::from_millis(80);
        let dial = Dial::new().at(ARRIVES, || fake.relents(Verb::Start));
        let place = brisk(&dial);
        fake.forget();
        place.start(&seat, &agent).expect("the shell arrived and the retry caught it");
        assert_eq!(
            times(&fake, Verb::Start),
            asks_in(ARRIVES, place.poll),
            "it stopped retrying at the poll the shell turned up on, and not a poll later"
        );

        // Anything that is not that refusal is a real failure, immediately —
        // which is now the strongest of the three: not *sooner than the window*
        // but without waiting at all.
        fake.refuses(Verb::Start, Snub::Backend("unknown agent kind `nonesuch`".into()));
        let dial = Dial::new();
        let place = brisk(&dial);
        fake.forget();
        let err = place.start(&seat, &agent).expect_err("a refusal that is not the race");
        assert_eq!(dial.elapsed(), Duration::ZERO, "a real refusal was waited on: {err}");
        assert_eq!(times(&fake, Verb::Start), 1, "a real refusal was retried: {err}");

    }

    /// `available()` was a question asked in advance about a socket. This is what
    /// it became: the answer to the call that wanted it, from the caller's own
    /// verb, in both of the ways a backend goes quiet.
    #[test]
    fn a_backend_that_did_not_answer_is_unreachable_rather_than_a_refusal() {
        let (fake, _env) = bound("quiet", Stage::of(vec![Spot::agent("w1:p1", "claude", "t-1", State::Idle)]));
        let dial = Dial::new();
        let place = brisk(&dial);
        let seat = Seat::new("w1:p1");
        assert_eq!(place.state(&seat).unwrap(), State::Idle);

        fake.goes(Quiet::HangsUp);
        assert!(matches!(place.open(&Order::default()), Err(Refusal::Unreachable(_))));
        assert!(matches!(place.state(&seat), Err(Refusal::Unreachable(_))));
        assert!(matches!(place.tell(&seat, "go"), Err(Refusal::Unreachable(_))));
        assert!(matches!(place.census(), Err(Refusal::Unreachable(_))), "an error is not an empty census");
        // The one that costs something to get wrong: a backend that did not
        // answer has not told us the seat is gone, and `despawn` releases a claim
        // on `NoSeat` alone.
        assert!(matches!(place.stop(&seat), Err(Refusal::Unreachable(_))));

        // A seat that is not there is a different sentence from a backend that
        // is not there, which is the distinction `available()` could not make.
        fake.goes(Quiet::No);
        assert_eq!(place.state(&Seat::new("nosuch")), Err(Refusal::NoSeat(Seat::new("nosuch"))));

    }

    /// The listing asymmetry, corrected: a census reads its seats from the call
    /// that knows about empty ones and its states from the call that can tell a
    /// starting agent from an idle one.
    ///
    /// The stage is one a live herdr cannot be put in — an agent mid-launch, one
    /// working, one that has gone, and a shell nobody is in.
    #[test]
    fn a_census_has_the_empty_seats_and_the_states_a_pane_row_cannot_carry() {
        let (_fake, _env) = bound(
            "census",
            Stage::of(vec![
                Spot::agent("w1:p1", "claude", "t-1", State::Starting).labelled("one"),
                Spot::agent("w1:p2", "claude", "t-2", State::Working).labelled("two"),
                Spot::agent("w2:p1", "claude", "t-3", State::Gone).labelled("three"),
                Spot::empty("w2:p2").labelled("a shell").at("/tmp"),
            ]),
        );
        let seats = Herdr::new().census().expect("the fake answered");
        let of = |id: &str| seats.iter().find(|s| s.seat == Seat::new(id)).cloned();

        assert_eq!(of("w1:p1").unwrap().state, State::Starting, "a pane row would have said idle");
        assert_eq!(of("w1:p1").unwrap().agent.name, "t-1", "what it was started as");
        assert_eq!(of("w1:p2").unwrap().state, State::Working);
        // herdr's blind spot, kept rather than smoothed over: an agent that has
        // exited leaves a pane indistinguishable from a shell, and `Gone` is
        // raised from the event stream instead.
        assert_eq!(of("w2:p1").unwrap().state, State::Empty);
        let shell = of("w2:p2").unwrap();
        assert_eq!(shell.state, State::Empty, "a seat somebody could sit in is still a seat");
        assert_eq!(shell.cwd, "/tmp");
        assert_eq!(shell.agent.kind, "");

    }

    /// "Tell me when it stops" is the clause no poll carries, and this is the
    /// only reading that can say it: from a listing, an agent that has exited
    /// leaves a pane that looks like a shell.
    #[test]
    fn an_agent_that_stops_is_the_one_event_a_listing_could_not_have_told_us() {
        let started = events_of(
            "pane_agent_detected",
            &json!({ "pane_id": "w1:p1", "workspace_id": "w1", "agent": "claude" }),
        );
        assert_eq!(started, vec![Event::Started(Seat::new("w1:p1"))]);

        let stopped = events_of(
            "pane_agent_detected",
            &json!({ "pane_id": "w1:p1", "agent": Value::Null, "released": true }),
        );
        assert_eq!(stopped, vec![Event::Stopped(Seat::new("w1:p1"))]);

        assert_eq!(
            events_of("pane_closed", &json!({ "pane_id": "w1:p1", "workspace_id": "w1" })),
            vec![Event::Closed(Seat::new("w1:p1"))]
        );
        assert_eq!(
            events_of("pane_created", &json!({ "pane": { "pane_id": "w1:p3" } })),
            vec![Event::Opened(Seat::new("w1:p3"))]
        );

        // The noisy one. A shell's output changing is not a state change, and an
        // event nothing can be read out of is not one either.
        let moved = events_of(
            "pane_updated",
            &json!({ "pane": { "pane_id": "w1:p1", "agent": "claude", "agent_status": "working" } }),
        );
        assert_eq!(moved, vec![Event::Moved(Seat::new("w1:p1"), State::Working)]);
        assert!(events_of("pane_updated", &json!({ "pane": { "pane_id": "w1:p1" } })).is_empty());
        assert!(events_of("pane_focused", &json!({ "pane_id": "w1:p1" })).is_empty());
    }

    /// The stream, over a real socket, with wsp's own subscriber on it: a
    /// watcher that was already listening hears the seat it is waiting on stop.
    #[test]
    fn a_watcher_hears_what_the_backend_did_without_being_asked() {
        let (fake, _env) = bound("watch", Stage::of(vec![Spot::agent("w1:p1", "claude", "t-1", State::Idle)]));

        let (tx, rx) = std::sync::mpsc::channel::<Event>();
        std::thread::spawn(move || {
            let _ = Herdr::new().watch(&mut |e| tx.send(e).is_ok());
        });
        // The stream has to be up before the change, or this asserts nothing —
        // and the fake's own register is what says so. A sleep long enough to be
        // right on a quiet machine is the flake robustness-054 was filed for.
        assert!(fake.watched(), "wsp's subscriber never reached the backend");

        let seat = Seat::new("w1:p1");
        fake.stops(&seat);
        let mut heard = Vec::new();
        while let Ok(e) = rx.recv_timeout(DELIVERY) {
            let done = matches!(e, Event::Stopped(_));
            heard.push(e);
            if done {
                break;
            }
        }
        assert!(heard.contains(&Event::Stopped(seat.clone())), "{heard:?}");

        fake.closes(&seat);
        let closed = rx.recv_timeout(DELIVERY).expect("no close arrived");
        assert_eq!(closed, Event::Closed(seat));

    }

    /// The seat an agent is standing in is the one herdr put in its environment,
    /// and no other name will do.
    ///
    /// Three readings, and the third is the one worth the test. A stale
    /// `WSP_SEAT_ID` — the port's name for a backend that has no name of its
    /// own — must not be believed here: it would name a pane this herdr never
    /// issued, and `wsp claim` would bind a task to it without a word, which is
    /// the phantom binding this codebase has spent the most time on.
    ///
    /// No socket, either. Nothing in this test binds a fake, and if `here` ever
    /// grows a call it will fail here rather than in an agent's pane — which is
    /// the port's rule that the answer must be local, made mechanical.
    #[test]
    fn an_agent_reads_its_own_seat_from_what_herdr_set_and_from_nothing_else() {
        let _env = crate::util::env_lock();
        let place = Herdr::new();

        std::env::remove_var("HERDR_PANE_ID");
        std::env::remove_var("HERDR_PLUGIN_EVENT_JSON");
        std::env::set_var(crate::place::SEAT_ENV, "sup-7");
        assert_eq!(place.here(), None, "a seat name from another backend was believed");

        std::env::set_var("HERDR_PANE_ID", "w31:p1");
        assert_eq!(place.here(), Some(Seat::new("w31:p1")));

        // The shim qualifies before forwarding, so an executor's id arrives
        // already carrying its machine and is passed through whole.
        std::env::set_var("HERDR_PANE_ID", "w0:p3@mb2");
        assert_eq!(place.here(), Some(Seat::new("w0:p3@mb2")), "the suffix is not ours to touch");

        std::env::set_var("HERDR_PANE_ID", "");
        assert_eq!(place.here(), None, "an emptied variable is not a seat called nothing");

        std::env::remove_var("HERDR_PANE_ID");
        std::env::remove_var(crate::place::SEAT_ENV);
    }
}
