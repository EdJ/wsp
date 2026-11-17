//! One check on one barrier, as a worklist's `checks:` block holds it: the record the model reads and writes.
//! The run that opens, reads and ends checks is `barrier`.

use crate::util;


/// Where a check is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Opened, and no verdict yet. Its seat may not exist yet either.
    Running,
    /// `go`: the barrier is passed.
    Passes,
    /// `hold`: the run stops here, and the governor is told.
    Holds,
    /// `followup --next`: it passes, and the rows it found are the next group.
    Next,
    /// `followup --blocking`: it holds until the rows it found land, and a
    /// fresh check reads the group again.
    Blocking,
    /// Its agent went away before it said anything, and the step opened a
    /// fresh check rather than wait on a seat with nobody in it.
    Abandoned,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Running => "running",
            State::Passes => "passes",
            State::Holds => "holds",
            State::Next => "next",
            State::Blocking => "blocking",
            State::Abandoned => "abandoned",
        }
    }

    fn parse(s: &str) -> Option<State> {
        Some(match s {
            "running" => State::Running,
            "passes" => State::Passes,
            "holds" => State::Holds,
            "next" => State::Next,
            "blocking" => State::Blocking,
            "abandoned" => State::Abandoned,
            _ => return None,
        })
    }

    /// What a reader is told it said.
    pub fn said(self) -> &'static str {
        match self {
            State::Running => "checking",
            State::Passes => "passed",
            State::Holds => "held",
            State::Next => "passed, with follow-ups next",
            State::Blocking => "held for follow-ups",
            State::Abandoned => "abandoned",
        }
    }
}

/// Whether the check's seat has been ended. **Failed is terminal**, for
/// `verification::Ending`'s reason: tried once, said once in `cycle.log`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Ending {
    #[default]
    Standing,
    Ended,
    Failed,
}

/// One check on one barrier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    /// When it was opened, as `util::now_iso` writes it.
    pub at: String,
    pub state: State,
    /// The trunk commit it was opened against. `None` where there is no
    /// repository to read one from.
    pub read: Option<String>,
    /// The seat its agent was started in.
    pub pane: Option<String>,
    /// What it ran on: the policy's label, its spaces written as `+` — a
    /// model's own name has `/` in it (`openrouter/…`), so that cannot be it.
    pub on: Option<String>,
    pub ending: Ending,
    /// The Barrier row this entry was migrated from.
    pub was: Option<String>,
    /// Starts of this entry's agent so far, the first included. `wsp-328`: an
    /// entry whose agent never came up is started again on a backoff, up to
    /// [`TRIES`], and then left for a person.
    pub tries: u32,
    /// The tries ran out and the person was flagged. Set once, so a tick that
    /// sees the entry spent again says nothing again.
    pub gave_up: bool,
    /// The mechanical part, run by wsp before any agent is asked (`wsp-326`).
    /// `None` where nothing was declared to run.
    pub suite: Option<Suite>,
    /// The process making the mechanical run, while `suite` is running, and when
    /// it started, so a pid that has been reused is not taken for it. A run that
    /// is killed leaves its entry at `suite:running`; this is how that is seen.
    pub run: Option<Run>,
    /// Who settled it, when it was not an agent: `wsp`.
    pub by: Option<String>,
}

/// The process of a barrier's mechanical run: `pid:25037@Tue_Nov_4_11:19:05_2026`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub pid: u32,
    /// `ps -o lstart=` for the pid, its spaces written as `_`.
    pub started: String,
}

impl Run {
    /// The running process's own. `start_of` is the host's reading of when a
    /// pid started (`wire::host::Host::started`): this layer asks it nothing.
    pub fn this(start_of: StartOf) -> Option<Run> {
        let pid = std::process::id();
        Some(Run { pid, started: start_of(pid).ok()?? })
    }

    fn parse(v: &str) -> Option<Run> {
        let (pid, started) = v.split_once('@')?;
        Some(Run { pid: pid.parse().ok()?, started: started.to_string() })
    }

    fn text(&self) -> String {
        format!("{}@{}", self.pid, self.started)
    }

    /// Whether the process is gone: no such pid, or one that started at another
    /// time. A `ps` that cannot be run says nothing, and the run is believed.
    pub fn gone(&self, start_of: StartOf) -> bool {
        match start_of(self.pid) {
            Ok(now) => now.as_deref() != Some(self.started.as_str()),
            Err(()) => false,
        }
    }
}

/// When process `pid` started, as the host's `ps` says it, or `None` where
/// there is none; `Err` where the host could not tell.
pub type StartOf<'a> = &'a dyn Fn(u32) -> Result<Option<String>, ()>;

/// What a finished `ps -o lstart=,stat= -p <pid>` says. A defunct (zombie)
/// process, its `stat` starting with `Z`, still has a pid and a start time but
/// is no run: it reads as no process at all, so a barrier whose run died
/// unreaped is not waited on forever. It says there is no such
/// process by exiting 1 with nothing on stdout (stderr is free text, and a pid
/// too large for the system gets a complaint there with the same exit). Any
/// other end — killed by a signal, or another exit status — says nothing about
/// the pid, so it is `Err` and a live run is not read as dead on it. A `ps` that
/// itself fails with exit 1 and prints nothing cannot be told from a missing
/// pid, and reads as gone: the lock (`open_unless_held`) is what covers that.
pub fn ps_says(code: Option<i32>, stdout: &[u8]) -> Result<Option<String>, ()> {
    if code != Some(0) && !(code == Some(1) && stdout.iter().all(u8::is_ascii_whitespace)) {
        return Err(());
    }
    let s = String::from_utf8_lossy(stdout);
    let mut words = s.split_whitespace().collect::<Vec<_>>();
    let Some(stat) = words.pop() else { return Ok(None) };
    if stat.starts_with('Z') {
        return Ok(None);
    }
    let s = words.join("_");
    Ok((!s.is_empty()).then_some(s))
}

/// Where the barrier's mechanical run is: the project's full suite, the
/// members' recorded checks and the stop condition's `stop-checks`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Suite {
    Running,
    Green,
    Red,
}

impl Suite {
    pub fn as_str(self) -> &'static str {
        match self {
            Suite::Running => "running",
            Suite::Green => "green",
            Suite::Red => "red",
        }
    }

    fn parse(s: &str) -> Option<Suite> {
        Some(match s {
            "running" => Suite::Running,
            "green" => Suite::Green,
            "red" => Suite::Red,
            _ => return None,
        })
    }
}

impl Check {
    pub fn opened(read: Option<String>, on: Option<String>) -> Check {
        Check {
            at: util::now_iso(),
            state: State::Running,
            read,
            pane: None,
            on,
            ending: Ending::Standing,
            was: None,
            tries: 1,
            gave_up: false,
            suite: None,
            run: None,
            by: None,
        }
    }

    /// Its mechanical run was started by a process that is no longer there, and
    /// left no result.
    pub fn run_died(&self, start_of: StartOf) -> bool {
        self.suite == Some(Suite::Running) && self.pane.is_none() && self.run.as_ref().is_some_and(|r| r.gone(start_of))
    }

    /// How long this entry waits with no seat before its agent is started
    /// again: after its first start and after its second, and after its last
    /// never. A run known to be dead waits for nothing (while it has tries
    /// left, each retake being one); one whose process is alive, or unknown, is
    /// believed for [`SUITE_WEDGED_AFTER`] at most.
    pub fn wait_before_next_try(&self, start_of: StartOf) -> Option<i64> {
        if self.run_died(start_of) {
            return (self.tries < TRIES).then_some(-1);
        }
        if self.suite == Some(Suite::Running) {
            return Some(SUITE_WEDGED_AFTER);
        }
        wait_after(self.tries)
    }

    /// Still somebody's: opened and not yet settled.
    pub fn open(&self) -> bool {
        self.state == State::Running
    }

    /// A seat that has not been ended.
    pub fn standing(&self) -> Option<&str> {
        self.pane.as_deref().filter(|_| self.ending == Ending::Standing)
    }

    /// An open entry with no agent in it and no more starts to give it.
    pub fn spent(&self) -> bool {
        self.open() && self.pane.is_none() && self.tries >= TRIES
    }
}

/// Whether a token starts an entry: an instant as `now_iso` writes one.
pub fn instant(tok: &str) -> bool {
    let b = tok.as_bytes();
    b.len() == 20 && b[4] == b'-' && b[10] == b'T' && b[19] == b'Z' && b[0].is_ascii_digit()
}

/// One token stream of a `checks:` block, folded into the entries already read
/// — the walk `model::landed_push` does, for the same reason: a continuation
/// line is the same walk run again, so wrapping is free.
pub fn push(acc: &mut Vec<Check>, text: &str) {
    for tok in text.split_whitespace() {
        if tok == "·" {
            continue;
        }
        if instant(tok) {
            acc.push(Check {
                at: tok.to_string(),
                state: State::Running,
                read: None,
                pane: None,
                on: None,
                ending: Ending::Standing,
                was: None,
                tries: 1,
                gave_up: false,
                suite: None,
                run: None,
                by: None,
            });
            continue;
        }
        let Some(c) = acc.last_mut() else { continue };
        match tok.split_once(':') {
            Some(("suite", v)) => c.suite = Suite::parse(v),
            Some(("pid", v)) => c.run = Run::parse(v),
            Some(("by", v)) => c.by = Some(v.to_string()),
            Some(("read", v)) => c.read = Some(v.to_string()),
            Some(("pane", v)) => c.pane = Some(v.to_string()),
            Some(("on", v)) => c.on = Some(v.replace('+', " ")),
            Some(("was", v)) => c.was = Some(v.to_string()),
            Some(("tries", v)) => c.tries = v.parse().unwrap_or(1),
            Some(_) => {}
            None => match tok {
                "gave-up" => c.gave_up = true,
                "ended" => c.ending = Ending::Ended,
                "end-failed" => c.ending = Ending::Failed,
                w => {
                    if let Some(s) = State::parse(w) {
                        c.state = s;
                    }
                }
            },
        }
    }
}

/// A `checks:` block as the file holds it: the inverse of [`push`].
pub fn text(checks: &[Check]) -> String {
    let mut out: Vec<String> = Vec::new();
    for c in checks {
        let mut e = vec![c.at.clone(), c.state.as_str().to_string()];
        if let Some(r) = &c.read {
            e.push(format!("read:{r}"));
        }
        if let Some(p) = &c.pane {
            e.push(format!("pane:{p}"));
        }
        if let Some(o) = &c.on {
            e.push(format!("on:{}", o.split_whitespace().collect::<Vec<_>>().join("+")));
        }
        if let Some(w) = &c.was {
            e.push(format!("was:{w}"));
        }
        if c.tries > 1 {
            e.push(format!("tries:{}", c.tries));
        }
        if c.gave_up {
            e.push("gave-up".into());
        }
        if let Some(s) = c.suite {
            e.push(format!("suite:{}", s.as_str()));
        }
        if let Some(r) = &c.run {
            e.push(format!("pid:{}", r.text()));
        }
        if let Some(b) = &c.by {
            e.push(format!("by:{b}"));
        }
        match c.ending {
            Ending::Standing => {}
            Ending::Ended => e.push("ended".into()),
            Ending::Failed => e.push("end-failed".into()),
        }
        out.push(e.join(" "));
    }
    out.join(" · ")
}

/// How many times an entry's agent is started before the run stops trying and
/// flags the person (`wsp-328`).
pub const TRIES: u32 = 3;

/// How long an open entry waits with no seat after its first start, and after
/// its second, before the agent is started again (`wsp-328`). Thirty seconds
/// first because `wsp spawn` names its seat within seconds of being asked, so
/// an entry still bare at thirty is a start that failed or never arrived, and
/// two minutes after that so a full disk or an overloaded API has room to
/// recover (wsp-427: the three and five minutes this was cost react-pass-1 90
/// minutes on its critical paths).
pub const RETRY_BACKOFF: [i64; 2] = [30, 2 * 60];

/// The wait after `tries` starts, in seconds, and `None` once they are spent.
pub fn wait_after(tries: u32) -> Option<i64> {
    match tries {
        0 => RETRY_BACKOFF.first().copied(),
        n => RETRY_BACKOFF.get(n as usize - 1).copied().filter(|_| n < TRIES),
    }
}

/// And while its suite is running: the longest a barrier's run is believed,
/// the steps' own limits being what end it sooner.
pub const SUITE_WEDGED_AFTER: i64 = 3 * 60 * 60;

