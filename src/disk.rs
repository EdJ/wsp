//! Free space on the volumes holding the project roots, and a warning before it
//! runs out: `wsp-327`.
//!
//! The machine reached 3.4 GB free on 2026-11-01 with every cause already in
//! `.worktrees/` and `state/warm/` as `target/` directories, and nothing said
//! so until agents started failing to write. [`read`] is one reading shared by
//! the daemon's tick, `wsp watch --status`, `wsp wip` and `wsp doctor`: free
//! space on each volume a project root is on, and under [`LOW`] the largest
//! `target/` the machine could lose without losing work.
//!
//! Sizing is `du` over every candidate, minutes on a machine with a hundred
//! trees, so it is done where nobody is waiting: the daemon measures in a
//! thread when a volume is low and writes `disk.json` under the state
//! directory, and every surface reads that record while it is younger than
//! [`FRESH`] and measures inline only when there is none.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde_json::json;

use crate::cmd_checkout::WORKTREES;
use crate::store::Store;
use crate::util;

pub(crate) const LOW: u64 = 30 * 1024 * 1024 * 1024;

/// How long a measured largest build output is believed, in seconds.
const FRESH: i64 = 10 * 60;

pub(crate) struct Removable {
    pub path: String,
    pub bytes: u64,
}

pub(crate) struct Disk {
    pub volume: String,
    pub free: u64,
    pub limit: u64,
    /// Only measured on a low volume.
    pub largest: Option<Removable>,
}

impl Disk {
    pub(crate) fn low(&self) -> bool {
        self.free < self.limit
    }

    /// `212 GB free on /System/Volumes/Data`, and when low, what to remove.
    pub(crate) fn line(&self) -> String {
        let free = format!("{} free on {}", gb(self.free), self.volume);
        if !self.low() {
            return free;
        }
        let remove = match &self.largest {
            Some(r) => format!("the largest build output that can go is {} ({}) — it is rebuilt on demand", util::contract(Path::new(&r.path)), gb(r.bytes)),
            None => "no removable build output measured yet — `wsp doctor` measures it".into(),
        };
        format!("disk is low: {free}, under {} — {remove}", gb(self.limit))
    }
}

fn gb(bytes: u64) -> String {
    match bytes {
        b if b >= 10 * 1024 * 1024 * 1024 => format!("{} GB", b / (1024 * 1024 * 1024)),
        b => format!("{:.1} GB", b as f64 / (1024.0 * 1024.0 * 1024.0)),
    }
}

/// The mount point and free bytes `df -Pk` reports for `path`.
fn parse_df(out: &str) -> Option<(String, u64)> {
    let row = out.lines().nth(1)?;
    let fields: Vec<&str> = row.split_whitespace().collect();
    let free: u64 = fields.get(3)?.parse().ok()?;
    let volume = fields.get(5..)?.join(" ");
    (!volume.is_empty()).then_some((volume, free * 1024))
}

fn free_of(path: &Path) -> Option<(String, u64)> {
    let out = Command::new("df").args(["-Pk"]).arg(path).output().ok().filter(|o| o.status.success())?;
    parse_df(&String::from_utf8_lossy(&out.stdout))
}

/// Every `target/` wsp can rebuild: a tree under a repository's `.worktrees/`,
/// and a warm verify slot.
pub(crate) fn candidates(store: &Store) -> Vec<PathBuf> {
    let mut trees = Vec::new();
    for repo in crate::check_tree::repos(store) {
        trees.extend(children(&repo.join(WORKTREES)));
    }
    for slot in children(&store.state.join("warm")) {
        trees.push(slot.join("tree"));
        trees.push(slot);
    }
    trees.into_iter().map(|t| t.join("target")).filter(|t| std::fs::symlink_metadata(t).is_ok_and(|m| m.is_dir())).collect()
}

fn children(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
    out.sort();
    out
}

fn size_of(path: &Path) -> Option<u64> {
    let out = Command::new("du").args(["-sk"]).arg(path).output().ok()?;
    let kb: u64 = String::from_utf8_lossy(&out.stdout).split_whitespace().next()?.parse().ok()?;
    Some(kb * 1024)
}

/// The largest of `paths` that is on the volume mounted at `volume`.
pub(crate) fn largest(paths: &[PathBuf], volume: &str) -> Option<Removable> {
    paths
        .iter()
        .filter(|p| free_of(p).is_some_and(|(v, _)| v == volume))
        .filter_map(|p| size_of(p).map(|bytes| Removable { path: p.display().to_string(), bytes }))
        .max_by_key(|r| r.bytes)
}

fn record_path(state: &Path) -> PathBuf {
    state.join("disk.json")
}

fn recorded(state: &Path, volume: &str) -> Option<Removable> {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(record_path(state)).ok()?).ok()?;
    let row = v.get(volume)?;
    let at = row.get("at")?.as_i64()?;
    if util::epoch_secs() - at > FRESH {
        return None;
    }
    let path = row.get("path")?.as_str()?.to_string();
    // The directory may have been removed since it was measured; a record naming
    // one is a pointer to nothing.
    Path::new(&path).is_dir().then_some(())?;
    Some(Removable { path, bytes: row.get("bytes")?.as_u64()? })
}

fn record(state: &Path, volume: &str, found: &Removable) {
    let mut all: serde_json::Value = std::fs::read_to_string(record_path(state))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| json!({}));
    all[volume] = json!({ "at": util::epoch_secs(), "path": found.path, "bytes": found.bytes });
    let _ = std::fs::write(record_path(state), all.to_string());
}

/// The volumes the project roots are on, deduplicated, with free space.
fn volumes(store: &Store) -> Vec<(String, u64)> {
    let mut seen = BTreeSet::new();
    crate::check_tree::repos(store)
        .iter()
        .filter_map(|r| free_of(r))
        .filter(|(v, _)| seen.insert(v.clone()))
        .collect()
}

/// One reading of every volume against `limit`. On a low one, the largest
/// removable build output from the record, or, with `measure`, measured now
/// when there is none.
pub(crate) fn read(store: &Store, limit: u64, measure: bool) -> Vec<Disk> {
    volumes(store)
        .into_iter()
        .map(|(volume, free)| {
            let mut disk = Disk { volume, free, limit, largest: None };
            if disk.low() {
                disk.largest = recorded(&store.state, &disk.volume).or_else(|| {
                    let found = measure.then(|| largest(&candidates(store), &disk.volume)).flatten()?;
                    record(&store.state, &disk.volume, &found);
                    Some(found)
                });
            }
            disk
        })
        .collect()
}

/// What `wsp doctor` says: the free space as a note, and a low volume as a
/// problem, measuring what can go if the daemon has not.
pub(crate) fn health(store: &Store, problems: &mut Vec<String>, notes: &mut Vec<String>) {
    for disk in read(store, LOW, true) {
        match disk.low() {
            true => problems.push(disk.line()),
            false => notes.push(disk.line()),
        }
    }
}

/// The low volumes alone, for the headings that are for faults: `wsp wip`.
pub(crate) fn loud(store: &Store) -> Option<String> {
    let low: Vec<String> = read(store, LOW, false).iter().filter(|d| d.low()).map(Disk::line).collect();
    (!low.is_empty()).then(|| low.join("\n"))
}

static MEASURING: AtomicBool = AtomicBool::new(false);
static SAID: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// The daemon's half: on a low volume, measure in a thread when the record has
/// gone stale, and say it in `cycle.log` once per volume and largest output.
///
/// Not under test, for `check_tree::tidy`'s reason; the tests drive [`Disk`]
/// and [`read`].
pub(crate) fn tidy(store: &Store) {
    if cfg!(test) {
        return;
    }
    for disk in read(store, LOW, false) {
        if !disk.low() {
            continue;
        }
        if disk.largest.is_none() && !MEASURING.swap(true, Ordering::SeqCst) {
            let (paths, volume, state) = (candidates(store), disk.volume.clone(), store.state.clone());
            std::thread::spawn(move || {
                if let Some(found) = largest(&paths, &volume) {
                    record(&state, &volume, &found);
                }
                MEASURING.store(false, Ordering::SeqCst);
            });
        }
        let key = format!("{} {}", disk.volume, disk.largest.as_ref().map(|r| r.path.as_str()).unwrap_or(""));
        if disk.largest.is_some() && SAID.lock().map(|mut s| s.insert(key)).unwrap_or(true) {
            crate::cycle::log_line(store, &disk.line());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn df_is_read_for_the_volume_and_what_is_free() {
        let out = "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/disk1s1 976797816 866661464 88733768 91% /System/Volumes/Data\n";
        assert_eq!(parse_df(out), Some(("/System/Volumes/Data".into(), 88733768 * 1024)));
        assert_eq!(parse_df("Filesystem\n"), None);
        let spaced = "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/x 100 50 50 50% /Volumes/My Disk\n";
        assert_eq!(parse_df(spaced).map(|(v, _)| v), Some("/Volumes/My Disk".into()));
    }

    #[test]
    fn a_disk_under_the_limit_warns_and_names_what_to_remove() {
        let ok = Disk { volume: "/v".into(), free: LOW, limit: LOW, largest: None };
        assert!(!ok.low());
        assert_eq!(ok.line(), "30 GB free on /v");

        let tight = Disk { volume: "/v".into(), free: LOW - 1, limit: LOW, largest: Some(Removable { path: "/r/.worktrees/wsp-1/target".into(), bytes: 12 << 30 }) };
        assert!(tight.low());
        let line = tight.line();
        assert!(line.contains("disk is low") && line.contains("/r/.worktrees/wsp-1/target") && line.contains("12 GB"), "{line}");

        let nothing = Disk { volume: "/v".into(), free: 1 << 30, limit: LOW, largest: None };
        assert!(nothing.line().contains("no removable build output measured"), "{}", nothing.line());
    }

    fn fill(dir: &Path, kb: usize) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("big"), vec![1u8; kb * 1024]).unwrap();
    }

    /// The largest `target/` among the trees is the one named, and a second
    /// reading within the record's life does not measure again.
    #[test]
    fn the_largest_removable_target_is_found_and_recorded() {
        let env = util::isolated("disk-largest");
        let (small, big) = (env.path("a/target"), env.path("b/target"));
        fill(&small, 64);
        fill(&big, 512);
        let volume = free_of(&small).unwrap().0;
        let found = largest(&[small.clone(), big.clone()], &volume).unwrap();
        assert_eq!(found.path, big.display().to_string());
        assert!(found.bytes >= 512 * 1024, "{}", found.bytes);

        let state = env.path("state");
        std::fs::create_dir_all(&state).unwrap();
        record(&state, &volume, &found);
        assert_eq!(recorded(&state, &volume).map(|r| r.path), Some(big.display().to_string()));
        std::fs::remove_dir_all(&big).unwrap();
        assert!(recorded(&state, &volume).is_none(), "a record named a directory that is gone");
    }

    /// A store whose one project is rooted at a scratch repository holding two
    /// trees with a `target/` each: under the limit the larger is named, and
    /// over it nothing is measured. The limit is the test's, so the real free
    /// space of the machine running it does not matter.
    #[test]
    fn a_low_volume_names_the_largest_target_under_the_project_roots() {
        let env = util::isolated("disk-read");
        let repo = env.path("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        fill(&repo.join(".worktrees/wsp-1/target"), 64);
        fill(&repo.join(".worktrees/wsp-2/target"), 512);
        let store = Store::open();
        let mut p = crate::model::Project::new("p");
        p.roots = vec![repo.display().to_string()];
        store.save_project(&p).unwrap();

        let roomy = read(&store, 0, true);
        assert!(roomy.iter().all(|d| !d.low() && d.largest.is_none()));
        assert!(!roomy[0].line().contains("low"), "{}", roomy[0].line());

        assert!(read(&store, u64::MAX, false).iter().all(|d| d.low() && d.largest.is_none()), "measured when told not to");
        let tight = read(&store, u64::MAX, true);
        let largest = tight[0].largest.as_ref().expect("nothing named");
        assert!(largest.path.ends_with(".worktrees/wsp-2/target"), "{}", largest.path);
        assert!(tight[0].line().contains("disk is low") && tight[0].line().contains("wsp-2/target"), "{}", tight[0].line());
        assert!(read(&store, u64::MAX, false)[0].largest.is_some(), "the measurement was not recorded for the next reader");

    }
}
