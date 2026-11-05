//! A row's scratch space, with an owner: `wsp-373`.
//!
//! Experiment rows (compounding-024 to 088) made one build directory per run
//! and arm with `cp -cR` under `~/.cache/<row>`, outside their worktree, and
//! nothing removed them when the row closed: `~/.cache` read 545 GB on
//! 2026-11-05 and the disk was full. No tree owned them, so no sweep saw them.
//!
//! **An owner.** Every seat's environment carries `$WSP_SCRATCH`, a directory
//! under the state directory keyed by the row ([`path`]), and the orders say to
//! put build dirs and copies that live outside the tree there. When the row is
//! closed ([`closed`]: `done`, or at `review` with every list holding it
//! finished) the tick removes it ([`tidy`]), keeping what the row cites or
//! declares ([`keeps`]).
//!
//! **A sweep** for what predates this: `wsp scratch [-n]` finds
//! `~/.cache/<row-id>` directories whose row is closed, lists them with sizes,
//! and removes them on the same rule. `du` overstates `cp -cR` clones many
//! times over, so a removal reports what the volume really gained (free space
//! before and after) and a listing says its sizes are an upper bound.
//!
//! **What is kept.** Logs, packets and results are small; the cost is build
//! output. A row keeps a file or directory by naming it, one path per line, in
//! `.keep` at the top of its scratch (relative to it, or absolute), or by citing
//! its path in the row's own text. Whatever is kept keeps its parents; the rest
//! goes.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use crate::model::WorklistStatus;
use crate::store::Store;
use crate::util;

/// The declared keep list, at the top of a scratch directory.
pub(crate) const KEEP_FILE: &str = ".keep";

/// Directories never walked looking for something cited: a build tree is the
/// cost, not the record.
const BUILD: &[&str] = &["target", "node_modules"];

/// `$WSP_SCRATCH` for `row`.
pub(crate) fn path(store: &Store, row: &str) -> PathBuf {
    store.state.join("scratch").join(row)
}

/// Whether a row's work is over: `done`, or at `review` with every list holding
/// it finished. A row at review in no list, or in a list still running, may be
/// sent back and keeps its scratch.
pub(crate) fn closed(store: &Store, row: &str) -> bool {
    let Some(task) = store.task(row) else { return false };
    match task.status() {
        crate::model::Status::Done => true,
        crate::model::Status::Review => {
            let lists: Vec<_> = store
                .worklists()
                .into_iter()
                .filter(|w| w.groups().iter().any(|g| g.members.iter().any(|m| m == row)))
                .collect();
            !lists.is_empty() && lists.iter().all(|w| w.status() == WorklistStatus::Done)
        }
        _ => false,
    }
}

/// Paths under `dir`, to a depth that finds a cited result and no further,
/// not entering build trees.
fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        out.push(p.clone());
        let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
        let build = p.file_name().and_then(|n| n.to_str()).is_some_and(|n| BUILD.contains(&n));
        if is_dir && !build && depth > 1 && out.len() < 20_000 {
            walk(&p, depth - 1, out);
        }
    }
}

/// What in `dir` stays: every path named in its `.keep`, and every path the
/// row's text cites (in full, or contracted with `~`).
pub(crate) fn keeps(dir: &Path, text: &str) -> Vec<PathBuf> {
    let mut keep: Vec<PathBuf> = std::fs::read_to_string(dir.join(KEEP_FILE))
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| match l.starts_with('/') || l.starts_with('~') {
            true => util::expand(l),
            false => dir.join(l),
        })
        .filter(|p| p.starts_with(dir) && !p.components().any(|c| c.as_os_str() == ".."))
        .collect();
    keep.push(dir.join(KEEP_FILE));
    let mut all = Vec::new();
    walk(dir, 4, &mut all);
    for p in all {
        if text.contains(&p.display().to_string()) || text.contains(&util::contract(&p)) {
            keep.push(p);
        }
    }
    keep
}

/// Remove everything under `dir` that is not in `keep` and not above something
/// that is. Returns whether `dir` itself now holds nothing.
fn prune(dir: &Path, keep: &[PathBuf]) -> bool {
    let mut empty = true;
    let Ok(entries) = std::fs::read_dir(dir) else { return false };
    for e in entries.flatten() {
        let p = e.path();
        if keep.iter().any(|k| p.starts_with(k)) {
            empty = false;
        } else if keep.iter().any(|k| k.starts_with(&p)) {
            let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
            if !is_dir || !prune(&p, keep) {
                empty = false;
            } else {
                let _ = std::fs::remove_dir(&p);
            }
        } else if e.file_type().is_ok_and(|t| t.is_dir()) {
            if std::fs::remove_dir_all(&p).is_err() {
                empty = false;
            }
        } else if std::fs::remove_file(&p).is_err() {
            empty = false;
        }
    }
    empty
}

/// One scratch directory of a closed row.
pub(crate) struct Found {
    pub row: String,
    pub dir: PathBuf,
    /// `du`'s figure: an upper bound on a clone-built tree.
    pub bytes: u64,
    /// Paths that stay.
    pub kept: Vec<PathBuf>,
}

fn du(path: &Path) -> u64 {
    Command::new("du")
        .args(["-sk"])
        .arg(path)
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).split_whitespace().next()?.parse::<u64>().ok())
        .map_or(0, |kb| kb * 1024)
}

/// The cache directory experiment rows have used: `~/.cache/<row-id>`.
fn cache_dir() -> PathBuf {
    util::home().join(".cache")
}

/// Every scratch directory of a closed row: `$WSP_SCRATCH` and any
/// `~/.cache/<row-id>`. Unmeasured: `sized` does that.
pub(crate) fn find(store: &Store) -> Vec<Found> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let owned = std::fs::read_dir(store.state.join("scratch")).into_iter().flatten().flatten().map(|e| e.path());
    let cached = std::fs::read_dir(cache_dir()).into_iter().flatten().flatten().map(|e| e.path());
    for dir in owned.chain(cached) {
        let Some(name) = dir.file_name().and_then(|n| n.to_str()).map(str::to_string) else { continue };
        let row = crate::leg_tree::row_of(&name).to_string();
        if !dir.is_dir() || !seen.insert(dir.clone()) || !closed(store, &row) {
            continue;
        }
        let text = store.task(&row).map(|t| format!("{}\n{}", t.title, t.body)).unwrap_or_default();
        let kept = keeps(&dir, &text);
        out.push(Found { row, dir, bytes: 0, kept });
    }
    out.sort_by(|a, b| a.dir.cmp(&b.dir));
    out
}

/// [`find`], each measured.
pub(crate) fn sized(store: &Store) -> Vec<Found> {
    let mut found = find(store);
    for f in &mut found {
        f.bytes = du(&f.dir);
    }
    found
}

/// Take what is not kept out of `f`; the directory goes with it when nothing
/// is kept.
pub(crate) fn remove(f: &Found) {
    if prune(&f.dir, &f.kept) {
        let _ = std::fs::remove_dir(&f.dir);
    }
}

static SAID: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// The tick's half: the scratch the state directory owns, for rows that are
/// closed. The `~/.cache` sweep is a verb, because a directory there was not
/// made by wsp and is the person's to look at first.
///
/// Not under test, for `check_tree::tidy`'s reason; the tests drive [`find`]
/// and [`remove`].
pub(crate) fn tidy(store: &Store) {
    if cfg!(test) {
        return;
    }
    let owned = store.state.join("scratch");
    for f in find(store).into_iter().filter(|f| f.dir.starts_with(&owned)) {
        let before = crate::disk::free_at(&f.dir);
        remove(&f);
        let freed = match (before, crate::disk::free_at(&f.dir.parent().unwrap_or(&owned))) {
            (Some(a), Some(b)) if b > a => format!(", {} freed", crate::disk::gb(b - a)),
            _ => String::new(),
        };
        let line = format!("removed scratch of {}: its row is closed{freed}", f.row);
        if SAID.lock().map(|mut s| s.insert(line.clone())).unwrap_or(true) {
            crate::cycle::log_line(store, &line);
        }
    }
}

/// `wsp scratch [sweep] [-n] [--json]`: list the scratch of closed rows with
/// sizes, and with no `-n` remove it, reporting the space really gained.
pub(crate) fn scratch(store: &Store, args: &crate::Args) -> i32 {
    let dry = args.has("dry-run");
    let found = sized(store);
    let volume_of = |p: &Path| crate::disk::free_at(p);
    let before = found.first().and_then(|f| volume_of(&f.dir));
    if !dry {
        for f in &found {
            remove(f);
        }
    }
    let after = found.first().and_then(|f| volume_of(f.dir.parent().unwrap_or(&f.dir)));
    let gained = match (before, after) {
        (Some(a), Some(b)) if !dry => Some(b.saturating_sub(a)),
        _ => None,
    };
    if args.json() {
        let rows: Vec<_> = found
            .iter()
            .map(|f| serde_json::json!({ "row": f.row, "path": f.dir.display().to_string(), "du_bytes": f.bytes,
                                         "kept": f.kept.iter().map(|k| k.display().to_string()).collect::<Vec<_>>() }))
            .collect();
        println!("{}", serde_json::json!({ "scratch": rows, "dry_run": dry, "freed_bytes": gained }));
        return 0;
    }
    if found.is_empty() {
        println!("no scratch of a closed row");
        return 0;
    }
    let verb = if dry { "would remove" } else { "removed" };
    for f in &found {
        println!("{verb} {}  {} by du  (row {})", util::contract(&f.dir), crate::disk::gb(f.bytes), f.row);
    }
    let total: u64 = found.iter().map(|f| f.bytes).sum();
    match gained {
        Some(g) => println!("freed {} on the volume (du counted {}; clones are counted once per copy)", crate::disk::gb(g), crate::disk::gb(total)),
        None => println!("du counts {} at most — clones are counted once per copy; the real gain shows without -n", crate::disk::gb(total)),
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Task, Worklist};

    fn put(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "x").unwrap();
    }

    #[test]
    fn a_declared_or_cited_path_stays_and_the_rest_goes() {
        let env = util::isolated("scratch-keep");
        let dir = env.path("s/row-1");
        put(&dir.join("r1/target/obj"));
        put(&dir.join("r1/results/out.json"));
        put(&dir.join("logs/run.log"));
        put(&dir.join("cited/notes.md"));
        std::fs::write(dir.join(KEEP_FILE), "logs\n../escape\n").unwrap();
        let text = format!("see {}", dir.join("cited/notes.md").display());
        let keep = keeps(&dir, &text);
        assert!(!keep.iter().any(|k| k.to_string_lossy().contains("..")), "a keep list left the directory");
        let f = Found { row: "row-1".into(), dir: dir.clone(), bytes: 0, kept: keep };
        remove(&f);
        assert!(dir.join("logs/run.log").exists() && dir.join("cited/notes.md").exists());
        assert!(!dir.join("r1").exists(), "build dirs and results nobody kept stayed");
    }

    #[test]
    fn nothing_kept_removes_the_directory() {
        let env = util::isolated("scratch-gone");
        let dir = env.path("s/row-2");
        put(&dir.join("r1/target/obj"));
        remove(&Found { row: "row-2".into(), dir: dir.clone(), bytes: 0, kept: Vec::new() });
        assert!(!dir.exists());
    }

    fn task(store: &Store, id: &str, status: &str) {
        let mut t = Task::new("t", id);
        t.status_raw = status.into();
        store.save_task(&t).unwrap();
    }

    /// Done is closed. Review is closed only when every list holding the row is
    /// finished, and a row in no list may still be sent back.
    #[test]
    fn a_row_is_closed_when_done_or_at_review_with_its_list_done() {
        let env = util::isolated("scratch-closed");
        let _ = &env;
        let store = Store::open();
        task(&store, "p-001", "done");
        task(&store, "p-002", "review");
        task(&store, "p-003", "review");
        task(&store, "p-004", "doing");
        let mut w = Worklist::new("run", "p");
        let mut g = crate::model::Group::default();
        g.members = vec!["p-002".into()];
        w.set_groups(&[g]);
        w.set_status(WorklistStatus::Done);
        store.save_worklist(&w).unwrap();
        assert!(closed(&store, "p-001"));
        assert!(closed(&store, "p-002"), "review with its list done");
        assert!(!closed(&store, "p-003"), "review in no list");
        assert!(!closed(&store, "p-004"));
        assert!(!closed(&store, "p-999"));
    }
}
