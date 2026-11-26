//! The plain data an agent kind trades in: what a spawn asks for (`Spawn`), what a
//! finished run reports (`Ran`, `Spend`), and where a started agent was found
//! (`Address`). Moved out of `agent_commands` (wsp-403) so the store, the engine
//! and the wire read them without reaching up into the seat commands; the `Kind`
//! trait that produces them stays there and `agent_commands` re-exports each.


use crate::util;

/// The tier a session was actually served at, and how much of it there was.
///
/// Lists rather than scalars, and that is the whole point of reading this
/// instead of trusting the spawn flag: a session can change model or effort
/// under itself, and the honest record of one that did is *both* names in the
/// order they served. Collapsing them to the first would name the tier a
/// decision was escalated away from; collapsing to the last would erase the
/// cheap attempt that failed, which is exactly the datum a try-cheap-then-
/// escalate policy is calibrated against.
///
/// `turns` is assistant turns on the main thread, and it is here because it is
/// the one number that separates an attempt that ground for an hour from one
/// that was spawned and said nothing — wall-clock cannot, because it counts
/// the agent waiting for a person the same as the agent working.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Ran {
    /// Distinct model ids, in the order they first served a turn, with the
    /// `claude-` prefix off: `opus-5`, `haiku-4-5`. opencode's are
    /// `provider/model`, which is that kind's own spelling and the one a spawn
    /// states.
    pub models: Vec<String>,
    /// Distinct effort levels, same ordering rule. Empty where the runtime
    /// recorded none, which is a real answer and not a missing one — an older
    /// Claude Code wrote no `effort` at all.
    pub efforts: Vec<String>,
    /// Requests to the model, which is the same number as assistant turns:
    /// one turn is one call and one line of the bill. See [`read_ran`] for
    /// why counting the transcript's lines gave three times this.
    pub turns: usize,
    /// The largest context any one turn read — cache reads, fresh input and
    /// cache writes of a single request. It is the number staging is meant to
    /// bring down (`docs/legs.md`, *Measured*), and a sum cannot say it: 63
    /// turns of 40k and 63 turns growing to 400k read the same total.
    pub peak: u64,
    pub spend: Spend,
}

impl Ran {
    /// `opus-5/high`, or `haiku-4-5→opus-5/high` for a session that moved.
    ///
    /// One string because it is what goes in a log line and what a report
    /// groups by, and those two must be the same string or the report is
    /// grouping on a rendering. The `/` is the same separator `spawned at`
    /// uses, so the intent and the fact line up column-wise when both are
    /// printed.
    pub fn label(&self) -> String {
        match self.efforts.is_empty() {
            true => self.models.join("→"),
            false => format!("{}/{}", self.models.join("→"), self.efforts.join("→")),
        }
    }
}

/// What an attempt consumed, in the one vocabulary both kinds can answer in.
///
/// **The point of this type is that it is the same shape for every kind.**
/// `core-029` exists because opencode publishes cost and Claude Code does not,
/// and the failure it was written against is opencode arriving as a second,
/// better-looking column that nobody can set against the first. So the fields
/// are the ones *both* runtimes report, and a kind that knows more than this
/// says it in [`Spend::micros`] or not at all.
///
/// # Why three numbers and not five
///
/// Both runtimes report input, output and two kinds of cache separately, and
/// this keeps two of the four apart:
///
/// - **`cache` is cache read**, and it is an order of magnitude cheaper than
///   everything else per token while being a hundred times more of them — 673M
///   against 4k in one session measured here. Folding it into input would price
///   an attempt about ten times too high, which is the one error a routing
///   argument cannot survive.
/// - **`input` is input plus cache write**, which are within 25% of each other
///   per token: both are the model reading something it has not read before.
///   The record loses that 25% and says so here.
///
/// Rounded to three significant figures on the way to disk, because the copy
/// that survives is a clause in a task's log — see [`util::count_human`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Spend {
    /// Tokens read from cache: the cheap half, and nearly all of the volume.
    pub cache: u64,
    /// Tokens the model read for the first time — input plus cache write.
    pub input: u64,
    /// Tokens the model produced, thinking included.
    pub output: u64,
    /// What the runtime says it cost, in millionths of a dollar — `None` where
    /// the runtime says nothing, which is Claude Code, and `Some(0)` for a free
    /// model, which is a different fact and has to read as one.
    pub micros: Option<u64>,
}

impl Spend {
    /// Nothing to say: no tokens and no price. A session that was started and
    /// killed before it answered produces one of these, and writing a clause
    /// of zeroes for it would be the record claiming a measurement it does not
    /// have.
    pub fn nothing(&self) -> bool {
        self.cache == 0 && self.input == 0 && self.output == 0 && self.micros.is_none()
    }

    /// The clause that goes on a release line: `673M cache 21M in 3.5M out $1.20`.
    ///
    /// One clause rather than four, and every term self-describing. The task's
    /// log is the only durable copy of this — a transcript is one machine's
    /// disk, cleaned up on its own schedule and absent altogether for an agent
    /// that ran on the other box — and it is also prose a person reads in
    /// `wsp brief`, four entries of it on every request of every session. So
    /// the cost of the record is four words on a line that was already being
    /// written, and zero terms are left out rather than printed as `0`.
    ///
    /// [`Spend::of_clause`] reads it back, and the two are here together
    /// because a format with its parser in another file drifts.
    pub fn clause(&self) -> Option<String> {
        if self.nothing() {
            return None;
        }
        let mut parts: Vec<String> = Vec::new();
        for (n, what) in [(self.cache, "cache"), (self.input, "in"), (self.output, "out")] {
            if n > 0 {
                parts.push(format!("{} {what}", util::count_human(n)));
            }
        }
        if let Some(m) = self.micros {
            parts.push(util::money_human(m));
        }
        Some(parts.join(" "))
    }

    /// That clause back, and `None` for a clause that is not one.
    ///
    /// Strict on purpose: the log is prose people write in, so anything that is
    /// not exactly pairs of `<count> <unit>` with an optional price is somebody's
    /// sentence and must not be read as a measurement.
    pub fn of_clause(text: &str) -> Option<Spend> {
        let mut spend = Spend::default();
        let mut words = text.split_whitespace().peekable();
        let mut any = false;
        while let Some(w) = words.next() {
            if let Some(m) = util::money_of(w) {
                spend.micros = Some(m);
                any = true;
                continue;
            }
            let n = util::count_of(w)?;
            match words.next()? {
                "cache" => spend.cache = n,
                "in" => spend.input = n,
                "out" => spend.output = n,
                _ => return None,
            }
            any = true;
        }
        any.then_some(spend)
    }
}

/// How the recovery sentence names an agent, and how sure it is of it.
///
/// Two answers rather than one because the failure path has two truths to tell
/// and a reading that could only tell the first told neither. The distinction is
/// **not** the trait's "a wrong handle is worse than no handle": that rule is
/// about naming *somebody else's* agent, and it still holds without exception.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Address {
    /// A live session answers to this name — the runtime's own census says so.
    Confirmed(String),
    /// The name wsp gave this agent, which no live session answers to.
    ///
    /// **Not a guess at which name.** wsp passed this exact string to that
    /// launch as `-n`, and [`mint`] is task *and* seat precisely so that no
    /// other agent of wsp's can be answering to it — backend issues at most one
    /// live agent per pane. So what is unconfirmed here is whether the agent
    /// exists, never who it would be, and that is what makes saying it safe: the
    /// only thing this can name wrongly is nothing at all.
    ///
    /// Which is worth having, because "nothing at all" is what it says. A spawn
    /// that failed *early* is the case — the registry lands 1.3-1.4s after
    /// launch and a failure can beat it, measured in the module docs — and an
    /// agent that comes up a moment later comes up under this name. The
    /// alternative on offer was silence, and a person recovering a spawn by hand
    /// at midnight can use a hedged name and cannot use nothing.
    Unconfirmed(String),
}

/// The efforts `pi` takes. Held here, below the registry, so `model::Policy`
/// can tell an effort word from a model name without reaching up to
/// `agent_commands`.
pub const PI_THINKING: &[&str] = &["off", "minimal", "low", "medium", "high", "xhigh", "max"];

/// The efforts `claude` takes.
pub const CLAUDE_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// The effort levels a kind accepts: what `agent_commands::of(kind).efforts()`
/// answers, which only `pi` and `claude` give (a data kind and the rest have no
/// such knob).
pub fn efforts_of(kind: &str) -> &'static [&'static str] {
    match kind.trim() {
        "claude" => CLAUDE_EFFORTS,
        "pi" => PI_THINKING,
        _ => &[],
    }
}

/// backend's default when nobody says which agent. Every other kind it knows is
/// spelt the way its own CLI spells it and passed straight through — an
/// unknown one is refused by backend with the whole catalogue in the message,
/// which is a better list than one kept here and left to go stale.
pub const DEFAULT_KIND: &str = "claude";

/// The kind a record names, or [`DEFAULT_KIND`] for one that names none.
///
/// **`core-031`'s second decision, and it is a decision rather than a
/// convenience.** A record with no kind can be read two ways — as `claude`,
/// which is compatible and wrong for exactly the agents `core-020` is adding,
/// or as *unknown*, which is honest and costs more than it sounds.
///
/// Unknown resolves to [`crate::agent_commands::Plain`], and `Plain` has no
/// resume flag: `wsp resume` on every record written before this existed would
/// stop working, and the ones written before this existed are all `claude`
/// because `--kind` was read and thrown away. So honesty about the absence buys
/// nothing true and loses the whole back catalogue.
///
/// What makes the default safe rather than merely convenient is that an absence
/// no longer lasts: [`crate::cmd_agent::learn_sessions`] and
/// [`crate::cmd_govern::learn_seats`] take the kind off backend's own reading on
/// every `sync` tick, so a live record is corrected within a tick of anything
/// looking at it and only a record whose agent is already gone can still be
/// reading its default. For those, the default is what they were.
///
/// One function because there are three call sites and `cmd_resume`'s census
/// path already had this line written out; the store path now reads the same
/// fact the same way.
pub fn kind_or_default(recorded: &str) -> String {
    match recorded.trim() {
        "" => DEFAULT_KIND.to_string(),
        k => k.to_string(),
    }
}
