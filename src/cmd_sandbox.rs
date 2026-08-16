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
//! # What a sandbox is not
//!
//! Its herdr starts empty, and manufacturing twenty-two workspaces is not a
//! thing this can do. `--seed` copies the store — projects and tasks, so `ls`,
//! `tree`, `show` and the panel have something to draw — and deliberately not
//! the machine state, because a claim names a workspace id that exists in
//! nobody's herdr but the live one. When what you need is this machine's actual
//! workspaces and agents, no sandbox reproduces it; that residue is
//! t-260816-057.

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

    /// The environment a command runs in here.
    fn env(&self) -> Vec<(String, String)> {
        vec![
            ("HERDR_SOCKET_PATH".into(), self.socket.display().to_string()),
            ("WSP_HOME".into(), self.home.display().to_string()),
            ("WSP_STATE".into(), self.state.display().to_string()),
            ("WSP_BIN".into(), self.bin.display().to_string()),
            ("PATH".into(), path_with(&self.shim, std::env::var("PATH").ok().as_deref())),
        ]
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
fn forgettable(key: &str) -> bool {
    key == "GIT_INDEX_FILE" || (key.starts_with("HERDR_") && !KEEP.contains(&key))
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
fn start_session(name: &str) -> Result<PathBuf, String> {
    Command::new(herdr_bin())
        .args(["--session", name, "server"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot start herdr: {e}"))?;

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

/// Stop it and delete it, in that order, because `delete` refuses a session
/// that is running. Both are best-effort: what this is usually clearing up is a
/// half-torn-down sandbox, where one of the two has already happened.
fn stop_session(name: &str) -> bool {
    let existed = session(name).is_some();
    let _ = herdr(&["session", "stop", name]);
    let _ = herdr(&["session", "delete", name]);
    existed
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
        Some(other) => {
            eprintln!("wsp sandbox: unknown subcommand `{other}` — try `ls` or `rm`");
            2
        }
        None => up(store, args),
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
    let replaced = stop_session(&name) || sb.dir.exists();
    let _ = std::fs::remove_dir_all(&sb.dir);

    let seed = args.has("seed").then_some(store);
    if let Err(e) = lay_out(&sb, seed) {
        eprintln!("wsp: {e}");
        return 1;
    }

    let started = Instant::now();
    match start_session(&name) {
        Ok(sock) => sb.socket = sock,
        Err(e) => {
            eprintln!("wsp: {e}");
            // A session that did not answer in twenty seconds may still be
            // coming up, and a half-started one nobody knows about is the leak
            // this command exists not to leave.
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
            stop_session(&sb.name);
            let _ = std::fs::remove_dir_all(&sb.dir);
            if !json_out {
                println!("{}", p.dim(&format!("sandbox {} torn down", sb.name)));
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
                "env": sb.env().into_iter().collect::<std::collections::BTreeMap<_, _>>(),
                "unset": forget_keys(),
                "seconds": (up_secs * 10.0).round() / 10.0,
            })
        );
        return 0;
    }

    println!("{} {}", p.dim("sandbox"), p.bold(&sb.name));
    println!("{} {} {}", p.dim("herdr  "), util::contract(&sb.socket), p.dim(&format!("(up in {up_secs:.1}s, empty)")));
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
    println!("{} herdr --session {}", p.dim("attach "), sb.name);
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
    for k in forget_keys() {
        c.env_remove(k);
    }
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

    // Both halves, because either can outlive the other: a directory whose
    // session was stopped, and a session whose directory was removed. Both are
    // leaks, and a list that showed only one kind would hide the other.
    let mut names: Vec<String> = std::fs::read_dir(&root)
        .map(|d| {
            d.flatten()
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default();
    let live = sessions();
    for (name, _, _) in &live {
        if name.starts_with(PREFIX) && !names.contains(name) {
            names.push(name.clone());
        }
    }
    names.sort();

    let rows: Vec<Value> = names
        .iter()
        .map(|n| {
            let s = live.iter().find(|(m, _, _)| m == n);
            json!({
                "name": n,
                "running": s.map(|(_, r, _)| *r).unwrap_or(false),
                "socket": s.map(|(_, _, sock)| sock.display().to_string()).unwrap_or_default(),
                "dir": util::contract(&root.join(n)),
                "orphan": !root.join(n).is_dir(),
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
        let state = if r["orphan"].as_bool() == Some(true) {
            p.yellow("no directory")
        } else if r["running"].as_bool() == Some(true) {
            p.green("up")
        } else {
            p.dim("herdr stopped")
        };
        println!("{:<24} {:<16} {}", name, state, p.dim(r["dir"].as_str().unwrap_or("")));
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
        for (n, _, _) in sessions() {
            if n.starts_with(PREFIX) && !names.contains(&n) {
                names.push(n);
            }
        }
    } else if names.is_empty() {
        names.push(name_for(args));
    }
    names.sort();
    names.dedup();

    let mut removed: Vec<String> = Vec::new();
    for name in &names {
        let dir = root.join(name);
        let had_session = stop_session(name);
        let had_dir = dir.exists();
        let _ = std::fs::remove_dir_all(&dir);
        if had_session || had_dir {
            removed.push(name.clone());
        }
    }

    if args.json() {
        println!("{}", json!({ "removed": removed, "asked": names }));
        return 0;
    }
    if removed.is_empty() {
        println!("{}", p.dim(&format!("no sandbox called {}", names.join(", "))));
    } else {
        for name in &removed {
            println!("removed {name}");
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
