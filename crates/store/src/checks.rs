//! One check on one barrier, as a worklist's `checks:` block holds it: the record the model reads and writes.
//! The run that opens, reads and ends checks is `barrier`.

use crate::util;
use std::path::PathBuf;


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
    /// The process making the mechanical run, while `suite` is running, and the
    /// lease it beats. A run that is killed leaves its entry at
    /// `suite:running` and its lease untouched; this is how that is seen.
    pub run: Option<Run>,
    /// Who settled it, when it was not an agent: `wsp`.
    pub by: Option<String>,
}

/// The process of a barrier's mechanical run and its lease (wsp-057):
/// `pid:25037@lease=25037-1763639349.run`. The run beats its lease
/// ([`crate::lease::Lease`]) while it lives, so whether it is gone is the
/// lease's age and no process is asked. An entry an older wsp wrote,
/// `pid:25037@<lstart>`, names no lease and reads as no run: believed for
/// [`SUITE_WEDGED_AFTER`] at most.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub pid: u32,
    /// The lease file's name, in [`Run::dir`]. A name and not a path, because
    /// the `checks:` block wraps at a line's width and splits on whitespace,
    /// and a whole path would not survive it.
    pub lease: String,
}

impl Run {
    /// Where every barrier run's lease is: `<state>/barrier/`, the state being
    /// `WSP_STATE`'s or the default one, as a store opened here reads it.
    pub fn dir() -> PathBuf {
        std::env::var_os("WSP_STATE").map(PathBuf::from).unwrap_or_else(crate::store::Store::default_state).join("barrier")
    }

    /// Where a new run of this process keeps its lease, for
    /// [`crate::lease::Lease::take`].
    pub fn lease_for_this() -> PathBuf {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        Self::dir().join(format!("{}-{secs}.run", std::process::id()))
    }

    /// The running process's own, beating `lease`. `None` for a lease that is
    /// not in [`Run::dir`] or whose name an entry could not hold.
    pub fn this(lease: &crate::lease::Lease) -> Option<Run> {
        let path = lease.path();
        let name = path.file_name()?.to_str()?;
        let ok = path.parent() == Some(Self::dir().as_path()) && !name.chars().any(char::is_whitespace);
        ok.then(|| Run { pid: std::process::id(), lease: name.to_string() })
    }

    fn parse(v: &str) -> Option<Run> {
        let (pid, lease) = v.split_once('@')?;
        let lease = lease.strip_prefix("lease=")?;
        if lease.is_empty() || lease.contains('/') {
            return None;
        }
        Some(Run { pid: pid.parse().ok()?, lease: lease.to_string() })
    }

    fn text(&self) -> String {
        format!("{}@lease={}", self.pid, self.lease)
    }

    /// Whether the run is gone: its lease is not there, or was last touched
    /// longer than [`crate::lease::GRACE`] ago.
    pub fn gone(&self) -> bool {
        !crate::lease::fresh(&Self::dir().join(&self.lease), crate::lease::GRACE)
    }

    /// Removes the lease of a run that is gone: what a retake leaves no more.
    pub fn forget(&self) {
        if self.gone() {
            let _ = std::fs::remove_file(Self::dir().join(&self.lease));
        }
    }
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
    pub fn run_died(&self) -> bool {
        self.suite == Some(Suite::Running) && self.pane.is_none() && self.run.as_ref().is_some_and(Run::gone)
    }

    /// How long this entry waits with no seat before its agent is started
    /// again: after its first start and after its second, and after its last
    /// never. A run known to be dead waits for nothing (while it has tries
    /// left, each retake being one); one whose process is alive, or unknown, is
    /// believed for [`SUITE_WEDGED_AFTER`] at most.
    pub fn wait_before_next_try(&self) -> Option<i64> {
        if self.run_died() {
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


#[cfg(test)]
mod tests {
    use super::*;

    /// wsp-057: a run is its lease, and reads back as written; an entry
    /// that names an lstart where its lease would be is no run.
    #[test]
    fn a_run_is_known_by_its_lease_and_not_by_a_start_time() {
        let _iso = crate::util::isolated("checks-run-lease");
        let lease = crate::lease::Lease::take(Run::lease_for_this(), crate::lease::GRACE).unwrap();
        let run = Run::this(&lease).unwrap();
        assert!(run.text().len() < 50, "short enough for one wrapped line: {}", run.text());
        assert_eq!(Run::parse(&run.text()), Some(run.clone()), "it reads back as written");
        assert!(!run.gone(), "its holder beats it");
        drop(lease);
        assert!(run.gone(), "the lease went with its holder");
        assert_eq!(Run::parse("25037@Tue_Nov_4_11:19:05_2026"), None, "an older entry names no lease");
        assert_eq!(Run::parse("25037@lease=../x"), None, "a name, never a path");
        let elsewhere = crate::lease::Lease::take(_iso.path("other/1.run"), crate::lease::GRACE).unwrap();
        assert_eq!(Run::this(&elsewhere), None, "a lease outside the barrier's directory");
    }
}
