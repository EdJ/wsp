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
use std::process::{Command, Stdio};
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
    let mut c = Command::new("ssh");
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
/// keeps. Names that are not shell identifiers are dropped.
pub fn seat_command(m: &Machine, label: &str, cwd: &str, env: &BTreeMap<String, String>, program: &str, args: &[String]) -> String {
    let token = token_path(m);
    let msg = format!("no Claude token at {token} on {} (claude setup-token there)", m.name);
    let before = format!("t=$(cat {} 2>/dev/null) && [ -n \"$t\" ] || {{ echo {} >&2; exit 1; }}; ", quote(&token), quote(&msg));
    let mut pre = String::from("CLAUDE_CODE_OAUTH_TOKEN=\"$t\" env ");
    // `env` stops reading options at the first NAME=value (POSIX, and macOS's), so
    // every `-u K` goes before any `K=V`.
    let named: Vec<(&String, &String)> = env
        .iter()
        .filter(|(k, _)| {
            let ident = !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && !k.starts_with(|c: char| c.is_ascii_digit());
            ident && k.as_str() != "CLAUDE_CODE_OAUTH_TOKEN"
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
  if (op === "add") { doc.projects[n] = doc.projects[n] || {}; doc.projects[n].hasTrustDialogAccepted = true; }
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
}

/// Whether a far machine is silent: its link is down, or its seats' beat has gone
/// unanswered for longer than its lease (`docs/remote-agents.md` section 3).
/// A machine never beaten (`heard` empty) while the link is up is not silent: it
/// has simply had no far seat to beat.
pub fn is_silent(m: &Machine, l: &MachineLive) -> bool {
    !l.reachable || (!l.heard.is_empty() && crate::util::since(&l.heard) > i64::from(m.lease_secs()))
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
    /// The seat's forwarded controller socket, on this side.
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
            Ok(()) => answered = true,
            Err(why) => failed.push((s.id.clone(), why)),
        }
    }
    if answered {
        live.heard = now.to_string();
    }
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

/// What the hook may take, in seconds: the gate waits up to the lease, so the
/// hook's own timeout is above it.
pub fn gate_timeout(m: &Machine) -> u32 {
    m.lease_secs() + 60
}

/// `existing` (the far tree's `.claude/settings.json`, empty when there is none)
/// with the gate under `hooks.PreToolUse`. An earlier gate entry is replaced and
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

    /// A machine is silent when its link is down, or when its beat has gone
    /// unanswered past its lease; one never beaten with the link up is not. The
    /// sentence and the seat word say how long, and a machine that is heard says
    /// nothing at all (`docs/remote-agents.md` section 3).
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
