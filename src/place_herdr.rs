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
//! [`Place::open`] returns one handle and herdr has two ids: a workspace, which
//! survives a session restore, and a pane, which is reissued on every restart.
//! This adapter answers with the **pane id**, because that is what every verb
//! underneath takes — `agent.start` takes a `pane_id`, `agent.prompt` a
//! `target`, and `wsp claim --pane` takes the same string. A workspace-id seat
//! would have to be resolved back to its root pane on every call, and the claim
//! would have to be handed something else.
//!
//! So the durability [`Seat`] promises is not yet kept here. `place.rs` names
//! the means — re-find the seat by its label, which is what `workspace_label`
//! already does for claims — and that work belongs with the claim's own
//! migration (`cmd_agent.rs`), because the label is written and matched there.
//! Until then this adapter is exactly as durable as today's code, which is to
//! say: a claim survives a restart and a binding does not.
//!
//! # `WSP_SEAT` cannot be delivered by this backend, and that is a finding
//!
//! `place.rs` requires that whatever runs in a seat finds the seat's own name in
//! its environment under [`crate::place::SEAT_ENV`], because an agent that
//! cannot name its own seat cannot claim, say or release. **herdr cannot do
//! this.** The environment is fixed by `workspace.create`, and the pane id that
//! call returns does not exist until it has returned — so there is no moment at
//! which wsp knows the seat and can still put it in the shell's environment.
//!
//! herdr solves its own version by setting `HERDR_PANE_ID` itself, per pane,
//! which is why every agent-side verb reads that today. A backend that cannot
//! set a variable per seat leaves two options and both are wsp's to choose:
//! **wsp mints the seat** and passes it down in `Order::env` (making [`Seat`]
//! wsp's name for a place rather than the backend's), or the agent asks the
//! backend which seat it is standing in. The first is cheaper and would let
//! `WSP_SEAT` land with `t-260816-064`'s rename; neither is decided here, and
//! this adapter goes on reading `HERDR_PANE_ID` because nothing has migrated off
//! it yet.
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
//!    two swap. Absence is the signal in both directions. [`state_of_agent`]
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
//!    `launch_pending: true`, `agent_status: "unknown"` and no agent name; the
//!    name appears about three tenths of a second later. `Place::start` promises
//!    the caller that the agent exists when it returns, so [`Herdr::start`]
//!    waits for it — and that wait is where the retype lives.
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

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::herdr;
use crate::place::{Agent, Event, Order, Place, Refusal, Result, Seat, Seated, State};
use crate::util;

/// herdr as a place to put work.
///
/// The three durations are fields rather than constants because they are the
/// only interesting thing about `start`: every one of them was set by a failure,
/// and a test that had to sit through five real seconds to check the shell race
/// is a test nobody runs. [`Herdr::new`] is what the CLI uses.
pub struct Herdr {
    /// How long a brand-new pane gets to grow a shell before `agent_pane_busy`
    /// stops being a race and starts being a failure.
    pub shell: Duration,
    /// How long to wait for the agent to appear at all, having typed its name.
    pub appear: Duration,
    /// When to decide the typing was eaten, clear the line and type again. Once.
    pub retype: Duration,
    /// How often to look while waiting.
    pub poll: Duration,
}

impl Default for Herdr {
    fn default() -> Herdr {
        Herdr {
            // A cold Claude Code measured four seconds to readiness on this
            // machine; herdr's own default for the same wait is thirty.
            shell: Duration::from_millis(5_000),
            appear: Duration::from_millis(30_000),
            retype: Duration::from_millis(6_000),
            poll: Duration::from_millis(150),
        }
    }
}

impl Herdr {
    pub fn new() -> Herdr {
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

/// The four herdr error codes wsp has ever had to tell apart.
///
/// Written down once and matched in this file only. `spawn` matching on
/// `agent_pane_busy` at a call site was the coupling that survived every
/// previous attempt at a seam: a caller that knows a backend's error strings has
/// not been decoupled from it whatever the signatures say.
const NOT_READY: &str = "agent_not_ready";
const NO_AGENT: &str = "agent_not_found";
const NO_PANE: &str = "pane_not_found";
const PANE_BUSY: &str = "agent_pane_busy";

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
    let deadline = Instant::now() + place.shell;
    loop {
        match herdr::call_for("agent.start", params.clone(), SLOW) {
            Ok(_) => return Ok(()),
            Err(e) => {
                if !e.to_string().contains(PANE_BUSY) || Instant::now() >= deadline {
                    return Err(refusal(seat, &e));
                }
                std::thread::sleep(place.poll);
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

impl Place for Herdr {
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

    /// Returns when the agent exists, which herdr does not tell us and has to be
    /// waited for. Not when it will take a prompt: those are different moments,
    /// three seconds apart, and conflating them is the `agent_not_ready` bug.
    fn start(&self, seat: &Seat, agent: &Agent) -> Result<()> {
        launch(self, seat, agent)?;
        let began = Instant::now();
        let mut retyped = false;
        loop {
            if look(seat)?.is_some() {
                return Ok(());
            }
            let waited = began.elapsed();
            if waited >= self.appear {
                return Err(Refusal::Backend(format!(
                    "nothing that looks like {} appeared in {seat}",
                    agent.kind
                )));
            }
            if !retyped && waited >= self.retype {
                retyped = true;
                let _ = herdr::call(
                    "pane.send_text",
                    json!({ "pane_id": seat.as_str(), "text": "\x15" }),
                );
                launch(self, seat, agent)?;
            }
            std::thread::sleep(self.poll);
        }
    }

    fn tell(&self, seat: &Seat, text: &str) -> Result<()> {
        herdr::call_for("agent.prompt", json!({ "target": seat.as_str(), "text": text }), SLOW)
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
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wsp-adapter-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A fake bound at the socket wsp's own client reads, so every test below
    /// goes over the wire rather than round it.
    fn bound(name: &str, stage: Stage) -> (Fake, PathBuf) {
        let dir = scratch(name);
        let fake = Fake::bind(dir.join("herdr.sock"), stage).unwrap();
        let (k, v) = fake.socket_env();
        std::env::set_var(k, v);
        (fake, dir)
    }

    fn brisk() -> Herdr {
        Herdr { shell: Duration::from_millis(400), retype: Duration::from_millis(150),
                appear: Duration::from_millis(800), poll: Duration::from_millis(20) }
    }

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
        let _env = util::env_lock();
        let mut stage = Stage::new();
        // The window a live herdr passes through in half a second whether you
        // wanted it to or not.
        stage.settle = false;
        let (fake, dir) = bound("order", stage);
        let place = brisk();

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

        std::env::remove_var("HERDR_SOCKET_PATH");
        let _ = std::fs::remove_dir_all(&dir);
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
        let _env = util::env_lock();
        let (fake, dir) = bound(
            "stop",
            Stage::of(vec![
                Spot::agent("w1:p1", "claude", "t-1", State::Idle).labelled("robustness/095"),
                Spot::agent("w2:p1", "claude", "t-2", State::Working).labelled("robustness/094"),
            ]),
        );
        let place = brisk();
        let seat = Seat::new("w1:p1");

        place.stop(&seat).expect("the seat was there");
        assert!(fake.stage().find(&seat).is_none(), "the seat outlived its stop");
        // Somebody else's seat is not swept up with it. On herdr this is why the
        // verb is `pane.close` and not `workspace.close`.
        assert_eq!(place.state(&Seat::new("w2:p1")).unwrap(), State::Working);

        assert_eq!(place.stop(&seat), Err(Refusal::NoSeat(seat.clone())), "already gone");
        assert_eq!(place.state(&seat), Err(Refusal::NoSeat(seat)));

        std::env::remove_var("HERDR_SOCKET_PATH");
        let _ = std::fs::remove_dir_all(&dir);
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

    /// The shell race, at a hundredth of its real length. `agent.start` into a
    /// pane ten milliseconds old is refused with `agent_pane_busy` as a matter
    /// of course, so the refusal is retried while it is that one — and giving up
    /// on it is a failure with herdr's own words in it rather than a hang.
    #[test]
    fn a_pane_with_no_shell_yet_is_retried_and_nothing_else_is() {
        let _env = util::env_lock();
        let (fake, dir) = bound("busy", Stage::new());
        let place = brisk();
        let agent = Agent { kind: "claude".into(), name: "t-1".into(), args: Vec::new() };

        let seat = place.open(&Order::default()).expect("a seat");
        fake.refuses(Verb::Start, Snub::Busy);
        let began = Instant::now();
        let err = place.start(&seat, &agent).expect_err("a shell that never arrived");
        assert!(began.elapsed() >= place.shell, "it gave up before it had waited");
        assert!(matches!(&err, Refusal::Backend(m) if m.contains("agent_pane_busy")), "{err}");

        // The same refusal, relenting inside the window, is the case this is
        // for: a brand-new pane whose shell turns up.
        std::thread::scope(|s| {
            s.spawn(|| {
                std::thread::sleep(Duration::from_millis(80));
                fake.relents(Verb::Start);
            });
            place.start(&seat, &agent).expect("the shell arrived and the retry caught it");
        });

        // Anything that is not that refusal is a real failure, immediately.
        fake.refuses(Verb::Start, Snub::Backend("unknown agent kind `nonesuch`".into()));
        let began = Instant::now();
        let err = place.start(&seat, &agent).expect_err("a refusal that is not the race");
        assert!(began.elapsed() < place.shell, "a real refusal was retried: {err}");

        std::env::remove_var("HERDR_SOCKET_PATH");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `available()` was a question asked in advance about a socket. This is what
    /// it became: the answer to the call that wanted it, from the caller's own
    /// verb, in both of the ways a backend goes quiet.
    #[test]
    fn a_backend_that_did_not_answer_is_unreachable_rather_than_a_refusal() {
        let _env = util::env_lock();
        let (fake, dir) = bound("quiet", Stage::of(vec![Spot::agent("w1:p1", "claude", "t-1", State::Idle)]));
        let place = brisk();
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

        std::env::remove_var("HERDR_SOCKET_PATH");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The listing asymmetry, corrected: a census reads its seats from the call
    /// that knows about empty ones and its states from the call that can tell a
    /// starting agent from an idle one.
    ///
    /// The stage is one a live herdr cannot be put in — an agent mid-launch, one
    /// working, one that has gone, and a shell nobody is in.
    #[test]
    fn a_census_has_the_empty_seats_and_the_states_a_pane_row_cannot_carry() {
        let _env = util::env_lock();
        let (_fake, dir) = bound(
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

        std::env::remove_var("HERDR_SOCKET_PATH");
        let _ = std::fs::remove_dir_all(&dir);
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
        let _env = util::env_lock();
        let (fake, dir) = bound("watch", Stage::of(vec![Spot::agent("w1:p1", "claude", "t-1", State::Idle)]));

        let (tx, rx) = std::sync::mpsc::channel::<Event>();
        std::thread::spawn(move || {
            let _ = Herdr::new().watch(&mut |e| tx.send(e).is_ok());
        });
        // The stream has to be up before the change, or this asserts nothing.
        std::thread::sleep(Duration::from_millis(150));

        let seat = Seat::new("w1:p1");
        fake.stops(&seat);
        let mut heard = Vec::new();
        while let Ok(e) = rx.recv_timeout(Duration::from_secs(2)) {
            let done = matches!(e, Event::Stopped(_));
            heard.push(e);
            if done {
                break;
            }
        }
        assert!(heard.contains(&Event::Stopped(seat.clone())), "{heard:?}");

        fake.closes(&seat);
        let closed = rx.recv_timeout(Duration::from_secs(2)).expect("no close arrived");
        assert_eq!(closed, Event::Closed(seat));

        std::env::remove_var("HERDR_SOCKET_PATH");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
