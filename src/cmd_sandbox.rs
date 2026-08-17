//! `wsp sandbox` — a whole isolated wsp: its own herdr, its own store, its own
//! state, in one command.
//!
//! `wsp verify` isolates the *build*. This isolates the *run*. A worktree gets
//! you a compile that means something; it does nothing for the verbs that write
//! — `sync`, `reconcile --reap`, `adopt`, `spawn`, the panel's keys — because
//! those talk to the one herdr everybody is standing in and the one store
//! everybody's claims are in. Testing them in the live instance is how an agent
//! ends another agent's claim while checking that reaping works.
//!
//! It needs no new mechanism. Three environment variables wsp has always read,
//! and one herdr feature we had not noticed:
//!
//!     herdr --session <name> server
//!
//! brings up a second headless server under `~/.config/herdr/sessions/<name>/`
//! with its own socket and log, beside the running one and without disturbing
//! it. With `WSP_HOME` and `WSP_STATE` that is a complete instance. Proved on
//! 2026-08-16 with four agents working in this checkout: a probe session came
//! up in under a second, `wsp reconcile --reap` — the most destructive verb
//! there is — ran against it, and `claims.json` and `bindings.json` in
//! `~/.local/state/wsp` were byte-identical before and after.
//!
//! So this command is a wrapper, and its whole value is that nobody has to
//! remember the incantation *or the teardown*.
//!
//! # `wsp` inside a sandbox means the binary under test
//!
//! The rule this exists to enforce by construction: **run the built binary by
//! path, never `~/.local/bin/wsp`**. The install is one file that every panel,
//! every detail pane and the daemon re-execs into; making it a step in testing
//! something puts every agent's live session downstream of your experiment.
//!
//! Remembering to type `target/debug/wsp` is exactly the sort of rule that gets
//! followed for a day. So the sandbox holds a `bin/wsp` symlink at whichever
//! binary invoked it, and puts that directory first on `PATH`. Inside the
//! sandbox, `wsp` *is* the binary under test — including for anything the child
//! shells out to, and for `herdr-plugin/run.sh`, which checks `WSP_BIN` before
//! `~/.local/bin/wsp`.
//!
//! # …or no herdr at all
//!
//! `--fake` swaps the one part of the instance that has to be real for one that
//! does not: [`crate::fake`] answers the same socket out of a state written
//! down in a file. Everything else is unchanged — same store, same state, same
//! `wsp` shim — because the value of it is precisely that nothing downstream
//! can tell.
//!
//! It is for the states a live herdr cannot be put in, which is most of the
//! ones that have cost this store anything: a machine that stops answering
//! mid-tick, an agent stuck in its launch window, twenty-two workspaces, a
//! pane list that goes empty. A herdr you can start in 0.1s is not the problem
//! and never was.
//!
//! The division stays explicit: **the fake is for wsp's reaction to a state,
//! and the herdr sandbox stays the contract check against real behaviour**. A
//! fake that is wrong about herdr makes tests green on a lie, so anything
//! asserting what herdr *does* is recorded from a live one — see the module
//! docs there, including the two things it found the port had wrong.
//!
//! # What a sandbox is not
//!
//! Its herdr starts empty, and manufacturing twenty-two workspaces is not a
//! thing this can do. (`--fake` can, which is the point of it.) `--seed` copies the store — projects and tasks, so `ls`,
//! `tree`, `show` and the panel have something to draw — and deliberately not
//! the machine state, because a claim names a workspace id that exists in
//! nobody's herdr but the live one. When what you need is this machine's actual
//! workspaces and agents, no sandbox reproduces it; that residue is
//! t-260816-057.
//!
//! # It is not empty of *processes*
//!
//! `plugins.json` is one file for the whole machine and a headless session
//! server loads it — which was got wrong first, on two probes that had pointed
//! `WSP_HOME` at an empty directory to be safe, so the plugin's `wsp daemon`
//! found no store, exited, and left nothing to find. The redirect that made the
//! probe safe is what suppressed the evidence.
//!
//! So a sandbox is a set of processes as well as a socket and two directories,
//! and all three of the places it can leak are handled here: the server is
//! started *inside* the sandbox so its children get the sandbox's store
//! ([`Sandbox::store_env`]), teardown reaps what it started ([`stop_session`]),
//! and `ls` counts processes so a stray with no session and no directory is
//! still on the list. The daemon's own refusal is the fourth, in
//! [`crate::daemon`]. See t-260816-076.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::store::Store;
use crate::util;
use crate::Args;

/// How long to wait for a session's socket to answer. Measured at well under a
/// second on this machine; the deadline is for the case where herdr is not
/// coming up at all, and it has to be long enough that a loaded machine is not
/// mistaken for a broken one.
const READY: Duration = Duration::from_secs(20);

/// Every sandbox session is named `wsp-…`, so `herdr session list` says whose
/// it is and `wsp sandbox ls` can find one whose directory has been removed
/// from underneath it.
const PREFIX: &str = "wsp-";

/// A whole instance, in the five variables that address it.
struct Sandbox {
    /// The herdr session name, which is also the directory name and the handle
    /// `wsp sandbox rm` takes.
    name: String,
    /// `<state>/sandbox/<name>` — everything below is inside it.
    dir: PathBuf,
    /// The store: `WSP_HOME`.
    home: PathBuf,
    /// The machine state: `WSP_STATE`.
    state: PathBuf,
    /// The binary under test — whichever one is running right now.
    bin: PathBuf,
    /// The directory holding the `wsp` symlink, which goes first on `PATH`.
    shim: PathBuf,
    /// herdr's socket, once the session answers. Empty until then.
    socket: PathBuf,
}

impl Sandbox {
    fn new(store: &Store, name: &str, bin: PathBuf) -> Sandbox {
        let dir = store.state.join("sandbox").join(name);
        Sandbox {
            name: name.to_string(),
            home: dir.join("wsp"),
            state: dir.join("state"),
            shim: dir.join("bin"),
            socket: PathBuf::new(),
            bin,
            dir,
        }
    }

    /// Which instance this is, without saying where its herdr is.
    ///
    /// Split out because the herdr *server* has to be started with these and
    /// cannot be told the socket: it has not chosen one yet. That is not a
    /// detail. A session server hands its own environment to every plugin it
    /// starts, and the `[[startup]]` entry in the global `plugins.json` is `wsp
    /// daemon` — so a server started without these gives its daemon the socket
    /// of the sandbox and the store of the *live* instance, which is precisely
    /// the pair that makes `sync` reap every binding in the real store. See
    /// t-260816-076, and t-260816-058 for the reaping half.
    fn store_env(&self) -> Vec<(String, String)> {
        vec![
            ("WSP_HOME".into(), self.home.display().to_string()),
            ("WSP_STATE".into(), self.state.display().to_string()),
            ("WSP_BIN".into(), self.bin.display().to_string()),
            ("PATH".into(), path_with(&self.shim, std::env::var("PATH").ok().as_deref())),
        ]
    }

    /// The environment a command runs in here: the instance, and where to
    /// reach it.
    fn env(&self) -> Vec<(String, String)> {
        let mut env = self.store_env();
        env.insert(0, ("HERDR_SOCKET_PATH".into(), self.socket.display().to_string()));
        env
    }
}

/// The two `HERDR_` variables that are about *where herdr is* rather than about
/// which pane the caller is standing in. Everything else with that prefix is a
/// fact about the caller's session, and false in here.
const KEEP: &[&str] = &["HERDR_SOCKET_PATH", "HERDR_BIN"];

/// What a sandbox has to forget.
///
/// The caller's environment is full of things its herdr told it — pane id,
/// workspace id, tab id, the plugin event it was invoked from — and in a
/// sandbox every one of them names something that herdr has never heard of.
/// Left in place, `wsp claim` inside a sandbox binds a task to a pane that does
/// not exist there: a phantom binding, which is the class of bug this codebase
/// has spent the most time on. So the rule is the prefix rather than a list of
/// the ones we happen to know about — a variable herdr adds next month is a
/// fact about the caller's session too — and the two locators are kept.
///
/// `GIT_INDEX_FILE` goes for the same reason it goes in `verify`: an agent
/// halfway through `wsp commit-help` has one exported, and a `git` command in
/// here would stage into their commit.
///
/// [`crate::place::SEAT_ENV`] is named because it is the same fact under a
/// prefix this rule does not cover: it is what a backend that is not herdr calls
/// the caller's pane id, and a phantom binding made through it would be the
/// identical bug. Nothing sets it today — herdr's name for the seat is
/// `HERDR_PANE_ID` — so this is inert, and it is here because the moment to
/// write it down is before the first backend that does.
fn forgettable(key: &str) -> bool {
    key == "GIT_INDEX_FILE"
        || key == crate::place::SEAT_ENV
        || (key.starts_with("HERDR_") && !KEEP.contains(&key))
}

/// The same rule, against this process's actual environment: what a caller
/// would have to `unset` by hand.
fn forget_keys() -> Vec<String> {
    let mut keys: Vec<String> = std::env::vars_os()
        .filter_map(|(k, _)| k.into_string().ok())
        .filter(|k| forgettable(k))
        .collect();
    keys.sort();
    keys
}

/// Everything a process wsp starts in here must not inherit, taken off it.
///
/// Two lists and they are not the same list, which is why this is a function
/// rather than one more clause in [`forgettable`]. What that one names is
/// wrong *in a sandbox* and is printed for the caller to `unset` in their own
/// shell. [`crate::place::shed_keys`] names the caller's Claude Code session,
/// which is wrong in **any** new session and is nobody's business to unset in
/// the shell they are sitting in — an agent that unset `CLAUDECODE` for its own
/// pane would have changed how every command it runs behaves.
///
/// It belongs here because **this is the one server wsp starts itself**, and a
/// server is what starts every process under it. Recorded 2026-08-17 off the
/// sandbox server of an agent's own `wsp sandbox`: `CLAUDECODE`,
/// `CLAUDE_CODE_CHILD_SESSION`, `CLAUDE_CODE_SESSION_ID`, and the caller's
/// `CLAUDE_CODE_MESSAGING_SOCKET` with the token for it — inherited whole, and
/// handed on to every pane the sandbox would ever open. `cmd_spawn::order`
/// sheds them again for the seats *wsp* opens; this is what a pane somebody
/// opens by hand in here gets, and it is where the measurement that opened
/// t-260817-006 lost its transcript.
fn forget(c: &mut Command) {
    for k in forget_keys() {
        c.env_remove(k);
    }
    for k in crate::place::shed_keys() {
        c.env_remove(k);
    }
}

/// `PATH` is prefixed rather than replaced: a sandbox is still a shell, and a
/// command that cannot find `git` or `cargo` is not isolated, it is broken.
fn path_with(shim: &Path, current: Option<&str>) -> String {
    match current {
        Some(p) if !p.is_empty() => format!("{}:{p}", shim.display()),
        _ => shim.display().to_string(),
    }
}

/// herdr, wherever it is.
///
/// `Command::new("herdr")` is right nearly always, and wrong in the one case
/// that matters most: a `wsp` started by herdr's own plugin runner inherits
/// whatever `PATH` herdr was launched with, which is why `run.sh` looks the
/// binary up by hand rather than trusting it. Same reasoning, same fallback.
fn herdr_bin() -> PathBuf {
    if let Some(v) = std::env::var_os("HERDR_BIN") {
        let p = PathBuf::from(v);
        if !p.as_os_str().is_empty() {
            return p;
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let p = dir.join("herdr");
            if p.is_file() {
                return p;
            }
        }
    }
    util::home().join(".local/bin/herdr")
}

fn herdr(args: &[&str]) -> Result<String, String> {
    let out = Command::new(herdr_bin())
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("herdr {}: {e}", args.join(" ")))?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
    Err(if msg.is_empty() { format!("herdr {} failed", args.join(" ")) } else { msg })
}

/// What herdr says its sessions are: name, running, socket. Asked rather than
/// computed — the layout under `~/.config/herdr` is herdr's business, and a
/// path we built ourselves would go stale silently.
fn sessions() -> Vec<(String, bool, PathBuf)> {
    let Ok(text) = herdr(&["session", "list", "--json"]) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    v.get("sessions")
        .and_then(|s| s.as_array())
        .map(|arr| {
            arr.iter()
                .map(|s| {
                    let name = s.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
                    let running = s.get("running").and_then(|x| x.as_bool()).unwrap_or(false);
                    let sock = s.get("socket_path").and_then(|x| x.as_str()).unwrap_or("");
                    (name, running, PathBuf::from(sock))
                })
                .filter(|(n, _, _)| !n.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn session(name: &str) -> Option<(bool, PathBuf)> {
    sessions().into_iter().find(|(n, _, _)| n == name).map(|(_, r, s)| (r, s))
}

/// Bring the session up and wait until its socket actually answers.
///
/// The socket file appearing is not the same as herdr being ready to speak, and
/// the difference is a race that only shows up on a busy machine — so this
/// connects, which is the only question worth asking.
///
/// The server is started **inside the sandbox**, not beside it. It was not, and
/// the first live run left a `wsp daemon` on the real store: a headless session
/// server does load plugins, it starts them with its own environment, and the
/// environment it had was the caller's. Everything a sandbox knows about itself
/// has to be in place before the server exists, because the server is what
/// starts the processes.
fn start_session(sb: &Sandbox) -> Result<PathBuf, String> {
    let name = &sb.name;
    let mut c = Command::new(herdr_bin());
    c.args(["--session", name, "server"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    forget(&mut c);
    // Not in `forget_keys` — it is how a *child* is told where the sandbox is,
    // and we do not know that yet. What we do know is that the caller's is
    // wrong, and the server sets its own for everything it starts.
    c.env_remove("HERDR_SOCKET_PATH");
    for (k, v) in sb.store_env() {
        c.env(k, v);
    }
    c.spawn().map_err(|e| format!("cannot start herdr: {e}"))?;

    let deadline = Instant::now() + READY;
    loop {
        if let Some((running, sock)) = session(name) {
            if running && std::os::unix::net::UnixStream::connect(&sock).is_ok() {
                return Ok(sock);
            }
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "herdr session `{name}` did not come up in {}s — its log is {}",
                READY.as_secs(),
                util::contract(&util::home().join(format!(
                    ".config/herdr/sessions/{name}/herdr-server.log"
                )))
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Every process this machine is running, as `(pid, ppid, the rest of the
/// line)` — where "the rest" includes the environment.
///
/// `ps -E` is how a process's environment is readable without a dependency or a
/// `/proc`, and the environment is the only place a plugin's child says which
/// herdr started it: its command line is `wsp daemon`, identical to the live
/// one's. `ppid` comes back in the same call so the ancestors of this process
/// can be ruled out of anything we are about to kill — a shell whose command
/// line happens to mention the session is not a child of it.
///
/// Shared with [`crate::daemon`], which asks the same output the same two
/// questions — what is running, and which store its environment points it at —
/// and would otherwise keep a second copy of the parsing that was already
/// silently wrong once (see [`ps_line`]).
pub(crate) fn processes() -> Vec<(u32, u32, String)> {
    let Ok(out) = Command::new("ps").args(["-A", "-E", "-o", "pid=,ppid=,command="]).output() else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout).lines().filter_map(ps_line).collect()
}

/// One line of that, as `(pid, ppid, the rest)`.
///
/// Its own function because it is the part that was wrong, and wrong silently:
/// `ps` right-aligns the numeric columns, so a `splitn` on whitespace found an
/// empty second field, failed to parse it and dropped **every** line. The
/// teardown then reaped nothing and said nothing, which is precisely the shape
/// of the defect it was written to fix.
fn ps_line(line: &str) -> Option<(u32, u32, String)> {
    let (pid, rest) = line.trim_start().split_once(char::is_whitespace)?;
    let (ppid, rest) = rest.trim_start().split_once(char::is_whitespace)?;
    Some((pid.parse().ok()?, ppid.parse().ok()?, rest.trim_start().to_string()))
}

/// What the session started and still has running.
///
/// A herdr session server starts the `[[startup]]` command of every enabled
/// plugin — globally configured, not per session — and puts `HERDR_SESSION` in
/// their environment. That name is the only thing that distinguishes a
/// sandbox's daemon from the live one, so it is what this matches on, with a
/// boundary so `wsp-w1` does not reap `wsp-w12`.
///
/// This process and its ancestors are never in the answer. The shell that typed
/// `wsp sandbox rm` can have the session name on its own command line, and `ps
/// -E` puts the command and the environment in one field.
fn session_children(name: &str) -> Vec<u32> {
    let all = processes();
    children_of(&all, name)
}

/// This process and everything above it, so nothing we are standing on can end
/// up in a list of things to kill — or, for [`crate::daemon`], in a list of
/// daemons we are about to say are running: the shell that typed `wsp doctor`
/// has `wsp doctor` on its command line and its parent has whatever started it.
pub(crate) fn ancestors(all: &[(u32, u32, String)]) -> Vec<u32> {
    let mut out: Vec<u32> = vec![std::process::id()];
    // Bounded by the list itself, so a cycle cannot spin.
    for _ in 0..all.len() {
        let Some(&(_, ppid, _)) = out.last().and_then(|p| all.iter().find(|(pid, _, _)| pid == p))
        else {
            break;
        };
        if ppid == 0 || out.contains(&ppid) {
            break;
        }
        out.push(ppid);
    }
    out
}

/// `HERDR_SESSION=<name>` in a process's environment, with a boundary after it
/// so `wsp-w1` does not match `wsp-w12`.
fn holds_session(rest: &str, name: &str) -> bool {
    let needle = format!("HERDR_SESSION={name}");
    rest.match_indices(&needle)
        .any(|(i, _)| matches!(rest.as_bytes().get(i + needle.len()), None | Some(b' ') | Some(b'\t')))
}

fn children_of(all: &[(u32, u32, String)], name: &str) -> Vec<u32> {
    let mine = ancestors(all);
    all.iter()
        .filter(|(pid, _, _)| !mine.contains(pid))
        .filter(|(_, _, rest)| holds_session(rest, name))
        .map(|(pid, _, _)| *pid)
        .collect()
}

/// Every sandbox some process still says it belongs to.
///
/// The third place a sandbox can be, after the session list and the state
/// directory — and the one `ls` was blind to. It said "no sandboxes" with a
/// stray daemon up, because it asked herdr what sessions existed and a session
/// is not the only thing a sandbox creates.
fn sessions_with_processes() -> Vec<(String, usize)> {
    let all = processes();
    let mine = ancestors(&all);
    let mut out: Vec<(String, usize)> = Vec::new();
    for (pid, _, rest) in &all {
        if mine.contains(pid) {
            continue;
        }
        for (i, _) in rest.match_indices("HERDR_SESSION=") {
            let name: String = rest[i + "HERDR_SESSION=".len()..]
                .chars()
                .take_while(|c| !c.is_whitespace())
                .collect();
            if !name.starts_with(PREFIX) {
                continue;
            }
            match out.iter_mut().find(|(n, _)| *n == name) {
                Some((_, n)) => *n += 1,
                None => out.push((name, 1)),
            }
        }
    }
    out
}

/// Said out loud rather than counted silently: a teardown that killed something
/// is a fact about what the sandbox was running, and the whole reason this
/// exists is that "torn down" was printed once while a daemon carried on.
fn reaped_note(n: usize) -> String {
    match n {
        0 => String::new(),
        1 => " — 1 process it started reaped".to_string(),
        n => format!(" — {n} processes it started reaped"),
    }
}

fn kill(pids: &[u32], signal: &str) {
    if pids.is_empty() {
        return;
    }
    let mut c = Command::new("kill");
    c.arg(signal);
    for p in pids {
        c.arg(p.to_string());
    }
    let _ = c.stdout(Stdio::null()).stderr(Stdio::null()).status();
}

/// Stop it, reap what it started, and delete it. Best-effort throughout: what
/// this is usually clearing up is a half-torn-down sandbox, where some of it
/// has already happened.
///
/// The reaping is the part `herdr session stop` does not do and cannot be
/// expected to: it ends the server, and the server's children are reparented to
/// `launchd` and carry on. One did — a `wsp daemon` holding the socket of a
/// session that no longer existed and, because it had not been told otherwise,
/// the live store. So the sandbox reaps what it started, on the only handle
/// there is, and does it *after* the stop so that nothing is restarted behind
/// us.
///
/// Returns whether there was a session, and how many processes it left.
fn stop_session(name: &str) -> (bool, usize) {
    let existed = session(name).is_some();
    let _ = herdr(&["session", "stop", name]);

    let children = session_children(name);
    kill(&children, "-TERM");
    if !children.is_empty() {
        // A daemon in the middle of a sync has a store lock and a socket read
        // to finish. Long enough to let it, short enough that a teardown is
        // still something you wait for.
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(50));
            if session_children(name).is_empty() {
                break;
            }
        }
        kill(&session_children(name), "-KILL");
    }

    let _ = herdr(&["session", "delete", name]);
    (existed, children.len())
}

/// Copy a tree, leaving `.git` out of it.
///
/// Seeding wants the store's *content*, not its history: the history is a
/// hundred megabytes of somebody else's commits, and a sandbox that shared it
/// would be one `git commit` away from writing into the real store's object
/// database.
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        if entry.file_name() == ".git" {
            continue;
        }
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        let ft = entry.file_type()?;
        if ft.is_dir() {
            copy_tree(&src, &dst)?;
        } else if ft.is_file() {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

/// Everything about a sandbox that is not the herdr session: the directories,
/// the `wsp` shim, and a store that exists.
///
/// Separate from bringing the session up so it can be tested, and so the
/// expensive half is the one that can fail last.
fn lay_out(sb: &Sandbox, seed: Option<&Store>) -> Result<(), String> {
    std::fs::create_dir_all(&sb.shim).map_err(|e| format!("cannot make {}: {e}", util::contract(&sb.shim)))?;
    let link = sb.shim.join("wsp");
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(&sb.bin, &link)
        .map_err(|e| format!("cannot link {} at {}: {e}", util::contract(&sb.bin), util::contract(&link)))?;

    let store = Store::at(sb.home.clone(), sb.state.clone());
    if let Some(live) = seed {
        std::fs::create_dir_all(&sb.home).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&sb.state).map_err(|e| e.to_string())?;
        copy_tree(&live.root, &sb.home)
            .map_err(|e| format!("cannot copy {}: {e}", util::contract(&live.root)))?;
    }
    crate::cmd_project::init_store(&store).map_err(|e| format!("cannot make the store: {e}"))?;
    Ok(())
}

/// The name of this agent's sandbox.
///
/// Keyed on the agent, exactly as `wsp verify` keys its build tree, and for the
/// same reason: one per agent to leak rather than one per run. Two agents get
/// two sandboxes and never collide; one agent running `wsp sandbox` twice gets
/// its own back, freshly made.
fn name_for(args: &Args) -> String {
    session_name(args.get("name").as_deref(), &crate::cmd_verify::agent_key())
}

fn session_name(given: Option<&str>, key: &str) -> String {
    let given = given.unwrap_or("").trim();
    let base = util::slugify(if given.is_empty() { key } else { given });
    let base = if base.is_empty() { "solo".to_string() } else { base };
    if base.starts_with(PREFIX) {
        base
    } else {
        format!("{PREFIX}{base}")
    }
}

pub fn sandbox(store: &Store, args: &Args) -> i32 {
    match args.rest.first().map(|s| s.as_str()) {
        Some("rm" | "remove" | "stop") => rm(store, args),
        Some("ls" | "list") => ls(store, args),
        Some("serve") => serve(args),
        Some(other) => {
            eprintln!("wsp sandbox: unknown subcommand `{other}` — try `ls` or `rm`");
            2
        }
        None => up(store, args),
    }
}

/// `wsp sandbox serve` — be the fake, on a socket, until something kills us.
///
/// Not a second command surface, which is why it is a subcommand of the one
/// that already means *an isolated instance*: `--fake` spawns this, and nobody
/// is expected to type it. It exists as a verb at all because the fake has to
/// outlive the process that asked for it, exactly as the herdr session does.
fn serve(args: &Args) -> i32 {
    let Some(socket) = args.get("socket") else {
        eprintln!("wsp sandbox serve: --socket <path> is what it binds");
        return 2;
    };
    let stage = args.get("stage").map(PathBuf::from);
    match crate::fake::serve_forever(Path::new(&socket), stage.as_deref()) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("wsp: cannot serve a fake on {socket}: {e}");
            1
        }
    }
}

/// The socket a fake sandbox binds, and the marker that it is one.
///
/// Inside the sandbox directory rather than under `~/.config/herdr`, because
/// there is no herdr here to own it — which is also what makes it a reliable
/// answer to "is this sandbox a fake" for `ls`.
fn fake_socket(dir: &Path) -> PathBuf {
    dir.join("herdr.sock")
}

/// The stage a fake starts from, and goes on watching.
fn stage_file(dir: &Path) -> PathBuf {
    dir.join("stage.json")
}

/// The process that will be the fake, and the environment it gets.
///
/// Split out for the same reason [`child`] is: what a spawned process is handed
/// is the thing that has gone wrong before, and the only honest check is the
/// command itself rather than what we meant by it.
fn fake_command(sb: &Sandbox, stage: &Path) -> Command {
    let socket = fake_socket(&sb.dir);
    let mut c = Command::new(&sb.bin);
    c.args(["sandbox", "serve"])
        .arg("--socket")
        .arg(&socket)
        .arg("--stage")
        .arg(stage)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    forget(&mut c);
    for (k, v) in sb.store_env() {
        c.env(k, v);
    }
    c.env("HERDR_SESSION", &sb.name);
    c.env("HERDR_SOCKET_PATH", &socket);
    c
}

/// Start the fake and wait until its socket answers.
///
/// The wait is the same question `start_session` asks and for the same reason —
/// a socket file appearing is not the same as something answering on it — but
/// the deadline is short, because there is no process to boot here and nothing
/// to be patient about. A fake that has not answered in two seconds is not
/// coming.
///
/// `HERDR_SESSION` is put back into the child's environment on purpose, having
/// just been stripped with the rest of the caller's. It is not a claim that
/// there is a herdr session: it is the one handle `stop_session` and
/// `sandbox ls` have for "which sandbox does this process belong to", and a
/// second mechanism for the same question is how a stray ends up invisible to
/// both — which is the defect t-260816-076 was opened for.
fn start_fake(sb: &Sandbox, stage: &Path) -> Result<PathBuf, String> {
    let socket = fake_socket(&sb.dir);
    fake_command(sb, stage).spawn().map_err(|e| format!("cannot start the fake: {e}"))?;

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if std::os::unix::net::UnixStream::connect(&socket).is_ok() {
            return Ok(socket);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the fake did not answer on {} — is `{}` the binary you meant?",
                util::contract(&socket),
                util::contract(&sb.bin)
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn up(store: &Store, args: &Args) -> i32 {
    let p = util::Paint::new();
    let json_out = args.json();

    let Ok(bin) = std::env::current_exe() else {
        eprintln!("wsp: cannot tell which binary is running, so cannot put it in a sandbox");
        return 2;
    };
    let bin = std::fs::canonicalize(&bin).unwrap_or(bin);

    let name = name_for(args);
    let mut sb = Sandbox::new(store, &name, bin);

    // A sandbox is scratch by definition, so the only thing anybody means by
    // asking for one is a clean one. Whatever is there under this name — a
    // session left running, a directory left behind by a torn-down session —
    // goes first, and says so rather than being quietly reused.
    let (had_session, reaped) = stop_session(&name);
    let replaced = had_session || reaped > 0 || sb.dir.exists();
    let _ = std::fs::remove_dir_all(&sb.dir);

    let seed = args.has("seed").then_some(store);
    if let Err(e) = lay_out(&sb, seed) {
        eprintln!("wsp: {e}");
        return 1;
    }

    // A fake instead of a herdr: everything above this line is the same
    // instance — the store, the state, the `wsp` shim — and the only thing that
    // changes is what is answering the socket. That is the whole claim of
    // t-260816-080, and it is why this is a flag rather than a command.
    let fake = args.has("fake");
    let stage = stage_file(&sb.dir);
    if fake {
        if let Some(given) = args.get("stage") {
            let from = util::expand(&given);
            if let Err(e) = std::fs::copy(&from, &stage) {
                eprintln!("wsp: cannot read the stage at {}: {e}", util::contract(&from));
                return 1;
            }
        } else if let Err(e) = std::fs::write(&stage, "{\n  \"seats\": []\n}\n") {
            eprintln!("wsp: cannot write {}: {e}", util::contract(&stage));
            return 1;
        }
    }

    let started = Instant::now();
    let brought_up = match fake {
        true => start_fake(&sb, &stage),
        false => start_session(&sb),
    };
    match brought_up {
        Ok(sock) => sb.socket = sock,
        Err(e) => {
            eprintln!("wsp: {e}");
            // A session that did not answer in twenty seconds may still be
            // coming up, and a half-started one nobody knows about is the leak
            // this command exists not to leave — including whatever its plugins
            // got as far as starting.
            stop_session(&name);
            let _ = std::fs::remove_dir_all(&sb.dir);
            return 1;
        }
    }
    let up_secs = started.elapsed().as_secs_f64();

    // The command form: run one thing in it and take it down again.
    if let Some(cmd) = args.get("run") {
        let code = run_in(&sb, &cmd, args, up_secs);
        if !args.has("keep") {
            let (_, reaped) = stop_session(&sb.name);
            let _ = std::fs::remove_dir_all(&sb.dir);
            if !json_out {
                println!("{}", p.dim(&format!("sandbox {} torn down{}", sb.name, reaped_note(reaped))));
            }
        } else if !json_out {
            println!("{}", p.dim(&format!("sandbox {} kept — wsp sandbox rm {}", sb.name, sb.name)));
        }
        return code;
    }

    if json_out {
        println!(
            "{}",
            json!({
                "name": sb.name,
                "replaced": replaced,
                "socket": sb.socket.display().to_string(),
                "home": sb.home.display().to_string(),
                "state": sb.state.display().to_string(),
                "bin": sb.bin.display().to_string(),
                "seeded": seed.is_some(),
                "fake": fake,
                "stage": fake.then(|| stage.display().to_string()).unwrap_or_default(),
                "env": sb.env().into_iter().collect::<std::collections::BTreeMap<_, _>>(),
                "unset": forget_keys(),
                "seconds": (up_secs * 10.0).round() / 10.0,
            })
        );
        return 0;
    }

    println!("{} {}", p.dim("sandbox"), p.bold(&sb.name));
    println!(
        "{} {} {}",
        p.dim(if fake { "fake   " } else { "herdr  " }),
        util::contract(&sb.socket),
        p.dim(&format!("(up in {up_secs:.1}s, empty)"))
    );
    if fake {
        println!("{} {} {}", p.dim("stage  "), util::contract(&stage), p.dim("(edit it; the fake pushes what changed)"));
    }
    println!(
        "{} {} {}",
        p.dim("store  "),
        util::contract(&sb.home),
        p.dim(if seed.is_some() { "(seeded from the live store)" } else { "(empty)" })
    );
    println!("{} {}", p.dim("binary "), util::contract(&sb.bin));
    if replaced {
        println!("{}", p.dim("(replaced the one that was there)"));
    }
    installed_warning(&p, &sb.bin);

    println!();
    println!("export HERDR_SOCKET_PATH={}", sb.socket.display());
    println!("export WSP_HOME={}", sb.home.display());
    println!("export WSP_STATE={}", sb.state.display());
    println!("export WSP_BIN={}", sb.bin.display());
    println!("export PATH={}:$PATH", sb.shim.display());
    let forget = forget_keys();
    if !forget.is_empty() {
        println!("unset {}", forget.join(" "));
    }
    println!();
    println!("{} wsp <anything> — `wsp` here is the binary above", p.dim("then   "));
    // A fake has nothing to attach to, and saying so is better than printing a
    // herdr command that would open the caller's own session.
    match fake {
        true => println!("{} nothing to attach to — it is a state, not a terminal", p.dim("attach ")),
        false => println!("{} herdr --session {}", p.dim("attach "), sb.name),
    }
    println!("{} wsp sandbox rm {}", p.dim("finish "), sb.name);
    0
}

/// The mistake this command is built to make impossible, said out loud in the
/// one case construction cannot fix: if you invoked the *installed* binary,
/// that is the binary the sandbox is testing, and installing is the one act
/// every other agent's panel is downstream of.
fn installed_warning(p: &util::Paint, bin: &Path) {
    let installed = util::home().join(".local/bin/wsp");
    if std::fs::canonicalize(&installed).as_deref().unwrap_or(&installed) == bin {
        println!(
            "{} {}",
            p.yellow("note"),
            p.dim("this is the installed binary — run target/debug/wsp sandbox to test a build instead"),
        );
    }
}

/// The command `--run` runs: `sh -c` in the sandbox's environment, with what a
/// sandbox has to forget taken back out of it.
fn child(sb: &Sandbox, cmd: &str) -> Command {
    let mut c = Command::new("sh");
    c.arg("-c").arg(cmd);
    // Forgotten first, set second: `HERDR_SOCKET_PATH` is both a variable the
    // caller may hold and the one thing the sandbox most needs to say.
    forget(&mut c);
    for (k, v) in sb.env() {
        c.env(k, v);
    }
    c
}

/// `--run`: one command, in the sandbox's environment, in the caller's
/// directory.
///
/// Through `sh -c` because what people want to run is a line of shell — `wsp
/// add "x" && wsp ls` — and in the caller's cwd because a relative path in that
/// line means what it says where it was typed.
fn run_in(sb: &Sandbox, cmd: &str, args: &Args, up_secs: f64) -> i32 {
    let p = util::Paint::new();
    let json_out = args.json();
    if !json_out {
        println!("{} {} {}", p.dim("sandbox"), p.bold(&sb.name), p.dim(&format!("up in {up_secs:.1}s")));
        installed_warning(&p, &sb.bin);
        println!("{} {cmd}", p.dim("→"));
    }

    let mut c = child(sb, cmd);
    let started = Instant::now();
    let (code, output) = if json_out {
        match c.output() {
            Ok(out) => {
                let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
                text.push_str(&String::from_utf8_lossy(&out.stderr));
                (out.status.code().unwrap_or(1), text)
            }
            Err(e) => (127, format!("sh: {e}")),
        }
    } else {
        match c.status() {
            Ok(st) => (st.code().unwrap_or(1), String::new()),
            Err(e) => {
                eprintln!("wsp: sh: {e}");
                (127, String::new())
            }
        }
    };
    let secs = started.elapsed().as_secs_f64();

    if json_out {
        let tail: Vec<&str> = output.lines().rev().take(200).collect::<Vec<_>>().into_iter().rev().collect();
        println!(
            "{}",
            json!({
                "ok": code == 0,
                "code": code,
                "name": sb.name,
                "socket": sb.socket.display().to_string(),
                "home": sb.home.display().to_string(),
                "kept": args.has("keep"),
                "seconds": (secs * 10.0).round() / 10.0,
                "output": tail.join("\n"),
            })
        );
        return code;
    }

    if code == 0 {
        println!("{} {} in {:.0}s", p.green("✓"), p.bold("ran in the sandbox"), secs);
    } else {
        println!("{} exit {code} in {:.0}s", p.red("✗"), secs);
    }
    code
}

fn ls(store: &Store, args: &Args) -> i32 {
    let p = util::Paint::new();
    let root = store.state.join("sandbox");

    // All three halves, because any of them can outlive the others: a directory
    // whose session was stopped, a session whose directory was removed, and a
    // process whose session and directory are both gone. That last one is why
    // this list exists to be read at all — it said "no sandboxes" with a stray
    // daemon on the live store.
    let mut names: Vec<String> = std::fs::read_dir(&root)
        .map(|d| {
            d.flatten()
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default();
    let live = sessions();
    let running = sessions_with_processes();
    for name in live
        .iter()
        .map(|(n, _, _)| n)
        .chain(running.iter().map(|(n, _)| n))
        .filter(|n| n.starts_with(PREFIX))
    {
        if !names.contains(name) {
            names.push(name.clone());
        }
    }
    names.sort();

    let rows: Vec<Value> = names
        .iter()
        .map(|n| {
            let s = live.iter().find(|(m, _, _)| m == n);
            // A fake has no herdr session to be found in, so its socket is the
            // only thing that says it exists — and without this the row would
            // read "N process(es), no session" in red, which is what a *stray*
            // looks like. The one state this list exists to make loud must not
            // be the one a working fake sandbox reports.
            let fake = fake_socket(&root.join(n)).exists();
            json!({
                "name": n,
                "running": s.map(|(_, r, _)| *r).unwrap_or(false),
                "fake": fake,
                "socket": match fake {
                    true => util::contract(&fake_socket(&root.join(n))),
                    false => s.map(|(_, _, sock)| sock.display().to_string()).unwrap_or_default(),
                },
                "dir": util::contract(&root.join(n)),
                "orphan": !root.join(n).is_dir(),
                "processes": running.iter().find(|(m, _)| m == n).map(|(_, c)| *c).unwrap_or(0),
            })
        })
        .collect();

    if args.json() {
        println!("{}", json!({ "sandboxes": rows }));
        return 0;
    }
    if rows.is_empty() {
        println!("no sandboxes");
        return 0;
    }
    for r in &rows {
        let name = r["name"].as_str().unwrap_or("");
        let fake = r["fake"].as_bool() == Some(true);
        let state = if r["orphan"].as_bool() == Some(true) {
            p.yellow("no directory")
        } else if r["running"].as_bool() == Some(true) {
            p.green("up")
        } else if fake {
            p.green("up (fake)")
        } else {
            p.dim("herdr stopped")
        };
        // The process count is loud when the session is gone, because that is
        // the state nothing else on this line would tell you about — and a fake
        // is exactly that shape and is not that thing.
        let procs = match (r["processes"].as_u64().unwrap_or(0), r["running"].as_bool(), fake) {
            (0, _, _) => String::new(),
            (n, Some(true), _) | (n, _, true) => p.dim(&format!("{n} process(es)")),
            (n, _, _) => p.red(&format!("{n} process(es), no session")),
        };
        println!("{:<24} {:<16} {:<22} {}", name, state, procs, p.dim(r["dir"].as_str().unwrap_or("")));
    }
    println!("{}", p.dim("wsp sandbox rm <name> — or --all"));
    0
}

fn rm(store: &Store, args: &Args) -> i32 {
    let p = util::Paint::new();
    let root = store.state.join("sandbox");

    // Through the same naming as `up`, so `rm selftest` drops what `--name
    // selftest` made — and so a name typed at a shell can never be a path that
    // `remove_dir_all` walks out of the sandbox directory with.
    let mut names: Vec<String> =
        args.rest.iter().skip(1).map(|n| session_name(Some(n), "")).collect();
    if args.has("all") {
        names = std::fs::read_dir(&root)
            .map(|d| d.flatten().filter_map(|e| e.file_name().into_string().ok()).collect())
            .unwrap_or_default();
        // Sessions and processes as well as directories — `--all` has to reach
        // a stray whose session and directory are both already gone, since that
        // is the only state in which it is still doing damage.
        for n in sessions()
            .into_iter()
            .map(|(n, _, _)| n)
            .chain(sessions_with_processes().into_iter().map(|(n, _)| n))
            .filter(|n| n.starts_with(PREFIX))
        {
            if !names.contains(&n) {
                names.push(n);
            }
        }
    } else if names.is_empty() {
        names.push(name_for(args));
    }
    names.sort();
    names.dedup();

    let mut removed: Vec<Value> = Vec::new();
    for name in &names {
        let dir = root.join(name);
        let (had_session, reaped) = stop_session(name);
        let had_dir = dir.exists();
        let _ = std::fs::remove_dir_all(&dir);
        if had_session || had_dir || reaped > 0 {
            removed.push(json!({ "name": name, "processes": reaped }));
        }
    }

    if args.json() {
        println!("{}", json!({ "removed": removed, "asked": names }));
        return 0;
    }
    if removed.is_empty() {
        println!("{}", p.dim(&format!("no sandbox called {}", names.join(", "))));
    } else {
        for r in &removed {
            let n = r["processes"].as_u64().unwrap_or(0) as usize;
            println!("removed {}{}", r["name"].as_str().unwrap_or(""), reaped_note(n));
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wsp-sandbox-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sandbox_at(dir: &Path, name: &str) -> Sandbox {
        let store = Store::at(dir.join("wsp"), dir.join("state"));
        Sandbox::new(&store, name, dir.join("target/debug/wsp"))
    }

    /// The whole instance has to be inside the sandbox, or it is not one. The
    /// failure this guards is the quiet one: a `WSP_STATE` that still points at
    /// `~/.local/state/wsp` gives you an isolated *store* and a `reconcile
    /// --reap` that ends everybody's real claims.
    #[test]
    fn nothing_in_it_points_at_the_live_instance() {
        let dir = scratch("env");
        let mut sb = sandbox_at(&dir, "wsp-w1");
        sb.socket = sb.dir.join("herdr.sock");
        let env: std::collections::HashMap<String, String> = sb.env().into_iter().collect();

        for key in ["HERDR_SOCKET_PATH", "WSP_HOME", "WSP_STATE", "WSP_BIN"] {
            assert!(env.contains_key(key), "{key} is not in the sandbox environment");
        }
        for key in ["HERDR_SOCKET_PATH", "WSP_HOME", "WSP_STATE"] {
            let v = &env[key];
            assert!(
                v.starts_with(&sb.dir.display().to_string()),
                "{key}={v} escapes the sandbox at {}",
                sb.dir.display()
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `PATH` is prefixed, never replaced. A sandbox is still a shell: a `--run`
    /// that could not find `git` or `cargo` would look like isolation and be a
    /// broken environment.
    #[test]
    fn the_shim_goes_first_and_the_rest_of_path_survives() {
        let shim = Path::new("/s/bin");
        assert_eq!(path_with(shim, Some("/usr/bin:/bin")), "/s/bin:/usr/bin:/bin");
        assert_eq!(path_with(shim, None), "/s/bin");
        assert_eq!(path_with(shim, Some("")), "/s/bin");
    }

    /// `wsp` inside a sandbox must be the binary that made it — not whatever
    /// `~/.local/bin/wsp` happens to be this minute, which is the one file
    /// every other agent's panel re-execs into.
    #[test]
    fn wsp_in_the_sandbox_is_the_binary_under_test() {
        let dir = scratch("shim");
        let bin = dir.join("target/debug/wsp");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();

        let store = Store::at(dir.join("wsp"), dir.join("state"));
        let sb = Sandbox::new(&store, "wsp-w1", bin.clone());
        lay_out(&sb, None).unwrap();

        let link = sb.shim.join("wsp");
        assert_eq!(std::fs::read_link(&link).unwrap(), bin);
        // A symlink rather than a copy, so a rebuild is picked up without
        // anybody remaking the sandbox.
        assert!(link.exists(), "the shim does not resolve to a binary");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A pane id and a workspace id name things in the *caller's* herdr, and
    /// the sandbox's has never heard of either. Inherited, `wsp claim` in a
    /// sandbox binds a task to a pane that does not exist there. `--run` has to
    /// take them back out, and the check is what the child is actually handed
    /// rather than what we meant to hand it.
    #[test]
    fn a_command_in_a_sandbox_does_not_inherit_the_callers_pane() {
        let dir = scratch("forget");
        let mut sb = sandbox_at(&dir, "wsp-w1");
        sb.socket = sb.dir.join("herdr.sock");

        // The rule is the prefix, so a variable herdr adds next month is
        // forgotten too — and the two locators survive it.
        for key in ["HERDR_PANE_ID", "HERDR_WORKSPACE_ID", "HERDR_TAB_ID", "HERDR_SOMETHING_NEW", "GIT_INDEX_FILE"] {
            assert!(forgettable(key), "{key} would have been inherited");
        }
        for key in KEEP {
            assert!(!forgettable(key), "{key} is how the sandbox is reached and cannot be forgotten");
        }

        let c = child(&sb, "wsp wip");
        let envs: Vec<(String, Option<String>)> = c
            .get_envs()
            .map(|(k, v)| {
                (k.to_string_lossy().into_owned(), v.map(|v| v.to_string_lossy().into_owned()))
            })
            .collect();
        for (k, v) in &envs {
            assert!(
                !(forgettable(k) && v.is_some()),
                "{k} was handed to the child as {v:?} rather than taken out"
            );
        }
        // …and the socket, which is forgettable-looking and set by us, is the
        // sandbox's. Order matters here and this is what checks it.
        let socket = envs.iter().find(|(k, _)| k == "HERDR_SOCKET_PATH").and_then(|(_, v)| v.clone());
        assert_eq!(socket, Some(sb.socket.display().to_string()), "the child was pointed at the wrong herdr");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The defect in t-260816-076, at the line it turned on.
    ///
    /// The herdr *server* is what starts plugins, and it starts them with its
    /// own environment. Started without the store, its `wsp daemon` came up on
    /// the sandbox's socket and the *live* store — the one pair of facts that
    /// makes `sync` reap every binding in the real store. So the store has to be
    /// in the server's environment, before the server exists, and the caller's
    /// socket must not be: it is the live one, and we have no sandbox socket to
    /// give yet.
    #[test]
    fn the_herdr_a_sandbox_starts_is_told_which_store_it_is_for() {
        let dir = scratch("server-env");
        let sb = sandbox_at(&dir, "wsp-w1");
        let env: std::collections::HashMap<String, String> = sb.store_env().into_iter().collect();

        for key in ["WSP_HOME", "WSP_STATE"] {
            let v = env.get(key).unwrap_or_else(|| panic!("{key} is not in the server's environment"));
            assert!(
                v.starts_with(&sb.dir.display().to_string()),
                "{key}={v} points outside the sandbox — this is the live-store daemon"
            );
        }
        assert!(
            !env.contains_key("HERDR_SOCKET_PATH"),
            "the server was handed a socket, and the only one we have here is the live one"
        );
        // …and a command run *in* the sandbox does get one, or it would talk to
        // whatever herdr the caller was standing in.
        let mut sb = sb;
        sb.socket = sb.dir.join("herdr.sock");
        let full: std::collections::HashMap<String, String> = sb.env().into_iter().collect();
        assert_eq!(full.get("HERDR_SOCKET_PATH"), Some(&sb.socket.display().to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The server is what starts every process in here, so what it inherits is
    /// what every pane in the sandbox inherits.
    ///
    /// Recorded 2026-08-17 off a real one: an agent's `wsp sandbox` handed its
    /// server the whole of its own Claude Code session, and the agent started in
    /// there believed it was that session's child and saved no transcript. This
    /// is the same list `cmd_spawn::order` sheds, and it is here as well because
    /// a pane opened by hand in a sandbox never passes through an `Order`.
    ///
    /// Checked against a `Command`, which is what is actually handed over,
    /// rather than against the map we meant to build — the same reason
    /// `fake_command` exists as a function at all.
    #[test]
    fn the_herdr_a_sandbox_starts_is_not_handed_the_callers_session() {
        let _env = util::env_lock();
        std::env::set_var("CLAUDE_CODE_MESSAGING_TOKEN", "the-callers-credential");
        let dir = scratch("server-session");
        let sb = sandbox_at(&dir, "wsp-w1");

        let c = fake_command(&sb, &stage_file(&sb.dir));
        let envs: Vec<(String, Option<String>)> = c
            .get_envs()
            .map(|(k, v)| {
                (k.to_string_lossy().into_owned(), v.map(|v| v.to_string_lossy().into_owned()))
            })
            .collect();
        for (k, v) in &envs {
            assert!(
                !(crate::place::shed(k) && v.is_some()),
                "{k} was handed to the server as {v:?} — every pane under it gets this"
            );
        }
        let named: Vec<&str> = envs.iter().map(|(k, _)| k.as_str()).collect();
        assert!(
            named.contains(&crate::place::CHILD_MARKER),
            "the marker is not taken off, and it is the one that costs the transcript"
        );
        std::env::remove_var("CLAUDE_CODE_MESSAGING_TOKEN");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `herdr session stop` ends the server, not what it started: the daemon it
    /// left was reparented to launchd and went on holding the live store. The
    /// environment is the only handle — the command line is `wsp daemon`, the
    /// same as the real one's — so the match has to be exact at the end of the
    /// name, and it must never include a process we are standing on.
    /// `ps` right-aligns its numeric columns, and the first version of this
    /// split on every run of whitespace — so the second field came back empty,
    /// the parse failed, and every line was dropped. A teardown that reaps
    /// nothing looks exactly like a teardown with nothing to reap: it printed
    /// "torn down" over a live daemon, which is the defect verbatim. Pinned
    /// against real output rather than a tidy invention.
    #[test]
    fn a_padded_ps_line_is_still_a_process() {
        assert_eq!(
            ps_line("  1628     1 /Users/e/wsp daemon HERDR_SESSION=wsp-w24"),
            Some((1628, 1, "/Users/e/wsp daemon HERDR_SESSION=wsp-w24".to_string()))
        );
        assert_eq!(
            ps_line("22121 38749 /Users/e/.local/bin/wsp daemon -v"),
            Some((22121, 38749, "/Users/e/.local/bin/wsp daemon -v".to_string()))
        );
        assert_eq!(ps_line("  PID  PPID COMMAND"), None, "a header parsed as a process");
        assert_eq!(ps_line(""), None);
    }

    #[test]
    fn teardown_reaps_by_session_and_never_reaps_itself() {
        assert!(holds_session("wsp daemon HERDR_SESSION=wsp-w1", "wsp-w1"));
        assert!(holds_session("HERDR_SESSION=wsp-w1 WSP_HOME=/x", "wsp-w1"));
        assert!(!holds_session("HERDR_SESSION=wsp-w12 WSP_HOME=/x", "wsp-w1"), "reaped a neighbour");
        assert!(!holds_session("HERDR_SESSION=wsp-w1x", "wsp-w1"), "reaped a neighbour");
        assert!(!holds_session("wsp daemon", "wsp-w1"));

        // A shell with the session name on its own command line is the caller,
        // and killing the process that asked for the teardown would be its own
        // kind of leak. Ancestors are excluded by pid, not by guesswork.
        let me = std::process::id();
        let all = vec![
            (1u32, 0u32, "launchd".to_string()),
            (me, 900, format!("wsp sandbox rm HERDR_SESSION=wsp-w1")),
            (900, 1, "zsh -c HERDR_SESSION=wsp-w1 wsp sandbox rm".to_string()),
            (901, 1, "wsp daemon HERDR_SESSION=wsp-w1".to_string()),
        ];
        assert_eq!(
            children_of(&all, "wsp-w1"),
            vec![901],
            "the teardown was about to kill itself or the shell that asked for it"
        );
    }

    /// A fake is a *process this sandbox started*, and everything that has ever
    /// leaked out of a sandbox leaked because a spawned process was handed the
    /// wrong environment. So it is checked the same way the `--run` child is:
    /// against the command itself.
    ///
    /// Two things have to be true and one of them looks wrong at first glance.
    /// The store and socket must point inside the sandbox — a fake serving the
    /// sandbox's socket with the live store is the pair from t-260816-076. And
    /// `HERDR_SESSION` must be *put back* after the caller's `HERDR_` variables
    /// are stripped, because it is the only handle `stop_session` has: without
    /// it the fake outlives the teardown and `sandbox ls` cannot see it either.
    #[test]
    fn the_fake_a_sandbox_starts_is_reapable_and_points_at_nothing_live() {
        let dir = scratch("fake-env");
        let sb = sandbox_at(&dir, "wsp-w1");
        let c = fake_command(&sb, &stage_file(&sb.dir));
        let env: std::collections::HashMap<String, Option<String>> = c
            .get_envs()
            .map(|(k, v)| {
                (k.to_string_lossy().into_owned(), v.map(|v| v.to_string_lossy().into_owned()))
            })
            .collect();

        assert_eq!(
            env.get("HERDR_SESSION").cloned().flatten().as_deref(),
            Some("wsp-w1"),
            "the fake cannot be reaped: nothing on it says which sandbox it belongs to"
        );
        for key in ["HERDR_SOCKET_PATH", "WSP_HOME", "WSP_STATE"] {
            let v = env.get(key).cloned().flatten().unwrap_or_default();
            assert!(
                v.starts_with(&sb.dir.display().to_string()),
                "{key}={v} escapes the sandbox — a fake on the live store is the 076 pair"
            );
        }
        assert_eq!(
            env.get("HERDR_PANE_ID").cloned().flatten(),
            None,
            "the fake inherited the caller's pane"
        );
        // …and it serves inside its own directory, so `sandbox ls` can tell a
        // fake from a stray by looking rather than by remembering.
        assert!(fake_socket(&sb.dir).starts_with(&sb.dir));
        assert!(stage_file(&sb.dir).starts_with(&sb.dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A sandbox with no store is a `wsp` that exits 2 on every command, so
    /// laying one out has to leave one behind.
    #[test]
    fn the_store_exists_before_anything_is_run_against_it() {
        let dir = scratch("store");
        let sb = sandbox_at(&dir, "wsp-w1");
        lay_out(&sb, None).unwrap();
        assert!(Store::at(sb.home.clone(), sb.state.clone()).exists(), "the sandbox has no store");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `--seed` copies the store's content and never its history. Sharing the
    /// history would put a sandbox one `git commit` away from writing into the
    /// real store — the exact failure this command exists to make impossible.
    #[test]
    fn seeding_takes_the_content_and_leaves_the_history() {
        let dir = scratch("seed");
        let live = Store::at(dir.join("live"), dir.join("live-state"));
        std::fs::create_dir_all(live.tasks_dir()).unwrap();
        std::fs::create_dir_all(live.root.join(".git/objects")).unwrap();
        std::fs::write(live.tasks_dir().join("t-1.md"), "# a task\n").unwrap();
        std::fs::write(live.root.join(".git/objects/theirs"), "a commit of somebody's\n").unwrap();

        let sb = sandbox_at(&dir, "wsp-w1");
        lay_out(&sb, Some(&live)).unwrap();

        assert!(sb.home.join("tasks/t-1.md").is_file(), "the seeded store has no tasks");
        // The sandbox has a `.git` of its own — `init` makes one, and a store
        // that did not commit would be a sandbox that answers commit questions
        // wrongly. What it must not have is a byte of the live one's.
        assert!(
            !sb.home.join(".git/objects/theirs").exists(),
            "the live store's history was copied in"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One sandbox per agent, keyed exactly as `wsp verify` keys its build tree
    /// — one to leak rather than one per run — and two agents never collide.
    #[test]
    fn a_sandbox_belongs_to_an_agent_and_is_always_named_as_one() {
        assert_eq!(session_name(None, "w24"), "wsp-w24");
        assert_ne!(session_name(None, "w25"), session_name(None, "w24"), "two agents shared one sandbox");
        assert_eq!(session_name(None, ""), "wsp-solo");

        // A name given by hand is still recognisably ours, however it is typed,
        // because `ls` and `rm --all` find sandboxes by that prefix — and it is
        // still a herdr session name, which is a directory name.
        for given in ["probe", "wsp-probe", "Probe!", "  ../etc  "] {
            let name = session_name(Some(given), "w24");
            assert!(name.starts_with(PREFIX), "--name {given} produced a session nobody can find again");
            assert!(!name.contains('/'), "--name {given} escaped its own directory as {name}");
        }
    }

    /// `rm` takes a name typed at a shell and hands it to `remove_dir_all`.
    /// Everything it removes has to be inside the sandbox directory, whatever
    /// was typed — the same normalisation that made the name is what guarantees
    /// it, which is why `rm` calls it rather than trusting the argument.
    #[test]
    fn rm_cannot_be_pointed_out_of_the_sandbox_directory() {
        for given in ["../../wsp", "/etc", "..", "wsp-selftest"] {
            let name = session_name(Some(given), "");
            let dir = Path::new("/state/sandbox").join(&name);
            assert!(
                dir.starts_with("/state/sandbox") && dir.components().count() == 4,
                "`wsp sandbox rm {given}` would have removed {}",
                dir.display()
            );
        }
    }
}
