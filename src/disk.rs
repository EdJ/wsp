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

/// Under this the status surfaces say so, and name what could be reclaimed.
pub(crate) const LOW: u64 = 50 * 1024 * 1024 * 1024;

/// Under this nothing new that builds is started (`wsp-373`): a build that
/// fails halfway for want of room costs more than the wait.
pub(crate) const HOLD: u64 = 20 * 1024 * 1024 * 1024;

/// How many reclaimable items a low volume names.
const NAMED: usize = 3;

/// How long a measured largest build output is believed, in seconds.
const FRESH: i64 = 10 * 60;

pub(crate) struct Removable {
    pub path: String,
    /// `du`'s figure, which counts every `cp -cR` clone in full: an upper bound.
    pub bytes: u64,
    /// What it is: `build output`, `scratch of a closed row`, …
    pub kind: String,
}

pub(crate) struct Disk {
    pub volume: String,
    pub free: u64,
    pub limit: u64,
    /// The biggest things that could go, largest first. Only measured on a low
    /// volume.
    pub items: Vec<Removable>,
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
        let remove = match self.items.is_empty() {
            false => {
                let names: Vec<String> = self
                    .items
                    .iter()
                    .map(|r| format!("{} {} ({})", r.kind, util::contract(Path::new(&r.path)), gb(r.bytes)))
                    .collect();
                format!("it can be reclaimed from: {} — sizes by du, which counts clones in full", names.join("; "))
            }
            true => "no reclaimable item measured yet — `wsp doctor` measures them".into(),
        };
        let hold = match self.free < HOLD {
            true => format!(" · under {} nothing new that builds is started", gb(HOLD)),
            false => String::new(),
        };
        format!("disk is low: {free}, under {} — {remove}{hold}", gb(self.limit))
    }
}

pub(crate) fn gb(bytes: u64) -> String {
    match bytes {
        b if b >= 10 * 1024 * 1024 * 1024 => format!("{} GB", b / (1024 * 1024 * 1024)),
        b if b >= 1024 * 1024 * 1024 => format!("{:.1} GB", b as f64 / (1024.0 * 1024.0 * 1024.0)),
        b => format!("{} MB", b / (1024 * 1024)),
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

/// Free bytes on the volume holding `path`.
pub(crate) fn free_at(path: &Path) -> Option<u64> {
    free_of(path).map(|(_, f)| f)
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

/// Everything wsp knows could be given back, with what it is: the build output
/// of [`candidates`], the scratch of closed rows, the trees of finished
/// barrier checks and of closed rows, and the trees a sandbox made in a real
/// repository.
pub(crate) fn reclaimable(store: &Store) -> Vec<(PathBuf, &'static str)> {
    let mut out: Vec<(PathBuf, &'static str)> = candidates(store).into_iter().map(|p| (p, "build output")).collect();
    out.extend(crate::scratch::find(store).into_iter().map(|f| (f.dir, "scratch of a closed row")));
    for repo in crate::check_tree::repos(store) {
        for dir in children(&repo.join(WORKTREES)) {
            let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
            match crate::check_tree::named(store, &name) {
                Some((list, at)) if !crate::check_tree::in_use(store, &list, at) => out.push((dir, "finished check tree")),
                Some(_) => {}
                None if store.task(&name).is_some_and(|t| t.status() == crate::model::Status::Done) => out.push((dir, "tree of a done row")),
                None => {}
            }
        }
    }
    out.extend(crate::cmd_sandbox::strays(store).into_iter().map(|p| (p, "sandbox tree")));
    out
}

fn size_of(path: &Path) -> Option<u64> {
    let out = Command::new("du").args(["-sk"]).arg(path).output().ok()?;
    let kb: u64 = String::from_utf8_lossy(&out.stdout).split_whitespace().next()?.parse().ok()?;
    Some(kb * 1024)
}

/// The biggest of `items` that are on the volume mounted at `volume`, at most
/// [`NAMED`], largest first.
pub(crate) fn biggest(items: &[(PathBuf, &str)], volume: &str) -> Vec<Removable> {
    let mut found: Vec<Removable> = items
        .iter()
        .filter(|(p, _)| free_of(p).is_some_and(|(v, _)| v == volume))
        .filter_map(|(p, kind)| size_of(p).map(|bytes| Removable { path: p.display().to_string(), bytes, kind: kind.to_string() }))
        .collect();
    found.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    found.truncate(NAMED);
    found
}

fn record_path(state: &Path) -> PathBuf {
    state.join("disk.json")
}

fn recorded(state: &Path, volume: &str) -> Vec<Removable> {
    let read = || -> Option<Vec<Removable>> {
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(record_path(state)).ok()?).ok()?;
        let row = v.get(volume)?;
        if util::epoch_secs() - row.get("at")?.as_i64()? > FRESH {
            return None;
        }
        Some(
            row.get("items")?
                .as_array()?
                .iter()
                .filter_map(|i| {
                    let path = i.get("path")?.as_str()?.to_string();
                    // The directory may have been removed since it was measured; a
                    // record naming one is a pointer to nothing.
                    Path::new(&path).is_dir().then_some(())?;
                    Some(Removable { path, bytes: i.get("bytes")?.as_u64()?, kind: i.get("kind")?.as_str()?.to_string() })
                })
                .collect(),
        )
    };
    read().unwrap_or_default()
}

fn record(state: &Path, volume: &str, found: &[Removable]) {
    let mut all: serde_json::Value = std::fs::read_to_string(record_path(state))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| json!({}));
    let items: Vec<_> = found.iter().map(|f| json!({ "path": f.path, "bytes": f.bytes, "kind": f.kind })).collect();
    all[volume] = json!({ "at": util::epoch_secs(), "items": items });
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

/// One reading of every volume against `limit`. On a low one, the biggest
/// reclaimable items from the record, or, with `measure`, measured now when
/// there are none.
pub(crate) fn read(store: &Store, limit: u64, measure: bool) -> Vec<Disk> {
    volumes(store)
        .into_iter()
        .map(|(volume, free)| {
            let mut disk = Disk { volume, free, limit, items: Vec::new() };
            if disk.low() {
                disk.items = recorded(&store.state, &disk.volume);
                if disk.items.is_empty() && measure {
                    disk.items = biggest(&reclaimable(store), &disk.volume);
                    record(&store.state, &disk.volume, &disk.items);
                }
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

/// Why nothing that builds may start for work at `path`, or `None` while there
/// is room (`wsp-373`). A plain sentence: it is printed by a refused spawn and
/// logged by a barrier run that waits.
///
/// Not under test: the machine running the tests may itself be short of room.
pub(crate) fn hold(store: &Store, path: &Path) -> Option<String> {
    if cfg!(test) {
        return None;
    }
    let (volume, free) = free_of(path)?;
    hold_reason(&volume, free, &recorded(&store.state, &volume))
}

fn hold_reason(volume: &str, free: u64, items: &[Removable]) -> Option<String> {
    if free >= HOLD {
        return None;
    }
    let disk = Disk { volume: volume.into(), free, limit: HOLD, items: items.iter().map(|r| Removable { path: r.path.clone(), bytes: r.bytes, kind: r.kind.clone() }).collect() };
    Some(format!("{} — a build would fail partway; free space first (`wsp scratch -n`, `wsp checkout --sweep -n`, `wsp sandbox rm --all -n`) and it goes ahead", disk.line()))
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
        if disk.items.is_empty() && !MEASURING.swap(true, Ordering::SeqCst) {
            let (paths, volume, state) = (reclaimable(store), disk.volume.clone(), store.state.clone());
            std::thread::spawn(move || {
                let found = biggest(&paths, &volume);
                if !found.is_empty() {
                    record(&state, &volume, &found);
                }
                MEASURING.store(false, Ordering::SeqCst);
            });
        }
        let key = format!("{} {}", disk.volume, disk.items.iter().map(|r| r.path.as_str()).collect::<Vec<_>>().join(","));
        if !disk.items.is_empty() && SAID.lock().map(|mut s| s.insert(key)).unwrap_or(true) {
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
    fn a_disk_under_the_limit_warns_and_names_what_to_reclaim() {
        let ok = Disk { volume: "/v".into(), free: LOW, limit: LOW, items: Vec::new() };
        assert!(!ok.low());
        assert_eq!(ok.line(), "50 GB free on /v");

        let item = |p: &str, gib: u64, kind: &str| Removable { path: p.into(), bytes: gib << 30, kind: kind.into() };
        let tight = Disk {
            volume: "/v".into(),
            free: LOW - 1,
            limit: LOW,
            items: vec![item("/r/.worktrees/wsp-1/target", 12, "build output"), item("/s/scratch/c-1", 7, "scratch of a closed row")],
        };
        assert!(tight.low());
        let line = tight.line();
        assert!(line.contains("disk is low") && line.contains("/r/.worktrees/wsp-1/target") && line.contains("12 GB"), "{line}");
        assert!(line.contains("scratch of a closed row") && line.contains("7.0 GB") && line.contains("clones"), "{line}");
        assert!(!line.contains("nothing new that builds"), "above the hold line: {line}");

        let nothing = Disk { volume: "/v".into(), free: 1 << 30, limit: LOW, items: Vec::new() };
        assert!(nothing.line().contains("no reclaimable item measured") && nothing.line().contains("nothing new that builds"), "{}", nothing.line());
    }

    /// Over 20 GB nothing is held; under it the reason is plain and names what
    /// can go.
    #[test]
    fn under_twenty_gb_the_hold_says_why_and_over_it_does_not() {
        assert!(hold_reason("/v", HOLD, &[]).is_none());
        let why = hold_reason("/v", HOLD - 1, &[Removable { path: "/s/scratch/c-1".into(), bytes: 9 << 30, kind: "scratch of a closed row".into() }]).unwrap();
        assert!(why.contains("free on /v") && why.contains("scratch of a closed row") && why.contains("wsp scratch"), "{why}");
    }

    fn fill(dir: &Path, kb: usize) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("big"), vec![1u8; kb * 1024]).unwrap();
    }

    /// The biggest items come back largest first, whatever their kind, and a
    /// second reading within the record's life does not measure again.
    #[test]
    fn the_biggest_reclaimable_items_are_found_and_recorded() {
        let env = util::isolated("disk-largest");
        let (small, big, mid) = (env.path("a/target"), env.path("b/target"), env.path("c"));
        fill(&small, 64);
        fill(&big, 512);
        fill(&mid, 256);
        let volume = free_of(&small).unwrap().0;
        let found = biggest(&[(small.clone(), "build output"), (big.clone(), "build output"), (mid.clone(), "scratch of a closed row")], &volume);
        assert_eq!(found.iter().map(|r| r.path.clone()).collect::<Vec<_>>(), [big.clone(), mid.clone(), small.clone()].map(|p| p.display().to_string()));
        assert_eq!(found[1].kind, "scratch of a closed row");
        assert!(found[0].bytes >= 512 * 1024, "{}", found[0].bytes);

        let state = env.path("state");
        std::fs::create_dir_all(&state).unwrap();
        record(&state, &volume, &found);
        assert_eq!(recorded(&state, &volume).len(), 3);
        std::fs::remove_dir_all(&big).unwrap();
        let after = recorded(&state, &volume);
        assert_eq!(after.len(), 2, "a record named a directory that is gone");
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
        assert!(roomy.iter().all(|d| !d.low() && d.items.is_empty()));
        assert!(!roomy[0].line().contains("low"), "{}", roomy[0].line());

        assert!(read(&store, u64::MAX, false).iter().all(|d| d.low() && d.items.is_empty()), "measured when told not to");
        let tight = read(&store, u64::MAX, true);
        let largest = tight[0].items.first().expect("nothing named");
        assert!(largest.path.ends_with(".worktrees/wsp-2/target"), "{}", largest.path);
        assert!(tight[0].line().contains("disk is low") && tight[0].line().contains("wsp-2/target"), "{}", tight[0].line());
        assert!(!read(&store, u64::MAX, false)[0].items.is_empty(), "the measurement was not recorded for the next reader");

    }
}
