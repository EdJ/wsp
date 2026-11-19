//! The words a seat's operations answer in: which seat, what state it is in,
//! why a call did not happen, what became of a delivery. Moved down from
//! `wsp-wire`'s `place` (wsp-020), which re-exports every one of them, so the
//! in-process host can answer in them without linking the wire.

/// A durable handle to somewhere an agent can run.
///
/// **Durable** is the requirement and it is the one the port will not bend on:
/// a seat outlives the agent standing in it and the backend restarting. wsp
/// writes it into a claim and expects to find the same place behind it
/// tomorrow.
///
/// backend does not quite supply this, and the shortfall is not the one this
/// comment used to claim. Measured 2026-08-19 (`robustness-084`, and see
/// `place_backend` for the transcript): a pane number inside a surviving
/// workspace never comes round again, but a *workspace* id does — backend's
/// counter is process-local, and a restart reserves only one above the highest
/// workspace that survived. So an id above that mark is handed out again, and
/// with it every pane id qualified by it. A handle can therefore fail in both
/// directions: name nothing, or name somebody else. That is why claims carry a
/// `workspace_label` and a cwd — not to re-find an id that changed, but to tell
/// the workspace we meant from the one that took its name. Under this port the
/// shortfall is the backend adapter's to make good, by whatever means, and wsp
/// stops carrying the workaround. It is also why
/// nothing here is a `pane` or a `workspace`: today's two ids, of two different
/// lifetimes, become one handle of one lifetime.
///
/// Opaque on purpose. Constructed from whatever the backend says, compared, and
/// never parsed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Seat(String);

impl Seat {
    pub fn new(id: impl Into<String>) -> Seat {
        Seat(id.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Display for Seat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What an agent in a seat is doing, as far as the backend can tell.
///
/// The README says why a backend's own vocabulary is not enough: "`idle` is an
/// answer to a question nobody asked". This enum is the answers wsp acts on,
/// each with a caller today.
///
/// **The distinction this is for is [`State::turn_in_flight`], and it is the
/// only one that separates work happening from a slot producing nothing.**
/// Everything else here is about what wsp may *do* to a seat. That one is about
/// whether anything is happening in it, and until robustness-083 nothing in wsp
/// asked it — a census counted processes, and a process that had stopped
/// mid-task counted the same as one three hours into a turn.
///
/// The two that look like luxuries are the two that have cost the most:
///
/// - [`State::Starting`] is separate from [`State::Idle`] because backend reports
///   `agent_status: idle` while an agent is still coming up, and `agent.prompt`
///   refuses in that window with `agent_not_ready`. Waiting for "idle" returned
///   in half a second every time and the work order went into a pane still
///   drawing its banner. Every existing caller that tests `state == "idle"`
///   before telling an agent something is one reading of this away from the
///   same bug.
/// - [`State::Unknown`] exists because **an absence is not a fact**. It is the
///   README's `·`, and it is the same rule as `sync`'s "`Err` is not an empty
///   list" and `reconcile`'s unreachable-is-not-empty: one `pane.list` that
///   timed out once unbound every agent on the seat. A backend that does not
///   know must be able to say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum State {
    /// The seat exists and nothing is running in it. A terminal somebody
    /// opened, or a seat opened and never started.
    Empty,
    /// An agent is coming up and will refuse a prompt.
    Starting,
    /// Waiting for input. The only state in which `Place::tell` is expected
    /// to succeed.
    Idle,
    /// Busy. Nothing to do about it and nothing to ask it. **The only state in
    /// which a turn is in flight** — see [`State::turn_in_flight`].
    Working,
    /// Stopped in front of a question only a person can answer: a permission
    /// prompt, a trust dialog. Running, and going nowhere until somebody
    /// answers.
    ///
    /// Separate from [`State::Idle`] because the two want opposite handling and
    /// were indistinguishable until robustness-083: an idle agent is told the
    /// next thing, a blocked one is *answered*, and a sentence sent to it lands
    /// in the modal rather than in the composer. It is also the state a Claude
    /// Code reaches by responding to a work order with a permission request,
    /// which is why `fork-015` could not have `agent.prompt` wait for
    /// `working` — a prompt plainly taken passes through here and never
    /// through there.
    Blocked,
    /// There was an agent here and it has stopped. The seat may still exist.
    ///
    /// backend cannot distinguish this from [`State::Empty`] in a listing — an
    /// exited agent leaves a pane whose `agent` field is empty, which is what a
    /// plain shell looks like — so its adapter answers `Empty` from a census
    /// and `Gone` from the event stream, where a released agent is named. A
    /// supervisor that watched a pid exit answers it exactly, which is the point
    /// of writing the port in wsp's vocabulary rather than backend's.
    Gone,
    /// The seat is on a far machine that has gone silent: the link is down or
    /// the beat is unanswered past the lease (`docs/remote-agents.md` section 3).
    /// The agent is held, not dead — its claim stays, a reap leaves it alone and
    /// a tell to it is held until the link returns. Not [`State::Gone`], which
    /// is what a blip used to read as, and not [`State::Unknown`], which is a
    /// backend that cannot say: here wsp knows exactly why it cannot see.
    Unreachable,
    /// The backend did not say, or could not be asked.
    #[default]
    Unknown,
}

impl State {
    pub fn as_str(&self) -> &'static str {
        match self {
            State::Empty => "empty",
            State::Starting => "starting",
            State::Idle => "idle",
            State::Working => "working",
            State::Blocked => "blocked",
            State::Gone => "gone",
            State::Unreachable => "unreachable",
            State::Unknown => "unknown",
        }
    }

    /// Whether `Place::tell` is worth attempting.
    ///
    /// The one question every caller of `state == "idle"` is actually asking,
    /// written once. `Unknown` is false: telling an agent we cannot see is how
    /// a prompt lands in somebody's shell.
    pub fn will_take_a_prompt(&self) -> bool {
        matches!(self, State::Idle)
    }

    /// Whether something is running here, whatever it is doing.
    pub fn is_running(&self) -> bool {
        matches!(self, State::Starting | State::Idle | State::Working | State::Blocked)
    }

    /// Whether a turn is actually running — the reading robustness-083 was
    /// opened for, and the one nothing in wsp had.
    ///
    /// [`State::is_running`] answers "is there a process here", which is what
    /// every census in wsp asked and is not the question. Seven agents on
    /// 2026-08-18 answered yes to it with their turns abandoned by an API
    /// overload: process alive, conversation intact, nothing happening, and
    /// `doctor` reporting `backend up, 12 agents`. **An agent that is not turning
    /// will not start turning on its own**, whatever put it there.
    ///
    /// So this is deliberately not `!is_running()` and deliberately does not
    /// try to say *why* a turn ended. Idle after finishing, idle after an
    /// overload and blocked on a permission are three different repairs and one
    /// identical fact: the slot is costing something and producing nothing.
    /// Telling them apart is a job for whoever reads the census, and wsp does
    /// not have to understand the cause to raise a hand — see
    /// `cmd_agent::Bound`.
    ///
    /// [`State::Unknown`] answers false, on the rule the rest of this file is
    /// built on: an absence is not a fact, and least of all a fact about work
    /// getting done.
    pub fn turn_in_flight(&self) -> bool {
        matches!(self, State::Working)
    }

    /// The same fact from the other side: **there is an agent here and it is
    /// doing nothing.**
    ///
    /// Not merely `!turn_in_flight()`, and the difference is [`State::Unknown`]
    /// and [`State::Empty`]. An agent nobody can see is not an agent that has
    /// stopped, and a seat with nothing in it has not stopped either — it is a
    /// different report with a different verb under it. Only a seat known to
    /// hold something known not to be turning belongs here.
    ///
    /// Written once because four censuses ask it — `wip`'s `needs you`,
    /// `overlap`'s, the panel's `←`, and `doctor`'s quiet check — and until
    /// robustness-083 each of them asked it as `agent_status == "idle"`. That
    /// is one of *three* words backend has for a pane running no turn, and the
    /// other two are not corner cases: `done` was four of twelve agents on this
    /// machine when it was measured, and `blocked` is a permission prompt,
    /// which is the failure most worth interrupting somebody for.
    pub fn stopped(&self) -> bool {
        self.is_running() && !self.turn_in_flight()
    }
}

/// Why a call did not happen, in wsp's terms.
///
/// The point of naming these is that a caller should never match on a backend's
/// error string. `agent_not_ready`, `agent_pane_busy` and `pane_not_found` are
/// backend's words for three of these, and `spawn` matching on `agent_pane_busy`
/// is the kind of coupling that survives a port unless the port has a word of
/// its own. It has one now, and the match is in `place_backend` and nowhere else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// No backend answered. This is what `backend::available()` becomes: not a
    /// question asked in advance, but the answer to the call that wanted it.
    Unreachable(String),
    /// No such seat. It may have been closed under us.
    NoSeat(Seat),
    /// The agent is not in a state to be told anything.
    NotReady(State),
    /// The sentence was delivered and the agent never acted on it.
    ///
    /// Only a backend that *watched* can say this, and saying it is the whole
    /// value: it is `Place::tell`'s `Ok` and `Place::nudge`'s reason
    /// arriving in one answer, so a caller learns in the reply what it
    /// otherwise had to go and find out by looking. Not a failure on its own —
    /// the work order is sitting where the agent can see it, and a submit is
    /// what it is waiting for.
    NotTaken,
    /// Typed, read back off the seat's screen, and still sitting in its prompt
    /// after `submit` pressed submit again — `wsp-252`.
    ///
    /// [`Refusal::NotTaken`] said from a backend that **looked**, which is the
    /// half of that answer it never had: where the text went. Every spawn that
    /// failed this way on 2026-10-08 was reported as "either sitting unsent or
    /// dropped — wsp cannot see which", and every one of them, looked at, was
    /// sitting there. The repair is a return in the seat, and not a resend,
    /// which would put a second copy behind the first.
    Unsubmitted(Seat),
    /// This backend cannot do that at all — a seat for a person on a backend
    /// with no terminal, say. Distinct from a failure, and not a bug.
    Unsupported(&'static str),
    /// The backend said no, in its own words.
    Backend(String),
    /// The seat's own supervisor answered `failed` to a delivery
    /// (`compound-sup deliver`), with its reason. Not [`Refusal::Backend`]: a
    /// supervisor that owns the retry has already retried, so this is the one
    /// answer wsp escalates (wsp-477).
    Failed(String),
}

/// What became of a sentence `Place::tell` delivered.
///
/// `Ok` has always meant *delivered* and never *taken* — the paragraph on
/// `Place::tell` is older than this enum and is unchanged by it. What this
/// adds is the answer to the second question where a backend has one, because
/// worklist-010 was a verb that could not tell **delivered and watched** from
/// **delivered and unwatchable** and so said neither, and then said something
/// false instead.
///
/// Two variants and not three. A backend with no way to watch, and a backend
/// that watched and could not tell, are the same fact to every caller: nothing
/// here confirms a turn, nothing here is owed, and a retry is the wrong move.
/// Telling *those* two apart would be a fact about the backend rather than
/// about the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Delivered, and the agent moved in answer to it — watched by something in
    /// a position to see it.
    ///
    /// Moved, rather than *is working*: an agent that answers a work order by
    /// asking for a permission has plainly taken it and is `blocked`. That is
    /// why `cmd_spawn::hand_over` still takes its own reading afterwards — a
    /// folder-trust modal is the same status and the opposite outcome, and this
    /// says the sentence landed on something awake, not what it decided.
    Started,
    /// Delivered, with no turn observed and none claimed.
    ///
    /// Either the backend does not watch, or it watched an agent that was
    /// already mid-turn: a sentence queued behind a turn in progress changes no
    /// status, so there is nothing for a watch to see and the agent has not read
    /// it yet. **This is not a failure and must never be reported as one** —
    /// that report is worklist-010, where a governor retried a message that had
    /// arrived and sent the same paragraph three times.
    Unconfirmed,
    /// The seat's supervisor acknowledged the delivery (`delivered`): the
    /// sentence reached the agent exactly once, and the supervisor owns any
    /// retry. Nothing is owed and nothing is looked for after it (wsp-477).
    Acked,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::Unreachable(w) => write!(f, "no backend answered: {w}"),
            Refusal::NoSeat(s) => write!(f, "no seat {s}"),
            Refusal::NotReady(s) => write!(f, "not ready — {}", s.as_str()),
            Refusal::NotTaken => write!(f, "the sentence arrived and no turn started"),
            Refusal::Unsubmitted(s) => write!(f, "typed into {s} and not submitted — it is sitting in the prompt"),
            Refusal::Unsupported(w) => write!(f, "this backend cannot {w}"),
            Refusal::Backend(w) => write!(f, "{w}"),
            Refusal::Failed(w) => write!(f, "the seat's supervisor could not deliver it: {w}"),
        }
    }
}

pub type Result<T> = std::result::Result<T, Refusal>;
