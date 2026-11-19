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
//! | `wsp check <row> --set --why …` | any row | rows on a list it governs, any change | its own row: raise a budget, add or correct a `cd`, narrow to its files, add a step |
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

pub(crate) const RETRO_SETTINGS: &str = include_str!("../claude-code/retro.settings.json");
/// The settings a member seat is started with, `<row>` its own row (wsp-681).
pub(crate) const MEMBER_SETTINGS: &str = include_str!("../claude-code/member.settings.json");
const RETRO_FILE: &str = "wsp-retro.settings.json";
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

/// Whether the process in `pane` is a seat wsp spawned or bound to a row, and so
/// a member, verifier or check unless it holds a slot.
///
/// The one exemption is by name: a seat's own deferred ending (`wsp despawn
/// --pane <seat> --keep-tree`, run detached under `WSP_END_WHEN_IDLE`) is the
/// seat finishing, not a reach into somebody else's, so it reads as unbound.
pub(crate) fn bound_here(store: &Store, pane: Option<&str>) -> bool {
    if std::env::var_os("WSP_END_WHEN_IDLE").is_some_and(|v| !v.is_empty()) {
        return false;
    }
    let spawned = std::env::var_os(crate::base::seat::SEAT_ENV).is_some_and(|v| !v.is_empty());
    spawned || pane.is_some_and(|p| store.bindings().contains_key(p))
}

/// The role a `Caller` (already resolved by `despawn`) stands for. `bound` is
/// [`bound_here`]: a member, verifier or check seat is refused by wsp itself.
pub(crate) fn role_of_caller(pane: Option<&str>, governs: Option<&str>, bound: bool) -> Role {
    Role::of(pane, governs, bound)
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

/// The list a row is on, if any.
pub(crate) fn list_of(store: &Store, row: &str) -> Option<String> {
    list_of_row(store, row)
}

// ---- wsp check --set (wsp-681) ------------------------------------------------

/// The row the caller's seat is bound to, as wsp recorded it (never from what
/// the caller says): the binding of its pane.
pub(crate) fn own_row(store: &Store) -> Option<String> {
    let pane = crate::cmd_agent::my_pane()?;
    store.bindings().get(&pane)?.get("task_id")?.as_str().map(str::to_string)
}

/// Who may make what a member may not, for a row on `list`.
pub(crate) fn who_may(list: Option<&str>) -> String {
    match list {
        Some(l) => format!("the governor of {l}, or Ed"),
        None => "Ed, or the primary seat".into(),
    }
}

/// Who a change to a row's checks is logged as.
pub(crate) fn who_set(role: &Role) -> String {
    match role {
        Role::Seat => "the row's member seat".into(),
        r => r.name(),
    }
}

/// Whether `role` may write `row`'s checks, as a pure decision. `own` is the
/// row the caller's seat is bound to, `list` and `project` the row's, and
/// `refused` what of the change a member may not make ([`crate::check::edits`]).
///
/// The primary and a person set anything. A governor sets the checks of the
/// rows on the list (or project) it governs, whatever the change. A member sets
/// its own row's only, and only what [`crate::check::edits`] lets it.
pub(crate) fn checks_allowed(
    role: &Role,
    row: &str,
    own: Option<&str>,
    list: Option<&str>,
    project: Option<&str>,
    refused: &[String],
) -> Result<(), String> {
    match role {
        Role::Outside | Role::Primary => Ok(()),
        Role::Governor(scope) if list == Some(scope.as_str()) || project == Some(scope.as_str()) => Ok(()),
        Role::Governor(scope) => Err(format!(
            "{row} is {}; the governor of {scope} sets the checks of rows on {scope} only — it needs {}",
            list.map(|l| format!("on list {l}")).unwrap_or_else(|| "on no list".into()),
            who_may(list)
        )),
        Role::Seat if own != Some(row) => Err(format!(
            "a member sets the checks of its own row only ({}); {row}'s are set by {}",
            own.unwrap_or("this seat is bound to none"),
            who_may(list)
        )),
        Role::Seat if !refused.is_empty() => Err(format!(
            "a member may raise a step's budget, add or correct a leading `cd <dir> &&`, narrow a step to the files its commits touched, or add a step; {}: only {} may do that",
            refused.join("; "),
            who_may(list)
        )),
        Role::Seat => Ok(()),
    }
}

/// Whether `role` may write a project's checks: never a member, verifier or
/// check seat, and a governor only its own project's.
pub(crate) fn project_checks_allowed(role: &Role, project: &str) -> Result<(), String> {
    match role {
        Role::Outside | Role::Primary => Ok(()),
        Role::Governor(scope) if scope == project => Ok(()),
        Role::Governor(scope) => Err(format!("the governor of {scope} does not set project {project}'s checks; Ed or the primary seat does")),
        Role::Seat => Err(format!("a member, verifier or check seat does not set project {project}'s checks; Ed or the primary seat does")),
    }
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

/// The list a verifier's or a barrier check's seat belongs to, for a seat that
/// is on no row of its own (`compounding-285`): a verifier stands on the member
/// it reads, a check on its list's group, and a governor ends both on its list.
fn list_of_seat(store: &Store, seat: &str) -> Option<String> {
    if let Some(member) = crate::verification::seats(&store.tasks()).get(seat) {
        return list_of_row(store, member);
    }
    crate::barrier::seats(&store.worklists()).remove(seat).map(|(list, _)| list)
}

/// The gate `despawn` runs once it knows which seat it is about. `Err` carries
/// the exit code, after the refusal has been said and logged.
pub(crate) fn gate_despawn(
    store: &Store,
    args: &Args,
    caller: (Option<&str>, Option<&str>, bool),
    seat: &str,
    task: Option<&str>,
) -> Result<(), i32> {
    let role = role_of_caller(caller.0, caller.1, caller.2);
    if role == Role::Outside {
        return Ok(());
    }
    // A successor ending the seat it is replacing is the rotation finishing, not
    // a reach into somebody's list: the handover record names both seats.
    if crate::cmd_govern::incoming(&store.handovers(), caller.0).is_some_and(|(_, from)| from == seat) {
        return Ok(());
    }
    let list = task.and_then(|t| list_of_row(store, t)).or_else(|| list_of_seat(store, seat));
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
    crate::cmd_task::done(store, args, &crate::cmd_spawn::seat_is_live)
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
    #[cfg(target_os = "macos")]
    let reloaded = crate::launchd::reload();
    #[cfg(not(target_os = "macos"))]
    let reloaded = crate::systemd::reload();
    match reloaded {
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

pub(crate) fn processes() -> Vec<Proc> {
    let Ok(out) = wsp_base::calls::Command::new("ps").args(["-A", "-o", "pid=,ppid=,etime=,pcpu=,time=,command="]).output() else {
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
    /// Whether a compound-sup label names a seat that is bound to a row or holds
    /// a governor's slot.
    pub seat_live: &'a dyn Fn(&str) -> bool,
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
    if let Some(label) = session_label(&p.command) {
        return prove_session(p, &label, cwd, f);
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
        // `held` was refused above, so a doing row here has no seat and no claim:
        // nobody may be using what stands in its tree.
        Some(Status::Doing) => format!("{name} is at doing with no seat and no claim"),
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
    Err("it is not a dev or test tool, a test binary, a busy loop or a leaked test session".into())
}

/// The `cpd-*` label of a compound-sup session: `compound-sup spawn --label cpd-N …`.
pub(crate) fn session_label(command: &str) -> Option<String> {
    let words: Vec<&str> = command.split_whitespace().collect();
    let first = words.first()?.rsplit('/').next()?;
    if first != "compound-sup" || words.get(1) != Some(&"spawn") {
        return None;
    }
    let at = words.iter().position(|w| *w == "--label")?;
    let label = words.get(at + 1)?;
    (label.starts_with("cpd-") && label[4..].chars().all(|c| c.is_ascii_digit()) && label.len() > 4).then(|| label.to_string())
}

/// A leaked compound-sup test session: its label is no live seat (no binding, no
/// governor slot, none of the live seats' processes), it is older than an hour,
/// and the row whose tree it stands in, if any, is not held (a doing row with no
/// seat and no claim is not held).
fn prove_session(p: &Proc, label: &str, cwd: Option<&str>, f: &Facts) -> Result<Proof, String> {
    if (f.seat_live)(label) {
        return Err(format!("{label} is a live seat — bound to a row or holding a governor's slot"));
    }
    if p.age < TOOL_AGE {
        return Err(format!("it is {} min old — a leaked session is stopped after an hour", p.age / 60));
    }
    let mut tree = None;
    if let Some((name, _)) = cwd.and_then(tree_named) {
        if (f.held)(&name) {
            return Err(format!("{name} is held by a live seat or a claim"));
        }
        tree = Some(name);
    }
    Ok(Proof { tree, text: format!("leaked test session {label}, {} min old, no live row", p.age / 60) })
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

/// Of the pids standing in a tree by working directory (`in_tree`, less every live
/// seat's), those a despawn ends with the seat: what the tree started
/// ([`crate::check_tree::verdict`]: a dev tool, or a child of something that stood
/// there), and a `compound-sup spawn --label cpd-N` test session, which a seat's
/// own test run leaves behind and which `verdict` spares as an installed server.
/// Never a host or a login shell, and never by a pattern on a name alone: each
/// pid is in the tree by its working directory.
pub(crate) fn left_by_a_seat(in_tree: &[u32], table: &[(u32, u32, String)], procs: &[Proc], tree: &std::path::Path) -> Vec<u32> {
    let session = |pid: u32| procs.iter().find(|p| p.pid == pid).is_some_and(|p| session_label(&p.command).is_some());
    in_tree
        .iter()
        .copied()
        .filter(|&pid| match crate::check_tree::verdict(pid, tree, in_tree, table) {
            crate::check_tree::Verdict::Stop => true,
            crate::check_tree::Verdict::Spare(_) => session(pid),
        })
        .collect()
}

/// The pids a seat started that stand in `dir`, to be ended with the tree.
/// Read before the tree is removed: a removed directory is not reliably a
/// working directory `lsof` can name.
pub(crate) fn left_in(dir: &std::path::Path) -> Vec<u32> {
    if cfg!(test) {
        return Vec::new();
    }
    let in_tree = crate::check_tree::running_in(dir);
    left_by_a_seat(&in_tree, &crate::check_tree::exes(), &processes(), dir)
}

/// `TERM`, a moment, then `KILL` for what is left. Returns what it signalled,
/// TERM first, then KILL.
pub(crate) fn end_pids(pids: &[u32]) -> (Vec<u32>, Vec<u32>) {
    if cfg!(test) || pids.is_empty() {
        return (Vec::new(), Vec::new());
    }
    crate::check_tree::signal("-TERM", pids);
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let still: Vec<u32> = util::alive(pids).into_iter().collect();
    crate::check_tree::signal("-KILL", &still);
    (pids.to_vec(), still)
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
    let _ = wsp_base::calls::Command::new("kill").args([sig, &pid.to_string()]).status();
}

/// Why `role` may not run `wsp reap` (`list` is the read-only form), if it may
/// not. Only stopping a pid is gated on the seat; listing changes nothing and is
/// open to every role.
pub(crate) fn reap_gate(role: &Role, list: bool) -> Option<&'static str> {
    (*role == Role::Seat && !list).then_some("a member, verifier or check seat does not reap")
}

/// The role whose scope `--list` shows. A seat has no scope of its own to reap
/// in, so it sees every candidate, as the primary would; every other role sees
/// what it could stop.
pub(crate) fn scope_for(role: &Role, list: bool) -> Role {
    if list && *role == Role::Seat { Role::Primary } else { role.clone() }
}

/// `wsp reap <pid>…` and `wsp reap --list`.
pub fn reap(store: &Store, args: &Args) -> i32 {
    let role = role(store);
    let list = args.has("list");
    if let Some(why) = reap_gate(&role, list) {
        return refuse(store, "reap", &role, why);
    }
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
    let cwds = wsp_base::calls::Command::new("lsof")
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
    let bindings = store.bindings();
    let governors = store.governors();
    let seat_live = |label: &str| {
        bindings.contains_key(label) || crate::place::governs(&governors, &crate::place::Seat::new(label)).is_some()
    };
    let facts = Facts { status_of: &status_of, check_in_use: &check_in_use, held: &held, keep: &keep, idle_for: &idle_for, seat_live: &seat_live };
    let list_of = |n: &str| list_of_row(store, n);
    let check_list = |n: &str| crate::check_tree::named(store, n).map(|(l, _)| l);

    let scope = scope_for(&role, list);
    let judge = |p: &Proc| -> Result<Proof, String> {
        let proof = prove(p, cwds.get(&p.pid).map(String::as_str), &facts)?;
        in_scope(&scope, &proof, &list_of, &check_list)?;
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

/// The deny rule the primary's `wsp done` used to hit. A deny beats any allow in
/// any `--settings` layer, so it has to go for the primary's allow to count; the
/// role gate in [`done`] is what stands in its place.
pub(crate) const STALE_DONE_DENY: &str = "\"Bash(wsp done:*)\"";

/// Whether `settings` (a `settings.json`) still denies `Bash(wsp done:*)`.
pub(crate) fn has_stale_done_deny(settings: &str) -> bool {
    strip_done_deny(settings).is_some()
}

/// `settings` without the `Bash(wsp done:*)` entry of `permissions.deny`, or
/// `None` when it has none. Text in, text out, so the rest of the file —
/// its key order, its spacing, its comments' neighbours — is byte for byte as it
/// was; the result is checked to parse and to differ from the original by that one
/// entry, or nothing is returned.
pub(crate) fn strip_done_deny(settings: &str) -> Option<String> {
    let before: Value = serde_json::from_str(settings).ok()?;
    let deny = before.pointer("/permissions/deny")?.as_array()?;
    if !deny.iter().any(|v| v.as_str() == Some("Bash(wsp done:*)")) {
        return None;
    }
    // The array is the one after the `"deny"` key inside `"permissions"`.
    let perm = settings.find("\"permissions\"")?;
    let key = perm + settings[perm..].find("\"deny\"")?;
    let open = key + settings[key..].find('[')?;
    // Walk to the matching `]`, splitting on commas outside strings.
    let (mut parts, mut start, mut in_str, mut esc, mut close) = (Vec::new(), open + 1, false, false, None);
    for (i, c) in settings[open + 1..].char_indices().map(|(i, c)| (i + open + 1, c)) {
        if in_str {
            match (esc, c) {
                (true, _) => esc = false,
                (false, '\\') => esc = true,
                (false, '"') => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            ',' => {
                parts.push(&settings[start..i]);
                start = i + 1;
            }
            ']' => {
                parts.push(&settings[start..i]);
                close = Some(i);
                break;
            }
            _ => {}
        }
    }
    let close = close?;
    let tail = &parts.last()?[parts.last()?.trim_end().len()..];
    let mut kept: Vec<&str> = parts.iter().copied().filter(|p| p.trim() != STALE_DONE_DENY).collect();
    if kept.len() == parts.len() {
        return None;
    }
    let mut body = match kept.pop() {
        Some(last) => {
            let mut b = kept.join(",");
            if !kept.is_empty() {
                b.push(',');
            }
            b.push_str(last.trim_end());
            b
        }
        None => String::new(),
    };
    body.push_str(tail);
    let out = format!("{}{}{}", &settings[..open + 1], body, &settings[close..]);
    let mut want = before;
    want.pointer_mut("/permissions/deny")?.as_array_mut()?.retain(|v| v.as_str() != Some("Bash(wsp done:*)"));
    (serde_json::from_str::<Value>(&out).ok()? == want).then_some(out)
}

/// Take the stale `Bash(wsp done:*)` deny out of `~/.claude/settings.json`,
/// touching nothing else in it. Whether it did.
pub(crate) fn remove_stale_done_deny() -> bool {
    let path = settings_path("settings.json");
    let Ok(text) = std::fs::read_to_string(&path) else { return false };
    match strip_done_deny(&text) {
        Some(out) => crate::store::write_atomic(&path, &out).is_ok(),
        None => false,
    }
}

/// What `doctor` says about the seat's settings: the deny that would still stop
/// the primary's `wsp done`.
pub(crate) fn health(warnings: &mut Vec<String>) {
    if std::fs::read_to_string(settings_path("settings.json")).is_ok_and(|s| has_stale_done_deny(&s)) {
        warnings.push("~/.claude/settings.json denies Bash(wsp done:*), which beats the primary's allow — `wsp install` removes it".into());
    }
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

/// Where the retro seat's settings file lives: under wsp's own state, not in
/// `~/.claude`. Only `--settings <path>` reads it, so it need not sit beside the
/// person's own settings, and a build run in `wsp sandbox` makes its own copy.
pub(crate) fn retro_settings_path(state: &std::path::Path) -> std::path::PathBuf {
    state.join("claude-code").join(RETRO_FILE)
}

/// The argument pair a retro seat's Claude Code is started with: `--settings
/// <file>`, which allows the read-only verbs its packet tells it to search with
/// (`wsp ls`, `wsp find`, `wsp show`). The file is written under `state` when
/// it is missing or differs; if it cannot be written the seat starts without.
pub(crate) fn retro_settings_args(kind: &str, state: &std::path::Path) -> Vec<String> {
    if kind != "claude" {
        return Vec::new();
    }
    let path = retro_settings_path(state);
    if !std::fs::read_to_string(&path).is_ok_and(|s| s == RETRO_SETTINGS) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if crate::store::write_atomic(&path, RETRO_SETTINGS).is_err() {
            return Vec::new();
        }
    }
    vec!["--settings".into(), path.display().to_string()]
}

/// Whether a spawn is a retro seat's: its row is tagged `retro` and it is not
/// the row's verifier.
pub(crate) fn retro_wanted(verifying: bool, row_tags: &[String]) -> bool {
    !verifying && row_tags.iter().any(|g| g == "retro")
}

/// The settings of a member seat on `row` (wsp-681): the allow rule for
/// `wsp check <row> --set` on its own row, as a permission and as the
/// classifier's own rule, and, on a retro row, the retro seat's besides. One
/// file, because a seat is started with one `--settings`.
pub(crate) fn member_settings(row: &str, retro: bool) -> String {
    let mut v: Value = serde_json::from_str(&MEMBER_SETTINGS.replace("<row>", row)).unwrap_or_else(|_| json!({}));
    if retro {
        let r: Value = serde_json::from_str(RETRO_SETTINGS).unwrap_or_else(|_| json!({}));
        for key in ["/permissions/allow", "/autoMode/allow"] {
            let more: Vec<Value> = r.pointer(key).and_then(Value::as_array).cloned().unwrap_or_default();
            if let Some(a) = v.pointer_mut(key).and_then(Value::as_array_mut) {
                a.extend(more.into_iter().filter(|m| m.as_str() != Some("$defaults")));
            }
        }
    }
    serde_json::to_string_pretty(&v).unwrap_or_default() + "\n"
}

/// Where a member seat's settings file for `row` lives, under wsp's state.
pub(crate) fn member_settings_path(state: &std::path::Path, row: &str) -> std::path::PathBuf {
    state.join("claude-code").join(format!("wsp-member-{row}.settings.json"))
}

/// What a spawn adds for its seat's own settings: a member seat on a row gets
/// its member settings (with the retro seat's on a retro row), a retro row's
/// other seats nothing, as before, and anyone else nothing. `member_row` is the
/// row of a member seat, `None` for any other spawn.
pub(crate) fn seat_spawn_args(kind: &str, member_row: Option<&str>, verifying: bool, row_tags: &[String], state: &std::path::Path) -> Vec<String> {
    let Some(row) = member_row.filter(|_| kind == "claude" && !verifying) else {
        return retro_spawn_args(kind, verifying, row_tags, state);
    };
    let body = member_settings(row, retro_wanted(verifying, row_tags));
    let path = member_settings_path(state, row);
    if !std::fs::read_to_string(&path).is_ok_and(|s| s == body) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if crate::store::write_atomic(&path, &body).is_err() {
            return retro_spawn_args(kind, verifying, row_tags, state);
        }
    }
    vec!["--settings".into(), path.display().to_string()]
}

/// What a spawn adds for a retro seat: the settings pair when the row is
/// tagged `retro` and the seat is not its verifier, nothing otherwise.
pub(crate) fn retro_spawn_args(kind: &str, verifying: bool, row_tags: &[String], state: &std::path::Path) -> Vec<String> {
    match retro_wanted(verifying, row_tags) {
        true => retro_settings_args(kind, state),
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
                                    row no seat or claim holds, a zsh busy loop, a test
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
        let facts = Facts { status_of: &status_of, check_in_use: &check, held: &held, keep: &[7], idle_for: &idle_for, seat_live: &|l| l == "cpd-9" };
        prove(p, Some(cwd), &facts)
    }

    fn world() -> World {
        World { rows: vec![("wsp-1", Status::Doing), ("wsp-2", Status::Review), ("wsp-3", Status::Done)], held: vec!["wsp-3"] }
    }

    const VITE: &str = "node /r/.worktrees/x/node_modules/.bin/vite --port 5173";

    #[test]
    fn reap_allows_a_pid_in_the_tree_of_a_doing_row_with_no_seat_and_refuses_one_a_seat_holds() {
        let ok = judge(&world(), &proc(10, 9000, 0.0, VITE), "/r/.worktrees/wsp-1/web", 0).unwrap();
        assert_eq!(ok.tree.as_deref(), Some("wsp-1"));
        assert!(ok.text.contains("no seat"), "{}", ok.text);
        let mut w = world();
        w.held.push("wsp-1");
        let why = judge(&w, &proc(10, 9000, 0.0, VITE), "/r/.worktrees/wsp-1/web", 0).unwrap_err();
        assert!(why.contains("held"), "{why}");
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
    fn a_leaked_compound_sup_session_with_no_live_row_is_reaped() {
        let sess = |l: &str| format!("/u/.local/bin/compound-sup spawn --label {l} --cwd /r/.worktrees/wsp-2 -- /bin/zsh");
        let ok = judge(&world(), &proc(10, 4000, 0.0, &sess("cpd-5")), "/r/.worktrees/wsp-2", 0).unwrap();
        assert!(ok.text.contains("cpd-5"), "{}", ok.text);
        assert!(judge(&world(), &proc(10, 4000, 0.0, &sess("cpd-9")), "/r/.worktrees/wsp-2", 0).is_err(), "live seat");
        assert!(judge(&world(), &proc(10, 100, 0.0, &sess("cpd-5")), "/r/.worktrees/wsp-2", 0).is_err(), "young");
        assert!(judge(&world(), &proc(10, 4000, 0.0, &sess("cpd-5")), "/r/.worktrees/wsp-1", 0).is_ok(), "doing, no seat");
        assert!(judge(&world(), &proc(10, 4000, 0.0, &sess("cpd-5")), "/r/.worktrees/wsp-3", 0).is_err(), "held");
        assert_eq!(session_label("compound-sup spawn --socket /x -- /bin/zsh"), None, "no label, no pattern");
        assert_eq!(session_label("vim --label cpd-5"), None);
    }

    #[test]
    fn a_despawn_ends_the_dev_tools_and_test_sessions_in_the_tree_and_no_host() {
        let t = std::path::Path::new("/r/.worktrees/wsp-1");
        let ex = |pid: u32, ppid: u32, exe: &str| (pid, ppid, exe.to_string());
        let table = vec![
            ex(100, 1, "/usr/local/bin/node"),
            ex(101, 1, "/Users/u/.local/bin/compound-sup"),
            ex(102, 101, "/bin/zsh"),
            ex(103, 1, "/Users/u/.local/bin/compound-sup"),
            ex(104, 1, "/opt/herdr"),
            ex(105, 1, "/Users/u/.local/bin/compound-sup"),
        ];
        let procs = vec![
            proc(100, 9000, 0.0, "node /r/.worktrees/wsp-1/node_modules/.bin/vite"),
            proc(101, 9000, 0.0, "/Users/u/.local/bin/compound-sup spawn --label cpd-7 -- /bin/zsh"),
            proc(102, 9000, 0.0, "/bin/zsh"),
            proc(103, 9000, 0.0, "/Users/u/.local/bin/compound-sup serve"),
            proc(104, 9000, 0.0, "/opt/herdr"),
            proc(105, 9000, 0.0, "/Users/u/.local/bin/compound-sup spawn --socket /x -- /bin/zsh"),
        ];
        let got = left_by_a_seat(&[100, 101, 102, 103, 104, 105], &table, &procs, t);
        assert_eq!(got, vec![100, 101, 102], "vite, the labelled session and its zsh; not the server, the host or an unlabelled one");
    }

    #[test]
    fn the_stale_done_deny_comes_out_and_nothing_else_does() {
        let old = "{\n  \"model\": \"x\",\n  \"permissions\": {\n    \"allow\": [\"Bash(ls:*)\"],\n    \"deny\": [\n      \"Bash(git push:*)\",\n      \"Bash(wsp done:*)\",\n      \"Bash(rm -rf:*)\"\n    ]\n  },\n  \"zed\": 1\n}\n";
        assert!(has_stale_done_deny(old));
        let out = strip_done_deny(old).unwrap();
        assert_eq!(out, old.replace("      \"Bash(wsp done:*)\",\n", ""));
        assert!(!has_stale_done_deny(&out));
        // last in the list, and alone
        let last = "{\"permissions\":{\"deny\":[\"Bash(a)\", \"Bash(wsp done:*)\"]}}";
        assert_eq!(strip_done_deny(last).unwrap(), "{\"permissions\":{\"deny\":[\"Bash(a)\"]}}");
        let only = "{\"permissions\":{\"deny\":[\"Bash(wsp done:*)\"]}}";
        assert_eq!(strip_done_deny(only).unwrap(), "{\"permissions\":{\"deny\":[]}}");
        // one without it is left alone
        assert_eq!(strip_done_deny("{\"permissions\":{\"deny\":[\"Bash(a)\"]}}"), None);
        assert_eq!(strip_done_deny("{}"), None);
        assert_eq!(strip_done_deny("not json"), None);
    }

    #[test]
    fn a_member_is_a_seat_and_not_outside_for_despawn() {
        assert_eq!(role_of_caller(Some("cpd-3"), None, true), Role::Seat);
        assert_eq!(role_of_caller(Some("w1:p1"), None, false), Role::Outside);
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
        assert!(judge(&world(), &bin, "/r/.worktrees/wsp-1", 15 * 60).is_ok(), "a doing row with no seat");
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
    fn reap_list_is_open_to_every_role_and_a_pid_stays_gated_for_a_seat() {
        // a check seat, a member seat and a verifier seat are all `Role::Seat`
        for role in [Role::Seat, Role::Outside, Role::Primary, Role::Governor("fleet".into())] {
            assert_eq!(reap_gate(&role, true), None, "{role:?} may list");
        }
        assert_eq!(reap_gate(&Role::Seat, false), Some("a member, verifier or check seat does not reap"));
        for role in [Role::Outside, Role::Primary, Role::Governor("fleet".into())] {
            assert_eq!(reap_gate(&role, false), None, "{role:?} may reap");
        }
    }

    #[test]
    fn a_seat_lists_every_candidate_but_stopping_one_stays_refused() {
        let list_of = |_: &str| None;
        let check_list = |_: &str| None;
        let p = Proof { tree: Some("wsp-2".into()), text: "x".into() };
        assert!(in_scope(&scope_for(&Role::Seat, true), &p, &list_of, &check_list).is_ok());
        let why = in_scope(&scope_for(&Role::Seat, false), &p, &list_of, &check_list).unwrap_err();
        assert!(why.contains("does not reap"), "{why}");
        let gov = Role::Governor("fleet".into());
        assert_eq!(scope_for(&gov, true), gov, "a governor still sees its own scope");
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
    fn a_verifiers_seat_is_on_the_list_of_the_row_it_reads() {
        let (env, store) = (crate::util::isolated("role-verifier-seat"), ());
        let _ = store;
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let mut m = crate::model::Task::new("m", "m-1");
        crate::verification::write(&mut m, &[crate::verification::Pass { pane: Some("w3:p1".into()), ..crate::verification::Pass::opened(None, None) }]);
        store.save_task(&m).unwrap();
        assert_eq!(crate::verification::seats(&store.tasks()).get("w3:p1").map(String::as_str), Some("m-1"));
        assert_eq!(list_of_seat(&store, "w3:p9"), None, "a seat on nothing is on no list");
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
        assert_eq!(allow(GOVERNOR_SETTINGS), ["Bash(wsp despawn:*)", "Bash(wsp reap:*)", "Bash(wsp check:*)"]);
        assert!(settings_args("opencode", Some("x"), false).is_empty());
        assert!(settings_args("claude", None, false).is_empty(), "a member gets neither file");
        assert_eq!(allow(RETRO_SETTINGS), ["Bash(wsp ls:*)", "Bash(wsp find:*)", "Bash(wsp show:*)"]);
        assert!(retro_settings_args("opencode", std::path::Path::new("/nonexistent")).is_empty());
        let v: Value = serde_json::from_str(RETRO_SETTINGS).unwrap();
        let prose = v["autoMode"]["allow"].as_array().unwrap();
        assert!(prose.iter().any(|p| {
            let p = p.as_str().unwrap();
            p.contains("wsp ls") && p.contains("wsp find") && p.contains("wsp show") && p.contains("read-only")
        }));
    }

    #[test]
    fn only_a_retro_rows_own_seat_is_started_with_the_retro_settings() {
        let retro = vec!["wsp".to_string(), "retro".to_string()];
        assert!(retro_wanted(false, &retro));
        assert!(!retro_wanted(true, &retro), "its verifier is not a retro seat");
        assert!(!retro_wanted(false, &["wsp".to_string()]), "a member is not");
        assert!(!retro_wanted(false, &[]));
        let state = std::env::temp_dir().join(format!("wsp-retro-args-{}", std::process::id()));
        assert!(retro_spawn_args("opencode", false, &retro, &state).is_empty());
        assert!(retro_spawn_args("claude", true, &retro, &state).is_empty());
        assert!(retro_spawn_args("claude", false, &["wsp".to_string()], &state).is_empty());
    }

    #[test]
    fn the_retro_args_name_a_file_under_the_state_holding_the_retro_settings() {
        let state = std::env::temp_dir().join(format!("wsp-retro-file-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        let args = retro_settings_args("claude", &state);
        assert_eq!(args.len(), 2);
        assert_eq!(args[0], "--settings");
        let path = std::path::PathBuf::from(&args[1]);
        assert!(path.starts_with(&state), "{path:?} is not under {state:?}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), RETRO_SETTINGS);
        // A stale copy is replaced, and the args are the same path.
        std::fs::write(&path, "stale").unwrap();
        assert_eq!(retro_settings_args("claude", &state), args);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), RETRO_SETTINGS);
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn a_member_seat_is_started_allowed_check_set_on_its_own_row_only_and_the_classifier_is_told() {
        let v: Value = serde_json::from_str(&member_settings("wsp-9", false)).unwrap();
        let allow: Vec<&str> = v["permissions"]["allow"].as_array().unwrap().iter().map(|x| x.as_str().unwrap()).collect();
        assert_eq!(allow, ["Bash(wsp check wsp-9 --set:*)"], "its own row, and nothing else");
        let prose: Vec<&str> = v["autoMode"]["allow"].as_array().unwrap().iter().map(|x| x.as_str().unwrap()).collect();
        assert_eq!(prose[0], "$defaults");
        assert!(!prose[1].contains("<row>") && prose[1].contains("wsp-9") && prose[1].contains("Ed"), "{prose:?}");
        // A retro row's member gets the retro rules in the same one file.
        let r: Value = serde_json::from_str(&member_settings("wsp-9", true)).unwrap();
        let allow: Vec<&str> = r["permissions"]["allow"].as_array().unwrap().iter().map(|x| x.as_str().unwrap()).collect();
        assert_eq!(allow, ["Bash(wsp check wsp-9 --set:*)", "Bash(wsp ls:*)", "Bash(wsp find:*)", "Bash(wsp show:*)"]);
        assert_eq!(r["autoMode"]["allow"].as_array().unwrap().iter().filter(|x| x.as_str() == Some("$defaults")).count(), 1);
        // The governor's file tells the classifier too.
        let g: Value = serde_json::from_str(GOVERNOR_SETTINGS).unwrap();
        assert!(g["autoMode"]["allow"].as_array().unwrap().iter().any(|p| p.as_str().unwrap().contains("wsp check <row> --set")));

        let state = std::env::temp_dir().join(format!("wsp-member-args-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        let args = seat_spawn_args("claude", Some("wsp-9"), false, &[], &state);
        assert_eq!(args, ["--settings".to_string(), member_settings_path(&state, "wsp-9").display().to_string()]);
        assert_eq!(std::fs::read_to_string(member_settings_path(&state, "wsp-9")).unwrap(), member_settings("wsp-9", false));
        assert!(seat_spawn_args("claude", Some("wsp-9"), true, &[], &state).is_empty(), "a verifier is no member");
        assert!(seat_spawn_args("claude", None, false, &[], &state).is_empty(), "no member row, no file");
        assert!(seat_spawn_args("opencode", Some("wsp-9"), false, &[], &state).is_empty());
        let retro = vec!["retro".to_string()];
        assert_eq!(seat_spawn_args("claude", None, false, &retro, &state), retro_settings_args("claude", &state), "a retro seat as before");
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn check_set_by_role_a_member_its_own_row_within_limits_a_governor_its_lists_rows() {
        let none: &[String] = &[];
        let deleted = vec!["`build` is deleted".to_string()];
        assert!(checks_allowed(&Role::Outside, "r-1", None, Some("fleet"), None, &deleted).is_ok());
        assert!(checks_allowed(&Role::Primary, "r-1", None, Some("fleet"), None, &deleted).is_ok());
        let gov = Role::Governor("fleet".into());
        assert!(checks_allowed(&gov, "r-1", None, Some("fleet"), None, &deleted).is_ok(), "a governor deletes on its list's row");
        assert!(checks_allowed(&gov, "r-1", None, None, Some("fleet"), &deleted).is_ok(), "and on its project's");
        let no = checks_allowed(&gov, "r-1", None, Some("other"), None, none).unwrap_err();
        assert!(no.contains("on list other") && no.contains("the governor of other, or Ed"), "{no}");
        assert!(checks_allowed(&Role::Seat, "r-1", Some("r-1"), Some("fleet"), None, none).is_ok(), "a member, its own row");
        let no = checks_allowed(&Role::Seat, "r-1", Some("r-2"), Some("fleet"), None, none).unwrap_err();
        assert!(no.contains("its own row only (r-2)") && no.contains("the governor of fleet, or Ed"), "{no}");
        let no = checks_allowed(&Role::Seat, "r-1", Some("r-1"), Some("fleet"), None, &deleted).unwrap_err();
        assert!(no.contains("`build` is deleted") && no.contains("only the governor of fleet, or Ed may"), "{no}");
        assert!(project_checks_allowed(&Role::Seat, "pj").is_err());
        assert!(project_checks_allowed(&Role::Governor("pj".into()), "pj").is_ok());
        assert!(project_checks_allowed(&Role::Governor("x".into()), "pj").is_err());
        assert!(project_checks_allowed(&Role::Outside, "pj").is_ok());
        assert_eq!(who_set(&Role::Seat), "the row's member seat");
    }
}
