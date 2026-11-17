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
#[cfg(test)]
use wsp_base::calls::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde_json::json;

use crate::cmd_checkout::WORKTREES;
use crate::store::Store;
use crate::host::{Host, LOCAL};
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
    /// What removing it frees, clones counted once ([`size_of`]); `du`'s upper
    /// bound where the filesystem would not say (`exact` false).
    pub bytes: u64,
    pub exact: bool,
    /// [`Sized::upto`].
    pub upto: u64,
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
                    .map(|r| format!("{} {} ({})", r.kind, util::contract(Path::new(&r.path)), text(r.bytes, r.exact, r.upto)))
                    .collect();
                let how = match self.items.iter().all(|r| r.exact) {
                    true => "what removing each frees on the volume, clones counted once",
                    false => "sizes by du where marked ≤, which counts clones in full",
                };
                format!("it can be reclaimed from: {} — {how}", names.join("; "))
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
    parse_df(&LOCAL.disk_free(path)?)
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
    let settled = crate::build_output::settled_trees(store);
    for repo in crate::check_tree::repos(store) {
        for dir in children(&repo.join(WORKTREES)) {
            let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            if settled.iter().any(|r| r == crate::leg_tree::row_of(name)) && !out.iter().any(|(p, _)| *p == dir) {
                out.push((dir, "tree of a landed and verified row"));
            }
        }
    }
    out.extend(crate::cmd_sandbox::strays(store).into_iter().map(|p| (p, "sandbox tree")));
    out
}

/// What removing `path` gives back: its **private** size, the bytes no other
/// file shares an extent with. `du` counts every `cp -cR` clone in full — 545 GB
/// by `du` freed 43 GB (`wsp-373`), and a 61.6 GB copy made four times is 246 GB
/// by `du` — so a figure that names what to delete is the volume's own: APFS
/// answers it per file (`ATTR_CMNEXT_PRIVATESIZE`) and a tree is the sum. A
/// tree and its clone each read as their own changes, and the sum is therefore
/// a floor on what both going would free, never more than one going frees.
/// Where the filesystem does not answer, `du` stands in and the caller is told
/// ([`Sized::exact`]).
pub(crate) struct Sized {
    pub bytes: u64,
    /// Whether `bytes` is the volume's figure and not `du`'s upper bound.
    pub exact: bool,
    /// What it would free if every clone sharing its extents went too (`du`'s
    /// figure): four copies made with `cp -cR` each read nearly nothing alone
    /// and the whole between them. At least `bytes`.
    pub upto: u64,
}

impl Sized {
    /// `4.2 GB`, or `0 MB (up to 61 GB with the clones it shares with)`, with a
    /// `≤` where the filesystem would not say.
    pub(crate) fn show(&self) -> String {
        text(self.bytes, self.exact, self.upto)
    }
}

pub(crate) fn text(bytes: u64, exact: bool, upto: u64) -> String {
    let own = format!("{}{}", if exact { "" } else { "≤" }, gb(bytes));
    match exact && upto > bytes + bytes / 2 + (64 << 20) {
        true => format!("{own}, up to {} with the clones it shares extents with", gb(upto)),
        false => own,
    }
}

pub(crate) fn size_of(path: &Path) -> Option<Sized> {
    if let Some((bytes, upto)) = private_tree(path) {
        return Some(Sized { bytes, exact: true, upto });
    }
    let kb: u64 = LOCAL.disk_use(path)?.split_whitespace().next()?.parse().ok()?;
    Some(Sized { bytes: kb * 1024, exact: false, upto: kb * 1024 })
}

/// The private size and the allocated size of every file under `path`,
/// symlinks not followed. `None`
/// when the first file will not say, which is a filesystem without the
/// attribute.
fn private_tree(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let (mut total, mut upto) = (0u64, 0u64);
    let mut stack = vec![path.to_path_buf()];
    let mut asked = false;
    while let Some(p) = stack.pop() {
        let Ok(meta) = std::fs::symlink_metadata(&p) else { continue };
        if meta.is_dir() {
            stack.extend(std::fs::read_dir(&p).into_iter().flatten().flatten().map(|e| e.path()));
        } else if meta.is_file() {
            upto += meta.blocks() * 512;
            match private_size(&p) {
                Some(b) => {
                    asked = true;
                    total += b;
                }
                None if !asked => return None,
                None => total += meta.len(),
            }
        }
    }
    asked.then_some((total, upto)).or_else(|| std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir()).then_some((0, 0)))
}

#[cfg(target_os = "macos")]
fn private_size(path: &Path) -> Option<u64> {
    use std::os::raw::{c_char, c_int, c_void};
    use std::os::unix::ffi::OsStrExt;
    #[repr(C)]
    struct AttrList {
        bitmapcount: u16,
        reserved: u16,
        commonattr: u32,
        volattr: u32,
        dirattr: u32,
        fileattr: u32,
        forkattr: u32,
    }
    extern "C" {
        fn getattrlist(path: *const c_char, list: *mut AttrList, buf: *mut c_void, size: usize, options: u32) -> c_int;
    }
    const ATTR_BIT_MAP_COUNT: u16 = 5;
    const ATTR_CMNEXT_PRIVATESIZE: u32 = 0x0000_0008;
    const FSOPT_NOFOLLOW: u32 = 0x0000_0001;
    const FSOPT_ATTR_CMN_EXTENDED: u32 = 0x0000_0020;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut list = AttrList { bitmapcount: ATTR_BIT_MAP_COUNT, reserved: 0, commonattr: 0, volattr: 0, dirattr: 0, fileattr: 0, forkattr: ATTR_CMNEXT_PRIVATESIZE };
    let mut buf = [0u8; 16];
    // SAFETY: `c` is NUL-terminated, `list` and `buf` outlive the call, and the
    // size passed is the buffer's.
    let rc = unsafe { getattrlist(c.as_ptr(), &mut list, buf.as_mut_ptr().cast(), buf.len(), FSOPT_NOFOLLOW | FSOPT_ATTR_CMN_EXTENDED) };
    if rc != 0 {
        return None;
    }
    // A u32 length, then the off_t. Extended common attributes are asked in
    // `forkattr` when FSOPT_ATTR_CMN_EXTENDED is set.
    Some(u64::from_ne_bytes(buf[4..12].try_into().ok()?))
}

#[cfg(not(target_os = "macos"))]
fn private_size(_: &Path) -> Option<u64> {
    None
}

/// The biggest of `items` that are on the volume mounted at `volume`, at most
/// [`NAMED`], largest first.
///
/// Each is sized through [`Sizes`] under `state`: a tree whose [`stamp`] is
/// what it was at its last sizing is not walked again (`wsp-630`).
pub(crate) fn biggest(state: &Path, items: &[(PathBuf, &str)], volume: &str) -> Vec<Removable> {
    let mut sizes = Sizes::load(state);
    let mut found: Vec<Removable> = items
        .iter()
        .filter(|(p, _)| free_of(p).is_some_and(|(v, _)| v == volume))
        .filter_map(|(p, kind)| sizes.size(p).map(|s| Removable { path: p.display().to_string(), bytes: s.bytes, exact: s.exact, upto: s.upto, kind: kind.to_string() }))
        .collect();
    sizes.save(items.iter().map(|(p, _)| p.as_path()));
    found.sort_by(|a, b| b.upto.max(b.bytes).cmp(&a.upto.max(a.bytes)));
    found.truncate(NAMED);
    found
}

/// What a tree looked like when it was last sized, cheaply: the mtime of every
/// directory down to [`STAMP_DEPTH`] below it, and how many there are. A file
/// made or removed anywhere in the top `STAMP_DEPTH + 1` levels moves one —
/// every cargo build adds to `<profile>/deps`, `.fingerprint` or `incremental`
/// — and reading it costs one `lstat` per directory near the top instead of
/// one `lstat` and one `getattrlist` per file in the tree.
const STAMP_DEPTH: usize = 2;

/// How long a size that its stamp still vouches for is believed, in seconds:
/// a file grown in place deep in a tree moves no directory, so it is walked
/// again at least this often.
const SIZE_LIFE: i64 = 6 * 60 * 60;

pub(crate) fn stamp(path: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let mut stack = vec![(path.to_path_buf(), 0usize)];
    let (mut newest, mut dirs) = (i128::MIN, 0u64);
    while let Some((p, depth)) = stack.pop() {
        let Ok(meta) = std::fs::symlink_metadata(&p) else { continue };
        if !meta.is_dir() {
            continue;
        }
        dirs += 1;
        newest = newest.max(meta.mtime() as i128 * 1_000_000_000 + meta.mtime_nsec() as i128);
        if depth < STAMP_DEPTH {
            for e in std::fs::read_dir(&p).into_iter().flatten().flatten() {
                if e.file_type().is_ok_and(|t| t.is_dir()) {
                    stack.push((e.path(), depth + 1));
                }
            }
        }
    }
    (dirs > 0).then(|| format!("{newest}/{dirs}"))
}

fn sizes_path(state: &Path) -> PathBuf {
    state.join("disk-sizes.json")
}

/// The sizes of the trees [`biggest`] has measured, by path, each with the
/// [`stamp`] it was measured at, kept in `disk-sizes.json`.
pub(crate) struct Sizes {
    state: PathBuf,
    known: serde_json::Map<String, serde_json::Value>,
    /// How many trees this reading walked; the test reads it.
    pub walked: usize,
}

impl Sizes {
    pub(crate) fn load(state: &Path) -> Sizes {
        let known = std::fs::read_to_string(sizes_path(state))
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        Sizes { state: state.to_path_buf(), known, walked: 0 }
    }

    /// `path`'s size: the one recorded while its stamp is unchanged and the
    /// record younger than [`SIZE_LIFE`], else walked now and recorded.
    pub(crate) fn size(&mut self, path: &Path) -> Option<Sized> {
        let key = path.display().to_string();
        let now = stamp(path);
        if let (Some(now), Some(was)) = (&now, self.known.get(&key)) {
            let fresh = was.get("at").and_then(|a| a.as_i64()).is_some_and(|at| util::epoch_secs() - at <= SIZE_LIFE);
            if fresh && was.get("stamp").and_then(|s| s.as_str()) == Some(now.as_str()) {
                if let (Some(bytes), Some(upto)) = (was.get("bytes").and_then(|b| b.as_u64()), was.get("upto").and_then(|u| u.as_u64())) {
                    return Some(Sized { bytes, exact: was.get("exact").and_then(|e| e.as_bool()).unwrap_or(false), upto });
                }
            }
        }
        self.walked += 1;
        let sized = size_of(path)?;
        if let Some(now) = now {
            self.known.insert(key, json!({ "stamp": now, "at": util::epoch_secs(), "bytes": sized.bytes, "exact": sized.exact, "upto": sized.upto }));
        }
        Some(sized)
    }

    /// Written back, keeping only the trees still among `live`.
    pub(crate) fn save<'a>(mut self, live: impl Iterator<Item = &'a Path>) {
        let live: BTreeSet<String> = live.map(|p| p.display().to_string()).collect();
        self.known.retain(|k, _| live.contains(k));
        let _ = std::fs::write(sizes_path(&self.state), serde_json::Value::Object(self.known).to_string());
    }
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
                    Some(Removable { path, bytes: i.get("bytes")?.as_u64()?, exact: i.get("exact").and_then(|e| e.as_bool()).unwrap_or(false), upto: i.get("upto").and_then(|e| e.as_u64()).unwrap_or(0), kind: i.get("kind")?.as_str()?.to_string() })
                })
                .collect(),
        )
    };
    read().unwrap_or_default()
}

/// When `volume`'s reclaimable items were last measured, found or not.
fn measured_at(state: &Path, volume: &str) -> Option<i64> {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(record_path(state)).ok()?).ok()?;
    v.get(volume)?.get("at")?.as_i64()
}

/// Whether the daemon should measure `volume` again: never measured, or the
/// last measurement older than [`FRESH`]. A measurement that found nothing
/// counts — re-walking every tick because nothing was found is what kept the
/// daemon at 140 % (`wsp-630`).
fn due(state: &Path, volume: &str) -> bool {
    measured_at(state, volume).is_none_or(|at| util::epoch_secs() - at > FRESH)
}

fn record(state: &Path, volume: &str, found: &[Removable]) {
    let mut all: serde_json::Value = std::fs::read_to_string(record_path(state))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| json!({}));
    let items: Vec<_> = found.iter().map(|f| json!({ "path": f.path, "bytes": f.bytes, "exact": f.exact, "upto": f.upto, "kind": f.kind })).collect();
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
                    disk.items = biggest(&store.state, &reclaimable(store), &disk.volume);
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
    let disk = Disk { volume: volume.into(), free, limit: HOLD, items: items.iter().map(|r| Removable { path: r.path.clone(), bytes: r.bytes, exact: r.exact, upto: r.upto, kind: r.kind.clone() }).collect() };
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
        if due(&store.state, &disk.volume) && !MEASURING.swap(true, Ordering::SeqCst) {
            let (paths, volume, state) = (reclaimable(store), disk.volume.clone(), store.state.clone());
            std::thread::spawn(move || {
                let found = biggest(&state, &paths, &volume);
                // Recorded even when empty, so the next is due a FRESH later
                // and not on the next tick.
                record(&state, &volume, &found);
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

        let item = |p: &str, gib: u64, kind: &str| Removable { path: p.into(), bytes: gib << 30, exact: true, upto: gib << 30, kind: kind.into() };
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
        let why = hold_reason("/v", HOLD - 1, &[Removable { path: "/s/scratch/c-1".into(), bytes: 9 << 30, exact: true, upto: 9 << 30, kind: "scratch of a closed row".into() }]).unwrap();
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
        let sizes = env.path("sizes");
        std::fs::create_dir_all(&sizes).unwrap();
        let found = biggest(&sizes, &[(small.clone(), "build output"), (big.clone(), "build output"), (mid.clone(), "scratch of a closed row")], &volume);
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

    /// A tree sized once is not walked again while nothing in it has moved;
    /// a build writing into it, or a record gone stale, walks it again
    /// (`wsp-630`).
    #[test]
    fn an_unchanged_tree_is_not_walked_again() {
        let env = util::isolated("disk-sizes");
        let (tree, state) = (env.path("t/target"), env.path("state"));
        fill(&tree.join("debug/deps"), 64);
        std::fs::create_dir_all(&state).unwrap();

        let mut first = Sizes::load(&state);
        let a = first.size(&tree).unwrap();
        assert_eq!(first.walked, 1);
        first.save([tree.as_path()].into_iter());

        let mut again = Sizes::load(&state);
        let b = again.size(&tree).unwrap();
        assert_eq!(again.walked, 0, "an unchanged tree was walked again");
        assert_eq!((a.bytes, a.upto, a.exact), (b.bytes, b.upto, b.exact));
        again.save([tree.as_path()].into_iter());

        // A build adds a file two levels down: its directory's mtime moves.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(tree.join("debug/deps/libx.rlib"), vec![2u8; 128 * 1024]).unwrap();
        let mut built = Sizes::load(&state);
        let c = built.size(&tree).unwrap();
        assert_eq!(built.walked, 1, "a tree a build wrote into was not walked");
        assert!(c.upto > b.upto, "{} {}", c.upto, b.upto);
        built.save(std::iter::empty());
        assert_eq!(Sizes::load(&state).known.len(), 0, "a tree no longer reclaimable was kept");
    }

    /// A measurement that found nothing still makes the next one wait.
    #[test]
    fn a_measurement_that_found_nothing_is_not_due_again_at_once() {
        let env = util::isolated("disk-due");
        let state = env.path("state");
        std::fs::create_dir_all(&state).unwrap();
        assert!(due(&state, "/v"));
        record(&state, "/v", &[]);
        assert!(!due(&state, "/v"), "an empty measurement was due again on the next tick");
    }

    /// `du` reads a `cp -cR` clone in full; the volume gives back only what
    /// nothing else shares. A copy of a tree made with clones is near nothing.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_clone_of_a_tree_is_sized_by_what_removing_it_frees() {
        let env = util::isolated("disk-clone");
        let (orig, copy) = (env.path("orig"), env.path("copy"));
        fill(&orig, 4096);
        let out = Command::new("cp").args(["-cR"]).arg(&orig).arg(&copy).output().unwrap();
        assert!(out.status.success());
        let (a, b) = (size_of(&orig).unwrap(), size_of(&copy).unwrap());
        assert!(a.exact && b.exact);
        assert!(a.upto >= 4096 * 1024 && b.upto >= 4096 * 1024, "{} {}", a.upto, b.upto);
        // Each still shares its extent with the other, so neither reads as the 4 MB `du` says.
        assert!(a.bytes < 1024 * 1024 && b.bytes < 1024 * 1024, "{} {}", a.bytes, b.bytes);

        // A file written afresh is private.
        let fresh = env.path("fresh");
        fill(&fresh, 2048);
        std::fs::write(fresh.join("big"), (0..2048 * 1024).map(|i| (i % 251) as u8).collect::<Vec<_>>()).unwrap();
        assert!(size_of(&fresh).unwrap().bytes >= 2048 * 1024);
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
