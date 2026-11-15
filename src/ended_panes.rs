//! A pane whose seat has ended: `wsp-637`.
//!
//! On 2026-11-15, 25 of 33 `compound-sup` supervisors held a bare idle shell and
//! a `compound-render`: the agent had exited, the pane stayed, and `wsp wip` did
//! not count it. Compound's host reattached to all of them on start.
//!
//! **An ended seat's pane** is a `compound-sup spawn --label cpd-N` whose label is
//! no live seat (no binding, no governor's slot), whose working directory is in a
//! row's tree, and under which nothing runs but a shell with no children and the
//! renderer. A person's own terminal has no row's tree for a working directory
//! and is never touched; a pane with an agent, or anything else running, is a
//! seat in use.
//!
//! `wsp wip` names them ([`wip_lines`]). The daemon's sweep ([`tidy`]) closes one
//! that has stood so for [`GRACE`] and says so in `cycle.log`: HUP to the shell,
//! then TERM to the supervisor and renderer if they stay.

use crate::cmd_role::Proc;
use crate::store::Store;

/// Minutes an ended seat's pane may stand idle before the daemon closes it.
pub(crate) const GRACE_MIN: u64 = 30;

const SHELLS: &[&str] = &["zsh", "bash", "sh", "fish"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Ended {
    pub label: String,
    pub sup: u32,
    pub shell: u32,
    /// Supervisor, shell and renderer: all that the pane holds.
    pub pids: Vec<u32>,
    pub age_min: u64,
    pub cwd: String,
}

fn base(command: &str) -> &str {
    command.split_whitespace().next().and_then(|w| w.rsplit('/').next()).unwrap_or("")
}

/// The ended panes among `procs`. `cwd_of` is a supervisor's working directory,
/// `live` whether a label is a live seat, `min_age` the idle minutes wanted.
pub(crate) fn find(
    procs: &[Proc],
    cwd_of: &dyn Fn(u32) -> Option<String>,
    live: &dyn Fn(&str) -> bool,
    in_row_tree: &dyn Fn(&str) -> bool,
    min_age: u64,
) -> Vec<Ended> {
    let mut out = Vec::new();
    for sup in procs {
        let Some(label) = crate::cmd_role::session_label(&sup.command) else { continue };
        if live(&label) || sup.age < min_age * 60 {
            continue;
        }
        let Some(cwd) = cwd_of(sup.pid).filter(|c| in_row_tree(c)) else { continue };
        let kids: Vec<&Proc> = procs.iter().filter(|p| p.ppid == sup.pid).collect();
        let shells: Vec<&&Proc> = kids.iter().filter(|p| SHELLS.contains(&base(&p.command).trim_start_matches('-'))).collect();
        let renders: Vec<&&Proc> = kids.iter().filter(|p| base(&p.command) == "compound-render").collect();
        let [shell] = shells.as_slice() else { continue };
        if kids.len() != shells.len() + renders.len() || procs.iter().any(|p| p.ppid == shell.pid) {
            continue;
        }
        let mut pids = vec![sup.pid, shell.pid];
        pids.extend(renders.iter().map(|p| p.pid));
        out.push(Ended { label, sup: sup.pid, shell: shell.pid, pids, age_min: sup.age / 60, cwd });
    }
    out
}

fn live_find(store: &Store, min_age: u64) -> Vec<Ended> {
    if cfg!(test) {
        return Vec::new();
    }
    let procs = crate::cmd_role::processes();
    let sups: Vec<String> =
        procs.iter().filter(|p| crate::cmd_role::session_label(&p.command).is_some()).map(|p| p.pid.to_string()).collect();
    if sups.is_empty() {
        return Vec::new();
    }
    let cwds = wsp_base::calls::Command::new("lsof")
        .args(["-a", "-d", "cwd", "-Fpn", "-p", &sups.join(",")])
        .output()
        .map(|o| crate::cmd_role::parse_cwds(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default();
    let bindings = store.bindings();
    let governors = store.governors();
    let live = |l: &str| bindings.contains_key(l) || crate::place::governs(&governors, &crate::place::Seat::new(l)).is_some();
    find(&procs, &|pid| cwds.get(&pid).cloned(), &live, &|c| crate::cmd_role::tree_named(c).is_some(), min_age)
}

pub(crate) fn line(e: &Ended) -> String {
    format!("ended seat {}: pane idle {} min in {} (supervisor {}, {} processes)", e.label, e.age_min, e.cwd, e.sup, e.pids.len())
}

/// What `wsp wip` says: the count, then each pane named.
pub(crate) fn wip_lines(store: &Store) -> Vec<String> {
    let found = live_find(store, 0);
    if found.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![format!("ended seats' panes: {} (the daemon closes one idle {GRACE_MIN} min)", found.len())];
    lines.extend(found.iter().map(|e| format!("  {}", line(e))));
    lines
}

/// Closes what has stood [`GRACE_MIN`] minutes, and says so in `cycle.log`.
pub(crate) fn tidy(store: &Store) {
    for e in live_find(store, GRACE_MIN) {
        crate::store::log_line(store, &format!("closed {}", line(&e)));
        crate::check_tree::signal("-HUP", &[e.shell]);
        std::thread::sleep(std::time::Duration::from_millis(1500));
        let left: Vec<u32> = crate::util::alive(&e.pids).into_iter().collect();
        crate::check_tree::signal("-TERM", &left);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, ppid: u32, age: u64, command: &str) -> Proc {
        Proc { pid, ppid, age, cpu: 0.0, cputime: 0, command: command.into() }
    }

    fn table() -> Vec<Proc> {
        vec![
            p(10, 1, 3600, "/u/compound-sup spawn --label cpd-1 -- /bin/zsh"),
            p(11, 10, 3600, "-zsh"),
            p(12, 10, 3600, "compound-render"),
            p(20, 1, 3600, "/u/compound-sup spawn --label cpd-2 -- /bin/zsh"),
            p(21, 20, 3600, "-zsh"),
            p(22, 21, 100, "claude"),
            p(30, 1, 60, "/u/compound-sup spawn --label cpd-3 -- /bin/zsh"),
            p(31, 30, 60, "-zsh"),
            p(40, 1, 3600, "/u/compound-sup spawn --label cpd-4 -- /bin/zsh"),
            p(41, 40, 3600, "-zsh"),
        ]
    }

    #[test]
    fn a_bare_shell_of_no_live_seat_is_an_ended_pane_and_an_agent_or_a_young_one_is_not() {
        let cwd = |_| Some("/r/.worktrees/wsp-1".to_string());
        let found = find(&table(), &cwd, &|l| l == "cpd-4", &|_| true, 30);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].label, "cpd-1");
        assert_eq!(found[0].pids, vec![10, 11, 12]);
        assert_eq!(find(&table(), &cwd, &|_| false, &|_| true, 0).len(), 3, "young ones count for wip");
    }

    #[test]
    fn a_pane_outside_every_rows_tree_is_a_persons_own_and_left_alone() {
        let cwd = |_| Some("/Users/u".to_string());
        assert!(find(&table(), &cwd, &|_| false, &|_| false, 0).is_empty());
    }
}
