//! A lease: a file its holder touches while it lives (wsp-057). What tells a
//! live holder from a dead one where a pid used to be probed — a tree lock
//! (`check::TreeLock`) and a barrier's mechanical run (`checks::Run`). No
//! process is asked anything: a holder that is killed stops touching its file,
//! and once the file is older than [`GRACE`] it is free.
//!
//! The touching is a thread of the holder's ([`Beat`]), so a holder busy in
//! one long call (a checkout, a disk wait) still reads live. A machine that
//! slept wakes with every lease old; the holder's beat touches its file again
//! within [`every`] of waking, and a reader that looks first sees it free —
//! the price of asking no process, and the same one a lock whose job ended
//! already pays.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, SystemTime};

/// How long a lease stands untouched before its holder is taken for gone.
pub const GRACE: Duration = Duration::from_secs(60);

/// How often a holder touches a lease whose grace is `grace`: four beats to a
/// grace, so a beat or two lost to a loaded machine does not free it.
pub fn every(grace: Duration) -> Duration {
    (grace / 4).max(Duration::from_millis(10))
}

/// Whether the file at `path` was touched within `grace`. A file that is not
/// there, or whose time cannot be read, is not fresh.
pub fn fresh(path: &Path, grace: Duration) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .is_ok_and(|t| t.elapsed().unwrap_or_default() < grace)
}

/// Touches `path`, which must already be there: a beat never makes a file.
pub fn touch(path: &Path) {
    if let Ok(f) = std::fs::File::options().write(true).open(path) {
        let _ = f.set_modified(SystemTime::now());
    }
}

/// A thread touching one file every `every` while `still(path)` says it is
/// still the holder's. Dropped, it stops, and has stopped when the drop
/// returns, so the holder may remove the file after it.
pub struct Beat {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Beat {
    pub fn start(path: PathBuf, every: Duration, still: impl Fn(&Path) -> bool + Send + 'static) -> Beat {
        let (stop, stopped) = mpsc::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("lease-beat".into())
            .spawn(move || {
                while let Err(mpsc::RecvTimeoutError::Timeout) = stopped.recv_timeout(every) {
                    if !still(&path) {
                        return;
                    }
                    touch(&path);
                }
            })
            .ok();
        Beat { stop: Some(stop), thread }
    }
}

impl Drop for Beat {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// A lease file of this process's own, beaten while it is held and removed
/// when it is dropped.
pub struct Lease {
    path: PathBuf,
    beat: Option<Beat>,
}

impl Lease {
    /// Writes this process's pid to `path` and starts beating it. `None` when
    /// the file cannot be written.
    pub fn take(path: PathBuf, grace: Duration) -> Option<Lease> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        std::fs::write(&path, format!("{}\n", std::process::id())).ok()?;
        let beat = Beat::start(path.clone(), every(grace), |_| true);
        Some(Lease { path, beat: Some(beat) })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        drop(self.beat.take());
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_lease_stays_fresh_past_its_grace_and_is_gone_once_dropped() {
        let iso = crate::util::isolated("lease-held");
        let path = iso.path("run.lease");
        let grace = Duration::from_millis(200);
        let lease = Lease::take(path.clone(), grace).unwrap();
        std::thread::sleep(grace * 3);
        assert!(fresh(lease.path(), grace), "the beat kept it");
        drop(lease);
        assert!(!path.exists(), "removed with its holder");
        assert!(!fresh(&path, grace), "no file is no holder");
    }

    #[test]
    fn a_file_nobody_touches_ages_out() {
        let iso = crate::util::isolated("lease-old");
        let path = iso.path("left.lease");
        std::fs::write(&path, "4000000000\n").unwrap();
        assert!(fresh(&path, GRACE));
        let f = std::fs::File::options().write(true).open(&path).unwrap();
        f.set_modified(SystemTime::now() - GRACE * 2).unwrap();
        assert!(!fresh(&path, GRACE), "a holder that stopped touching it is gone");
    }

    #[test]
    fn a_beat_stops_when_its_holder_no_longer_holds_the_file() {
        let iso = crate::util::isolated("lease-lost");
        let path = iso.path("lost.lease");
        std::fs::write(&path, "1\n").unwrap();
        let old = SystemTime::now() - Duration::from_secs(600);
        std::fs::File::options().write(true).open(&path).unwrap().set_modified(old).unwrap();
        let beat = Beat::start(path.clone(), Duration::from_millis(10), |_| false);
        std::thread::sleep(Duration::from_millis(100));
        drop(beat);
        assert!(!fresh(&path, GRACE), "a lost lease is not touched");
    }
}
