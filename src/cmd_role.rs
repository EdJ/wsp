//! Verbs whose reach depends on who is asking (`wsp-473`).
//!
//! The primary seat (the fleet watch) was refused by Claude Code's permission
//! classifier every time it tried to finish a step it had already checked, and
//! each refusal became a command for Ed to paste. The fix is not a wider allow
//! list on raw `kill` and `launchctl`: the risky steps are wsp verbs that check
//! the caller's role and scope themselves, so the allow rule only gets the call
//! to wsp and wsp decides.
//!
//! | verb | primary | governor | member, verifier, check |
//! |---|---|---|---|
//! | `wsp despawn <row> --why …` | any seat | seats of rows on a list it governs | refused |
//! | `wsp reap <pid>…` | any pid that passes the guards | pids in trees of rows on a list it governs, and its barrier tree | refused |
//! | `wsp done <row>` | rows at review | refused | refused |
//! | `wsp daemon reload` | yes | refused | refused |
//!
//! **The role is read from the seat wsp recorded**, never from anything the
//! caller says: the caller's seat (`WSP_SEAT_ID`, or herdr's pane) is looked up
//! in the governors record, which names the primary seat and each list's
//! governor. A seat that is bound to a row, or that wsp spawned (its environment
//! carries `WSP_SEAT_ID`), and is neither of those, is a member, verifier or
//! check. A caller in no seat at all is Ed at a terminal, or the daemon, and is
//! as free as it ever was: the refusals here are about seats.
//!
//! Where a despawn is refused for a *wrong scope* and not for a role, the
//! message names the scope it would have needed, and the refusal is logged.
//! The permission layer is the other half (`claude-code/*.settings.json`): a
//! member's Claude Code is not allowed to call these verbs without a prompt,
//! and wsp refuses the call besides.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::model::Status;
use crate::store::Store;
use crate::util;
use crate::base::args::Args;

/// The settings a primary seat is started with, passed with `--settings`.
pub(crate) const PRIMARY_SETTINGS: &str = include_str!("../claude-code/primary.settings.json");
/// The settings a governor seat is started with.
pub(crate) const GOVERNOR_SETTINGS: &str = include_str!("../claude-code/governor.settings.json");

const PRIMARY_FILE: &str = "wsp-primary.settings.json";
const GOVERNOR_FILE: &str = "wsp-governor.settings.json";

/// How old a dev or test tool must be before `wsp reap` will stop it.
pub(crate) const TOOL_AGE: u64 = 60 * 60;
/// How long a test binary must have been idle at 0 % CPU.
pub(crate) const IDLE_FOR: u64 = 15 * 60;
/// The CPU a `while :` loop must be over to be a busy loop.
const LOOP_CPU: f32 = 20.0;

/// Who is asking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Role {
    /// No seat: a person at a terminal, or the daemon.
    Outside,
    /// The primary seat.
    Primary,
    /// The governor of this list (or project).
    Governor(String),
    /// A member, verifier or check.
    Seat,
}

impl Role {
    /// The role of a caller in `pane`, which holds the slot of `governs` (if
    /// any), and which is `bound` (to a row, or started by wsp).
    pub(crate) fn of(pane: Option<&str>, governs: Option<&str>, bound: bool) -> Role {
        match (pane, governs) {
            (None, _) => Role::Outside,
            (Some(_), Some(s)) if crate::governors::is_primary(s) => Role::Primary,
            (Some(_), Some(s)) => Role::Governor(s.to_string()),
            (Some(_), None) if bound => Role::Seat,
            (Some(_), None) => Role::Outside,
        }
    }

    pub(crate) fn name(&self) -> String {
        match self {
            Role::Outside => "a person".into(),
            Role::Primary => "the primary seat".into(),
            Role::Governor(s) => format!("the governor of {s}"),
            Role::Seat => "a member, verifier or check seat".into(),
        }
    }
}

/// The caller's role, from the seat it is standing in.
pub(crate) fn role(store: &Store) -> Role {
    let pane = crate::cmd_agent::my_pane();
    let governs = pane
        .as_deref()
        .and_then(|p| crate::place::governs(&store.governors(), &crate::place::Seat::new(p)));
    let spawned = std::env::var_os(crate::base::seat::SEAT_ENV).is_some_and(|v| !v.is_empty());
    let bound = spawned || pane.as_deref().is_some_and(|p| store.bindings().contains_key(p));
    Role::of(pane.as_deref(), governs.as_deref(), bound)
}

/// The role a `Caller` (already resolved by `despawn`) stands for. A seat that
/// is neither primary nor a governor is left to the permission layer here, as
/// it always was: `despawn` is also what a seat's own deferred ending runs.
pub(crate) fn role_of_caller(pane: Option<&str>, governs: Option<&str>) -> Role {
    Role::of(pane, governs, false)
}

fn refuse(store: &Store, verb: &str, role: &Role, why: &str) -> i32 {
    eprintln!("wsp: {verb}: {why}");
    crate::store::log_line(store, &format!("refused {verb} by {}: {why}", role.name()));
    2
}

fn list_of_row(store: &Store, row: &str) -> Option<String> {
    let t = store.task(row)?;
    crate::worklist::Running::read(store).list_for(&t).map(str::to_string)
}

// ---- despawn ----------------------------------------------------------------

/// Whether `role` may end the seat of `row` (on `list`), as a pure decision.
///
/// `why` is required from the primary; a mid-turn seat is refused to it unless
/// `now`. A governor ends only the seats of rows on its own list.
pub(crate) fn despawn_allowed(
    role: &Role,
    why: Option<&str>,
    list: Option<&str>,
    mid_turn: bool,
    now: bool,
) -> Result<(), String> {
    match role {
        Role::Outside => Ok(()),
        Role::Seat => Err("a member, verifier or check seat does not end other seats".into()),
        Role::Primary => {
            if why.map(str::trim).unwrap_or_default().is_empty() {
                return Err("the primary seat says why: `wsp despawn <row> --why \"…\"`".into());
            }
            if mid_turn && !now {
                return Err("that seat is mid-turn — wait for it to be idle, or `--now` to end it anyway".into());
            }
            Ok(())
        }
        Role::Governor(scope) => match list {
            Some(l) if l == scope => Ok(()),
            Some(l) => Err(format!(
                "that seat is on a row of list {l}; the governor of {scope} ends seats of rows on {scope} only — it needs the governor of {l}, or the primary seat"
            )),
            None => Err(format!(
                "that seat is on no row of a list; the governor of {scope} ends seats of rows on {scope} only — it needs the primary seat"
            )),
        },
    }
}

/// The gate `despawn` runs once it knows which seat it is about. `Err` carries
/// the exit code, after the refusal has been said and logged.
pub(crate) fn gate_despawn(
    store: &Store,
    args: &Args,
    caller: (Option<&str>, Option<&str>),
    seat: &str,
    task: Option<&str>,
) -> Result<(), i32> {
    let role = role_of_caller(caller.0, caller.1);
    if role == Role::Outside {
        return Ok(());
    }
    // A successor ending the seat it is replacing is the rotation finishing, not
    // a reach into somebody's list: the handover record names both seats.
    if crate::cmd_govern::incoming(&store.handovers(), caller.0).is_some_and(|(_, from)| from == seat) {
        return Ok(());
    }
    let list = task.and_then(|t| list_of_row(store, t));
    let mid_turn = role == Role::Primary && seat_mid_turn(seat);
    let why = args.get("why");
    match despawn_allowed(&role, why.as_deref(), list.as_deref(), mid_turn, args.has("now")) {
        Ok(()) => {
            crate::store::log_line(
                store,
                &format!(
                    "despawn {seat}{} by {}{}",
                    task.map(|t| format!(" ({t})")).unwrap_or_default(),
                    role.name(),
                    why.map(|w| format!(" — {w}")).unwrap_or_default()
                ),
            );
            Ok(())
        }
        Err(why) => Err(refuse(store, "despawn", &role, &why)),
    }
}

fn seat_mid_turn(seat: &str) -> bool {
    for backend in crate::place::local_backends() {
        let Ok(census) = backend.census() else { continue };
        for s in census.seats() {
            if s.seat.as_str() == seat {
                return s.state.turn_in_flight();
            }
        }
    }
    false
}

// ---- done -------------------------------------------------------------------

/// Whether `role` may mark a row done. The primary only for a row at `review`
/// whose verifier held, or that owes none.
pub(crate) fn done_allowed(role: &Role, status: Status, owes: Option<bool>) -> Result<(), String> {
    match role {
        Role::Outside => Ok(()),
        Role::Governor(_) => Err("done is Ed's bookkeeping; a governor finishes nothing with it".into()),
        Role::Seat => Err("a member finishes with `wsp review`".into()),
        Role::Primary => {
            if status != Status::Review {
                return Err(format!("the primary seat marks done only rows at review — this one is {}", status.as_str()));
            }
            match owes {
                Some(false) => Err("its verifier has not held it — nothing lands as done before that".into()),
                _ => Ok(()),
            }
        }
    }
}

/// `wsp done`, behind the role gate.
pub fn done(store: &Store, args: &Args) -> i32 {
    let role = role(store);
    if role != Role::Outside {
        let row = args.rest.first().and_then(|n| store.find_task(n));
        let (status, owes) = match &row {
            Some(t) => {
                let all = store.tasks();
                // `Some(false)`: a verification stands and has not held.
                let owes = crate::verification::latest(&all, &t.id).map(|_| crate::verification::verified(&all, &t.id));
                (t.status(), owes)
            }
            None => (Status::Todo, None),
        };
        if let Err(why) = done_allowed(&role, status, owes) {
            return refuse(store, "done", &role, &why);
        }
        crate::store::log_line(store, &format!("done {} by {}", row.map(|t| t.id).unwrap_or_default(), role.name()));
    }
    crate::cmd_task::done(store, args)
}

// ---- daemon reload ------------------------------------------------------------

pub(crate) fn reload_allowed(role: &Role) -> Result<(), String> {
    match role {
        Role::Outside | Role::Primary => Ok(()),
        other => Err(format!("{} does not reload the daemon — that is the primary seat's", other.name())),
    }
}

/// `wsp daemon reload`: put the launchd job back when no daemon answers.
/// `answering` is the daemons found running for this store (`None`: the
/// process list could not be read), asked by the caller that owns that rule.
pub fn daemon_reload(store: &Store, answering: Option<Vec<u32>>) -> i32 {
    let role = role(store);
    if let Err(why) = reload_allowed(&role) {
        return refuse(store, "daemon reload", &role, &why);
    }
    match answering {
        None => return refuse(store, "daemon reload", &role, "the process list could not be read, so whether a daemon answers is not known"),
        Some(pids) if !pids.is_empty() => {
            return refuse(store, "daemon reload", &role, &format!("a daemon already answers (pid {}) — nothing to reload", pids[0]));
        }
        Some(_) => {}
    }
    if crate::launchd::sandboxed() {
        return refuse(store, "daemon reload", &role, "this is a sandbox, not the machine's own daemon");
    }
    match crate::launchd::reload() {
        Ok(said) => {
            crate::launchd::clear_unloaded(&store.state);
            crate::store::log_line(store, &format!("daemon reload by {}: {said}", role.name()));
            println!("{said}");
            0
        }
        Err(why) => {
            crate::store::log_line(store, &format!("daemon reload by {} failed: {why}", role.name()));
            eprintln!("wsp: daemon reload: {why}");
            1
        }
    }
}

// ---- reap -------------------------------------------------------------------

/// One row of `ps`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Proc {
    pub pid: u32,
    pub ppid: u32,
    /// Seconds since it started.
    pub age: u64,
    pub cpu: f32,
    /// CPU seconds it has used in all.
    pub cputime: u64,
    pub command: String,
}

/// `[[dd-]hh:]mm:ss` (`etime`) or `[hh:]mm:ss[.cc]` (`time`), in seconds.
pub(crate) fn clock_secs(s: &str) -> Option<u64> {
    let s = s.trim();
    let (days, rest) = match s.split_once('-') {
        Some((d, r)) => (d.parse::<u64>().ok()?, r),
        None => (0, s),
    };
    let rest = rest.split('.').next()?;
    let mut secs = 0u64;
    for part in rest.split(':') {
        secs = secs * 60 + part.parse::<u64>().ok()?;
    }
    Some(days * 86_400 + secs)
}

/// One `ps -o pid=,ppid=,etime=,pcpu=,time=,command=` line.
pub(crate) fn parse_ps(line: &str) -> Option<Proc> {
    let mut it = line.split_whitespace();
    let pid = it.next()?.parse().ok()?;
    let ppid = it.next()?.parse().ok()?;
    let age = clock_secs(it.next()?)?;
    let cpu = it.next()?.parse().ok()?;
    let cputime = clock_secs(it.next()?)?;
    let command = it.collect::<Vec<_>>().join(" ");
    Some(Proc { pid, ppid, age, cpu, cputime, command })
}

fn processes() -> Vec<Proc> {
    let Ok(out) = std::process::Command::new("ps").args(["-A", "-o", "pid=,ppid=,etime=,pcpu=,time=,command="]).output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout).lines().filter_map(parse_ps).collect()
}

/// Every process's working directory, from `lsof -d cwd -Fpn`.
pub(crate) fn parse_cwds(lsof: &str) -> BTreeMap<u32, String> {
    let mut out = BTreeMap::new();
    let mut pid = None;
    for line in lsof.lines() {
        match line.split_at_checked(1) {
            Some(("p", rest)) => pid = rest.trim().parse::<u32>().ok(),
            Some(("n", rest)) => {
                if let Some(p) = pid {
                    out.entry(p).or_insert_with(|| rest.to_string());
                }
            }
            _ => {}
        }
    }
    out
}

/// The tree a working directory is in: `(its name, its root)`, the directory
/// under a `.worktrees/` the path goes through.
pub(crate) fn tree_named(cwd: &str) -> Option<(String, String)> {
    let mut root = std::path::PathBuf::new();
    let mut parts = std::path::Path::new(cwd).components();
    while let Some(c) = parts.next() {
        root.push(c);
        if c.as_os_str() == ".worktrees" {
            let name = parts.next()?;
            root.push(name);
            return Some((name.as_os_str().to_string_lossy().into_owned(), root.display().to_string()));
        }
    }
    None
}

fn word_names(command: &str, names: &[&str]) -> bool {
    command.split_whitespace().take(6).any(|w| {
        let base = w.rsplit('/').next().unwrap_or(w);
        names.contains(&base)
    })
}

/// A dev server or test runner, by the program it runs.
pub(crate) fn is_dev_tool(command: &str) -> bool {
    word_names(command, &["vite", "vitest", "esbuild", "jest", "webpack", "webpack-dev-server", "playwright", "storybook", "tsx", "next"])
}

/// A cargo test binary: `target/<profile>/deps/<name>-<hash>`.
pub(crate) fn is_test_binary(command: &str) -> bool {
    let prog = command.split_whitespace().next().unwrap_or_default();
    prog.contains("/target/") && prog.contains("/deps/")
}

/// A zsh `while :` loop left running.
pub(crate) fn is_busy_loop(p: &Proc) -> bool {
    let prog = p.command.split_whitespace().next().unwrap_or_default();
    prog.rsplit('/').next() == Some("zsh") && p.command.contains("while :") && p.cpu > LOOP_CPU && p.ppid == 1
}

/// What the guards ask of the store, so the rule is testable without one.
pub(crate) struct Facts<'a> {
    /// A row's status, by its tree's name.
    pub status_of: &'a dyn Fn(&str) -> Option<Status>,
    /// For a barrier check's tree: whether the check is still in use.
    pub check_in_use: &'a dyn Fn(&str) -> Option<bool>,
    /// Whether a live seat or a claim holds the row.
    pub held: &'a dyn Fn(&str) -> bool,
    /// Every live seat's processes and this command's ancestors.
    pub keep: &'a [u32],
    /// Seconds a process has been idle (CPU time unchanged), at most its age.
    pub idle_for: &'a dyn Fn(&Proc) -> u64,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Proof {
    /// The tree it stands in, if the proof is a tree's.
    pub tree: Option<String>,
    pub text: String,
}

/// Whether `p` may be stopped, and the proof. Never by a pattern on its own:
/// a loop is a loop by its parent and its CPU, a tool or test binary by the
/// tree its working directory is in.
pub(crate) fn prove(p: &Proc, cwd: Option<&str>, f: &Facts) -> Result<Proof, String> {
    if f.keep.contains(&p.pid) {
        return Err("it is a live seat's process, or one of this command's own ancestors".into());
    }
    if is_busy_loop(p) {
        return Ok(Proof { tree: None, text: format!("busy loop: zsh `while :` at {:.0}% CPU, parent 1", p.cpu) });
    }
    let Some(cwd) = cwd else {
        return Err("it is no busy loop, and its working directory could not be read".into());
    };
    let Some((name, _)) = tree_named(cwd) else {
        return Err(format!("it is no busy loop, and {cwd} is in no row's tree"));
    };
    if (f.held)(&name) {
        return Err(format!("{name} is held by a live seat or a claim"));
    }
    let done_with = match (f.status_of)(&name) {
        Some(Status::Doing) => return Err(format!("{name} is at doing")),
        Some(s) => format!("{name} is at {}", s.as_str()),
        None => match (f.check_in_use)(&name) {
            Some(false) => format!("{name} is a barrier check's tree, no longer in use"),
            Some(true) => return Err(format!("{name} is a barrier check's tree still in use")),
            None => return Err(format!("{name} is neither a row nor a barrier check's tree")),
        },
    };
    if is_test_binary(&p.command) {
        let idle = (f.idle_for)(p);
        if p.cpu > 0.0 || idle < IDLE_FOR {
            return Err(format!(
                "a test binary is stopped only when idle at 0% CPU for {} min — idle {} min, at {:.1}%",
                IDLE_FOR / 60,
                idle / 60,
                p.cpu
            ));
        }
        return Ok(Proof { tree: Some(name), text: format!("test binary idle {} min at 0% CPU; {done_with}", idle / 60) });
    }
    if is_dev_tool(&p.command) {
        if p.age < TOOL_AGE {
            return Err(format!("it is {} min old — a dev or test tool is stopped after an hour", p.age / 60));
        }
        return Ok(Proof { tree: Some(name), text: format!("dev/test tool {} min old; {done_with}", p.age / 60) });
    }
    Err("it is not a dev or test tool, a test binary or a busy loop".into())
}

/// Whether a governor of `scope` may reap what `proof` found: only pids in the
/// trees of rows on its list, and that list's barrier tree.
pub(crate) fn in_scope(
    role: &Role,
    proof: &Proof,
    list_of: &dyn Fn(&str) -> Option<String>,
    check_list: &dyn Fn(&str) -> Option<String>,
) -> Result<(), String> {
    match role {
        Role::Outside | Role::Primary => Ok(()),
        Role::Seat => Err("a member, verifier or check seat does not reap".into()),
        Role::Governor(scope) => {
            let Some(tree) = &proof.tree else {
                return Err(format!("it is in no row's tree; the governor of {scope} reaps pids in trees of rows on {scope} only — it needs the primary seat"));
            };
            let list = check_list(tree).or_else(|| list_of(tree));
            match list {
                Some(l) if &l == scope => Ok(()),
                Some(l) => Err(format!("{tree} is on list {l}; the governor of {scope} reaps pids in trees of rows on {scope} (and its barrier tree) only — it needs the governor of {l}, or the primary seat")),
                None => Err(format!("{tree} is on no list; the governor of {scope} reaps pids in trees of rows on {scope} only — it needs the primary seat")),
            }
        }
    }
}

const SEEN: &str = "reap-seen.json";

/// What a test binary's CPU time was when it was last looked at, and since
/// when it has not changed. `ps` has no history; this is the history. Written
/// by every `wsp reap`, so `wsp reap --list` begins the fifteen minutes.
fn observe(store: &Store, procs: &[Proc], now: i64) -> BTreeMap<u32, u64> {
    let path = store.state_file(SEEN);
    let old: Value = std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!({}));
    let mut next = serde_json::Map::new();
    let mut idle = BTreeMap::new();
    for p in procs.iter().filter(|p| is_test_binary(&p.command)) {
        let key = p.pid.to_string();
        let since = match old.get(&key) {
            Some(rec) if rec.get("cputime").and_then(Value::as_u64) == Some(p.cputime) => {
                rec.get("since").and_then(Value::as_i64).unwrap_or(now)
            }
            _ => now,
        };
        next.insert(key, json!({ "cputime": p.cputime, "since": since }));
        idle.insert(p.pid, ((now - since).max(0) as u64).min(p.age));
    }
    let _ = crate::store::write_atomic(&path, &Value::Object(next).to_string());
    idle
}

fn signal(sig: &str, pid: u32) {
    let _ = std::process::Command::new("kill").args([sig, &pid.to_string()]).status();
}

/// `wsp reap <pid>…` and `wsp reap --list`.
pub fn reap(store: &Store, args: &Args) -> i32 {
    let role = role(store);
    if role == Role::Seat {
        return refuse(store, "reap", &role, "a member, verifier or check seat does not reap");
    }
    let list = args.has("list");
    let mut asked: Vec<u32> = Vec::new();
    for w in &args.rest {
        match w.parse::<u32>() {
            Ok(p) if p > 1 => asked.push(p),
            _ => {
                eprintln!("wsp: reap takes pids, never patterns — `{w}` is not one");
                return 2;
            }
        }
    }
    if !list && asked.is_empty() {
        eprintln!("usage: wsp reap <pid>… | wsp reap --list");
        return 2;
    }
    let Some(keep) = crate::check_tree::live_seat_pids() else {
        return refuse(store, "reap", &role, "no backend answered, so whose processes are whose is not known — nothing stopped");
    };
    let procs = processes();
    let cwds = std::process::Command::new("lsof")
        .args(["-d", "cwd", "-Fpn"])
        .output()
        .map(|o| parse_cwds(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default();
    let idle = observe(store, &procs, util::epoch_secs());

    let claims = store.claims();
    let status_of = |n: &str| store.task(n).map(|t| t.status());
    let check_in_use = |n: &str| crate::check_tree::named(store, n).map(|(l, at)| crate::check_tree::in_use(store, &l, at));
    let held = |n: &str| claims.contains_key(n) || !store.panes_for_task(n).is_empty();
    let idle_for = |p: &Proc| idle.get(&p.pid).copied().unwrap_or(0);
    let facts = Facts { status_of: &status_of, check_in_use: &check_in_use, held: &held, keep: &keep, idle_for: &idle_for };
    let list_of = |n: &str| list_of_row(store, n);
    let check_list = |n: &str| crate::check_tree::named(store, n).map(|(l, _)| l);

    let judge = |p: &Proc| -> Result<Proof, String> {
        let proof = prove(p, cwds.get(&p.pid).map(String::as_str), &facts)?;
        in_scope(&role, &proof, &list_of, &check_list)?;
        Ok(proof)
    };

    if list {
        let mut n = 0;
        for p in &procs {
            if let Ok(proof) = judge(p) {
                n += 1;
                println!("{}  {}  {}", p.pid, proof.text, util::truncate(&p.command, 80));
            }
        }
        if n == 0 {
            println!("nothing to reap");
        }
        return 0;
    }

    let mut refused = 0;
    let mut stopped = Vec::new();
    for pid in asked {
        let Some(p) = procs.iter().find(|p| p.pid == pid) else {
            eprintln!("wsp: reap: no process {pid}");
            refused += 1;
            continue;
        };
        match judge(p) {
            Ok(proof) => {
                crate::store::log_line(
                    store,
                    &format!("reap {pid} by {}: {} — {}", role.name(), proof.text, util::truncate(&p.command, 100)),
                );
                signal("-TERM", pid);
                stopped.push((pid, proof));
            }
            Err(why) => {
                refused += 1;
                eprintln!("wsp: reap {pid}: {why}");
                crate::store::log_line(store, &format!("refused reap {pid} by {}: {why}", role.name()));
            }
        }
    }
    if !stopped.is_empty() {
        std::thread::sleep(std::time::Duration::from_millis(1500));
        let still = util::alive(&stopped.iter().map(|(p, _)| *p).collect::<Vec<_>>());
        for (pid, proof) in &stopped {
            if still.contains(pid) {
                signal("-KILL", *pid);
                println!("{pid}  killed ({})", proof.text);
            } else {
                println!("{pid}  stopped ({})", proof.text);
            }
        }
    }
    (refused > 0) as i32
}

// ---- the seat's settings ------------------------------------------------------

fn settings_path(file: &str) -> std::path::PathBuf {
    util::home().join(".claude").join(file)
}

/// Write the two settings files beside the seat's other settings. Called by
/// `wsp install`; returns the paths written.
pub(crate) fn write_settings() -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for (file, body) in [(PRIMARY_FILE, PRIMARY_SETTINGS), (GOVERNOR_FILE, GOVERNOR_SETTINGS)] {
        let path = settings_path(file);
        if std::fs::read_to_string(&path).is_ok_and(|s| s == body) {
            continue;
        }
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if crate::store::write_atomic(&path, body).is_ok() {
            out.push(path);
        }
    }
    out
}

/// The argument pair a seat's Claude Code is started with: `--settings <file>`
/// for the primary and for a governor, and nothing for anyone else. Only when
/// the file is there, so a machine that has not run `wsp install` since this
/// landed starts its seats exactly as it did.
pub(crate) fn settings_args(kind: &str, scope: Option<&str>, primary: bool) -> Vec<String> {
    if kind != "claude" {
        return Vec::new();
    }
    let file = match (primary, scope) {
        (true, _) => PRIMARY_FILE,
        (false, Some(_)) => GOVERNOR_FILE,
        (false, None) => return Vec::new(),
    };
    let path = settings_path(file);
    match path.exists() {
        true => vec!["--settings".into(), path.display().to_string()],
        false => Vec::new(),
    }
}

pub const VERBS: &[crate::verbs::Verb] = &[
    crate::verbs::Verb {
        names: &["done", "close"],
        run: done,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Tasks,
                order: 200,
                text: r##"  wsp done <id> [--force]           the person's verb (a member finishes with
                                    `wsp review`); complete; --force over open sub-tasks. Its
                                    worktree and branch go too when the work is
                                    on the trunk and nobody is in it; otherwise
                                    kept and said why. --keep-tree leaves it"##,
            },
        ],
    },
    crate::verbs::Verb {
    names: &["reap"],
    run: reap,
    help: &[crate::verbs::Help {
        section: crate::verbs::Section::Agents,
        order: 310,
        text: r##"  wsp reap <pid>… | --list          stop processes by pid, never by pattern: a dev
                                    or test tool over an hour old in the tree of a
                                    row not at doing, a zsh busy loop, a test
                                    binary idle 15 min in a finished tree. Never
                                    a live seat's. The primary any such pid; a
                                    governor those in its list's trees"##,
    }],
}];

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: u32, age: u64, cpu: f32, command: &str) -> Proc {
        Proc { pid, ppid: 400, age, cpu, cputime: 5, command: command.into() }
    }

    struct World {
        rows: Vec<(&'static str, Status)>,
        held: Vec<&'static str>,
    }

    fn judge(w: &World, p: &Proc, cwd: &str, idle: u64) -> Result<Proof, String> {
        let status_of = |n: &str| w.rows.iter().find(|(r, _)| *r == n).map(|(_, s)| *s);
        let check = |n: &str| (n == "L-check-1").then_some(false);
        let held = |n: &str| w.held.contains(&n);
        let idle_for = |_: &Proc| idle;
        let facts = Facts { status_of: &status_of, check_in_use: &check, held: &held, keep: &[7], idle_for: &idle_for };
        prove(p, Some(cwd), &facts)
    }

    fn world() -> World {
        World { rows: vec![("wsp-1", Status::Doing), ("wsp-2", Status::Review), ("wsp-3", Status::Done)], held: vec!["wsp-3"] }
    }

    const VITE: &str = "node /r/.worktrees/x/node_modules/.bin/vite --port 5173";

    #[test]
    fn reap_refuses_a_pid_in_the_tree_of_a_doing_row() {
        let why = judge(&world(), &proc(10, 9000, 0.0, VITE), "/r/.worktrees/wsp-1/web", 0).unwrap_err();
        assert!(why.contains("wsp-1 is at doing"), "{why}");
    }

    #[test]
    fn reap_refuses_a_pid_younger_than_an_hour() {
        let why = judge(&world(), &proc(10, 3599, 0.0, VITE), "/r/.worktrees/wsp-2/web", 0).unwrap_err();
        assert!(why.contains("younger") || why.contains("min old"), "{why}");
        let ok = judge(&world(), &proc(10, 3600, 0.0, VITE), "/r/.worktrees/wsp-2/web", 0).unwrap();
        assert_eq!(ok.tree.as_deref(), Some("wsp-2"));
    }

    #[test]
    fn reap_refuses_a_row_a_seat_or_claim_holds_and_a_live_seats_own_process() {
        let why = judge(&world(), &proc(10, 9000, 0.0, VITE), "/r/.worktrees/wsp-3", 0).unwrap_err();
        assert!(why.contains("held"), "{why}");
        let why = judge(&world(), &proc(7, 9000, 0.0, VITE), "/r/.worktrees/wsp-2", 0).unwrap_err();
        assert!(why.contains("live seat"), "{why}");
    }

    #[test]
    fn reap_refuses_what_is_no_tool_and_what_stands_in_no_tree() {
        assert!(judge(&world(), &proc(10, 9000, 0.0, "vim notes"), "/r/.worktrees/wsp-2", 0).is_err());
        assert!(judge(&world(), &proc(10, 9000, 0.0, VITE), "/Users/ed", 0).is_err());
        assert!(judge(&world(), &proc(10, 9000, 0.0, VITE), "/r/.worktrees/nobody", 0).is_err());
    }

    #[test]
    fn a_test_binary_needs_fifteen_idle_minutes_in_a_finished_tree() {
        let bin = proc(11, 9000, 0.0, "/r/target/debug/deps/wsp-abc123 --test-threads 4");
        assert!(judge(&world(), &bin, "/r/.worktrees/wsp-2", 14 * 60).is_err());
        assert!(judge(&world(), &bin, "/r/.worktrees/wsp-2", 15 * 60).is_ok());
        assert!(judge(&world(), &bin, "/r/.worktrees/wsp-1", 15 * 60).is_err(), "not in a doing row's tree");
        assert!(judge(&world(), &bin, "/r/.worktrees/L-check-1", 15 * 60).is_ok(), "a barrier check's tree no longer in use");
        let busy = proc(11, 9000, 3.0, "/r/target/debug/deps/wsp-abc123");
        assert!(judge(&world(), &busy, "/r/.worktrees/wsp-2", 99_999).is_err());
    }

    #[test]
    fn a_busy_loop_is_a_zsh_while_over_twenty_percent_with_parent_one() {
        let mut l = proc(12, 100, 90.0, "/bin/zsh -c while :; do :; done");
        l.ppid = 1;
        assert!(judge(&world(), &l, "/", 0).is_ok());
        l.ppid = 400;
        assert!(judge(&world(), &l, "/", 0).is_err());
        l.ppid = 1;
        l.cpu = 10.0;
        assert!(judge(&world(), &l, "/", 0).is_err());
    }

    #[test]
    fn a_governor_reaps_only_in_trees_of_its_own_list() {
        let list_of = |n: &str| (n == "wsp-2").then(|| "fleet".to_string());
        let check_list = |n: &str| (n == "L-check-1").then(|| "L".to_string());
        let p = |tree: Option<&str>| Proof { tree: tree.map(str::to_string), text: "x".into() };
        let gov = |s: &str| Role::Governor(s.into());
        assert!(in_scope(&gov("fleet"), &p(Some("wsp-2")), &list_of, &check_list).is_ok());
        let why = in_scope(&gov("other"), &p(Some("wsp-2")), &list_of, &check_list).unwrap_err();
        assert!(why.contains("other") && why.contains("fleet"), "names both scopes: {why}");
        assert!(in_scope(&gov("L"), &p(Some("L-check-1")), &list_of, &check_list).is_ok(), "its barrier tree");
        assert!(in_scope(&gov("fleet"), &p(None), &list_of, &check_list).is_err(), "a busy loop is the primary's");
        assert!(in_scope(&Role::Primary, &p(None), &list_of, &check_list).is_ok());
        assert!(in_scope(&Role::Seat, &p(Some("wsp-2")), &list_of, &check_list).is_err());
    }

    #[test]
    fn roles_come_from_the_seat_and_not_from_the_caller() {
        assert_eq!(Role::of(None, None, false), Role::Outside);
        assert_eq!(Role::of(Some("cpd-1"), Some("primary"), true), Role::Primary);
        assert_eq!(Role::of(Some("cpd-2"), Some("fleet"), true), Role::Governor("fleet".into()));
        assert_eq!(Role::of(Some("cpd-3"), None, true), Role::Seat);
        assert_eq!(Role::of(Some("w1:p1"), None, false), Role::Outside, "Ed's own pane");
    }

    #[test]
    fn despawn_scope_by_role() {
        let gov = Role::Governor("fleet".into());
        assert!(despawn_allowed(&gov, None, Some("fleet"), false, false).is_ok());
        let why = despawn_allowed(&gov, None, Some("other"), false, false).unwrap_err();
        assert!(why.contains("other") && why.contains("fleet"), "{why}");
        assert!(despawn_allowed(&gov, None, None, false, false).is_err());
        assert!(despawn_allowed(&Role::Seat, None, Some("fleet"), false, false).is_err());
        assert!(despawn_allowed(&Role::Primary, None, None, false, false).is_err(), "--why is required");
        assert!(despawn_allowed(&Role::Primary, Some("dropped"), None, false, false).is_ok());
        assert!(despawn_allowed(&Role::Primary, Some("dropped"), None, true, false).is_err(), "mid-turn");
        assert!(despawn_allowed(&Role::Primary, Some("dropped"), None, true, true).is_ok(), "--now");
        assert!(despawn_allowed(&Role::Outside, None, None, true, false).is_ok());
    }

    #[test]
    fn done_and_reload_by_role() {
        assert!(done_allowed(&Role::Primary, Status::Review, None).is_ok(), "owes no verifier");
        assert!(done_allowed(&Role::Primary, Status::Review, Some(true)).is_ok());
        assert!(done_allowed(&Role::Primary, Status::Review, Some(false)).is_err());
        assert!(done_allowed(&Role::Primary, Status::Doing, None).is_err());
        assert!(done_allowed(&Role::Governor("fleet".into()), Status::Review, None).is_err());
        assert!(done_allowed(&Role::Seat, Status::Review, None).is_err());
        assert!(done_allowed(&Role::Outside, Status::Doing, None).is_ok());
        assert!(reload_allowed(&Role::Primary).is_ok());
        assert!(reload_allowed(&Role::Governor("fleet".into())).is_err());
        assert!(reload_allowed(&Role::Seat).is_err());
    }

    #[test]
    fn ps_lines_and_clocks_parse() {
        assert_eq!(clock_secs("01:02:03"), Some(3723));
        assert_eq!(clock_secs("2-00:00:10"), Some(172_810));
        assert_eq!(clock_secs("0:01.50"), Some(1));
        let p = parse_ps("  412     1  1-02:03:04  97.3   0:12.00 /bin/zsh -c while :; do :; done").unwrap();
        assert_eq!((p.pid, p.ppid, p.age, p.cputime), (412, 1, 93_784, 12));
        assert!(is_busy_loop(&p));
    }

    #[test]
    fn the_settings_files_allow_exactly_the_verbs_of_their_role() {
        let allow = |s: &str| -> Vec<String> {
            let v: Value = serde_json::from_str(s).unwrap();
            v["permissions"]["allow"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect()
        };
        assert_eq!(
            allow(PRIMARY_SETTINGS),
            ["Bash(wsp despawn:*)", "Bash(wsp done:*)", "Bash(wsp reap:*)", "Bash(wsp daemon reload)"]
        );
        assert_eq!(allow(GOVERNOR_SETTINGS), ["Bash(wsp despawn:*)", "Bash(wsp reap:*)"]);
        assert!(settings_args("opencode", Some("x"), false).is_empty());
        assert!(settings_args("claude", None, false).is_empty(), "a member gets neither file");
    }
}
