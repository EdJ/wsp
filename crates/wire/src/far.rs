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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use wsp_base::calls::Command;
use std::time::{Duration, Instant};

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

/// `compound-sup wire-version` output → the range it accepts, `(OLDEST,
/// VERSION)`. A current build prints `OLDEST VERSION` (`9 14`); an older build
/// prints a bare number, which is the range `VERSION VERSION`. Anything else
/// (a word, a third token, OLDEST above VERSION) is no answer.
pub fn parse_wire(out: &str) -> Option<(u32, u32)> {
    let toks: Vec<u32> = out.split_whitespace().map(|t| t.parse().ok()).collect::<Option<_>>()?;
    match toks[..] {
        [v] => Some((v, v)),
        [o, v] if o <= v => Some((o, v)),
        _ => None,
    }
}

/// Does the machine's recorded range hold the seat's wire version? A record
/// with no `wire_oldest` (a bare version, or one written before the range was
/// kept) accepts only its own version.
pub fn accepts(l: &MachineLive, seat_wire: u32) -> bool {
    match l.wire {
        Some(top) => l.wire_oldest.unwrap_or(top) <= seat_wire && seat_wire <= top,
        None => false,
    }
}

/// How long the probe may take: it runs inside the daemon's tick.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
/// The session step: start, read the handshake, stop.
pub const SESSION_TIMEOUT: Duration = Duration::from_secs(30);
/// The headless claude. A model call over a bad network must end the check
/// rather than hang it (and with it the tick's `checking` child, which would
/// then never recheck).
pub const CLAUDE_TIMEOUT: Duration = Duration::from_secs(120);

/// Run one command on the machine over ssh and return its stdout, giving up
/// after `timeout`.
///
/// `control` is the daemon's ControlMaster socket when one is wanted, so this
/// rides the connection the tunnel already holds. `BatchMode` because nothing
/// here may prompt, and short timeouts because the tick must not hang on a
/// machine that has gone away. A command still running at the deadline is
/// killed and reported as [`Failed::Ran`] (the link answered; the command did
/// not finish).
pub fn ssh_run(m: &Machine, control: Option<&Path>, cmd: &str, timeout: Duration) -> Result<String, Failed> {
    ssh_run_with("ssh", m, control, cmd, timeout)
}

/// [`ssh_run`] with the ssh program named, so a test can pass a script.
pub fn ssh_run_with(ssh: &str, m: &Machine, control: Option<&Path>, cmd: &str, timeout: Duration) -> Result<String, Failed> {
    let mut c = Command::new(ssh);
    c.args(["-T", "-o", "BatchMode=yes", "-o", "ConnectTimeout=5"]);
    c.args(["-o", "ServerAliveInterval=5", "-o", "ServerAliveCountMax=2"]);
    if let Some(p) = control {
        c.arg("-o").arg(format!("ControlPath={}", p.display()));
    }
    c.arg(&m.ssh).arg("--").arg(cmd);
    let out = run_bounded(c, timeout).map_err(|e| Failed::Link(format!("cannot start ssh: {e}")))?;
    let Some(out) = out else {
        return Err(Failed::Ran(format!("no answer in {}s", timeout.as_secs())));
    };
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

/// Whether ssh's words say the far machine was never reached: the TCP connect
/// itself failed (`connect to host … : Operation timed out`, `No route to host`,
/// `Connection refused`, `Network is unreachable`) or the name did not resolve.
/// That is a dead link. Anything else — a banner that came late, a server that
/// stopped answering keepalives, a master still connecting, a command that did
/// not finish — means a far side that a packet reached, which on a loaded
/// machine (load 34 on the i5, wsp-561) is a slow answer and not a lost one.
pub fn dead_link(why: &str) -> bool {
    let w = why.to_ascii_lowercase();
    [
        "connect to host",
        "could not resolve hostname",
        "no route to host",
        "network is unreachable",
        "network is down",
        "connection refused",
    ]
        .iter()
        .any(|k| w.contains(k))
}

/// `Command::output` with a deadline: `None` when it was still running at the
/// deadline and was killed. The pipes are drained on threads so a chatty child
/// cannot fill one and stall.
pub fn run_bounded(mut c: Command, timeout: Duration) -> std::io::Result<Option<std::process::Output>> {
    use std::io::Read;
    c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn()?;
    let drain = |mut r: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = r.read_to_end(&mut buf);
            buf
        })
    };
    let out = drain(Box::new(child.stdout.take().expect("piped")));
    let err = drain(Box::new(child.stderr.take().expect("piped")));
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break st;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    Ok(Some(std::process::Output {
        status,
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
    }))
}

/// What the tick learns from one probe, folded into the machine's live record.
///
/// A link failure is *unreachable*; a command that ran and failed is
/// *connected* with the reason in `error` and no wire (so it reads as not
/// ready, never as unreachable — the link is fine).
pub fn fold_probe(live: &mut MachineLive, probed: Result<String, Failed>, now: &str) {
    match probed {
        Ok(out) => match parse_wire(&out) {
            Some((o, w)) => {
                live.reachable = true;
                live.tunnel = "up".into();
                live.last_seen = now.to_string();
                live.wire = Some(w);
                live.wire_oldest = Some(o);
                live.error.clear();
            }
            None => {
                live.reachable = true;
                live.tunnel = "up".into();
                live.last_seen = now.to_string();
                live.wire = None;
                live.wire_oldest = None;
                live.error = format!("compound-sup wire-version answered `{}`, not a version range", out.trim());
            }
        },
        Err(Failed::Ran(why)) => {
            live.reachable = true;
            live.tunnel = "up".into();
            live.last_seen = now.to_string();
            live.wire = None;
            live.wire_oldest = None;
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
    Mismatch { seat: u32, oldest: u32, far: u32 },
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
        Some(w) if !accepts(l, seat_wire) => Readiness::Mismatch { seat: seat_wire, oldest: l.wire_oldest.unwrap_or(w), far: w },
        Some(_) if l.checked.is_empty() => Readiness::Unchecked,
        Some(_) if !l.ready && l.check_error == IN_PROGRESS => Readiness::Unchecked,
        Some(_) if !l.ready => Readiness::NotReady(l.check_error.clone()),
        Some(_) => Readiness::Ready,
    }
}

impl Readiness {
    /// The sentence for a wire mismatch, naming the seat's number, the far
    /// range, and which binary is behind (seat below OLDEST: this seat's wsp;
    /// above VERSION: the far compound-sup). The one place it is worded, so
    /// `spawn --on`, `machine ls` and `doctor` cannot disagree about it.
    pub fn mismatch_words(machine: &str, seat: u32, oldest: u32, far: u32) -> String {
        let behind = match seat < oldest {
            false => format!("the far compound-sup is behind — rebuild compound-sup and compound-render on {machine} from the seat's compound commit"),
            true => "this seat's wsp is behind — bump WIRE_VERSION in src/place_compound.rs".to_string(),
        };
        let range = match oldest == far {
            true => format!("{far}"),
            false => format!("{oldest} to {far}"),
        };
        format!("wire mismatch v{seat}/v{range}: this seat speaks {seat}, {machine}'s compound-sup accepts {range} — {behind}")
    }
}

/// The machines that said nothing, each under its own name, as a census.
///
/// `None` when no active machine is silent — the common case, and every seat
/// that has no machine. **Only the silent are filed.** A machine that is
/// reachable is not claimed to be empty (that is `Census::heard(m, vec![])`,
/// a statement a reap would act on, and this row does not ask the far
/// supervisor what it holds); it is simply absent from the census, so nothing
/// reaps against it. A machine whose silence is filed under `""` instead would
/// read as *this* machine being silent, which a host drawing this machine's
/// workspace takes as no census here: hence the name is the machine's, always.
pub fn silent_machines(
    machines: &[Machine],
    live: &std::collections::BTreeMap<String, MachineLive>,
) -> Option<crate::place::Census> {
    use crate::place::{Census, Refusal};
    let mut out: Option<Census> = None;
    for m in machines.iter().filter(|m| m.is_active() && !m.name.is_empty()) {
        let why = match live.get(&m.name) {
            None => "no daemon report yet".to_string(),
            Some(l) if l.reachable => continue,
            Some(l) => {
                let seen = match l.last_seen.as_str() {
                    "" => "never seen".to_string(),
                    at => format!("last seen {}s ago", crate::util::since(at)),
                };
                match l.error.is_empty() {
                    true => format!("not answering ({seen})"),
                    false => format!("{} ({seen})", l.error),
                }
            }
        };
        let c = Census::silent(&m.name, Refusal::Unreachable(why));
        out = Some(match out {
            None => c,
            Some(prev) => prev.and(c),
        });
    }
    out
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
    pub wire_oldest: Option<u32>,
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
pub fn check_with(m: &Machine, seat_wire: u32, run: &dyn Fn(&str, Duration) -> Result<String, Failed>) -> Check {
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
    let wire = match run(&probe_command(m), PROBE_TIMEOUT) {
        Ok(out) => parse_wire(&out).ok_or_else(|| format!("`{}` is not a wire version", out.trim())),
        Err(f) => Err(f.why().to_string()),
    };
    match wire {
        Ok((o, w)) => {
            c.wire = Some(w);
            c.wire_oldest = Some(o);
            if seat_wire < o || seat_wire > w {
                let words = Readiness::mismatch_words(&m.name, seat_wire, o, w);
                step(&mut c, "wire", Err(words));
                return c;
            }
            step(&mut c, "wire", Ok(match o == w {
                true => format!("v{w}"),
                false => format!("v{seat_wire} in {o} to {w}"),
            }));
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
    let sock = match run(&start_command(m, &label), SESSION_TIMEOUT) {
        Ok(out) => socket_of(&out).ok_or_else(|| format!("compound-sup spawn gave no `socket` line: {}", out.trim())),
        Err(f) => Err(f.why().to_string()),
    };
    let sock = match sock {
        Ok(p) => p,
        Err(e) => {
            step(&mut c, "session", Err(e));
            return c;
        }
    };
    // `close <socket>` is the supervisor's own way to end a session, and ends
    // the agent with it; no pid or process group is guessed at from here.
    let stopped = run(&format!("{} close {}", quote(&sup_of(m)), quote(&sock)), SESSION_TIMEOUT);
    step(
        &mut c,
        "session",
        match stopped {
            Ok(_) => Ok(format!("started and stopped ({sock})")),
            Err(f) => Err(format!("started ({sock}) but would not stop: {}", f.why())),
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
        match run(&claude, CLAUDE_TIMEOUT) {
            Ok(out) if out.trim().is_empty() => Err("claude -p answered nothing".into()),
            Ok(_) => Ok("answered".into()),
            Err(f) => Err(f.why().to_string()),
        },
    );
    c
}

/// The far command that starts the throwaway session and returns once the
/// handshake is read.
///
/// `compound-sup spawn` prints `sup`/`pid`/`socket`/`render` and then keeps the
/// stdout it inherited open for the whole life of the session (which is why
/// `place_compound::read_handshake` reads with a timeout and never to EOF). Run
/// under ssh with its stdout on the channel, the call would not return until the
/// session ended. So its stdout and stderr go to a file, in a backgrounded
/// subshell with stdin on `/dev/null`, nothing holds the channel, and the shell
/// polls the file for the `socket` line (up to 10 s) and prints it.
/// `--no-renderer`: the question here is whether a pty starts, and a renderer
/// started over ssh is outside the GUI session anyway.
pub fn start_command(m: &Machine, label: &str) -> String {
    launch(m, label, &m.exec_dir, "", "", " --no-renderer", "sleep 60")
}

/// The one shape of far start: `before` runs in the far shell first, `pre` is
/// what stands in front of `compound-sup` inside the subshell (environment),
/// `flags` follow `--cwd`, and `tail` is the pty's own argv.
fn launch(m: &Machine, label: &str, cwd: &str, before: &str, pre: &str, flags: &str, tail: &str) -> String {
    format!(
        "{before}f=$(mktemp) && ( {pre}{sup} spawn --label {label} --cwd {cwd}{flags} -- {tail} </dev/null >\"$f\" 2>&1 & ); \
         i=0; while [ $i -lt 100 ] && ! grep -q '^socket ' \"$f\"; do sleep 0.1; i=$((i+1)); done; \
         cat \"$f\"; rm -f \"$f\"",
        sup = quote(&sup_of(m)),
        label = quote(label),
        cwd = quote(cwd),
    )
}

/// Where the machine's Claude token is, as a path on that machine.
pub fn token_path(m: &Machine) -> String {
    format!("{}/{TOKEN_FILE}", m.exec_dir.trim_end_matches('/'))
}

/// The far command that starts a seat's agent (`docs/remote-agents.md` sections
/// 2 and 8): `compound-sup spawn --label <seat> --cwd <far path> -- <agent>`,
/// with the Claude token exported **on the far side**.
///
/// The token is read from its `0600` file by the far shell (`t=$(cat …)` is in
/// the command string, quoted here and never expanded here), so it does not cross
/// the link, is in no argv and is in no wsp log: what this returns holds the
/// file's path and `"$t"`, and nothing else. A missing or empty file stops the
/// command with a sentence that says what to do, before a session is started that
/// could never answer.
///
/// `env` rides in front of `compound-sup` as `env K=V`, and an empty value is
/// `env -u K` — the same "override, never replace" contract the local backend
/// keeps. Names that are not shell identifiers are dropped, and so is `PATH`: the
/// far agent's is the far machine's own with `<exec_dir>/bin` in front.
pub fn seat_command(m: &Machine, label: &str, cwd: &str, env: &BTreeMap<String, String>, program: &str, args: &[String]) -> String {
    let token = token_path(m);
    let msg = format!("no Claude token at {token} on {} (claude setup-token there)", m.name);
    let before = format!("t=$(cat {} 2>/dev/null) && [ -n \"$t\" ] || {{ echo {} >&2; exit 1; }}; ", quote(&token), quote(&msg));
    // The executor's `bin` (the `wsp` shim and `wsp-trust`) first on the agent's
    // PATH, whatever else the machine has installed (`wsp-563`). Set in the far
    // shell, where an assignment expands a leading `~/`. A PATH in `env` below
    // is the controller's own (`throttle::seat_env` sends `<shims>:<its PATH>`),
    // meaningless on this machine and able to override the shim, so it is never
    // sent: the far seat loses the cargo throttle shim, deliberately.
    let before = format!("{before}{}; ", path_with_shim(m));
    let mut pre = String::from("CLAUDE_CODE_OAUTH_TOKEN=\"$t\" env ");
    // `env` stops reading options at the first NAME=value (POSIX, and macOS's), so
    // every `-u K` goes before any `K=V`.
    let named: Vec<(&String, &String)> = env
        .iter()
        .filter(|(k, _)| {
            let ident = !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && !k.starts_with(|c: char| c.is_ascii_digit());
            ident && k.as_str() != "CLAUDE_CODE_OAUTH_TOKEN" && k.as_str() != "PATH"
        })
        .collect();
    for (k, _) in named.iter().filter(|(_, v)| v.is_empty()) {
        pre.push_str(&format!("-u {k} "));
    }
    for (k, v) in named.iter().filter(|(_, v)| !v.is_empty()) {
        pre.push_str(&format!("{k}={} ", quote_plain(v)));
    }
    let mut tail = quote_plain(program);
    for a in args {
        tail.push(' ');
        tail.push_str(&quote_plain(a));
    }
    launch(m, label, cwd, &before, &pre, "", &tail)
}

/// The far shell assignment that puts `<exec_dir>/bin` first on PATH.
pub fn path_with_shim(m: &Machine) -> String {
    format!("PATH={}:\"$PATH\"; export PATH", quote(&shim_dir(m)))
}

/// `<exec_dir>/bin`: where the `wsp` shim and `wsp-trust` live on the machine.
pub fn shim_dir(m: &Machine) -> String {
    format!("{}/bin", m.exec_dir.trim_end_matches('/'))
}

/// The far command that says which `wsp` the machine's default PATH finds:
/// `ok` (the shim, or none at all), or `shadow <path>`.
pub fn shim_probe_command(m: &Machine) -> String {
    format!(
        "s={shim}; w=$(command -v wsp 2>/dev/null); \
         if [ -z \"$w\" ] || [ \"$w\" = \"$s\" ]; then echo ok; else echo \"shadow $w\"; fi",
        shim = quote(&format!("{}/wsp", shim_dir(m))),
    )
}

/// What the probe's answer means: `Some(words)` when another `wsp` is first on
/// the far default PATH. A warning, not a failed step: a seat's own PATH puts
/// the shim first. `None` for the shim, for none, and for an answer that is not
/// understood.
pub fn shim_shadow(m: &Machine, out: &str) -> Option<String> {
    let path = out.lines().find_map(|l| l.trim().strip_prefix("shadow "))?.trim();
    (!path.is_empty()).then(|| {
        format!(
            "{path} is first on {}'s default PATH, ahead of {}/wsp — a seat is started with {} first, but a shell there runs {path}; move it aside",
            m.name,
            shim_dir(m),
            shim_dir(m),
        )
    })
}

/// Ask the machine which `wsp` it finds, and say what shadows the shim, if
/// anything. A machine that cannot be asked says nothing.
pub fn shim_check(m: &Machine, run: &dyn Fn(&str, Duration) -> Result<String, Failed>) -> Option<String> {
    if m.exec_dir.is_empty() {
        return None;
    }
    run(&shim_probe_command(m), PROBE_TIMEOUT).ok().and_then(|out| shim_shadow(m, &out))
}

/// The helper that sets or clears Claude Code's folder trust for one directory
/// in the far `~/.claude.json` (`docs/remote-agents.md` section 8).
///
/// **Trust on `<exec_dir>/trees` did not cover the row's tree** (`wsp-445`): a
/// far claude started in `trees/<row>` stopped at "Quick safety check", whose
/// default is "No, exit", so nothing was typed and the order never landed. The
/// row's own tree is therefore trusted by name, before claude starts.
///
/// A shell script that hands the edit to `python3` or else `node`, so no binary
/// of ours has to be built for the far platform. The file is read, one key
/// changed, and written beside itself and renamed over, so a claude writing it
/// at the same moment sees the old or the new and never half. A file that is not
/// a JSON object is left alone and the helper fails, as `settings_with_gate`
/// does. No single quote appears in either program, because each sits in a
/// single-quoted shell string.
const TRUST_HELPER: &str = r#"#!/bin/sh
# wsp-trust add|remove <dir>: Claude Code's folder trust for <dir>, in ~/.claude.json
op=$1; dir=$2
case "$op" in add|remove) ;; *) echo "usage: wsp-trust add|remove <dir>" >&2; exit 2;; esac
[ -n "$dir" ] || { echo "wsp-trust: no directory" >&2; exit 2; }
file="${CLAUDE_CONFIG_DIR:-$HOME}/.claude.json"
PY='
import json, os, sys, tempfile
op, d, f = sys.argv[1:4]
try:
    with open(f) as h:
        doc = json.load(h)
except FileNotFoundError:
    doc = {}
if not isinstance(doc, dict):
    sys.exit("wsp-trust: " + f + " is not a JSON object; not touching it")
projects = doc.setdefault("projects", {})
if not isinstance(projects, dict):
    sys.exit("wsp-trust: projects in " + f + " is not an object; not touching it")
for n in {d, os.path.realpath(d)}:
    if op == "add":
        entry = projects.setdefault(n, {})
        entry["hasTrustDialogAccepted"] = True
        # The prompt that follows the trust dialog when CLAUDE.md imports a file outside
        # the tree: acknowledged with the trust, so no person is asked.
        entry["hasClaudeMdExternalIncludesApproved"] = True
        entry["hasClaudeMdExternalIncludesWarningShown"] = True
    else:
        projects.pop(n, None)
mode = 0o600
try:
    mode = os.stat(f).st_mode & 0o777
except OSError:
    pass
fd, tmp = tempfile.mkstemp(prefix=".claude.json.", dir=os.path.dirname(f) or ".")
with os.fdopen(fd, "w") as h:
    json.dump(doc, h, indent=2)
os.chmod(tmp, mode)
os.replace(tmp, f)
'
JS='
const fs = require("fs"), path = require("path");
const [op, d, f] = process.argv.slice(1);
let doc = {};
try { doc = JSON.parse(fs.readFileSync(f, "utf8")); } catch (e) { if (e.code !== "ENOENT") { console.error("wsp-trust: " + f + " is not JSON; not touching it"); process.exit(1); } }
if (doc === null || typeof doc !== "object" || Array.isArray(doc)) { console.error("wsp-trust: " + f + " is not a JSON object; not touching it"); process.exit(1); }
if (!doc.projects || typeof doc.projects !== "object") doc.projects = {};
let real = d; try { real = fs.realpathSync(d); } catch (e) {}
for (const n of new Set([d, real])) {
  if (op === "add") { doc.projects[n] = doc.projects[n] || {}; Object.assign(doc.projects[n], { hasTrustDialogAccepted: true, hasClaudeMdExternalIncludesApproved: true, hasClaudeMdExternalIncludesWarningShown: true }); }
  else delete doc.projects[n];
}
let mode = 0o600; try { mode = fs.statSync(f).mode & 0o777; } catch (e) {}
const tmp = path.join(path.dirname(f), ".claude.json." + process.pid);
fs.writeFileSync(tmp, JSON.stringify(doc, null, 2), { mode });
fs.renameSync(tmp, f);
'
if command -v python3 >/dev/null 2>&1 && python3 -c 'import json' 2>/dev/null; then
  exec python3 -c "$PY" "$op" "$dir" "$file"
elif command -v node >/dev/null 2>&1; then
  exec node -e "$JS" "$op" "$dir" "$file"
fi
echo "wsp-trust: neither python3 nor node is on this machine; cannot edit $file" >&2
exit 1
"#;

/// The helper's text, for a local sandbox seat to run it the same way
/// (`wsp-466`): the same file edit, in the same two programs.
pub fn trust_helper_script() -> &'static str {
    TRUST_HELPER
}

/// Where the trust helper lives on the machine.
pub fn trust_helper_path(m: &Machine) -> String {
    format!("{}/bin/wsp-trust", m.exec_dir.trim_end_matches('/'))
}

/// The far command that trusts `dir` (the row's tree) before claude starts in it:
/// it writes [`TRUST_HELPER`] under `<exec_dir>/bin` and runs it. `op` is `add`
/// or `remove`; the helper is rewritten each time so it never lags wsp.
pub fn trust_command(m: &Machine, op: &str, dir: &str) -> String {
    let helper = trust_helper_path(m);
    format!(
        "mkdir -p {bin} && cat > {h} <<'WSP_TRUST_EOF'\n{body}WSP_TRUST_EOF\nchmod 755 {h} && {h} {op} {dir}",
        bin = quote(&format!("{}/bin", m.exec_dir.trim_end_matches('/'))),
        h = quote(&helper),
        body = TRUST_HELPER,
        dir = quote(dir),
    )
}

/// The far command that reads whether the display is on: macOS's display
/// wrangler power state, 4 awake and below that dimmed or asleep. Prints
/// `display 4`, say; prints nothing on a machine that has no such thing.
pub const DISPLAY_PROBE: &str =
    "ioreg -r -n IODisplayWrangler -w0 2>/dev/null | grep -o '\"CurrentPowerState\"=[0-9]*' | head -1 | sed 's/.*=/display /'";

/// Whether the probe's output says the far display is asleep (compound-1012:
/// libghostty's display link needs an active display, so a far renderer never
/// comes up while it sleeps). `None` when it cannot say.
pub fn display_asleep(out: &str) -> Option<bool> {
    let n: u32 = out.lines().find_map(|l| l.trim().strip_prefix("display "))?.trim().parse().ok()?;
    Some(n < 4)
}

/// The sentence a far start that never became ready is given when the far display
/// is asleep, else `None`: the cause, in place of "never became ready".
pub fn asleep_words(m: &Machine, wire: &dyn Wire) -> Option<String> {
    let out = wire.run(m, DISPLAY_PROBE, Duration::from_secs(10)).ok()?;
    display_asleep(&out)?.then(|| {
        format!(
            "the far display of {} is asleep or dimmed, so its renderer cannot start (the seat's agent may still be working): wake it, or keep it awake (`caffeinate -d` there)",
            m.name
        )
    })
}

/// A far start's error, with the cause appended when it is the far display: only
/// a start that "never became ready" is looked into, and a probe that cannot
/// say leaves the error as it was. `seen` is whether the seat's screen reads
/// now (the signal `wsp peek` uses): a renderer that serves a screen is not
/// stopped by the display, whatever its power state (wsp-589).
pub fn with_far_cause(e: String, m: &Machine, wire: &dyn Wire, seen: bool) -> String {
    if seen || !e.contains("never became ready") {
        return e;
    }
    match asleep_words(m, wire) {
        Some(why) => format!("{e}: {why}"),
        None => e,
    }
}

/// A word quoted for the far shell with no `~/` expansion: an environment value or
/// an argument is data, even when it starts with `~/`.
fn quote_plain(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// What `compound-sup spawn` announced: `sup <pid>`, `pid <pid>`, `socket <path>`,
/// `render <path>`. Only `socket` is required (an older build prints three lines).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Handshake {
    pub sup: Option<u32>,
    pub pid: Option<u32>,
    pub socket: String,
    pub render: Option<String>,
}

pub fn parse_handshake(out: &str) -> Option<Handshake> {
    let socket = socket_of(out)?;
    let word = |k: &str| out.lines().find_map(|l| l.strip_prefix(k)).map(|p| p.trim().to_string());
    Some(Handshake {
        sup: word("sup ").and_then(|p| p.parse().ok()),
        pid: word("pid ").and_then(|p| p.parse().ok()),
        socket,
        render: word("render ").filter(|p| !p.is_empty()),
    })
}

/// What a seat's forwards are called here: short (a unix socket path may be 103
/// bytes) and the seat's own, since a seat id is unique across machines.
pub fn forward_paths(dir: &Path, seat: &str) -> (PathBuf, PathBuf) {
    (dir.join(format!("{seat}.sock")), dir.join(format!("{seat}.rsock")))
}

/// The controller's socket on the far machine: `compound-remote`'s, under the
/// machine's `exec_dir`. Empty when the machine has no `exec_dir` to say where.
/// **Not the seat's session socket**, which is `compound-sup`'s and does not
/// speak the lease line (wsp-475).
pub fn controller_far(m: &Machine) -> String {
    match m.exec_dir.trim_end_matches('/') {
        "" => String::new(),
        d => format!("{d}/controller.msock"),
    }
}

/// Where the machine's controller socket is forwarded to here: one per machine,
/// since the lease line names the seat. `dir` is where the seats' forwards are.
pub fn controller_local(dir: &Path, machine: &str) -> PathBuf {
    dir.join(format!("ctl-{machine}.sock"))
}

/// The ssh words for adding (`forward`) or dropping (`cancel`) one forward on the
/// ControlMaster the tunnel holds.
pub fn forward_args(m: &Machine, control: &Path, op: &str, local: &Path, far: &str) -> Vec<String> {
    let s = |x: &str| x.to_string();
    vec![
        s("-S"), control.display().to_string(),
        s("-o"), s("BatchMode=yes"),
        s("-O"), s(op),
        s("-L"), format!("{}:{}", local.display(), far),
        m.ssh.clone(),
    ]
}

/// How wsp reaches a far machine: a command, a forward, and the record of the
/// machine. The real one is [`Ssh`]; a test passes a script and needs no machine.
pub trait Wire {
    fn machine(&self, name: &str) -> Option<Machine>;
    fn run(&self, m: &Machine, cmd: &str, timeout: Duration) -> Result<String, Failed>;
    /// `op` is `forward` or `cancel`.
    fn forward(&self, m: &Machine, op: &str, local: &Path, far: &str) -> Result<(), String>;
    /// The directory a seat's forwarded sockets are made in.
    fn dir(&self, m: &Machine) -> PathBuf;
    /// One beat to one seat: `lease <seat> <state>` on the seat's forwarded
    /// controller socket `socket`. `Ok` only when the far side answered.
    fn lease(&self, m: &Machine, socket: &Path, seat: &str, state: &str) -> Result<(), String>;
    /// Whether the machine `name` is silent right now, and if so when it was
    /// last heard (empty: never). `None` is a machine that is not silent, or one
    /// this wire cannot say anything about — a seat on it is not drawn
    /// unreachable on that evidence.
    fn silent_for(&self, _name: &str) -> Option<String> {
        None
    }
    /// Whether the link to machine `name` is down right now: a short command over
    /// ssh fails as a link (ssh exited 255, or never started). Asked of a seat whose
    /// forwarded socket does not answer and whose machine is not yet silent by its
    /// lease: the tunnel dropping makes that socket refuse at once, and a seat
    /// behind a down link is `unreachable`, never `gone` (`docs/remote-agents.md`
    /// section 3). A command that ran and failed is a link that answered.
    fn link_down(&self, name: &str) -> bool {
        let Some(m) = self.machine(name) else { return false };
        matches!(self.run(&m, "true", LINK_PROBE), Err(Failed::Link(_)))
    }
}

/// How long [`Wire::link_down`] waits: ssh's own connect timeout is 5 s.
pub const LINK_PROBE: Duration = Duration::from_secs(8);

/// Whether a far machine is silent: its link is down, or its seats' beat has gone
/// unanswered for longer than its lease and one beat more
/// (`docs/remote-agents.md` section 3). A machine never beaten (`heard` empty)
/// while the link is up is not silent: it has simply had no far seat to beat.
/// The daemon empties `heard` when the machine's last far seat ends
/// ([`beat_idle`]), so an idle machine reads off the probe, not off the last
/// beat of a seat that is gone (wsp-588).
pub fn is_silent(m: &Machine, l: &MachineLive) -> bool {
    !l.reachable || (!l.heard.is_empty() && heard_is_stale(m, crate::util::since(&l.heard)))
}

/// The silence rule with no clock in it: a beat last answered `age` seconds ago
/// is stale once older than the lease plus one beat. The beat is the allowance
/// for a late tick, which is a slow daemon and not a dead link (wsp-588).
pub fn heard_is_stale(m: &Machine, age: i64) -> bool {
    age > i64::from(m.lease_secs()) + beat_every(m).as_secs() as i64
}

/// When a silent machine was last heard: the last answered beat, else the
/// probe's last sight of it. Empty when it never has been.
pub fn last_heard(l: &MachineLive) -> &str {
    match l.heard.is_empty() {
        false => &l.heard,
        true => &l.last_seen,
    }
}

/// How long ago, as `unreachable` prints it: `4m`, or `never heard`.
pub fn heard_ago(at: &str) -> String {
    match at {
        "" => "never heard".to_string(),
        at => crate::util::duration_human(crate::util::since(at)),
    }
}

/// A machine's silence as a seat or a machine row words it: `unreachable 4m`.
pub fn unreachable_word(at: &str) -> String {
    format!("unreachable {}", heard_ago(at))
}

/// What the silent far machines cost the seats on them, read once for a surface
/// that draws seats: the sentence ([`loud`]) and, per far seat on a silent
/// machine, the word its row wears (`unreachable 4m`).
#[derive(Debug, Clone, Default)]
pub struct Silence {
    pub line: Option<String>,
    pub seats: std::collections::BTreeMap<String, String>,
}

impl Silence {
    pub fn read(store: &crate::store::Store) -> Silence {
        let (machines, live) = (store.machines(), store.machines_live());
        let mut out = Silence { line: loud(&machines, &live), seats: Default::default() };
        if out.line.is_none() {
            return out;
        }
        for m in machines.iter().filter(|m| m.is_active() && !m.name.is_empty()) {
            let Some(l) = live.get(&m.name).filter(|l| is_silent(m, l)) else { continue };
            let word = unreachable_word(last_heard(l));
            for seat in crate::place_compound::beat_seats(store, &m.name) {
                out.seats.insert(seat.id, word.clone());
            }
        }
        out
    }
}

/// The one sentence four surfaces show when far machines are silent — `wsp wip`'s
/// heading, a brief's seat line, `wsp doctor`. Drawn the way a dead daemon is
/// (`wsp-145`): a fault in its own line, not a clause on a census. `None` when no
/// active machine is silent, which is every machine that has no far seat.
pub fn loud(machines: &[Machine], live: &std::collections::BTreeMap<String, MachineLive>) -> Option<String> {
    let words: Vec<String> = machines
        .iter()
        .filter(|m| m.is_active() && !m.name.is_empty())
        .filter_map(|m| {
            let l = live.get(&m.name)?;
            is_silent(m, l).then(|| format!("{} ({})", m.name, unreachable_word(last_heard(l))))
        })
        .collect();
    match words.is_empty() {
        true => None,
        false => Some(format!(
            "unreachable: {} — their seats are held, not gone; claims stay, tells wait for the link",
            words.join(", ")
        )),
    }
}

/// How often the daemon beats each far seat (`docs/remote-agents.md` section 3).
pub const BEAT_SECS: u64 = 30;

/// How often a machine's seats are beaten: [`BEAT_SECS`], or a third of the
/// machine's lease where that is shorter, so a beat that is late once or twice
/// still lands inside the lease. At a 60 s lease that is 20 s, which is the
/// daemon's tick: a beat at the lease's own interval put `heard` past the lease
/// on any late tick (wsp-588).
pub fn beat_every(m: &Machine) -> Duration {
    Duration::from_secs((u64::from(m.lease_secs()) / 3).clamp(1, BEAT_SECS))
}

/// How early before it is due a tick still beats. The daemon ticks every 20 s
/// and a beat is stamped when it is sent, so a beat due in 19.9 s would wait for
/// the tick after (40 s apart, and 60 s under load); one due within this is sent
/// now. The cost is a beat a few seconds early, which nothing minds.
pub const BEAT_SLACK: Duration = Duration::from_secs(5);

/// How long after a beat the next is due: [`beat_every`] less [`BEAT_SLACK`].
pub fn beat_wait(m: &Machine) -> Duration {
    beat_every(m).saturating_sub(BEAT_SLACK)
}

/// What the daemon does when a machine has no far seat to beat: forget that it
/// ever heard one. `heard` and `leases` are the beats of seats; with none there
/// is nothing to be silent about, and `heard` would only age until the machine
/// read `unreachable` over a working link (wsp-588).
pub fn beat_idle(live: &mut MachineLive) {
    live.heard.clear();
    live.leases.clear();
}

/// How long one beat waits for the far side's answer. Short, because beats are
/// sent from the tick, one seat after another.
pub const BEAT_TIMEOUT: Duration = Duration::from_secs(3);

/// How long a seat with no claim yet is left alone: it is opened a moment
/// before its claim lands, and a `stop` in that gap would end an agent that
/// has not begun. Two beats.
pub const BEAT_GRACE_SECS: i64 = 2 * BEAT_SECS as i64;

/// The controller wire version compound-remote's socket speaks: its
/// `proto::VERSION`, which docs/remote.md says stays 1. Not
/// `place_compound::WIRE_VERSION`, which is the compound-sup session wire and
/// would be refused (`unsupported ver`).
pub const REMOTE_WIRE: u32 = 1;

/// The line one beat sends, as compound-remote reads it (docs/remote.md, "Lease
/// and gate"): `{"ver":1,"lease":{"seat":"<seat>","word":"ok"}}`, `word` being
/// `ok` or `stop`. Written by hand so the key order is the documented one. The
/// one place the frame is written.
pub fn lease_line(seat: &str, word: &str) -> String {
    let q = |s: &str| serde_json::to_string(s).unwrap_or_default();
    format!("{{\"ver\":{REMOTE_WIRE},\"lease\":{{\"seat\":{},\"word\":{}}}}}\n", q(seat), q(word))
}

/// The real way: ssh, over the machine's ControlMaster.
pub struct Ssh;

impl Wire for Ssh {
    fn machine(&self, name: &str) -> Option<Machine> {
        crate::store::Store::open().machine(name)
    }

    fn run(&self, m: &Machine, cmd: &str, timeout: Duration) -> Result<String, Failed> {
        let store = crate::store::Store::open();
        ssh_run(m, Some(&crate::tunnel::control_path(&store, &m.name)), cmd, timeout)
    }

    fn forward(&self, m: &Machine, op: &str, local: &Path, far: &str) -> Result<(), String> {
        let store = crate::store::Store::open();
        let control = crate::tunnel::control_path(&store, &m.name);
        // ssh binds the local end and refuses a path that exists: a headstone from
        // a master that died.
        if op == "forward" {
            let _ = std::fs::remove_file(local);
        }
        let mut c = Command::new("ssh");
        c.args(forward_args(m, &control, op, local, far));
        let out = run_bounded(c, Duration::from_secs(10)).map_err(|e| format!("cannot start ssh: {e}"))?;
        let Some(out) = out else { return Err(format!("ssh -O {op} did not answer in 10s")) };
        match out.status.success() {
            true => Ok(()),
            false => {
                let err = String::from_utf8_lossy(&out.stderr);
                Err(err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("ssh -O failed").trim().to_string())
            }
        }
    }

    fn dir(&self, m: &Machine) -> PathBuf {
        let store = crate::store::Store::open();
        store.machine_socket(&m.name).parent().map(Path::to_path_buf).unwrap_or(store.state.clone())
    }

    fn silent_for(&self, name: &str) -> Option<String> {
        let store = crate::store::Store::open();
        let (m, l) = (store.machine(name)?, store.machines_live().get(name).cloned()?);
        is_silent(&m, &l).then(|| last_heard(&l).to_string())
    }

    fn lease(&self, _m: &Machine, socket: &Path, seat: &str, state: &str) -> Result<(), String> {
        use std::io::{BufRead, BufReader, Write};
        let mut stream = std::os::unix::net::UnixStream::connect(socket).map_err(|e| format!("{}: {e}", socket.display()))?;
        let _ = stream.set_write_timeout(Some(BEAT_TIMEOUT));
        let _ = stream.set_read_timeout(Some(BEAT_TIMEOUT));
        stream
            .write_all(lease_line(seat, state).as_bytes())
            .map_err(|e| format!("lease not sent: {e}"))?;
        let mut reply = String::new();
        let n = BufReader::new(&stream).read_line(&mut reply).map_err(|e| format!("no answer to the lease: {e}"))?;
        let _ = stream.shutdown(std::net::Shutdown::Both);
        if n == 0 {
            return Err("the far side closed without answering the lease".into());
        }
        // A reply that says `error` was heard and refused; the seat is not
        // known to be there, which is not an answer.
        match serde_json::from_str::<serde_json::Value>(&reply).ok().and_then(|v| v.get("error").map(|e| e.to_string())) {
            Some(e) => Err(format!("the far side refused the lease: {e}")),
            None => Ok(()),
        }
    }
}

/// A far seat as the beat sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BeatSeat {
    pub id: String,
    /// The machine's forwarded controller socket, on this side — not the seat's
    /// own session socket.
    pub socket: PathBuf,
    /// Whether a claim or a binding still holds the seat.
    pub held: bool,
    /// Seconds since the seat was started; `None` when the record does not say.
    pub age: Option<i64>,
}

impl BeatSeat {
    /// `ok` while the seat is held, `stop` once its claim is released. A seat
    /// younger than [`BEAT_GRACE_SECS`] with no hold yet is still `ok`.
    pub fn state(&self) -> &'static str {
        match (self.held, self.age) {
            (true, _) => "ok",
            (false, Some(a)) if a < BEAT_GRACE_SECS => "ok",
            (false, _) => "stop",
        }
    }
}

/// One round of beats to the far seats of `m`: each gets its own `ok` or `stop`
/// on its own socket. `live.heard` moves to `now` only when at least one beat
/// was answered; a beat that failed changes nothing. Returns the seats that did
/// not answer, with why, for the daemon's log.
pub fn beat(m: &Machine, seats: &[BeatSeat], wire: &dyn Wire, live: &mut MachineLive, now: &str) -> Vec<(String, String)> {
    let mut failed = Vec::new();
    let mut answered = false;
    for s in seats {
        match wire.lease(m, &s.socket, &s.id, s.state()) {
            Ok(()) => {
                answered = true;
                live.leases.insert(s.id.clone(), now.to_string());
            }
            Err(why) => failed.push((s.id.clone(), why)),
        }
    }
    if answered {
        live.heard = now.to_string();
    }
    live.leases.retain(|id, _| seats.iter().any(|s| &s.id == id));
    failed
}

/// The far `compound-remote`: beside the far `compound-sup` when the record
/// names where that is, else the bare name.
pub fn remote_of(m: &Machine) -> String {
    match m.sup_at.rsplit_once('/') {
        Some((dir, _)) => format!("{dir}/compound-remote"),
        None => "compound-remote".to_string(),
    }
}

/// How long the gate counts a lease file fresh, in seconds: compound-remote's
/// `lease::FRESH` (two 30 s beats and slack), which the gate uses because
/// [`gate_command`] passes no `--fresh`. Mirrored here; a test pins that the
/// command still leaves it alone.
pub const GATE_FRESH_SECS: u64 = 75;

/// The PreToolUse hook command of a far seat (`docs/remote-agents.md` section 3).
pub fn gate_command(m: &Machine, seat: &str) -> String {
    format!(
        "{} gate --exec-dir {} --seat {} --lease {}",
        remote_of(m),
        quote(&m.exec_dir),
        quote(seat),
        m.lease_secs()
    )
}

/// The far shell that writes a seat's **first lease**, `ok`, to
/// `<exec_dir>/lease/<seat>` (temp, then rename, as compound-remote's own `lease`
/// verb writes it; the file's mtime is the beat). `spawn --on` runs it with the
/// gate, before the agent starts: the first beat goes through the forwarded
/// controller socket, which is up minutes after the spawn, and a gate that finds
/// no lease file waits out the whole lease and then refuses the seat's first
/// tool call (agent-029 run 3, wsp-562). Ends in ` && ` so it chains.
///
/// What it buys is bounded: the gate counts a lease fresh for [`GATE_FRESH_SECS`]
/// (compound-remote's own `lease::FRESH`; [`gate_command`] passes no `--fresh`),
/// so a first call within that long of the spawn passes at once. A later one,
/// before the first beat, finds the lease stale and the gate polls for the beat
/// up to the lease (`m.lease_secs()`) before it refuses.
pub fn first_lease_command(m: &Machine, seat: &str) -> String {
    let dir = format!("{}/lease", m.exec_dir.trim_end_matches('/'));
    let tmp = format!("{dir}/.{seat}.wsp.tmp");
    format!(
        "mkdir -p {d} && printf ok > {t} && mv {t} {f} && ",
        d = quote(&dir),
        t = quote(&tmp),
        f = quote(&format!("{dir}/{seat}")),
    )
}

/// The far shell that removes a seat's lease file, `<exec_dir>/lease/<seat>`
/// (and a first-lease temp the spawn left), when its session is ended. A lease
/// file nobody beats is read as stale, but it stays on the machine and keeps its
/// name in `leases` until something removes it (wsp-590).
pub fn drop_lease_command(m: &Machine, seat: &str) -> String {
    let dir = format!("{}/lease", m.exec_dir.trim_end_matches('/'));
    format!(
        "rm -f {} {}",
        quote(&format!("{dir}/{seat}")),
        quote(&format!("{dir}/.{seat}.wsp.tmp")),
    )
}

/// The far command that lists what the machine holds for seats: one
/// `lease <name>` line per file in `<exec_dir>/lease`, and one line per running
/// `compound-sup spawn --label …` (the process's own command line). Read by
/// [`strays`].
pub fn holdings_command(m: &Machine) -> String {
    let dir = format!("{}/lease", m.exec_dir.trim_end_matches('/'));
    format!(
        "for f in {d}/*; do [ -e \"$f\" ] && echo \"lease ${{f##*/}}\"; done; ps -axo command= | grep -F 'spawn --label' | grep -v 'grep -F' | grep -F -- 'compound-sup'; true",
        d = quote(&dir),
    )
}

/// What the far machine holds for a seat that no seat of this store owns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stray {
    pub label: String,
    /// A `compound-sup` session is running under that label.
    pub session: bool,
    /// A lease file is there under that name.
    pub lease: bool,
}

impl Stray {
    /// One line for `cycle.log`.
    pub fn words(&self, machine: &str) -> String {
        let what = match (self.session, self.lease) {
            (true, true) => "a compound-sup session and a lease file",
            (true, false) => "a compound-sup session",
            _ => "a lease file",
        };
        format!(
            "wsp: {machine}: {} holds {what} on {machine} but no seat here has it — a seat ended on this side and not there; end it on the machine (`compound-sup close <socket>`, `rm <exec_dir>/lease/{}`)",
            self.label, self.label
        )
    }
}

/// [`holdings_command`]'s output → the labels held that none of `seats` owns.
/// A throwaway check session (`wsp-check-*`) and a lease temp are not seats'.
pub fn strays(out: &str, seats: &[String]) -> Vec<Stray> {
    let mut by: std::collections::BTreeMap<String, Stray> = std::collections::BTreeMap::new();
    for line in out.lines() {
        let line = line.trim();
        let (label, session) = match line.strip_prefix("lease ") {
            Some(name) => (name.trim().to_string(), false),
            None => {
                let words: Vec<&str> = line.split_whitespace().collect();
                match words.iter().position(|w| *w == "--label").and_then(|i| words.get(i + 1)) {
                    Some(l) if words.iter().any(|w| w.rsplit('/').next() == Some(SUP)) => (l.trim_matches('\'').to_string(), true),
                    _ => continue,
                }
            }
        };
        if label.is_empty() || label.starts_with('.') || label.starts_with("wsp-check-") || seats.contains(&label) {
            continue;
        }
        let e = by.entry(label.clone()).or_insert(Stray { label, session: false, lease: false });
        match session {
            true => e.session = true,
            false => e.lease = true,
        }
    }
    by.into_values().collect()
}

/// What the hook may take, in seconds: the gate waits up to the lease, so the
/// hook's own timeout is above it.
pub fn gate_timeout(m: &Machine) -> u32 {
    m.lease_secs() + 60
}

/// The `Stop` hook command of a far seat (`docs/remote-agents.md` section 6,
/// "Checkpoint"): the shim's `checkpoint`, which has the seat copy the files that
/// changed back at the end of every turn. Absolute, because the hook does not
/// get the agent's `PATH`; silent and always successful, because a `Stop` hook
/// that exits 2 keeps the agent working, and a checkpoint missed on a down link
/// is caught by the next turn's, or by `review`.
pub fn checkpoint_command(m: &Machine) -> String {
    format!("{} checkpoint >/dev/null 2>&1 || true", quote(&format!("{}/wsp", shim_dir(m))))
}

/// How long a checkpoint may take: a copy-back is a manifest on each side and
/// the files that differ.
pub const CHECKPOINT_TIMEOUT_SECS: u32 = 300;

/// `existing` (the far tree's `.claude/settings.json`, empty when there is none)
/// with the gate under `hooks.PreToolUse` and the checkpoint under `hooks.Stop`. An earlier gate entry is replaced and
/// everything else is kept. An existing file that is not a JSON object is an
/// error: it is somebody's configuration and is not overwritten.
pub fn settings_with_gate(existing: &str, m: &Machine, seat: &str) -> Result<String, String> {
    let mut doc: serde_json::Value = match existing.trim() {
        "" => serde_json::json!({}),
        text => serde_json::from_str(text).map_err(|e| format!("the far .claude/settings.json is not JSON ({e}); not overwriting it"))?,
    };
    let Some(top) = doc.as_object_mut() else {
        return Err("the far .claude/settings.json is not a JSON object; not overwriting it".into());
    };
    let hooks = top.entry("hooks").or_insert_with(|| serde_json::json!({}));
    let Some(hooks) = hooks.as_object_mut() else { return Err("`hooks` in the far .claude/settings.json is not an object".into()) };
    let pre = hooks.entry("PreToolUse").or_insert_with(|| serde_json::json!([]));
    let Some(pre) = pre.as_array_mut() else { return Err("`hooks.PreToolUse` in the far .claude/settings.json is not a list".into()) };
    pre.retain(|entry| {
        !entry["hooks"].as_array().into_iter().flatten().any(|h| h["command"].as_str().is_some_and(|c| c.contains(" gate --exec-dir ")))
    });
    pre.push(serde_json::json!({
        "matcher": "",
        "hooks": [{ "type": "command", "command": gate_command(m, seat), "timeout": gate_timeout(m) }],
    }));
    let stop = hooks.entry("Stop").or_insert_with(|| serde_json::json!([]));
    let Some(stop) = stop.as_array_mut() else { return Err("`hooks.Stop` in the far .claude/settings.json is not a list".into()) };
    stop.retain(|entry| {
        !entry["hooks"].as_array().into_iter().flatten().any(|h| h["command"].as_str().is_some_and(|c| c.contains("/wsp' checkpoint") || c.contains("/wsp checkpoint")))
    });
    stop.push(serde_json::json!({
        "hooks": [{ "type": "command", "command": checkpoint_command(m), "timeout": CHECKPOINT_TIMEOUT_SECS }],
    }));
    serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())
}

/// The `socket <path>` line of `compound-sup spawn`'s handshake. (`sup <pid>` is
/// the supervisor and `pid <pid>` the agent: two processes in two process
/// groups, which is why the session is ended with `close <socket>` and not by
/// signalling one of them.)
fn socket_of(out: &str) -> Option<String> {
    out.lines().find_map(|l| l.strip_prefix("socket ").map(|p| p.trim().to_string())).filter(|p| !p.is_empty())
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

    /// A far side that answers the display probe from a script and remembers the commands.
    struct Display {
        says: Result<&'static str, Failed>,
        asked: RefCell<Vec<String>>,
    }

    impl Wire for Display {
        fn machine(&self, _: &str) -> Option<Machine> {
            None
        }
        fn run(&self, _: &Machine, cmd: &str, _: Duration) -> Result<String, Failed> {
            self.asked.borrow_mut().push(cmd.to_string());
            self.says.clone().map(String::from)
        }
        fn forward(&self, _: &Machine, _: &str, _: &Path, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn dir(&self, _: &Machine) -> PathBuf {
            PathBuf::new()
        }
        fn lease(&self, _: &Machine, _: &Path, _: &str, _: &str) -> Result<(), String> {
            Ok(())
        }
    }

    fn display(says: Result<&'static str, Failed>) -> Display {
        Display { says, asked: RefCell::new(Vec::new()) }
    }

    #[test]
    fn the_display_probe_reads_asleep_awake_and_unknown() {
        assert_eq!(display_asleep("display 4\n"), Some(false));
        assert_eq!(display_asleep("display 0\n"), Some(true));
        assert_eq!(display_asleep("display 3"), Some(true));
        assert_eq!(display_asleep(""), None);
        assert_eq!(display_asleep("something else"), None);
    }

    #[test]
    fn an_asleep_far_display_is_named_and_any_other_reading_is_not() {
        let m = machine();
        let w = display(Ok("display 0\n"));
        let why = asleep_words(&m, &w).expect("asleep is named");
        assert!(why.contains("mb2") && why.contains("asleep"), "{why}");
        assert_eq!(*w.asked.borrow(), vec![DISPLAY_PROBE.to_string()], "the probe is the one command asked");
        assert_eq!(asleep_words(&m, &display(Ok("display 4\n"))), None);
        assert_eq!(asleep_words(&m, &display(Ok(""))), None);
        assert_eq!(asleep_words(&m, &display(Err(Failed::Ran("boom".into())))), None);
        assert_eq!(asleep_words(&m, &display(Err(Failed::Link("down".into())))), None);
    }

    #[test]
    fn a_far_start_that_never_became_ready_gets_the_cause_and_no_other_error_does() {
        let m = machine();
        let e = "claude started but never became ready for input".to_string();
        let asleep = with_far_cause(e.clone(), &m, &display(Ok("display 0")), false);
        assert!(asleep.starts_with(&format!("{e}: ")) && asleep.contains("asleep"), "{asleep}");
        assert_eq!(with_far_cause(e.clone(), &m, &display(Ok("display 4")), false), e);
        assert_eq!(with_far_cause(e.clone(), &m, &display(Err(Failed::Link("down".into()))), false), e);
        let other = display(Ok("display 0"));
        assert_eq!(with_far_cause("no such pane".into(), &m, &other, false), "no such pane");
        assert!(other.asked.borrow().is_empty(), "another error is not probed for");
        // A seat whose screen reads is not held back by the display: no words, no probe.
        let up = display(Ok("display 0"));
        assert_eq!(with_far_cause(e.clone(), &m, &up, true), e);
        assert!(up.asked.borrow().is_empty(), "a readable screen is not probed past");
    }

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
    ) -> impl Fn(&str, Duration) -> Result<String, Failed> + 'a {
        move |cmd: &str, _t: Duration| {
            asked.borrow_mut().push(cmd.to_string());
            let ran = |r: Result<&str, &str>| r.map(String::from).map_err(|e| Failed::Ran(e.into()));
            if cmd.contains("wire-version") {
                Ok(wire.into())
            } else if cmd.contains(" spawn ") {
                ran(spawn)
            } else if cmd.contains(" close ") {
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
        assert_eq!(parse_wire("9 14\n"), Some((9, 14)));
        assert_eq!(parse_wire(" 9\n"), Some((9, 9)), "a bare version is the range VERSION VERSION");
        assert_eq!(parse_wire("compound-sup: unknown verb"), None);
        assert_eq!(parse_wire("14 9"), None, "OLDEST above VERSION is no answer");
        assert_eq!(parse_wire("9 10 14"), None);
    }

    /// The seat's version is tested against the far range, not the top of it.
    #[test]
    fn a_machine_is_ready_when_the_seat_wire_lies_in_its_range() {
        let none = RefCell::new(Vec::new());
        let m = machine();
        // inside, at the bottom edge (the reported case: seat 9, far "9 14").
        let c = check_with(&m, 9, &far(&none, "9 14\n", Ok("socket /s"), Ok("ok")));
        assert!(c.passed(), "{:?}", c.steps);
        assert_eq!((c.wire_oldest, c.wire), (Some(9), Some(14)));
        let c = check_with(&m, 12, &far(&none, "9 14", Ok("socket /s"), Ok("ok")));
        assert!(c.passed(), "{:?}", c.steps);
        let c = check_with(&m, 14, &far(&none, "9 14", Ok("socket /s"), Ok("ok")));
        assert!(c.passed(), "top edge: {:?}", c.steps);
        // below: the seat's wsp is behind.
        let c = check_with(&m, 8, &far(&none, "9 14", Ok("socket /s"), Ok("ok")));
        assert!(!c.passed());
        assert!(c.why().contains("v8/v9 to 14") && c.why().contains("this seat's wsp is behind"), "{}", c.why());
        // above: the far compound-sup is behind.
        let c = check_with(&m, 15, &far(&none, "9 14", Ok("socket /s"), Ok("ok")));
        assert!(!c.passed());
        assert!(c.why().contains("v15/v9 to 14") && c.why().contains("the far compound-sup is behind"), "{}", c.why());
        // a bare version accepts only itself.
        assert!(check_with(&m, 9, &far(&none, "9", Ok("socket /s"), Ok("ok"))).passed());
        let c = check_with(&m, 10, &far(&none, "9", Ok("socket /s"), Ok("ok")));
        assert!(c.why().contains("the far compound-sup is behind"), "{}", c.why());

        // and the recorded range reads the same way.
        let mut l = MachineLive::default();
        fold_probe(&mut l, Ok("9 14\n".into()), "now");
        assert_eq!((l.wire_oldest, l.wire), (Some(9), Some(14)));
        assert_eq!(readiness(&l, 9), Readiness::Unchecked);
        assert_eq!(readiness(&l, 14), Readiness::Unchecked);
        assert_eq!(readiness(&l, 8), Readiness::Mismatch { seat: 8, oldest: 9, far: 14 });
        assert_eq!(readiness(&l, 15), Readiness::Mismatch { seat: 15, oldest: 9, far: 14 });
        fold_probe(&mut l, Ok("9\n".into()), "later");
        assert_eq!(readiness(&l, 10), Readiness::Mismatch { seat: 10, oldest: 9, far: 9 });
    }

    #[test]
    fn a_check_that_passes_starts_and_stops_a_session_and_asks_claude_with_the_token() {
        let asked = RefCell::new(Vec::new());
        let m = machine();
        let c = check_with(&m, 9, &far(&asked, "9", Ok("sup 4241\npid 4242\nsocket /run/x.sock\nrender /run/x.rsock"), Ok("ok")));
        assert!(c.passed(), "{:?}", c.steps);
        assert_eq!(c.wire, Some(9));
        let asked = asked.borrow();
        assert_eq!(asked.len(), 4, "wire, start, stop, claude: {asked:?}");
        assert!(asked[2].contains("close") && asked[2].contains("/run/x.sock"), "the session is closed by its socket: {}", asked[2]);
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
        let c = check_with(&m, 9, &far(&none, "8", Ok("socket /s"), Ok("ok")));
        assert!(!c.passed());
        assert!(c.why().contains("v9/v8") && c.why().contains("behind"), "{}", c.why());
        assert_eq!(none.borrow().len(), 1, "nothing is started on a machine that speaks another wire");

        // the session will not start.
        let c = check_with(&m, 9, &far(&none, "9", Err("no such cwd"), Ok("ok")));
        assert!(c.why().starts_with("session: no such cwd"), "{}", c.why());

        // no auth: the headless claude fails.
        let c = check_with(&m, 9, &far(&none, "9", Ok("socket /s"), Err("Invalid API key")));
        assert!(c.why().starts_with("claude: Invalid API key"), "{}", c.why());
        assert!(c.steps[1].ok, "and the session before it did pass");

        // no exec_dir to start it in.
        let mut bare = m.clone();
        bare.exec_dir.clear();
        let c = check_with(&bare, 9, &far(&none, "9", Ok("socket /s"), Ok("ok")));
        assert!(c.why().contains("exec_dir"), "{}", c.why());
    }

    #[test]
    fn a_machine_reads_ready_only_when_the_check_passed() {
        let mut l = MachineLive { reachable: true, wire: Some(9), ..Default::default() };
        assert_eq!(readiness(&l, 9), Readiness::Unchecked, "connected, and wire matching, is not ready");
        assert_eq!(readiness(&l, 10), Readiness::Mismatch { seat: 10, oldest: 9, far: 9 });

        let failed = Check { wire: Some(9), wire_oldest: Some(9), steps: vec![Step { name: "claude", ok: false, said: "no auth".into() }] };
        record(&mut l, &failed, "2026-11-05T10:00:00Z");
        assert_eq!(readiness(&l, 9), Readiness::NotReady("claude: no auth".into()));

        let passed = Check { wire: Some(9), wire_oldest: Some(9), steps: vec![Step { name: "wire", ok: true, said: "v9".into() }] };
        record(&mut l, &passed, "2026-11-05T10:01:00Z");
        assert_eq!(readiness(&l, 9), Readiness::Ready);
        assert_eq!(readiness(&l, 10), Readiness::Mismatch { seat: 10, oldest: 9, far: 9 }, "a passed check does not outlive a wire change");

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

    /// A connect that failed is a dead link; a far side a packet reached and that
    /// was slow to answer is not (wsp-561: load 34 on the i5 read `unreachable`
    /// for 2.5 min while ssh worked). The words are ssh's own, macOS and Linux.
    #[test]
    fn a_connect_that_failed_is_a_dead_far_link_and_a_slow_answer_is_not() {
        for dead in [
            "ssh: connect to host 100.64.0.7 port 22: Operation timed out",
            "ssh: connect to host mb2 port 22: Connection timed out",
            "ssh: connect to host mb2 port 22: No route to host",
            "ssh: connect to host mb2 port 22: Connection refused",
            "ssh: Could not resolve hostname eds-macbook-i5: nodename nor servname provided, or not known",
        ] {
            assert!(dead_link(dead), "{dead}");
        }
        for slow in [
            "Connection timed out during banner exchange",
            "Timeout, server eds-macbook-i5 not responding.",
            "kex_exchange_identification: read: Connection reset by peer",
            "no answer in 15s",
            "the ssh master has not connected yet",
            "ssh exited 255",
        ] {
            assert!(!dead_link(slow), "{slow}");
        }
    }

    /// `Wire::link_down` over a fake ssh: one that drops (exit 255, "connect to
    /// host … timed out") is a down link; one that answers, and one whose command
    /// ran and failed, are not.
    #[test]
    fn a_fake_ssh_that_drops_is_a_down_link_and_one_that_answers_is_not() {
        use std::os::unix::fs::PermissionsExt;
        struct Fake(String);
        impl Wire for Fake {
            fn machine(&self, name: &str) -> Option<Machine> {
                Some(Machine::new(name, name))
            }
            fn run(&self, m: &Machine, cmd: &str, timeout: Duration) -> Result<String, Failed> {
                ssh_run_with(&self.0, m, None, cmd, timeout)
            }
            fn forward(&self, _: &Machine, _: &str, _: &Path, _: &str) -> Result<(), String> {
                Ok(())
            }
            fn dir(&self, _: &Machine) -> PathBuf {
                PathBuf::new()
            }
            fn lease(&self, _: &Machine, _: &Path, _: &str, _: &str) -> Result<(), String> {
                Ok(())
            }
        }
        let dir = std::env::temp_dir().join(format!("wsp-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let script = |name: &str, body: &str| {
            let p = dir.join(name);
            std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            Fake(p.to_string_lossy().into_owned())
        };
        let drops = script("drops", "echo 'ssh: connect to host mb2 port 22: Operation timed out' >&2; exit 255");
        let answers = script("answers", "exit 0");
        let ran = script("ran", "echo 'boom' >&2; exit 1");
        assert!(drops.link_down("mb2"), "a drop is a down link");
        assert!(!answers.link_down("mb2"), "an answer is a link");
        assert!(!ran.link_down("mb2"), "a command that ran and failed went over a link");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The defect the first verifier found: the real `compound-sup spawn` prints
    /// its handshake and then keeps its stdout open for the life of the session,
    /// so a call that waits for stdout to close waits the session out. This is
    /// that binary's behaviour in a script (handshake, then a holder of stdout
    /// for 8 s), and the start command run by a real shell with its stdout on a
    /// pipe, as ssh has it. It must return on the handshake.
    #[test]
    fn the_session_start_returns_once_the_handshake_is_read_not_when_the_session_ends() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("wsp-far-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sup = dir.join("compound-sup");
        std::fs::write(
            &sup,
            "#!/bin/sh\necho 'sup 1'; echo 'pid 2'; echo 'socket /run/x.sock'; echo 'render /run/x.rsock'; sleep 8\n",
        )
        .unwrap();
        std::fs::set_permissions(&sup, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut m = Machine::new("mb2", "mb2");
        m.sup_at = sup.to_string_lossy().into_owned();
        m.exec_dir = dir.to_string_lossy().into_owned();

        let mut sh = Command::new("sh");
        sh.arg("-c").arg(start_command(&m, "wsp-check-t"));
        let t0 = Instant::now();
        let out = run_bounded(sh, Duration::from_secs(20)).unwrap().expect("the start command must finish");
        let took = t0.elapsed();
        let said = String::from_utf8_lossy(&out.stdout).into_owned();

        assert!(took < Duration::from_secs(4), "returned on the handshake, not the session ({took:?})");
        assert_eq!(socket_of(&said).as_deref(), Some("/run/x.sock"), "{said}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_seat_command_reads_the_token_on_the_far_side_and_puts_none_in_the_text() {
        let m = machine();
        let env = BTreeMap::from([
            ("WSP_SEAT_ID".to_string(), "cpd-7".to_string()),
            ("WSP_TASK".to_string(), "it's".to_string()),
            ("STRIPPED".to_string(), String::new()),
            ("bad name".to_string(), "x".to_string()),
        ]);
        let cmd = seat_command(&m, "cpd-7", "~/wsp-exec/trees/t-1", &env, "claude", &["--model".into(), "a b".into()]);
        assert!(cmd.contains("t=$(cat ~/'wsp-exec/claude-token'"), "{cmd}");
        assert!(cmd.contains("CLAUDE_CODE_OAUTH_TOKEN=\"$t\" env "), "{cmd}");
        assert!(cmd.contains("-u STRIPPED") && cmd.contains("WSP_SEAT_ID='cpd-7'") && cmd.contains(r"WSP_TASK='it'\''s'"), "{cmd}");
        assert!(!cmd.contains("bad name"), "a name the shell cannot take is dropped: {cmd}");
        assert!(cmd.contains("spawn --label 'cpd-7' --cwd ~/'wsp-exec/trees/t-1' -- 'claude' '--model' 'a b'"), "{cmd}");
        assert!(!cmd.contains("--no-renderer"), "the seat gets its renderer: {cmd}");
    }

    /// A far agent started with another `wsp` first on the machine's PATH still
    /// runs the shim (`wsp-563`): the seat command is run by a real shell with a
    /// stale `wsp` ahead on its PATH and a `compound-sup` that runs the agent.
    #[test]
    fn a_far_agent_has_the_executors_bin_first_on_its_path_so_wsp_is_the_shim() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("wsp-far-shim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (exec, stale) = (dir.join("exec"), dir.join("stale"));
        std::fs::create_dir_all(exec.join("bin")).unwrap();
        std::fs::create_dir_all(&stale).unwrap();
        let exe = |p: std::path::PathBuf, body: &str| {
            std::fs::write(&p, body).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        exe(exec.join("bin/wsp"), "#!/bin/sh\n");
        exe(stale.join("wsp"), "#!/bin/sh\n");
        std::fs::write(exec.join("claude-token"), "tok\n").unwrap();
        // The supervisor: the handshake, then the agent (everything after `--`) in
        // the environment it was started with.
        let sup = dir.join("compound-sup");
        exe(sup.clone(), "#!/bin/sh\necho 'socket /run/x.sock'\nwhile [ \"$1\" != -- ]; do shift; done; shift\nexec \"$@\"\n");
        let mut m = Machine::new("mb2", "mb2");
        m.sup_at = sup.display().to_string();
        m.exec_dir = exec.display().to_string();

        let seen = dir.join("seen");
        let probe = format!("echo \"$(command -v wsp)\" > {}; echo \"${{PATH%%:*}}\" >> {}", seen.display(), seen.display());
        // The env `spawn --on` really produces: `throttle::seat_env`'s PATH, the
        // controller's shims ahead of the controller's own PATH.
        let mut env = BTreeMap::new();
        env.insert("PATH".to_string(), "/ctl/state/wsp/shims:/ctl/bin:/usr/bin:/bin".to_string());
        env.insert("WSP_ROW".to_string(), "wsp-1".to_string());
        let cmd = seat_command(&m, "cpd-1", &dir.display().to_string(), &env, "sh", &["-c".into(), probe]);
        assert!(!cmd.contains("/ctl/"), "the controller's PATH is never sent: {cmd}");
        assert!(cmd.contains("WSP_ROW=") , "{cmd}");
        let default_path = format!("{}:/usr/bin:/bin", stale.display());
        let out = Command::new("/bin/sh").arg("-c").arg(&cmd).env("PATH", &default_path).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        for _ in 0..50 {
            if seen.exists() && std::fs::read_to_string(&seen).map(|t| t.lines().count() == 2).unwrap_or(false) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let got = std::fs::read_to_string(&seen).unwrap();
        let bin = exec.join("bin");
        assert_eq!(got.lines().next(), Some(bin.join("wsp").to_str().unwrap()), "which wsp names the shim: {got}");
        assert_eq!(got.lines().nth(1), Some(bin.to_str().unwrap()), "{got}");

        // The same machine's default PATH, asked without the seat's: the stale
        // binary is named, and the shim alone is not a warning.
        let ask = |path: &str| {
            let o = Command::new("/bin/sh").arg("-c").arg(shim_probe_command(&m)).env("PATH", path).output().unwrap();
            shim_shadow(&m, &String::from_utf8_lossy(&o.stdout))
        };
        let warned = ask(&default_path).expect("a stale wsp ahead of the shim is named");
        assert!(warned.contains(&stale.join("wsp").display().to_string()), "{warned}");
        assert_eq!(ask(&format!("{}:{default_path}", bin.display())), None, "the shim first is no warning");
        assert_eq!(ask("/usr/bin:/bin"), None, "no wsp at all is not a shadow");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_seat_command_sets_the_path_in_the_far_shell_with_a_tilde_exec_dir() {
        let cmd = seat_command(&machine(), "cpd-7", "~/wsp-exec/trees/t-1", &BTreeMap::new(), "claude", &[]);
        assert!(cmd.contains("PATH=~/'wsp-exec/bin':\"$PATH\"; export PATH"), "{cmd}");
        assert_eq!(shim_shadow(&machine(), "ok\n"), None);
        assert_eq!(shim_shadow(&machine(), "shadow \n"), None);
    }

    /// The row's own tree is trusted by name in the far `~/.claude.json`, kept
    /// next to whatever else that file holds, and taken out again (`wsp-445`).
    #[test]
    fn the_trust_command_trusts_the_trees_own_path_keeps_the_rest_and_removes_it() {
        let dir = std::env::temp_dir().join(format!("wsp-far-trust-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let mut m = Machine::new("mb3", "mb3");
        m.exec_dir = dir.join("exec").display().to_string();
        let file = home.join(".claude.json");
        let run = |op: &str| {
            let out = Command::new("/bin/sh")
                .arg("-c")
                .arg(trust_command(&m, op, "/far/exec/trees/demos-002"))
                .env("HOME", &home)
                .env_remove("CLAUDE_CONFIG_DIR")
                .output()
                .unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&file).unwrap()).unwrap()
        };
        // No file yet: one is made.
        let doc = run("add");
        assert_eq!(doc["projects"]["/far/exec/trees/demos-002"]["hasTrustDialogAccepted"], true);
        assert_eq!(doc["projects"]["/far/exec/trees/demos-002"]["hasClaudeMdExternalIncludesApproved"], true);
        assert_eq!(doc["projects"]["/far/exec/trees/demos-002"]["hasClaudeMdExternalIncludesWarningShown"], true);
        // A file that holds other things keeps them, and the second add changes nothing.
        std::fs::write(
            &file,
            r#"{"userID":"u","projects":{"/far/exec/trees":{"hasTrustDialogAccepted":true},"/far/exec/trees/demos-002":{"allowedTools":["x"]}}}"#,
        )
        .unwrap();
        let doc = run("add");
        let again = run("add");
        assert_eq!(doc, again);
        assert_eq!(doc["userID"], "u");
        assert_eq!(doc["projects"]["/far/exec/trees"]["hasTrustDialogAccepted"], true);
        assert_eq!(doc["projects"]["/far/exec/trees/demos-002"]["allowedTools"][0], "x");
        assert_eq!(doc["projects"]["/far/exec/trees/demos-002"]["hasTrustDialogAccepted"], true);
        // Removed with the tree, and the parent's entry is not touched.
        let doc = run("remove");
        assert!(doc["projects"].get("/far/exec/trees/demos-002").is_none(), "{doc}");
        assert_eq!(doc["projects"]["/far/exec/trees"]["hasTrustDialogAccepted"], true);
        // Not JSON: refused and left as it was.
        std::fs::write(&file, "not json").unwrap();
        let out = Command::new("/bin/sh")
            .arg("-c")
            .arg(trust_command(&m, "add", "/far/x"))
            .env("HOME", &home)
            .env_remove("CLAUDE_CONFIG_DIR")
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "not json");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `env` stops reading options at the first `NAME=value`: a `-u` after one is
    /// taken as the program (`env: -u: No such file or directory` on macOS).
    #[test]
    fn the_seat_command_puts_every_unset_before_any_assignment_and_a_shell_starts_the_program() {
        use std::os::unix::fs::PermissionsExt;
        let m = machine();
        let env = BTreeMap::from([
            ("A_SET".to_string(), "1".to_string()),
            ("B_GONE".to_string(), String::new()),
            ("C_SET".to_string(), "2".to_string()),
            ("D_GONE".to_string(), String::new()),
        ]);
        let cmd = seat_command(&m, "cpd-7", "/work", &env, "agent", &[]);
        let last_u = cmd.rfind("-u ").expect("an unset");
        let first_set = cmd.find("A_SET=").expect("an assignment");
        assert!(last_u < first_set, "every -u comes before any K=V: {cmd}");
        assert!(cmd.contains("-u B_GONE -u D_GONE A_SET='1' C_SET='2' "), "{cmd}");

        let dir = std::env::temp_dir().join(format!("wsp-far-unset-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sup = dir.join("compound-sup");
        let seen = dir.join("seen");
        std::fs::write(
            &sup,
            format!(
                "#!/bin/sh\necho \"${{B_GONE-unset}} ${{D_GONE-unset}} $A_SET $C_SET\" > {}\necho 'socket /run/x.sock'\n",
                seen.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&sup, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(dir.join(TOKEN_FILE), "sekret\n").unwrap();
        let mut m = Machine::new("mb2", "mb2");
        m.sup_at = sup.to_string_lossy().into_owned();
        m.exec_dir = dir.to_string_lossy().into_owned();
        let mut sh = Command::new("/bin/sh");
        sh.arg("-c").arg(seat_command(&m, "cpd-7", "/work", &env, "agent", &[]));
        sh.env("B_GONE", "x").env("D_GONE", "y");
        let out = run_bounded(sh, Duration::from_secs(20)).unwrap().expect("returns");
        let said = String::from_utf8_lossy(&out.stdout).into_owned();
        let err = String::from_utf8_lossy(&out.stderr).into_owned();
        assert_eq!(socket_of(&said).as_deref(), Some("/run/x.sock"), "{said} {err}");
        assert_eq!(std::fs::read_to_string(&seen).unwrap().trim(), "unset unset 1 2");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The far shell, run for real: the token reaches `compound-sup`'s
    /// environment from the file, the handshake comes back on stdout and the
    /// token does not, and a missing token stops before a session starts.
    #[test]
    fn the_seat_command_run_by_a_shell_exports_the_token_and_returns_on_the_handshake() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("wsp-far-seat-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sup = dir.join("compound-sup");
        let seen = dir.join("seen");
        std::fs::write(
            &sup,
            format!(
                "#!/bin/sh\necho \"$CLAUDE_CODE_OAUTH_TOKEN $WSP_SEAT_ID $*\" > {}\necho 'sup 1'; echo 'pid 2'; echo 'socket /run/x.sock'; echo 'render /run/x.rsock'; sleep 6\n",
                seen.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&sup, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut m = Machine::new("mb2", "mb2");
        m.sup_at = sup.to_string_lossy().into_owned();
        m.exec_dir = dir.to_string_lossy().into_owned();
        let env = BTreeMap::from([("WSP_SEAT_ID".to_string(), "cpd-7".to_string())]);
        let run = |m: &Machine| {
            let mut sh = Command::new("sh");
            sh.arg("-c").arg(seat_command(m, "cpd-7", "/work", &env, "agent", &["x".into()]));
            run_bounded(sh, Duration::from_secs(20)).unwrap().expect("returns")
        };

        // No token file: refused, and nothing was started.
        let out = run(&m);
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("no Claude token"), "{}", String::from_utf8_lossy(&out.stderr));
        assert!(!seen.exists());

        std::fs::write(dir.join(TOKEN_FILE), "sekret\n").unwrap();
        let t0 = Instant::now();
        let out = run(&m);
        let said = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(t0.elapsed() < Duration::from_secs(4), "returned on the handshake, not the session");
        assert!(!said.contains("sekret"), "the token is not in what comes back: {said}");
        let hs = parse_handshake(&said).expect("a handshake");
        assert_eq!(hs, Handshake { sup: Some(1), pid: Some(2), socket: "/run/x.sock".into(), render: Some("/run/x.rsock".into()) });
        let seen_text = std::fs::read_to_string(&seen).unwrap();
        assert!(seen_text.starts_with("sekret cpd-7 spawn --label cpd-7 --cwd /work -- agent x"), "{seen_text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_handshake_needs_a_socket_and_takes_the_rest_if_there() {
        assert_eq!(parse_handshake("sup 1\npid 2\nsocket /a.sock\n").unwrap().render, None);
        assert!(parse_handshake("error: no such cwd").is_none());
        let (s, r) = forward_paths(Path::new("/d"), "cpd-9");
        assert_eq!((s.to_str(), r.to_str()), (Some("/d/cpd-9.sock"), Some("/d/cpd-9.rsock")));
        let a = forward_args(&machine(), Path::new("/d/cm-mb2"), "forward", &s, "/far/x.sock");
        assert_eq!(a.join(" "), "-S /d/cm-mb2 -o BatchMode=yes -O forward -L /d/cpd-9.sock:/far/x.sock mb2");
    }

    #[test]
    fn a_command_that_does_not_finish_is_killed_at_its_deadline() {
        let mut c = Command::new("sleep");
        c.arg("30");
        let t0 = Instant::now();
        assert!(run_bounded(c, Duration::from_millis(300)).unwrap().is_none());
        assert!(t0.elapsed() < Duration::from_secs(5));
    }

    /// By hand, against the installed binary: `cargo test far::tests::real -- --ignored`.
    /// The generated start command, a real `compound-sup spawn`, a pipe on
    /// stdout; then `close` must find the session.
    #[test]
    #[ignore = "needs a real compound-sup on PATH"]
    fn real_compound_sup_session_starts_promptly_and_closes() {
        let run_dir = std::env::temp_dir().join(format!("wsp-far-real-{}", std::process::id()));
        std::fs::create_dir_all(&run_dir).unwrap();
        let mut m = Machine::new("here", "here");
        m.exec_dir = "/tmp".into();
        let mut sh = Command::new("sh");
        sh.arg("-c").arg(start_command(&m, "wsp-check-real")).env("COMPOUND_RUN_DIR", &run_dir);
        let t0 = Instant::now();
        let out = run_bounded(sh, Duration::from_secs(30)).unwrap().expect("returns");
        let took = t0.elapsed();
        let said = String::from_utf8_lossy(&out.stdout).into_owned();
        eprintln!("{said}took {took:?}");
        assert!(took < Duration::from_secs(5), "{took:?}");
        let sock = socket_of(&said).expect("a socket line");
        let mut close = Command::new(SUP);
        close.args(["close", &sock]).env("COMPOUND_RUN_DIR", &run_dir);
        let closed = run_bounded(close, Duration::from_secs(10)).unwrap().expect("close returns").status;
        assert!(closed.success());
        let _ = std::fs::remove_dir_all(&run_dir);
    }

    fn seat(held: bool, age: Option<i64>) -> BeatSeat {
        BeatSeat { id: "cpd-7".into(), socket: PathBuf::from("/x/cpd-7.sock"), held, age }
    }

    /// `ok` while something holds the seat, `stop` once nothing does — except in
    /// the moments after it opened, before its claim has landed.
    #[test]
    fn strays_are_the_sessions_and_leases_no_seat_owns() {
        let out = "lease cpd-4070\nlease cpd-7\nlease .cpd-9.wsp.tmp\n\
                   /Users/e/bin/compound-sup spawn --label cpd-4070 --cwd /t -- claude\n\
                   compound-sup spawn --label cpd-7 --cwd /t -- claude\n\
                   compound-sup spawn --label wsp-check-5 --cwd /t -- sleep 9\n\
                   vim spawn --label foo\n";
        let got = strays(out, &["cpd-7".to_string()]);
        assert_eq!(got, vec![Stray { label: "cpd-4070".into(), session: true, lease: true }]);
        assert_eq!(drop_lease_command(&machine(), "cpd-1"), "rm -f ~/'wsp-exec/lease/cpd-1' ~/'wsp-exec/lease/.cpd-1.wsp.tmp'");
    }

    #[test]
    fn a_held_seat_is_told_ok_and_a_released_one_stop_but_not_before_its_claim_lands() {
        assert_eq!(seat(true, Some(3600)).state(), "ok");
        assert_eq!(seat(false, Some(3600)).state(), "stop");
        assert_eq!(seat(false, Some(5)).state(), "ok", "just opened");
        assert_eq!(seat(false, None).state(), "stop", "no start time to excuse it");
    }

    /// The frame is compound-remote's, character for character: the literals
    /// are docs/remote.md's "Lease and gate" example, so a drift on either side
    /// is a red test. The version is the controller's own (1), never the sup
    /// wire's.
    #[test]
    fn the_beat_frame_is_the_one_compound_remote_reads() {
        assert_eq!(lease_line("cpd-7", "ok"), "{\"ver\":1,\"lease\":{\"seat\":\"cpd-7\",\"word\":\"ok\"}}\n");
        assert_eq!(lease_line("cpd-7", "stop"), "{\"ver\":1,\"lease\":{\"seat\":\"cpd-7\",\"word\":\"stop\"}}\n");
        assert_ne!(REMOTE_WIRE, crate::place_compound::wire_version(), "the controller wire is not the sup wire");
        assert!(!lease_line("cpd-7", "ok").contains("state"));
    }

    /// The real wire speaks to a socket: an answer is a line back, a refusal is
    /// not an answer, and a socket nobody listens on fails.
    #[test]
    fn the_real_beat_reads_an_answer_off_the_socket_and_fails_without_one() {
        use std::io::{BufRead, BufReader, Write};
        let dir = PathBuf::from("/tmp").join(format!("wl-{}-{}", std::process::id(), crate::util::epoch_nanos() % 100_000));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("s.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let server = std::thread::spawn(move || {
            let mut got = Vec::new();
            for reply in ["{\"ok\":true}\n", "{\"error\":\"no such seat\"}\n"] {
                let (mut c, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(c.try_clone().unwrap()).read_line(&mut line).unwrap();
                got.push(line);
                c.write_all(reply.as_bytes()).unwrap();
            }
            got
        });
        let m = machine();
        assert_eq!(Ssh.lease(&m, &sock, "cpd-7", "ok"), Ok(()));
        let refused = Ssh.lease(&m, &sock, "cpd-7", "stop").unwrap_err();
        assert!(refused.contains("refused"), "{refused}");
        let got = server.join().unwrap();
        assert!(got[0].contains("\"word\":\"ok\"") && got[1].contains("\"word\":\"stop\""), "{got:?}");
        assert!(Ssh.lease(&m, &dir.join("none.sock"), "cpd-7", "ok").is_err(), "nobody listening");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A far seat's first lease is on disk from spawn, before any beat: run for
    /// real, the command leaves `ok` at `<exec_dir>/lease/<seat>` and no temp
    /// file, and a second run replaces it. The gate then finds a fresh lease on
    /// the seat's first tool call instead of waiting out the whole lease for a
    /// beat that goes through a forward made minutes later (wsp-562).
    #[test]
    fn the_first_lease_is_written_at_spawn_before_any_beat() {
        let root = std::path::PathBuf::from("/tmp").join(format!("wf-firstlease-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut m = Machine::new("mb2", "mb2");
        m.exec_dir = format!("{}/it's exec/", root.display());
        let run = || {
            let cmd = format!("{}true", first_lease_command(&m, "cpd-7"));
            let out = std::process::Command::new("sh").arg("-c").arg(&cmd).output().unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        };
        run();
        let dir = root.join("it's exec/lease");
        assert_eq!(std::fs::read_to_string(dir.join("cpd-7")).unwrap(), "ok");
        let age = std::fs::metadata(dir.join("cpd-7")).unwrap().modified().unwrap().elapsed().unwrap_or_default();
        assert!(age < Duration::from_secs(60), "fresh, not stale: {age:?}");
        run();
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, vec![std::ffi::OsString::from("cpd-7")], "no temp file left behind");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The real `compound-remote`, when this machine has one built: `$COMPOUND_REMOTE_BIN`,
    /// else the sibling compound checkout's release build.
    fn real_remote() -> Option<std::path::PathBuf> {
        let from_env = std::env::var_os("COMPOUND_REMOTE_BIN").map(std::path::PathBuf::from);
        let sibling = std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join("claude/compound/target/release/compound-remote"));
        from_env.into_iter().chain(sibling).find(|p| p.is_file())
    }

    /// Run the real gate on `seat` under `root` with a short lease; returns (exit code, how long it took).
    fn run_gate(bin: &std::path::Path, root: &std::path::Path, seat: &str, lease: &str) -> (i32, Duration) {
        let t = std::time::Instant::now();
        let out = std::process::Command::new(bin)
            .args(["gate", "--exec-dir"])
            .arg(root)
            .args(["--seat", seat, "--lease", lease, "--poll", "0.2"])
            .output()
            .unwrap();
        (out.status.code().unwrap_or(-1), t.elapsed())
    }

    /// The whole contract, against the real gate binary (skipped, saying so, where none
    /// is built): a seat spawned with its first lease passes its first tool call at
    /// once; a seat with no lease yet is **held** while the beat is delayed and passes
    /// when the beat lands; a lease that has really expired, a `stop`, and a seat that
    /// never beats are refused (exit 2) after the lease (wsp-562).
    #[test]
    fn the_gate_holds_a_first_call_for_a_delayed_beat_and_still_refuses_an_expired_lease() {
        let Some(bin) = real_remote() else {
            eprintln!("skipped: no compound-remote built (set COMPOUND_REMOTE_BIN)");
            return;
        };
        let root = std::path::PathBuf::from("/tmp").join(format!("wf-gate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut m = Machine::new("mb2", "mb2");
        m.exec_dir = root.display().to_string();
        let sh = |cmd: &str| assert!(std::process::Command::new("sh").arg("-c").arg(cmd).status().unwrap().success(), "{cmd}");
        let lease = root.join("lease");

        // Spawned with its first lease: the first call passes without waiting.
        sh(&format!("{}true", first_lease_command(&m, "cpd-1")));
        let (code, took) = run_gate(&bin, &root, "cpd-1", "3");
        assert_eq!((code, took < Duration::from_secs(1)), (0, true), "fresh first lease passes at once ({took:?})");

        // No lease yet, the beat delayed ~1s: held, then passing.
        std::fs::create_dir_all(&lease).unwrap();
        let late = lease.join("cpd-2");
        let writer = std::thread::spawn({
            let late = late.clone();
            move || {
                std::thread::sleep(Duration::from_millis(1200));
                std::fs::write(late, "ok").unwrap();
            }
        });
        let (code, took) = run_gate(&bin, &root, "cpd-2", "5");
        writer.join().unwrap();
        assert_eq!(code, 0, "the call passes once the beat lands");
        assert!(took >= Duration::from_millis(1000), "and was held for it, not passed on nothing ({took:?})");

        // A lease that has really expired (older than the gate's window), no beat since: refused after the lease.
        sh(&format!("{}true", first_lease_command(&m, "cpd-3")));
        let old = GATE_FRESH_SECS + 60;
        sh(&format!("touch -t $(date -r $(( $(date +%s) - {old} )) +%Y%m%d%H%M.%S) '{}'", lease.join("cpd-3").display()));
        let (code, took) = run_gate(&bin, &root, "cpd-3", "1");
        assert_eq!(code, 2, "an expired lease is refused");
        assert!(took >= Duration::from_millis(900), "after waiting out the lease ({took:?})");

        // `stop` is refused at once, and a seat that never beats after the lease.
        std::fs::write(lease.join("cpd-4"), "stop").unwrap();
        assert_eq!(run_gate(&bin, &root, "cpd-4", "3").0, 2);
        let (code, took) = run_gate(&bin, &root, "cpd-5", "1");
        assert_eq!(code, 2, "no lease and no beat: refused");
        assert!(took >= Duration::from_millis(900), "after the lease ({took:?})");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The first lease covers only the gate's freshness window, and the command must
    /// not quietly change that window: no `--fresh`, so `GATE_FRESH_SECS` (75 s) holds
    /// and a stale or missing lease is polled for up to the machine's lease.
    #[test]
    fn the_first_lease_covers_the_gates_fresh_window_and_the_command_leaves_it_alone() {
        let m = Machine::new("mb2", "mb2");
        let g = gate_command(&m, "cpd-7");
        assert!(!g.contains("--fresh"), "{g}");
        assert!(g.contains(&format!("--lease {}", m.lease_secs())), "{g}");
        assert_eq!(GATE_FRESH_SECS, 75, "compound-remote's lease::FRESH");
        assert!(u64::from(m.lease_secs()) > GATE_FRESH_SECS, "the poll outlasts the fresh window");
    }

    /// The hook carries the machine's lease, outlives it, replaces an earlier
    /// gate rather than doubling it, and will not overwrite a settings file it
    /// cannot read.
    #[test]
    fn the_gate_carries_the_lease_outlives_it_and_is_written_once() {
        let mut m = machine();
        m.exec_dir = "/far/exec".into();
        m.sup_at = "/opt/c/bin/compound-sup".into();
        m.lease = Some(300);
        let once = settings_with_gate("", &m, "cpd-7").unwrap();
        let twice = settings_with_gate(&once, &m, "cpd-7").unwrap();
        assert_eq!(once, twice, "idempotent");
        let v: serde_json::Value = serde_json::from_str(&once).unwrap();
        let h = &v["hooks"]["PreToolUse"][0]["hooks"][0];
        assert_eq!(h["command"], "/opt/c/bin/compound-remote gate --exec-dir '/far/exec' --seat 'cpd-7' --lease 300");
        assert!(h["timeout"].as_u64().unwrap() > 300);
        assert!(settings_with_gate("not json", &m, "cpd-7").unwrap_err().contains("not overwriting"));
        assert!(settings_with_gate("[]", &m, "cpd-7").is_err());
    }

    /// The far tree gets the turn-end checkpoint with the gate: the shim's
    /// `checkpoint`, by absolute path, written once, beside any other `Stop` hook
    /// the tree already has, and never able to fail the turn.
    #[test]
    fn the_far_tree_gets_a_stop_checkpoint_beside_the_gate_and_written_once() {
        let mut m = machine();
        m.exec_dir = "/far/exec".into();
        let theirs = r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"echo mine"}]}]},"model":"x"}"#;
        let once = settings_with_gate(theirs, &m, "cpd-7").unwrap();
        assert_eq!(once, settings_with_gate(&once, &m, "cpd-7").unwrap(), "idempotent");
        let v: serde_json::Value = serde_json::from_str(&once).unwrap();
        let stop = v["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2, "theirs is kept, ours is added once");
        assert_eq!(stop[0]["hooks"][0]["command"], "echo mine");
        assert_eq!(stop[1]["hooks"][0]["command"], "'/far/exec/bin/wsp' checkpoint >/dev/null 2>&1 || true");
        assert_eq!(v["model"], "x");
        assert!(v["hooks"]["PreToolUse"][0]["hooks"][0]["command"].as_str().unwrap().contains(" gate --exec-dir "));
        assert!(settings_with_gate(r#"{"hooks":{"Stop":{}}}"#, &m, "cpd-7").is_err());
    }

    /// A machine is silent when its link is down, or when its beat has gone
    /// unanswered past its lease; one never beaten with the link up is not. The
    /// sentence and the seat word say how long, and a machine that is heard says
    /// nothing at all (`docs/remote-agents.md` section 3).
    /// wsp-588: a 60 s lease never reads silent while beats answer on time, with
    /// no real clock: the daemon's 20 s tick (with a late one now and then) drives
    /// the beat's schedule, and the silence rule is asked every second.
    #[test]
    fn a_sixty_second_lease_never_reads_silent_while_beats_answer() {
        let mut m = Machine::new("mb2", "mb2");
        m.lease = Some(60);
        assert_eq!(beat_every(&m), Duration::from_secs(20), "a third of the lease");
        assert!(beat_wait(&m) < beat_every(&m));
        // The default lease keeps the 30 s beat.
        assert_eq!(beat_every(&Machine::new("mb2", "mb2")), Duration::from_secs(BEAT_SECS));

        // Ticks 20 s apart, a late one of 35 s (a loaded machine) among them.
        let gaps = [20, 20, 20, 35, 20, 20, 35, 35, 20, 20, 20, 20];
        let (mut now, mut next, mut heard) = (0i64, 0i64, 0i64);
        for gap in gaps.iter().cycle().take(200) {
            now += gap;
            if now >= next {
                heard = now;
                next = now + beat_wait(&m).as_secs() as i64;
            }
            // The next tick is `gap` away at worst; ask at every second up to it.
            for t in now..now + gap {
                assert!(!heard_is_stale(&m, t - heard), "silent {}s after a beat at tick {now}", t - heard);
            }
        }
        // And the rule still means something: a beat a lease and a beat old.
        assert!(!heard_is_stale(&m, 60 + 20));
        assert!(heard_is_stale(&m, 60 + 21));
    }

    /// wsp-588: with no far seat left to beat, the last beat is forgotten, so the
    /// machine reads off the probe and not off a seat that is gone.
    #[test]
    fn a_machine_with_no_far_seat_left_is_not_silent_off_an_old_beat() {
        let m = Machine::new("mb2", "mb2");
        let ago = |secs: i64| crate::util::iso_at(crate::util::epoch_secs() - secs);
        let mut l = MachineLive { reachable: true, last_seen: ago(5), heard: ago(4000), ..Default::default() };
        l.leases.insert("cpd-7".into(), ago(4000));
        assert!(is_silent(&m, &l), "an old beat with a seat to answer it is silence");
        beat_idle(&mut l);
        assert!(l.heard.is_empty() && l.leases.is_empty());
        assert!(!is_silent(&m, &l), "idle: the probe says reachable");
    }

    #[test]
    fn a_machine_is_silent_on_a_down_link_or_a_stale_beat_and_says_how_long() {
        let m = Machine::new("mb2", "mb2");
        let ago = |secs: i64| crate::util::iso_at(crate::util::epoch_secs() - secs);
        let up = |heard: String| MachineLive { reachable: true, last_seen: ago(5), heard, ..Default::default() };

        assert!(!is_silent(&m, &up(ago(30))), "a fresh beat");
        assert!(!is_silent(&m, &up(String::new())), "nothing beaten yet is not silence");
        assert!(is_silent(&m, &up(ago(m.lease_secs() as i64 + 60))), "a beat older than the lease");
        let down = MachineLive { reachable: false, last_seen: ago(240), heard: ago(300), ..Default::default() };
        assert!(is_silent(&m, &down), "a down link");
        assert_eq!(last_heard(&down), down.heard, "the beat, not the probe");
        assert_eq!(last_heard(&MachineLive { last_seen: ago(9), ..Default::default() }), ago(9), "the probe when never beaten");

        assert_eq!(unreachable_word(&ago(240)), "unreachable 4m");
        assert_eq!(unreachable_word(""), "unreachable never heard");

        let mut live = std::collections::BTreeMap::new();
        live.insert("mb2".to_string(), up(ago(10)));
        assert_eq!(loud(&[m.clone()], &live), None, "a heard machine is not a fault");
        live.insert("mb2".to_string(), down);
        let line = loud(&[m.clone()], &live).expect("a silent one is");
        assert!(line.contains("mb2 (unreachable 5m)") && line.contains("held, not gone"), "{line}");
        let mut retired = m;
        retired.status = "retired".into();
        assert_eq!(loud(&[retired], &live), None, "a retired machine is nobody's fault");
    }

    /// Every surface that draws seats reads one `Silence`: the sentence, and per
    /// far seat the word its row wears.
    #[test]
    fn silence_names_the_seats_on_a_silent_machine_and_nobody_else() {
        let env = crate::util::isolated("far-silence");
        let store = crate::store::Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        store.save_machine(&Machine::new("mb2", "mb2")).unwrap();
        assert!(Silence::read(&store).line.is_none(), "no report yet is not a silence");
        let at = crate::util::iso_at(crate::util::epoch_secs() - 240);
        store.set_machine_live("mb2", &MachineLive { reachable: false, heard: at, ..Default::default() });
        let silence = Silence::read(&store);
        assert!(silence.line.as_deref().is_some_and(|l| l.contains("mb2 (unreachable 4m)")));
        assert!(silence.seats.is_empty(), "no seat is on it here");
    }
}
