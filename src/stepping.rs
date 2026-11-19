//! A list's step, one at a time (`wsp-663`).
//!
//! Every trigger that moves a list — a verb's `wsp worklist advance`, the
//! daemon's tick — ends in [`crate::cycle::step`], and on 2026-11-17 two of
//! them for the same task ran the same step side by side: both read the group,
//! both found a member owed a start, and one of them spawned it a second time.
//! The state lock did not stop it: it guards a file's read and write, not a
//! step's minutes of reading, deciding and spawning, and under that load it was
//! carried on past anyway.
//!
//! So a step holds a lock **of its own list** while it runs — lists do not wait
//! on each other — and an advance that finds the list already being stepped
//! does not wait either. It leaves a mark, "step again", and goes; the holder
//! reads the mark before it lets go and takes one more step for every mark
//! that arrived while it ran, however many advances left one. No trigger is
//! lost and none piles up behind another. `docs/step-lock.md` has the argument.
//!
//! The files live in `<state>/stepping/`: `<list>.lock` holds `<pid> <epoch>`,
//! `<list>.again` is the mark.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::util;

/// A lock older than this is broken even if its pid answers: no step runs for
/// half an hour, so the pid has been reused.
const STALE_AFTER: i64 = 30 * 60;

/// What came of asking to step a list.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Stepped<T> {
    /// This process stepped it, `steps` times: the first and one more for
    /// each time a mark arrived while it ran.
    Ran { steps: usize, out: Vec<T> },
    /// Another step was under way: a mark was left for it, and nothing ran.
    Marked,
    /// Another step was under way and nothing was asked of it.
    Busy,
}

fn dir(state: &Path) -> PathBuf {
    state.join("stepping")
}

fn file(state: &Path, list: &str, ext: &str) -> PathBuf {
    dir(state).join(format!("{}.{ext}", list.replace('/', "_")))
}

/// The list's lock, held. Given back on drop, so a step that panics does
/// not wedge its list behind a live daemon's pid.
struct Held {
    path: PathBuf,
    ours: bool,
}

impl Drop for Held {
    fn drop(&mut self) {
        if self.ours {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Take the lock at `path`, or `None` when somebody alive holds it.
fn take(path: &Path) -> Option<Held> {
    for _ in 0..2 {
        match fs::OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(mut fh) => {
                let _ = writeln!(fh, "{} {}", std::process::id(), util::epoch_secs());
                return Some(Held { path: path.to_path_buf(), ours: true });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if !stale(path) {
                    return None;
                }
                let _ = fs::remove_file(path);
            }
            // A state directory that takes no file serialises nothing, and a
            // list that never steps again is worse than one stepped twice.
            Err(_) => return Some(Held { path: path.to_path_buf(), ours: false }),
        }
    }
    None
}

/// Whether the holder of `path` is gone. This process's own pid is a live
/// holder: a step that asks for its own list again is answered by the mark.
fn stale(path: &Path) -> bool {
    stale_by(path, |pid| crate::host::Host::signal(&crate::host::LOCAL, &[pid], "0"))
}

/// `alive(pid)` is `kill -0`'s answer: `Some(true)` reached, `Some(false)` not there, `None` could not ask.
fn stale_by(path: &Path, alive: impl Fn(u32) -> Option<bool>) -> bool {
    let text = fs::read_to_string(path).unwrap_or_default();
    let mut it = text.split_whitespace();
    let pid = it.next().and_then(|p| p.parse::<u32>().ok());
    let at = it.next().and_then(|a| a.parse::<i64>().ok());
    let (Some(pid), Some(at)) = (pid, at) else {
        // Being written this instant, or garbage: garbage ages out.
        return fs::metadata(path)
            .and_then(|m| m.modified())
            .map(|t| t.elapsed().unwrap_or_default().as_secs() > 60)
            .unwrap_or(false);
    };
    if util::epoch_secs() - at > STALE_AFTER {
        return true;
    }
    if pid == std::process::id() {
        return false;
    }
    // A `kill -0` that could not run keeps the lock: "could not ask" is not "not there" (host.rs `signal`; wsp-663).
    !alive(pid).unwrap_or(true)
}

/// Step `list` with its lock held: `step(0)` first, then `step(n)` once for
/// each mark found as it finishes.
///
/// When another step holds the list, `mark` decides what this call does: leave
/// the mark and go ([`Stepped::Marked`]) — what an advance does — or go and ask
/// nothing ([`Stepped::Busy`]), for a step that only meant to help the list
/// along and whose holder is already doing it.
pub(crate) fn one_at_a_time<T>(
    state: &Path,
    list: &str,
    mark: bool,
    mut step: impl FnMut(usize) -> Vec<T>,
) -> Stepped<T> {
    let _ = fs::create_dir_all(dir(state));
    let lock = file(state, list, "lock");
    let again = file(state, list, "again");
    let mut held = match take(&lock) {
        Some(h) => h,
        None if !mark => return Stepped::Busy,
        None => {
            let _ = fs::write(&again, format!("{} {}\n", std::process::id(), util::epoch_secs()));
            // Taken again after the mark, so a holder that let go between our
            // first try and the mark cannot leave the mark unread: either it
            // reads the mark after letting go, or this takes the lock.
            match take(&lock) {
                Some(h) => h,
                None => return Stepped::Marked,
            }
        }
    };
    let mut out = Vec::new();
    let mut steps = 0;
    loop {
        // A step that starts now answers every mark left so far.
        let _ = fs::remove_file(&again);
        out.extend(step(steps));
        steps += 1;
        if again.exists() {
            continue;
        }
        drop(held);
        // A mark left after the look above and before the lock went: its
        // advance found the lock held and did not step. Take it back for one
        // more, unless somebody else already has — then the mark is theirs.
        if !again.exists() {
            break;
        }
        match take(&lock) {
            Some(h) => held = h,
            None => break,
        }
    }
    Stepped::Ran { steps, out }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    fn state(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("wsp-stepping-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(dir(&d)).unwrap();
        d
    }

    /// Two advances arrive while a step runs, from other threads: neither
    /// steps nor waits, and the holder takes exactly one more step for both.
    #[test]
    fn two_advances_during_a_step_mark_it_and_it_steps_once_more() {
        let st = state("twice");
        let (started_tx, started) = mpsc::channel();
        let (go_tx, go) = mpsc::channel::<()>();
        let dir = st.clone();
        let holder = std::thread::spawn(move || {
            one_at_a_time(&dir, "run", true, |n| {
                if n == 0 {
                    started_tx.send(()).unwrap();
                    go.recv().unwrap();
                }
                vec![n]
            })
        });
        started.recv().unwrap();
        for _ in 0..2 {
            let dir = st.clone();
            let second = std::thread::spawn(move || one_at_a_time(&dir, "run", true, |n| vec![100 + n]));
            assert_eq!(second.join().unwrap(), Stepped::Marked, "a second advance does not run the step");
        }
        go_tx.send(()).unwrap();
        assert_eq!(holder.join().unwrap(), Stepped::Ran { steps: 2, out: vec![0, 1] }, "one step, then one follow-up");
        assert!(!file(&st, "run", "lock").exists() && !file(&st, "run", "again").exists());
        assert_eq!(one_at_a_time(&st, "run", true, |n| vec![n]), Stepped::Ran { steps: 1, out: vec![0] }, "and nothing is owed after");
        let _ = fs::remove_dir_all(&st);
    }

    /// Lists do not wait on each other: each step sees the other running.
    #[test]
    fn steps_of_two_lists_run_at_once() {
        let st = state("two");
        let (a_tx, a_rx) = mpsc::channel();
        let (b_tx, b_rx) = mpsc::channel();
        let dir = st.clone();
        let a = std::thread::spawn(move || {
            one_at_a_time(&dir, "a", true, |_| {
                a_tx.send(()).unwrap();
                vec![b_rx.recv_timeout(Duration::from_secs(10)).is_ok()]
            })
        });
        let b = one_at_a_time(&st, "b", true, |_| {
            b_tx.send(()).unwrap();
            vec![a_rx.recv_timeout(Duration::from_secs(10)).is_ok()]
        });
        assert_eq!(b, Stepped::Ran { steps: 1, out: vec![true] }, "b ran while a held its own list");
        assert_eq!(a.join().unwrap(), Stepped::Ran { steps: 1, out: vec![true] });
        let _ = fs::remove_dir_all(&st);
    }

    /// A step that only helps another list along leaves no mark on it.
    #[test]
    fn a_busy_list_asked_without_a_mark_is_left_to_its_holder() {
        let st = state("busy");
        let _held = take(&file(&st, "run", "lock")).unwrap();
        assert_eq!(one_at_a_time(&st, "run", false, |n| vec![n]), Stepped::Busy);
        assert!(!file(&st, "run", "again").exists());
        let _ = fs::remove_dir_all(&st);
    }

    /// A lock whose process is gone is broken; one this process holds is not.
    #[test]
    fn a_dead_holder_is_broken_and_a_live_one_is_not() {
        let st = state("dead");
        let lock = file(&st, "run", "lock");
        fs::write(&lock, format!("999999 {}\n", util::epoch_secs())).unwrap();
        assert_eq!(one_at_a_time(&st, "run", true, |n| vec![n]), Stepped::Ran { steps: 1, out: vec![0] });
        fs::write(&lock, format!("{} {}\n", std::process::id(), util::epoch_secs())).unwrap();
        assert_eq!(one_at_a_time(&st, "run", true, |n| vec![n]), Stepped::Marked);
        fs::write(&lock, format!("{} {}\n", std::process::id(), util::epoch_secs() - STALE_AFTER - 1)).unwrap();
        assert!(file(&st, "run", "again").exists());
        assert_eq!(one_at_a_time(&st, "run", true, |n| vec![n]), Stepped::Ran { steps: 1, out: vec![0] }, "old, and broken");
        assert!(!file(&st, "run", "again").exists(), "the mark is answered by the step that broke it");
        let _ = fs::remove_dir_all(&st);
    }

    /// A pid that is not this process, never probed for real: the probe is injected.
    fn lock_of_another_pid(st: &Path) -> PathBuf {
        let lock = file(st, "run", "lock");
        let other = (std::process::id() as u64 + 1_000_000) as u32;
        fs::write(&lock, format!("{other} {}\n", util::epoch_secs())).unwrap();
        lock
    }

    /// `kill -0` that could not run keeps the lock: "could not ask" is not "not there".
    #[test]
    fn a_lock_whose_holder_cannot_be_asked_is_kept() {
        let st = state("unasked");
        let lock = lock_of_another_pid(&st);
        assert!(!stale_by(&lock, |_| None), "a probe that could not run keeps the lock");
        let _ = fs::remove_dir_all(&st);
    }

    /// `kill -0` that reaches nothing breaks the lock.
    #[test]
    fn a_lock_whose_holder_is_gone_is_stale() {
        let st = state("gone");
        let lock = lock_of_another_pid(&st);
        assert!(stale_by(&lock, |_| Some(false)), "a holder that is not there is stale");
        let _ = fs::remove_dir_all(&st);
    }

    /// `kill -0` that reaches the holder keeps the lock.
    #[test]
    fn a_lock_whose_holder_answers_is_kept() {
        let st = state("answers");
        let lock = lock_of_another_pid(&st);
        assert!(!stale_by(&lock, |_| Some(true)), "a live holder keeps the lock");
        let _ = fs::remove_dir_all(&st);
    }
}
