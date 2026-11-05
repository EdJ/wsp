//! The far end of a machine, asked over ssh: the wire version, and whether an
//! agent can actually be run there (`docs/remote-agents.md` sections 2, 3 and 7).
//!
//! Three callers want the same few questions — the daemon's tick (is it up, and
//! at which wire), `spawn --on` (does its wire match mine) and `wsp machine
//! check` (can it start a session and answer a headless `claude`) — so they are
//! asked here, once, and each caller supplies how a command reaches the machine.
//! The real way is [`ssh_run`]; a test passes a closure and needs no machine.
//!
//! **Connected is not ready.** wsp-050's log has a machine reading `connected`
//! through five failed starts, because an ssh that answers says nothing about
//! whether an agent will start. [`Readiness`] keeps the two apart, and only
//! [`check_with`] — which starts a throwaway session and a headless claude —
//! can make a machine [`Readiness::Ready`].

use std::path::Path;
use std::process::{Command, Stdio};

use crate::model::Machine;
use crate::store::MachineLive;

/// What the far `compound-sup` is called when the record names no path: the
/// bare name, found on the far side's `PATH`.
pub const SUP: &str = "compound-sup";

/// The file under `<exec_dir>` that holds the machine's Claude token
/// (`docs/remote-agents.md` section 8): `0600`, produced there by
/// `claude setup-token`, and read only on the far side.
pub const TOKEN_FILE: &str = "claude-token";

/// What the tick writes in `check_error` while it has a check running, so the
/// placeholder reads as "not checked yet" and not as a failed check.
pub const IN_PROGRESS: &str = "check in progress";

/// How long a stale readiness check is trusted before the daemon asks again.
pub const RECHECK_SECS: u64 = 30 * 60;

/// Why a command on the far side did not answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failed {
    /// ssh itself failed (exit 255, or never started): the link is down. This is
    /// the *only* reading that makes a machine unreachable.
    Link(String),
    /// The link is fine and the command ran and failed: the machine is
    /// connected, and not ready.
    Ran(String),
}

impl Failed {
    pub fn why(&self) -> &str {
        match self {
            Failed::Link(s) | Failed::Ran(s) => s,
        }
    }
}

/// Single-quote a word for the far shell, leaving a leading `~/` to be expanded
/// over there — `ssh -L` does not expand it and neither does a quoted word.
pub fn quote(word: &str) -> String {
    let q = |s: &str| format!("'{}'", s.replace('\'', r"'\''"));
    match word.strip_prefix("~/") {
        Some(rest) => format!("~/{}", q(rest)),
        None => q(word),
    }
}

/// The far `compound-sup`: what the record says, or the bare name.
pub fn sup_of(m: &Machine) -> String {
    match m.sup_at.is_empty() {
        true => SUP.to_string(),
        false => m.sup_at.clone(),
    }
}

/// The command the tick runs as its liveness probe, which is also the wire
/// check: `compound-sup wire-version`.
pub fn probe_command(m: &Machine) -> String {
    format!("{} wire-version", quote(&sup_of(m)))
}

/// `compound-sup wire-version` output → the bare number.
pub fn parse_wire(out: &str) -> Option<u32> {
    out.trim().parse().ok()
}

/// Run one command on the machine over ssh and return its stdout.
///
/// `control` is the daemon's ControlMaster socket when one is wanted, so this
/// rides the connection the tunnel already holds. `BatchMode` because nothing
/// here may prompt, and short timeouts because the tick must not hang on a
/// machine that has gone away.
pub fn ssh_run(m: &Machine, control: Option<&Path>, cmd: &str) -> Result<String, Failed> {
    let mut c = Command::new("ssh");
    c.args(["-T", "-o", "BatchMode=yes", "-o", "ConnectTimeout=5"]);
    c.args(["-o", "ServerAliveInterval=5", "-o", "ServerAliveCountMax=2"]);
    if let Some(p) = control {
        c.arg("-o").arg(format!("ControlPath={}", p.display()));
    }
    c.arg(&m.ssh).arg("--").arg(cmd);
    c.stdin(Stdio::null());
    let out = c.output().map_err(|e| Failed::Link(format!("cannot start ssh: {e}")))?;
    let said = |b: &[u8]| String::from_utf8_lossy(b).trim().to_string();
    if out.status.success() {
        return Ok(said(&out.stdout));
    }
    let err = said(&out.stderr);
    let last = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
    match out.status.code() {
        Some(255) | None => Err(Failed::Link(if last.is_empty() { "ssh exited 255".into() } else { last })),
        Some(127) => Err(Failed::Ran(format!("{} not found on {}", sup_of(m), m.ssh))),
        Some(n) => Err(Failed::Ran(if last.is_empty() { format!("exited {n}") } else { last })),
    }
}

/// What the tick learns from one probe, folded into the machine's live record.
///
/// A link failure is *unreachable*; a command that ran and failed is
/// *connected* with the reason in `error` and no wire (so it reads as not
/// ready, never as unreachable — the link is fine).
pub fn fold_probe(live: &mut MachineLive, probed: Result<String, Failed>, now: &str) {
    match probed {
        Ok(out) => match parse_wire(&out) {
            Some(w) => {
                live.reachable = true;
                live.tunnel = "up".into();
                live.last_seen = now.to_string();
                live.wire = Some(w);
                live.error.clear();
            }
            None => {
                live.reachable = true;
                live.tunnel = "up".into();
                live.last_seen = now.to_string();
                live.wire = None;
                live.error = format!("compound-sup wire-version answered `{}`, not a number", out.trim());
            }
        },
        Err(Failed::Ran(why)) => {
            live.reachable = true;
            live.tunnel = "up".into();
            live.last_seen = now.to_string();
            live.wire = None;
            live.error = why;
        }
        Err(Failed::Link(why)) => {
            live.reachable = false;
            live.tunnel = "starting".into();
            live.error = why;
        }
    }
}

/// Where a machine stands, in the four words section 7 gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readiness {
    /// The link is down. Says nothing about the machine.
    Unreachable,
    /// Connected, but the far `compound-sup` did not give a wire version.
    NoWire(String),
    /// Connected, and the far `compound-sup` speaks another version.
    Mismatch { seat: u32, far: u32 },
    /// Connected and the wire matches; `machine check` has not passed yet.
    Unchecked,
    /// `machine check` ran and failed, for this reason.
    NotReady(String),
    /// `machine check` passed.
    Ready,
}

pub fn readiness(l: &MachineLive, seat_wire: u32) -> Readiness {
    if !l.reachable {
        return Readiness::Unreachable;
    }
    match l.wire {
        None => Readiness::NoWire(l.error.clone()),
        Some(w) if w != seat_wire => Readiness::Mismatch { seat: seat_wire, far: w },
        Some(_) if l.checked.is_empty() => Readiness::Unchecked,
        Some(_) if !l.ready && l.check_error == IN_PROGRESS => Readiness::Unchecked,
        Some(_) if !l.ready => Readiness::NotReady(l.check_error.clone()),
        Some(_) => Readiness::Ready,
    }
}

impl Readiness {
    /// The sentence for a wire mismatch, naming both numbers and which binary is
    /// behind. The one place it is worded, so `spawn --on`, `machine ls` and
    /// `doctor` cannot disagree about it.
    pub fn mismatch_words(machine: &str, seat: u32, far: u32) -> String {
        let behind = match far < seat {
            true => format!("the far compound-sup is behind — rebuild compound-sup and compound-render on {machine} from the seat's compound commit"),
            false => "this seat's wsp is behind — bump WIRE_VERSION in src/place_compound.rs".to_string(),
        };
        format!("wire mismatch v{seat}/v{far}: this seat speaks {seat}, {machine}'s compound-sup speaks {far} — {behind}")
    }
}

/// One step of a check, as the person reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub name: &'static str,
    pub ok: bool,
    pub said: String,
}

#[derive(Debug, Clone, Default)]
pub struct Check {
    pub wire: Option<u32>,
    pub steps: Vec<Step>,
}

impl Check {
    pub fn passed(&self) -> bool {
        !self.steps.is_empty() && self.steps.iter().all(|s| s.ok)
    }

    /// The first failing step, as the reason a machine is not ready.
    pub fn why(&self) -> String {
        self.steps.iter().find(|s| !s.ok).map(|s| format!("{}: {}", s.name, s.said)).unwrap_or_default()
    }
}

/// The headless claude's one line. A model answering at all proves the token,
/// the install and the network; the word is only there to make the reply short.
const PROMPT: &str = "Reply with the single word ok.";

/// Run the whole check through `run`, which sends one command line to the
/// machine and returns its stdout.
///
/// In order, stopping at the first failure: the wire matches; the far
/// `compound-sup` starts a throwaway session in `exec_dir` and stops it again;
/// a one-line headless `claude -p` answers with the machine's token. The token
/// is expanded on the far side (`$(cat …)` is inside the command string, which
/// is quoted here and not expanded locally), so it never crosses the link and
/// is in no output this returns.
pub fn check_with(m: &Machine, seat_wire: u32, run: &dyn Fn(&str) -> Result<String, Failed>) -> Check {
    let mut c = Check::default();
    let step = |c: &mut Check, name: &'static str, r: Result<String, String>| -> bool {
        let (ok, said) = match r {
            Ok(s) => (true, s),
            Err(s) => (false, s),
        };
        c.steps.push(Step { name, ok, said });
        ok
    };

    // 1. the wire.
    let wire = match run(&probe_command(m)) {
        Ok(out) => parse_wire(&out).ok_or_else(|| format!("`{}` is not a wire version", out.trim())),
        Err(f) => Err(f.why().to_string()),
    };
    match wire {
        Ok(w) => {
            c.wire = Some(w);
            if w != seat_wire {
                let words = Readiness::mismatch_words(&m.name, seat_wire, w);
                step(&mut c, "wire", Err(words));
                return c;
            }
            step(&mut c, "wire", Ok(format!("v{w}")));
        }
        Err(e) => {
            step(&mut c, "wire", Err(e));
            return c;
        }
    }

    if m.exec_dir.is_empty() {
        step(&mut c, "session", Err(format!("no exec_dir — wsp machine set {} exec_dir=…", m.name)));
        return c;
    }

    // 2. a throwaway session, started and stopped. `sleep` is the agent: the
    // question is whether the supervisor starts a pty here at all.
    let label = format!("wsp-check-{}", std::process::id());
    let start = format!(
        "{} spawn --label {} --cwd {} -- sleep 60",
        quote(&sup_of(m)),
        quote(&label),
        quote(&m.exec_dir),
    );
    let pid = match run(&start) {
        Ok(out) => pid_of(&out).ok_or_else(|| "compound-sup spawn gave no `pid` line".to_string()),
        Err(f) => Err(f.why().to_string()),
    };
    let pid = match pid {
        Ok(p) => p,
        Err(e) => {
            step(&mut c, "session", Err(e));
            return c;
        }
    };
    // `compound-sup` is its own process-group leader (it `setsid`s), so the
    // group is the whole session.
    let stopped = run(&format!("kill -TERM -- -{pid}"));
    step(
        &mut c,
        "session",
        match stopped {
            Ok(_) => Ok(format!("started and stopped (pid {pid})")),
            Err(f) => Err(format!("started (pid {pid}) but would not stop: {}", f.why())),
        },
    );
    if !c.passed() {
        return c;
    }

    // 3. a headless claude, with the machine's token.
    let token = format!("{}/{TOKEN_FILE}", m.exec_dir.trim_end_matches('/'));
    let claude = format!(
        "CLAUDE_CODE_OAUTH_TOKEN=\"$(cat {})\" claude -p {}",
        quote(&token),
        quote(PROMPT),
    );
    step(
        &mut c,
        "claude",
        match run(&claude) {
            Ok(out) if out.trim().is_empty() => Err("claude -p answered nothing".into()),
            Ok(_) => Ok("answered".into()),
            Err(f) => Err(f.why().to_string()),
        },
    );
    c
}

/// The `pid <n>` line of `compound-sup spawn`'s handshake. (`sup <pid>` is the
/// same number; `pid` is the one that is the group leader.)
fn pid_of(out: &str) -> Option<u32> {
    out.lines().find_map(|l| l.strip_prefix("pid ")?.trim().parse().ok())
}

/// Fold a finished check into the live record, for `machines.json`.
pub fn record(live: &mut MachineLive, c: &Check, now: &str) {
    live.checked = now.to_string();
    live.ready = c.passed();
    live.check_error = if c.passed() { String::new() } else { c.why() };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn machine() -> Machine {
        let mut m = Machine::new("mb2", "mb2");
        m.exec_dir = "~/wsp-exec".into();
        m.sup_at = "~/bin/compound-sup".into();
        m
    }

    /// A far side that answers from a script, and remembers what it was asked.
    fn far<'a>(
        asked: &'a RefCell<Vec<String>>,
        wire: &'a str,
        spawn: Result<&'a str, &'a str>,
        claude: Result<&'a str, &'a str>,
    ) -> impl Fn(&str) -> Result<String, Failed> + 'a {
        move |cmd: &str| {
            asked.borrow_mut().push(cmd.to_string());
            let ran = |r: Result<&str, &str>| r.map(String::from).map_err(|e| Failed::Ran(e.into()));
            if cmd.contains("wire-version") {
                Ok(wire.into())
            } else if cmd.contains(" spawn ") {
                ran(spawn)
            } else if cmd.starts_with("kill") {
                Ok(String::new())
            } else {
                ran(claude)
            }
        }
    }

    #[test]
    fn the_probe_is_compound_sups_wire_version_by_the_recorded_path() {
        assert_eq!(probe_command(&machine()), "~/'bin/compound-sup' wire-version");
        assert_eq!(probe_command(&Machine::new("a", "a")), "'compound-sup' wire-version");
        assert_eq!(parse_wire(" 9\n"), Some(9));
        assert_eq!(parse_wire("compound-sup: unknown verb"), None);
    }

    #[test]
    fn a_check_that_passes_starts_and_stops_a_session_and_asks_claude_with_the_token() {
        let asked = RefCell::new(Vec::new());
        let m = machine();
        let c = check_with(&m, 9, &far(&asked, "9", Ok("sup 4242\npid 4242\nsocket /x"), Ok("ok")));
        assert!(c.passed(), "{:?}", c.steps);
        assert_eq!(c.wire, Some(9));
        let asked = asked.borrow();
        assert_eq!(asked.len(), 4, "wire, start, stop, claude: {asked:?}");
        assert!(asked[2].contains("-4242"), "the session's group is stopped: {}", asked[2]);
        // The token is read on the far side, by the far shell, from the file.
        assert!(asked[3].contains("$(cat ~/'wsp-exec/claude-token')"), "{}", asked[3]);
        assert!(asked[3].contains("CLAUDE_CODE_OAUTH_TOKEN"), "{}", asked[3]);
        assert!(!asked[3].contains("--bare"), "--bare skips the hooks the brief arrives by");
    }

    #[test]
    fn connected_is_not_ready_each_failure_names_its_step() {
        let m = machine();
        let none = RefCell::new(Vec::new());

        // wire: both numbers, and which side is behind.
        let c = check_with(&m, 9, &far(&none, "8", Ok("pid 1"), Ok("ok")));
        assert!(!c.passed());
        assert!(c.why().contains("v9/v8") && c.why().contains("behind"), "{}", c.why());
        assert_eq!(none.borrow().len(), 1, "nothing is started on a machine that speaks another wire");

        // the session will not start.
        let c = check_with(&m, 9, &far(&none, "9", Err("no such cwd"), Ok("ok")));
        assert!(c.why().starts_with("session: no such cwd"), "{}", c.why());

        // no auth: the headless claude fails.
        let c = check_with(&m, 9, &far(&none, "9", Ok("pid 7"), Err("Invalid API key")));
        assert!(c.why().starts_with("claude: Invalid API key"), "{}", c.why());
        assert!(c.steps[1].ok, "and the session before it did pass");

        // no exec_dir to start it in.
        let mut bare = m.clone();
        bare.exec_dir.clear();
        let c = check_with(&bare, 9, &far(&none, "9", Ok("pid 7"), Ok("ok")));
        assert!(c.why().contains("exec_dir"), "{}", c.why());
    }

    #[test]
    fn a_machine_reads_ready_only_when_the_check_passed() {
        let mut l = MachineLive { reachable: true, wire: Some(9), ..Default::default() };
        assert_eq!(readiness(&l, 9), Readiness::Unchecked, "connected, and wire matching, is not ready");
        assert_eq!(readiness(&l, 10), Readiness::Mismatch { seat: 10, far: 9 });

        let failed = Check { wire: Some(9), steps: vec![Step { name: "claude", ok: false, said: "no auth".into() }] };
        record(&mut l, &failed, "2026-11-05T10:00:00Z");
        assert_eq!(readiness(&l, 9), Readiness::NotReady("claude: no auth".into()));

        let passed = Check { wire: Some(9), steps: vec![Step { name: "wire", ok: true, said: "v9".into() }] };
        record(&mut l, &passed, "2026-11-05T10:01:00Z");
        assert_eq!(readiness(&l, 9), Readiness::Ready);
        assert_eq!(readiness(&l, 10), Readiness::Mismatch { seat: 10, far: 9 }, "a passed check does not outlive a wire change");

        l.reachable = false;
        assert_eq!(readiness(&l, 9), Readiness::Unreachable);
    }

    /// The probe tells a dead link from a live link with a broken far side, and
    /// only the first is unreachable.
    #[test]
    fn a_link_that_is_down_is_unreachable_and_a_command_that_failed_is_connected() {
        let mut l = MachineLive::default();
        fold_probe(&mut l, Ok("9".into()), "now");
        assert!(l.reachable && l.wire == Some(9) && l.error.is_empty());

        fold_probe(&mut l, Err(Failed::Ran("compound-sup not found on mb2".into())), "later");
        assert!(l.reachable, "the link is fine");
        assert_eq!(l.wire, None);
        assert!(matches!(readiness(&l, 9), Readiness::NoWire(e) if e.contains("not found")));

        fold_probe(&mut l, Err(Failed::Link("Connection refused".into())), "later");
        assert!(!l.reachable);
        assert_eq!(l.last_seen, "later", "last seen is when it last answered, not when it last failed");
    }
}
