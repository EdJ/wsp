//! The supervisors a test starts: tied to the test process, kept out of the
//! person's run directory, and checked for at the thread's end (wsp-432).
//! Moved down from `wsp-wire`'s `place_compound` (wsp-020), which re-exports
//! it, because [`crate::inproc::sup_command`] and the in-process spawn are
//! where both rules are put on.
//!
//! What keeps a test from leaking a compound session (`wsp-432`).
//!
//! 212 sessions stood at once on 2026-11-06, opened by `cargo test` through the
//! real `compound-sup` into the person's `~/.compound/run` and never closed.
//! Two rules, both kept by [`crate::inproc::sup_command`], the one place a
//! supervisor's command is built: it is `--ephemeral <this pid>`, and it runs
//! with `COMPOUND_RUN_DIR` at a directory of this process's own. The third is
//! a check: a supervisor a test started and left running when the test's
//! thread ends is killed, named, and ends the run with a failure — so a test
//! that forgets its cleanup is found the first time it runs, not by a person
//! counting sessions.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use wsp_base::calls::Command;

use crate::inproc::{alive, signal_group};
use crate::port::Seat;
use std::cell::RefCell;

/// This process's run directory. A unix socket path may be 104 bytes: the
/// test runner (`.cargo/test-run`) keeps `$TMPDIR` short enough for it.
pub fn run_dir() -> PathBuf {
    static SWEPT: std::sync::Once = std::sync::Once::new();
    SWEPT.call_once(|| {
        // The run directories of test processes that have ended: a
        // supervisor killed rather than closed leaves its sockets behind.
        let Ok(dirs) = fs::read_dir(std::env::temp_dir()) else { return };
        for d in dirs.flatten() {
            let name = d.file_name().to_string_lossy().into_owned();
            let Some(pid) = name.strip_prefix("wsp-cpd-run-").and_then(|p| p.parse::<u32>().ok()) else { continue };
            if pid != std::process::id() && alive(&[pid]).is_empty() {
                let _ = fs::remove_dir_all(d.path());
            }
        }
    });
    let dir = std::env::temp_dir().join(format!("wsp-cpd-run-{}", std::process::id()));
    let _ = fs::create_dir_all(&dir);
    dir
}

/// The two rules, put on a supervisor's command.
pub fn tie_to_this_test(cmd: &mut Command) {
    cmd.env("COMPOUND_RUN_DIR", run_dir());
    cmd.args(["--ephemeral", &std::process::id().to_string()]);
}

/// Whether this command carries both rules. The one assertion every
/// supervisor command a test builds is held to.
pub fn tied(cmd: &Command) -> std::result::Result<(), String> {
    let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
    let sep = args.iter().position(|a| a == "--").ok_or_else(|| format!("no `--` in {args:?}"))?;
    let pid = std::process::id().to_string();
    match args.iter().position(|a| a == "--ephemeral") {
        Some(at) if at < sep && args.get(at + 1) == Some(&pid) => {}
        _ => return Err(format!("not `--ephemeral {pid}` before the `--`: {args:?}")),
    }
    let run = cmd
        .get_envs()
        .find(|(k, _)| *k == "COMPOUND_RUN_DIR")
        .and_then(|(_, v)| v)
        .map(PathBuf::from)
        .ok_or_else(|| "no COMPOUND_RUN_DIR on it".to_string())?;
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".compound");
    match run.starts_with(&home) {
        true => Err(format!("runs in {}, under the person's own ~/.compound", run.display())),
        false => Ok(()),
    }
}

thread_local! {
    static OPENED: RefCell<Watch> = const { RefCell::new(Watch(Vec::new())) };
}

/// The supervisors this thread (this test) started.
struct Watch(Vec<(u32, String)>);

pub fn note(pid: u32, seat: &Seat) {
    OPENED.with(|w| w.borrow_mut().0.push((pid, seat.to_string())));
}

/// The pids of this thread's supervisors that are still running, after a
/// moment for a `kill` just sent to take.
fn standing(pids: &[u32]) -> Vec<u32> {
    let mut left: Vec<u32> = alive(pids).into_iter().collect();
    for _ in 0..50 {
        if left.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
        left = alive(&left).into_iter().collect();
    }
    left
}

impl Drop for Watch {
    fn drop(&mut self) {
        let pids: Vec<u32> = self.0.iter().map(|(p, _)| *p).collect();
        let left = standing(&pids);
        if left.is_empty() {
            return;
        }
        for pid in &left {
            signal_group(*pid, "KILL");
        }
        let named: Vec<String> = self
            .0
            .iter()
            .filter(|(p, _)| left.contains(p))
            .map(|(p, s)| format!("{s} (pid {p})"))
            .collect();
        eprintln!(
            "\nwsp-432: test `{}` left compound-sup running past its end: {}.\n  \
             Stop the seat (`StopsOnDrop`, `EndsOnDrop`, or the `Scratch` guard) before the test returns. \
             Killed here so it does not stand.\n",
            std::thread::current().name().unwrap_or("?"),
            named.join(", ")
        );
        // The thread's result is already sent, so no assertion can reach
        // it; the run itself is what fails.
        std::process::exit(101);
    }
}
