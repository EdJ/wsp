//! Durable store (`~/wsp`, git) and ephemeral state (`~/.local/state/wsp`).
//!
//! Every mutation goes through here so that ID allocation, atomic writes and
//! git commits stay in one place — that is the whole reason agents are told to
//! use the CLI rather than editing files.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::fm;
use crate::model::{Machine, Project, Task};
use crate::util;

thread_local! {
    /// How deep we are in nested `locked` calls, so the lock can be taken once
    /// around a whole claim and again by each file it touches.
    static DEPTH: Cell<usize> = const { Cell::new(0) };
}

pub struct Store {
    pub root: PathBuf,
    pub state: PathBuf,
    /// Files this process has written to the store, waiting for the commit
    /// that names them. See `git_commit`.
    written: RefCell<BTreeSet<PathBuf>>,
}

/// What the daemon last saw of one machine. The ephemeral half of a
/// [`Machine`]; see `Store::machines_live` for why the halves are apart.
///
/// A struct rather than a bare `Value` because four different things read this
/// — the tunnel supervisor that writes it, `herdr::call` picking a socket, the
/// panel, and the reap guard that must not mistake unreachable for empty — and
/// a field name mistyped in one of them would read as `false` rather than as
/// an error.
#[derive(Debug, Clone, Default)]
pub struct MachineLive {
    /// Answering *now*, as of `last_seen`. The third state — "unreachable" as
    /// distinct from "answering with nothing" — is this being false, and it is
    /// the distinction t-260816-038 exists to protect.
    pub reachable: bool,
    /// When it last answered. Kept across a drop, so an offline row can say how
    /// long it has been offline rather than just that it is.
    pub last_seen: String,
    /// `up` | `down` | `starting` | `retrying`. The ssh connection, which can
    /// be down while the machine itself is fine and is worth telling apart when
    /// you are looking at why nothing is arriving.
    pub tunnel: String,
    /// The forwarded socket, as `Store::machine_socket` named it. Recorded
    /// rather than recomputed so a reader is talking to the socket the daemon
    /// actually made, not the one it would have made.
    pub socket: String,
    /// Whatever the far herdr says it is. A protocol that has moved under us is
    /// worth seeing in a list before it is worth handling.
    pub herdr_version: String,
    /// Why it is not reachable, in the words of whatever failed. Empty when it
    /// is. This is the whole diagnostic surface for "why is mb2 grey", so it
    /// keeps the message rather than a code.
    pub error: String,
}

impl MachineLive {
    pub fn from_value(v: &Value) -> MachineLive {
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        MachineLive {
            reachable: v.get("reachable").and_then(|x| x.as_bool()).unwrap_or(false),
            last_seen: s("last_seen"),
            tunnel: s("tunnel"),
            socket: s("socket"),
            herdr_version: s("herdr_version"),
            error: s("error"),
        }
    }

    pub fn to_value(&self) -> Value {
        json!({
            "reachable": self.reachable,
            "last_seen": self.last_seen,
            "tunnel": self.tunnel,
            "socket": self.socket,
            "herdr_version": self.herdr_version,
            "error": self.error,
        })
    }
}

impl Store {
    pub fn open() -> Store {
        let root = match std::env::var_os("WSP_HOME") {
            Some(v) => PathBuf::from(v),
            None => util::home().join("wsp"),
        };
        let state = match std::env::var_os("WSP_STATE") {
            Some(v) => PathBuf::from(v),
            None => util::home().join(".local/state/wsp"),
        };
        Store::at(root, state)
    }

    pub fn at(root: PathBuf, state: PathBuf) -> Store {
        Store { root, state, written: RefCell::new(BTreeSet::new()) }
    }

    pub fn projects_dir(&self) -> PathBuf {
        self.root.join("projects")
    }
    pub fn tasks_dir(&self) -> PathBuf {
        self.root.join("tasks")
    }
    pub fn archive_dir(&self) -> PathBuf {
        self.root.join("archive/tasks")
    }

    pub fn exists(&self) -> bool {
        self.projects_dir().is_dir() || self.tasks_dir().is_dir()
    }

    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        fs::create_dir_all(self.projects_dir())?;
        fs::create_dir_all(self.tasks_dir())?;
        fs::create_dir_all(&self.state)?;
        Ok(())
    }

    // ---- projects -------------------------------------------------------

    pub fn projects(&self) -> Vec<Project> {
        let mut out = Vec::new();
        let Ok(entries) = fs::read_dir(self.projects_dir()) else {
            return out;
        };
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("md") {
                continue;
            }
            let stem = path.file_stem().and_then(|x| x.to_str()).unwrap_or("").to_string();
            if let Ok(text) = fs::read_to_string(&path) {
                out.push(Project::from_doc(&fm::parse(&text), &stem));
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    pub fn project(&self, id: &str) -> Option<Project> {
        let path = self.projects_dir().join(format!("{id}.md"));
        let text = fs::read_to_string(path).ok()?;
        Some(Project::from_doc(&fm::parse(&text), id))
    }

    pub fn save_project(&self, p: &Project) -> std::io::Result<()> {
        fs::create_dir_all(self.projects_dir())?;
        let path = self.projects_dir().join(format!("{}.md", p.id));
        write_atomic(&path, &p.render())?;
        self.wrote(path);
        Ok(())
    }

    // ---- machines ---------------------------------------------------------
    //
    // Which machines exist is durable and committed, one file each as a
    // project is. How they are *doing* is not: see `machines_live` further
    // down, and the note there on why the two halves are deliberately apart.

    pub fn machines_dir(&self) -> PathBuf {
        self.root.join("machines")
    }

    pub fn machine_path(&self, name: &str) -> PathBuf {
        self.machines_dir().join(format!("{name}.md"))
    }

    /// Every machine in the store, retired ones included, by name.
    ///
    /// Retired ones are here rather than filtered out because the only caller
    /// that wants them gone — the daemon, which will not dial a retired
    /// machine — can say so, and every caller that draws a list wants them:
    /// that is the whole reason retiring is not deleting.
    pub fn machines(&self) -> Vec<Machine> {
        let mut out = Vec::new();
        let Ok(entries) = fs::read_dir(self.machines_dir()) else {
            return out;
        };
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("md") {
                continue;
            }
            let stem = path.file_stem().and_then(|x| x.to_str()).unwrap_or("").to_string();
            if let Ok(text) = fs::read_to_string(&path) {
                out.push(Machine::from_doc(&fm::parse(&text), &stem));
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    pub fn machine(&self, name: &str) -> Option<Machine> {
        let text = fs::read_to_string(self.machine_path(name)).ok()?;
        Some(Machine::from_doc(&fm::parse(&text), name))
    }

    pub fn save_machine(&self, m: &Machine) -> std::io::Result<()> {
        fs::create_dir_all(self.machines_dir())?;
        let path = self.machine_path(&m.name);
        write_atomic(&path, &m.render())?;
        self.wrote(path);
        Ok(())
    }

    // ---- tasks ----------------------------------------------------------

    pub fn tasks(&self) -> Vec<Task> {
        let mut out = Vec::new();
        let Ok(entries) = fs::read_dir(self.tasks_dir()) else {
            return out;
        };
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("md") {
                continue;
            }
            let stem = path.file_stem().and_then(|x| x.to_str()).unwrap_or("").to_string();
            if let Ok(text) = fs::read_to_string(&path) {
                out.push(Task::from_doc(&fm::parse(&text), &stem));
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    pub fn task(&self, id: &str) -> Option<Task> {
        let text = fs::read_to_string(self.task_path(id)).ok()?;
        Some(Task::from_doc(&fm::parse(&text), id))
    }

    pub fn task_path(&self, id: &str) -> PathBuf {
        self.tasks_dir().join(format!("{id}.md"))
    }

    /// Resolve a user-typed id: exact, then bare suffix (`003`), then unique
    /// case-insensitive title substring.
    pub fn find_task(&self, needle: &str) -> Option<Task> {
        if let Some(t) = self.task(needle) {
            return Some(t);
        }
        let all = self.tasks();
        let open: Vec<&Task> = all.iter().filter(|t| t.status().is_open()).collect();

        let by_suffix: Vec<&&Task> = open.iter().filter(|t| t.id.ends_with(needle)).collect();
        if by_suffix.len() == 1 {
            return Some((*by_suffix[0]).clone());
        }

        let lower = needle.to_ascii_lowercase();
        let by_title: Vec<&&Task> = open
            .iter()
            .filter(|t| t.title.to_ascii_lowercase().contains(&lower))
            .collect();
        if by_title.len() == 1 {
            return Some((*by_title[0]).clone());
        }
        None
    }

    pub fn save_task(&self, t: &Task) -> std::io::Result<()> {
        fs::create_dir_all(self.tasks_dir())?;
        let path = self.task_path(&t.id);
        write_atomic(&path, &t.render())?;
        self.wrote(path);
        Ok(())
    }

    /// The highest number ever handed out under a day's prefix, live *or*
    /// archived.
    ///
    /// The id space is everything that has ever existed, and `tasks/` is only
    /// the part of it still open. Archiving moves a file out, which frees its
    /// name — so probing for the first free *filename* handed the next task
    /// the id of one that had just been retired. Two tasks answered to one
    /// name, and every record keyed on it — the log, the claim, the ghost, a
    /// `parent` pointing at it — silently described both.
    fn highest_seq(&self, prefix: &str) -> usize {
        let mut top = 0;
        let mut scan = |dir: PathBuf| {
            let Ok(entries) = fs::read_dir(dir) else { return };
            for e in entries.flatten() {
                let name = e.file_name();
                let Some(stem) = Path::new(&name).file_stem().and_then(|x| x.to_str()) else {
                    continue;
                };
                if let Some(seq) = stem.strip_prefix(prefix).and_then(|r| r.parse::<usize>().ok()) {
                    top = top.max(seq);
                }
            }
        };
        scan(self.tasks_dir());
        // One directory per month. A task archived today lands under this one,
        // but a task whose `updated` is wrong lands anywhere, so read them all
        // — it is a handful of directories once a year.
        if let Ok(months) = fs::read_dir(self.archive_dir()) {
            for m in months.flatten() {
                scan(m.path());
            }
        }
        top
    }

    /// Ids the archive holds. `doctor` reads it to catch a name that belongs to
    /// two pieces of work.
    pub fn archived_ids(&self) -> Vec<String> {
        let mut out = Vec::new();
        let Ok(months) = fs::read_dir(self.archive_dir()) else { return out };
        for m in months.flatten() {
            let Ok(entries) = fs::read_dir(m.path()) else { continue };
            for e in entries.flatten() {
                let path = e.path();
                if path.extension().and_then(|x| x.to_str()) != Some("md") {
                    continue;
                }
                // `t-260815-001~2` is still that id's record, filed beside an
                // earlier one. Reporting the name on disk would hide exactly
                // the collision this is read to find.
                if let Some(stem) = path.file_stem().and_then(|x| x.to_str()) {
                    out.push(stem.split('~').next().unwrap_or(stem).to_string());
                }
            }
        }
        out.sort();
        out
    }

    /// Allocate `t-YYMMDD-NNN`, reserving the file with O_EXCL so two agents
    /// adding a task in the same second cannot collide.
    pub fn alloc_task_id(&self) -> std::io::Result<String> {
        fs::create_dir_all(self.tasks_dir())?;
        let stamp = util::today_stamp();
        let prefix = format!("t-{stamp}-");
        // Past everything the day has already used, rather than into the first
        // gap. A gap means something was retired, and reusing its number is
        // how one name comes to mean two things.
        let mut n = self.highest_seq(&prefix) + 1;
        loop {
            if n > 999 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "more than 999 tasks in one day; bump the id scheme",
                ));
            }
            let id = format!("{prefix}{n:03}");
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(self.task_path(&id))
            {
                Ok(_) => return Ok(id),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => n += 1,
                Err(e) => return Err(e),
            }
        }
    }

    /// Retire a task to `archive/tasks/YYYY-MM/`, and return the name it took.
    ///
    /// Never overwrites. The archive is keyed by id, so an id handed out twice
    /// filed the second task directly on top of the first — which is how four
    /// tasks came to share one archived file, three of them recoverable only
    /// from git. Ids are unique going forward, but an archive that can destroy
    /// the record it exists to keep should not be one bug away from doing it,
    /// so a name already taken gets a `~2` rather than a casualty.
    pub fn archive_task(&self, t: &Task) -> std::io::Result<String> {
        let month = if t.updated.len() >= 7 { &t.updated[0..7] } else { "unknown" };
        let dir = self.archive_dir().join(month);
        fs::create_dir_all(&dir)?;

        let mut name = t.id.clone();
        for n in 2..100 {
            if !dir.join(format!("{name}.md")).exists() {
                break;
            }
            name = format!("{}~{n}", t.id);
        }
        if dir.join(format!("{name}.md")).exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("archive already holds {} and 98 renamings of it", t.id),
            ));
        }

        let filed = dir.join(format!("{name}.md"));
        write_atomic(&filed, &t.render())?;
        let _ = fs::remove_file(self.task_path(&t.id));
        // Both halves of the move. A commit holding the arrival without the
        // departure is a store with the task in two places.
        self.wrote(filed);
        self.wrote(self.task_path(&t.id));
        Ok(name)
    }

    /// Newest mtime and file count across the store; the daemon and the panel
    /// poll this to notice out-of-band edits without a filesystem-watch
    /// dependency.
    ///
    /// The count is here because an mtime cannot see a file leave: archiving a
    /// task removes `tasks/x.md`, and the newest mtime left behind is the same
    /// one as before. So both halves have to reach the answer, which is why
    /// they are counted apart and mixed at the end. Folding the count into the
    /// same accumulator — `max` with each mtime, then `+1` per file — let one
    /// absorb the other: two files at `T` and one file at `T+1` both came out
    /// `T+2`, and that is precisely the pair `wsp done` writes when it
    /// archives one task and saves another a second later. Nothing compares
    /// fingerprints for order, only for equality, so a collision is silent —
    /// the panel keeps painting a task that is no longer there.
    ///
    /// Nanoseconds rather than seconds for the same reason: two writes to one
    /// task inside a second are two changes, and a store that rewrites a file
    /// the moment it claims it hits that window constantly. Filesystems that
    /// only keep whole seconds degrade to the old resolution rather than break.
    pub fn fingerprint(&self) -> u64 {
        let mut newest = 0u64;
        let mut files = 0u64;
        for dir in [self.projects_dir(), self.tasks_dir()] {
            let Ok(entries) = fs::read_dir(dir) else { continue };
            for e in entries.flatten() {
                files += 1;
                if let Ok(meta) = e.metadata() {
                    if let Ok(m) = meta.modified() {
                        if let Ok(d) = m.duration_since(std::time::UNIX_EPOCH) {
                            newest = newest.max(d.as_nanos() as u64);
                        }
                    }
                }
            }
        }
        newest.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(files)
    }

    // ---- ephemeral state ------------------------------------------------

    fn state_file(&self, name: &str) -> PathBuf {
        self.state.join(name)
    }

    // ---- the state lock ---------------------------------------------------
    //
    // Every state file is read, changed in memory and written back whole.
    // `write_atomic` makes the *write* indivisible, which is not the same
    // thing: two agents claiming at the same moment both read the old map,
    // both insert their own key, and the second write drops the first. One
    // binding simply vanishes, and the agent that lost it goes on believing it
    // holds a task the store has never heard of.
    //
    // Three agents ran in this tree on 2026-08-15 and the window is a
    // millisecond wide, so this has probably already happened and been read as
    // "herdr lost my claim". A lock file makes the whole cycle indivisible.

    fn lock_path(&self) -> PathBuf {
        self.state.join(".lock")
    }

    /// Run `f` with the state files to ourselves.
    ///
    /// Reentrant: a claim locks once around the several files it touches, and
    /// the mutators it calls take the same lock without deadlocking on it.
    ///
    /// It gives up rather than hangs. A critical section here is a couple of
    /// file writes — microseconds — so a lock still held after `PATIENCE` is
    /// not contention but a process that died holding it, and the only choices
    /// are to break it or to wedge every agent on the machine behind a corpse.
    /// A lost update is recoverable; `wsp claim` never returning is not.
    pub fn locked<T>(&self, f: impl FnOnce() -> T) -> T {
        const PATIENCE: Duration = Duration::from_millis(2000);
        const STALE: Duration = Duration::from_secs(30);

        if DEPTH.with(|d| d.get()) > 0 {
            return f();
        }
        let _ = fs::create_dir_all(&self.state);
        let path = self.lock_path();
        let start = Instant::now();
        loop {
            match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut fh) => {
                    let _ = write!(fh, "{} {}\n", std::process::id(), util::now_iso());
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let held = fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .map(|t| t.elapsed().unwrap_or_default())
                        .unwrap_or_default();
                    if held > STALE {
                        // Whoever held this is not coming back.
                        let _ = fs::remove_file(&path);
                        continue;
                    }
                    if start.elapsed() > PATIENCE {
                        eprintln!("wsp: state lock busy for 2s — carrying on without it");
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                // An unwritable state directory is a problem, but not one to
                // discover by refusing to work.
                Err(_) => break,
            }
        }

        DEPTH.with(|d| d.set(d.get() + 1));
        let out = f();
        DEPTH.with(|d| d.set(d.get() - 1));
        let _ = fs::remove_file(&path);
        out
    }

    /// Read a state file, change it, write it back — all inside the lock, so
    /// the read and the write cannot be split by another process.
    fn update_json(&self, name: &str, f: impl FnOnce(&mut serde_json::Map<String, Value>)) {
        self.locked(|| {
            let mut m = match self.read_json(name) {
                Value::Object(m) => m,
                _ => serde_json::Map::new(),
            };
            f(&mut m);
            self.write_json(name, &Value::Object(m));
        })
    }

    fn read_json(&self, name: &str) -> Value {
        fs::read_to_string(self.state_file(name))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| json!({}))
    }

    fn write_json(&self, name: &str, v: &Value) {
        let _ = fs::create_dir_all(&self.state);
        let _ = write_atomic(
            &self.state_file(name),
            &serde_json::to_string_pretty(v).unwrap_or_else(|_| "{}".into()),
        );
    }

    /// pane_id -> binding object
    pub fn bindings(&self) -> BTreeMap<String, Value> {
        match self.read_json("bindings.json") {
            Value::Object(m) => m.into_iter().collect(),
            _ => BTreeMap::new(),
        }
    }

    pub fn set_binding(&self, pane: &str, value: Value) {
        self.update_json("bindings.json", |b| {
            b.insert(pane.to_string(), value);
        });
    }

    /// Every pane bound to a task. A list rather than an option because the
    /// state file can hold two — briefly, while work is being taken off one
    /// agent and given to another — and the callers that free a task have to
    /// free all of them.
    pub fn panes_for_task(&self, task: &str) -> Vec<String> {
        self.bindings()
            .iter()
            .filter(|(_, b)| b.get("task_id").and_then(|x| x.as_str()) == Some(task))
            .map(|(pane, _)| pane.clone())
            .collect()
    }

    pub fn clear_binding(&self, pane: &str) -> bool {
        let mut removed = false;
        self.update_json("bindings.json", |b| removed = b.remove(pane).is_some());
        removed
    }

    /// Drop bindings whose pane no longer exists.
    // ---- claims -----------------------------------------------------------
    //
    // A binding says which *pane* is on a task, and a pane is the most
    // perishable identifier herdr has — ids are reissued, and one cascade of
    // `pane.exited` once cleared every binding on this machine at a stroke.
    //
    // A claim says which *workspace* the task is being worked in, keyed on
    // things herdr persists in its own session file: the workspace id, and as
    // a fallback its label and cwd, which survive even a workspace being
    // rebuilt under a new id. Claims outlive panes; bindings are derived from
    // them and are free to be lost.

    /// task id -> claim record
    pub fn claims(&self) -> BTreeMap<String, Value> {
        match self.read_json("claims.json") {
            Value::Object(m) => m.into_iter().collect(),
            _ => BTreeMap::new(),
        }
    }

    // ---- mandates ---------------------------------------------------------
    //
    // A claim says what an agent is doing now. A mandate says what it is *for*
    // — the question it has to answer for itself every time it finishes
    // something, and the one thing standing between "record what you were told"
    // and "pick up the next piece". Keyed on the workspace, like a pin, because
    // that is the unit a person points at a piece of work.

    /// workspace id -> mandate record
    pub fn mandates(&self) -> BTreeMap<String, Value> {
        match self.read_json("mandates.json") {
            Value::Object(m) => m.into_iter().collect(),
            _ => BTreeMap::new(),
        }
    }

    pub fn set_mandate(&self, workspace: &str, value: Value) {
        self.update_json("mandates.json", |m| {
            m.insert(workspace.to_string(), value);
        });
    }

    pub fn clear_mandate(&self, workspace: &str) -> bool {
        let mut removed = false;
        self.update_json("mandates.json", |m| removed = m.remove(workspace).is_some());
        removed
    }

    pub fn set_claim(&self, task: &str, value: Value) {
        self.update_json("claims.json", |c| {
            c.insert(task.to_string(), value);
        });
    }

    pub fn clear_claim(&self, task: &str) -> bool {
        let mut removed = false;
        self.update_json("claims.json", |c| removed = c.remove(task).is_some());
        removed
    }

    /// Drop claims whose task no longer exists. A claim on a removed task is
    /// the only way this file grows without bound.
    pub fn reap_claims(&self, live_tasks: &[String]) -> usize {
        let mut dropped = 0;
        self.update_json("claims.json", |c| {
            let before = c.len();
            c.retain(|task, _| live_tasks.iter().any(|t| t == task));
            dropped = before - c.len();
        });
        dropped
    }

    // ---- worked -----------------------------------------------------------
    //
    // What is left when a claim ends. An agent commonly works several tasks in
    // sequence, and the task it walked away from used to lose every trace of
    // it the moment the next claim overwrote the binding.
    //
    // Machine-local for the same reason claims are: it names a workspace, a
    // cwd and a host, none of which mean anything on another machine. The half
    // that belongs in git is the sentence in the task's own log, which `claim`,
    // `release` and `done` all write.

    /// task id -> the claim that ended, and how long it ran
    pub fn worked(&self) -> BTreeMap<String, Value> {
        match self.read_json("worked.json") {
            Value::Object(m) => m.into_iter().collect(),
            _ => BTreeMap::new(),
        }
    }

    pub fn set_worked(&self, task: &str, value: Value) {
        self.update_json("worked.json", |w| {
            w.insert(task.to_string(), value);
        });
    }

    /// One record per task, so this only grows with tasks — but a removed task
    /// would keep its ghost for ever without this.
    pub fn reap_worked(&self, live_tasks: &[String]) -> usize {
        let mut dropped = 0;
        self.update_json("worked.json", |w| {
            let before = w.len();
            w.retain(|task, _| live_tasks.iter().any(|t| t == task));
            dropped = before - w.len();
        });
        dropped
    }

    pub fn reap_bindings(&self, live_panes: &[String]) -> usize {
        let mut dropped = 0;
        self.update_json("bindings.json", |b| {
            let before = b.len();
            b.retain(|pane, _| live_panes.iter().any(|p| p == pane));
            dropped = before - b.len();
        });
        dropped
    }

    /// workspace_id -> project_id
    pub fn pins(&self) -> BTreeMap<String, String> {
        match self.read_json("pins.json") {
            Value::Object(m) => m
                .into_iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
                .collect(),
            _ => BTreeMap::new(),
        }
    }

    pub fn set_pin(&self, workspace: &str, project: &str) {
        self.update_json("pins.json", |p| {
            p.insert(workspace.to_string(), Value::String(project.to_string()));
        });
    }

    pub fn clear_pin(&self, workspace: &str) -> bool {
        let mut removed = false;
        self.update_json("pins.json", |p| removed = p.remove(workspace).is_some());
        removed
    }

    // ---- machine liveness -------------------------------------------------
    //
    // The other half of a machine, and state rather than store for two
    // reasons. Liveness has no business in git — a commit per reachability
    // flip would drown the history of the actual work — and it is *this
    // seat's* view: the same store is one directory, but whether `mb2` answers
    // is a fact about the connection between here and there, which the seat
    // owns and nobody else can write.
    //
    // Nothing here dials anything. The daemon's tunnel supervisor is the only
    // writer (t-260816-035); everybody else — the panel, the machines view,
    // `herdr::call` picking a socket — reads what it last wrote. A reader that
    // probed for itself would be a second opinion on reachability, and two
    // opinions is exactly the ambiguity that makes an offline machine look
    // like an empty one.

    /// Where the daemon forwards a machine's herdr socket to.
    ///
    /// Named here rather than by whoever gets there first, because the process
    /// that creates it (the daemon) and the process that connects to it (any
    /// `herdr::call` on an `@machine` id) are different processes that never
    /// speak — a convention each spelled out for itself would be a bug nobody
    /// could see. Under the state dir and not `/tmp`, so it goes when the
    /// state does.
    ///
    /// Kept short deliberately: a unix socket path is capped at 104 bytes on
    /// macOS, and `~/.local/state/wsp/sock/<name>.sock` leaves room for a long
    /// home directory and a long machine name without either being the thing
    /// that breaks.
    // Unused until the tunnel supervisor lands (t-260816-035) and `herdr::call`
    // learns to route on `@machine` (t-260816-036). Here now because it is the
    // agreement between those two, and an agreement written down after the
    // fact is one that was guessed at twice.
    #[allow(dead_code)]
    pub fn machine_socket(&self, name: &str) -> PathBuf {
        self.state.join("sock").join(format!("{name}.sock"))
    }

    /// machine name -> what the daemon last saw of it.
    pub fn machines_live(&self) -> BTreeMap<String, MachineLive> {
        match self.read_json("machines.json") {
            Value::Object(m) => m.into_iter().map(|(k, v)| (k, MachineLive::from_value(&v))).collect(),
            _ => BTreeMap::new(),
        }
    }

    pub fn machine_live(&self, name: &str) -> Option<MachineLive> {
        self.machines_live().remove(name)
    }

    /// The daemon's tunnel supervisor is the only caller (t-260816-035); see
    /// the section note above for why it is the only one.
    #[allow(dead_code)]
    pub fn set_machine_live(&self, name: &str, live: &MachineLive) {
        let v = live.to_value();
        self.update_json("machines.json", |m| {
            m.insert(name.to_string(), v);
        });
    }

    pub fn clear_machine_live(&self, name: &str) -> bool {
        let mut removed = false;
        self.update_json("machines.json", |m| removed = m.remove(name).is_some());
        removed
    }

    pub fn log_event(&self, kind: &str, data: Value) {
        let _ = fs::create_dir_all(&self.state);
        let line = json!({ "ts": util::now_iso(), "kind": kind, "data": data });
        if let Ok(mut f) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.state_file("events.jsonl"))
        {
            let _ = writeln!(f, "{line}");
        }
        self.run_hook(kind, &line);
    }

    /// `~/wsp/hooks/on-<kind>` gets the event JSON on stdin. Failures are
    /// deliberately silent — a broken hook must never break a write.
    fn run_hook(&self, kind: &str, payload: &Value) {
        let hook = self.root.join("hooks").join(format!("on-{kind}"));
        if !hook.is_file() {
            return;
        }
        use std::process::{Command, Stdio};
        if let Ok(mut child) = Command::new(&hook)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(payload.to_string().as_bytes());
            }
            let _ = child.wait();
        }
    }

    // ---- git ------------------------------------------------------------
    //
    // A commit here says one command did one thing, and it is the only record
    // of who did what in a store several agents write to at once. `add -A`
    // could not keep that promise: it committed the whole store, so whoever
    // ran a command next took everything anybody else had written and landed
    // it under their own message. Nothing was ever lost that way — what went
    // was the attribution, and the ability to revert one change without the
    // other.
    //
    // So the store remembers what it wrote and commits exactly that. The
    // record is kept here rather than threaded through the callers because
    // the set is assembled by helpers several layers down — a claim hands
    // other tasks off, `project rm` rewrites every task it orphans, an
    // archive sweep moves as many files as it finds — and a caller that
    // forgot one would silently leave it uncommitted, which is the same
    // invisible failure in a new place.

    /// Note a file written to the store, for the commit at the end of the
    /// command. Callers that write a store file without going through the
    /// save/archive methods — `init`'s README, `project rm`'s deletion, an
    /// editor let loose on a task — have to say so themselves.
    pub fn wrote(&self, path: impl Into<PathBuf>) {
        self.written.borrow_mut().insert(path.into());
    }

    /// Commit the files this command wrote, and nothing else.
    ///
    /// A no-op when it wrote nothing, so a command that only sometimes touches
    /// the store can call it unconditionally.
    pub fn git_commit(&self, msg: &str) {
        let paths: Vec<PathBuf> = std::mem::take(&mut *self.written.borrow_mut())
            .into_iter()
            // Relative to the root, because that is where git is being run and
            // a root reached through a symlink does not match its own absolute
            // paths as a pathspec.
            .map(|p| p.strip_prefix(&self.root).unwrap_or(&p).to_path_buf())
            .collect();
        if paths.is_empty() {
            return;
        }
        self.commit(msg, &paths);
    }

    /// `wsp init`, whose subject is the store itself rather than anything in
    /// it. The one command that legitimately wants everything.
    pub fn git_commit_all(&self, msg: &str) {
        self.written.borrow_mut().clear();
        self.commit(msg, &[PathBuf::from(".")]);
    }

    fn commit(&self, msg: &str, paths: &[PathBuf]) {
        if std::env::var_os("WSP_NO_COMMIT").is_some() || !self.root.join(".git").exists() {
            return;
        }
        let spec: Vec<&OsStr> = paths.iter().map(|p| p.as_os_str()).collect();

        // The `add` is not optional: a pathspec is matched against what git
        // already knows, so a task that has just been created matches nothing
        // until it is in the index.
        //
        // The `--` on the commit matters as much. A commit with paths takes
        // its content from the *working tree* and bypasses the index, so a
        // concurrent `add -A` in another agent's process cannot widen this
        // commit in the moment between the two calls — and whatever that agent
        // had staged is still staged afterwards, untouched.
        let mut add: Vec<&OsStr> = vec![OsStr::new("add"), OsStr::new("--")];
        add.extend(&spec);
        self.git(&add);

        // No HEAD is a store git has just been initialised in, where there is
        // nothing to diff against and the first commit takes what is staged.
        let born = self
            .git(&[OsStr::new("rev-parse"), OsStr::new("--verify"), OsStr::new("-q"), OsStr::new("HEAD")])
            .is_some();

        let mut cmd: Vec<&OsStr> = vec![OsStr::new("commit"), OsStr::new("-q"), OsStr::new("-m"), OsStr::new(msg)];
        if born {
            // Nothing to say. A `mutate` that changed no bytes, or a file
            // written back exactly as it was found — an empty commit is noise
            // and `git commit` would fail on it anyway.
            let mut diff: Vec<&OsStr> = vec![
                OsStr::new("diff"),
                OsStr::new("--cached"),
                OsStr::new("--quiet"),
                OsStr::new("HEAD"),
                OsStr::new("--"),
            ];
            diff.extend(&spec);
            if self.git(&diff).is_some() {
                return;
            }
            cmd.push(OsStr::new("--"));
            cmd.extend(&spec);
        }

        // `git commit` takes a lock, and with several agents writing a few
        // times a minute two of them land together often enough to matter.
        // Under `add -A` a commit lost that way was swept up by the next one;
        // now it would sit uncommitted until something else happened to touch
        // the same file, so it is worth waiting out — and worth saying so if
        // waiting does not help. Silence here is what let the original defect
        // run for a day.
        for wait in [20, 80, 200] {
            if self.git(&cmd).is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(wait));
        }
        eprintln!(
            "wsp: could not commit {} — the change is on disk; `git -C {} status`",
            paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(" "),
            util::contract(&self.root)
        );
    }

    /// `Some` when git said yes. Output is captured rather than inherited so a
    /// commit never prints over whatever the command is saying.
    fn git(&self, args: &[&OsStr]) -> Option<std::process::Output> {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.root)
            // The commit procedure has agents working through a private index.
            // A wsp command run from inside one would otherwise stage into it
            // and be committed later, by hand, as part of somebody's patch.
            .env_remove("GIT_INDEX_FILE")
            .args(args)
            .output()
            .ok()?;
        out.status.success().then_some(out)
    }

    pub fn git_init(&self) {
        if self.root.join(".git").exists() {
            return;
        }
        use std::process::{Command, Stdio};
        let _ = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .arg("init")
            .arg("-q")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Write via temp file + rename so a reader never sees a half-written task.
pub fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        path.file_name().and_then(|x| x.to_str()).unwrap_or("wsp"),
        std::process::id()
    ));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// The two halves of a machine, and the line between them.
    ///
    /// The durable half is a file in the store and moves the fingerprint,
    /// because adding a machine is a change to the setup. The live half is
    /// state and must not: a machine flapping in and out of reach would
    /// otherwise look to every panel on the seat like the work having changed.
    #[test]
    fn a_machine_is_a_committed_file_and_its_liveness_is_not() {
        let store = scratch("machines");
        assert!(store.machines().is_empty(), "no machines dir yet is no machines");

        let mut m = Machine::new("mb2", "mb2.tail");
        m.os = "darwin".into();
        store.save_machine(&m).unwrap();
        let after_add = store.fingerprint();

        let back = store.machine("mb2").expect("round-trips through the file");
        assert_eq!(back.ssh, "mb2.tail");
        assert_eq!(back.os, "darwin");
        assert!(back.is_active(), "a new machine is active");

        store.set_machine_live(
            "mb2",
            &MachineLive { reachable: true, tunnel: "up".into(), ..Default::default() },
        );
        assert_eq!(store.fingerprint(), after_add, "liveness is not a change to the setup");
        assert!(store.machine_live("mb2").unwrap().reachable);

        // Retiring keeps the row. This is the whole reason `rm` retires by
        // default: a machine that has gone away is still where those pane ids
        // came from.
        let mut retired = back;
        retired.status = "retired".into();
        store.save_machine(&retired).unwrap();
        assert_eq!(store.machines().len(), 1, "still listed");
        assert!(!store.machines()[0].is_active());
    }

    /// Absent is not offline. A machine the daemon has never reported on has no
    /// record at all, and reading that as `reachable: false` would be the same
    /// collapse — unreachable standing in for empty — one layer lower down.
    #[test]
    fn a_machine_nobody_has_reported_on_has_no_record_rather_than_a_false_one() {
        let store = scratch("machines-absent");
        store.save_machine(&Machine::new("mb2", "mb2")).unwrap();
        assert!(store.machine_live("mb2").is_none());

        store.set_machine_live("mb2", &MachineLive { reachable: false, error: "no route".into(), ..Default::default() });
        let l = store.machine_live("mb2").expect("now there is one, and it says why");
        assert!(!l.reachable);
        assert_eq!(l.error, "no route");

        assert!(store.clear_machine_live("mb2"));
        assert!(store.machine_live("mb2").is_none(), "back to no opinion, not a negative one");
    }

    fn scratch(tag: &str) -> Store {
        let root = std::env::temp_dir().join(format!("wsp-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let store = Store::at(root.clone(), root.join("state"));
        store.ensure_dirs().unwrap();
        store
    }

    /// A scratch store that is a git repo with one commit behind it, so the
    /// commits under test have a HEAD to be measured against.
    fn git_scratch(tag: &str) -> Store {
        assert!(
            std::env::var_os("WSP_NO_COMMIT").is_none(),
            "these tests are about the commit — unset WSP_NO_COMMIT to run them"
        );
        let store = scratch(tag);
        for args in [
            &["init", "-q"][..],
            &["config", "user.email", "wsp@test"],
            &["config", "user.name", "wsp test"],
            &["config", "commit.gpgsign", "false"],
        ] {
            git(&store, args);
        }
        // The store's own doc, which is nobody's command's output and is the
        // file the original defect kept sweeping into other people's commits.
        fs::write(store.root.join("agents.md"), "the rules\n").unwrap();
        git(&store, &["add", "-A"]);
        git(&store, &["commit", "-q", "-m", "store"]);
        store
    }

    fn git(store: &Store, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&store.root)
            .args(args)
            .env_remove("GIT_INDEX_FILE")
            .output()
            .expect("git");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// `--no-renames` because archiving a task is exactly the move git reads
    /// as a rename, and a rename is listed by its destination alone — which
    /// would hide the half of the commit these tests exist to check.
    fn head_files(store: &Store) -> Vec<String> {
        git(store, &["show", "--name-only", "--no-renames", "--format=", "HEAD"])
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.to_string())
            .collect()
    }

    fn head_message(store: &Store) -> String {
        git(store, &["log", "-1", "--format=%s"]).trim().to_string()
    }

    /// The defect this replaced `add -A` for: with several agents live, the
    /// commit a command makes must hold the files that command wrote and no
    /// others. Anything else and the record of who did what is whoever ran a
    /// command next — which is how a hand-written rule in `agents.md` came to
    /// be committed by another agent's `wsp done`, under a message about a
    /// task, as a change its author never made.
    #[test]
    fn a_commit_carries_only_the_files_the_command_wrote() {
        let store = git_scratch("commit-scope");

        // Somebody else, mid-edit, in the store this command is about to
        // write to.
        fs::write(store.root.join("agents.md"), "the rules, amended\n").unwrap();

        let t = Task::new("something", "t-260815-001");
        store.save_task(&t).unwrap();
        store.git_commit("wsp: add t-260815-001 — something");

        assert_eq!(head_files(&store), vec!["tasks/t-260815-001.md".to_string()]);
        assert_eq!(head_message(&store), "wsp: add t-260815-001 — something");
        assert!(
            git(&store, &["status", "--porcelain"]).contains("agents.md"),
            "their edit was taken into this commit rather than left for them"
        );

        let _ = fs::remove_dir_all(&store.root);
    }

    /// A task leaving is two files — the one that arrives in the archive and
    /// the one that goes from `tasks/` — and a commit holding one without the
    /// other is a store with the task in both places or neither.
    #[test]
    fn archiving_commits_the_departure_with_the_arrival() {
        let store = git_scratch("commit-archive");

        let t = Task::new("finished", "t-260815-002");
        store.save_task(&t).unwrap();
        store.git_commit("wsp: add t-260815-002 — finished");

        store.archive_task(&t).unwrap();
        store.git_commit("wsp: rm t-260815-002 — finished");

        let files = head_files(&store);
        assert!(
            files.contains(&"tasks/t-260815-002.md".to_string()),
            "the task file left the store without leaving the commit: {files:?}"
        );
        assert!(
            files.iter().any(|f| f.starts_with("archive/tasks/")),
            "the archived copy is not in the commit that retired it: {files:?}"
        );

        let _ = fs::remove_dir_all(&store.root);
    }

    /// `release` and `reconcile` commit whether or not they wrote anything,
    /// because whether they did depends on what was bound at the time. An
    /// empty commit for every one of those would bury the log the store keeps
    /// exists to be read.
    #[test]
    fn a_command_that_wrote_nothing_commits_nothing() {
        let store = git_scratch("commit-empty");
        store.git_commit("wsp: release nothing at all");
        assert_eq!(head_message(&store), "store");

        // And neither does one that wrote a file back exactly as it found it.
        let t = Task::new("unchanged", "t-260815-003");
        store.save_task(&t).unwrap();
        store.git_commit("wsp: add t-260815-003 — unchanged");
        store.save_task(&t).unwrap();
        store.git_commit("wsp: note t-260815-003 — unchanged");
        assert_eq!(head_message(&store), "wsp: add t-260815-003 — unchanged");

        let _ = fs::remove_dir_all(&store.root);
    }

    fn task_file(store: &Store, name: &str, at: SystemTime) {
        let path = store.tasks_dir().join(name);
        fs::write(&path, "x").unwrap();
        let f = fs::File::options().write(true).open(&path).unwrap();
        f.set_times(fs::FileTimes::new().set_modified(at)).unwrap();
    }

    /// The two halves of the fingerprint have to survive into the answer
    /// separately, and this is the pair that proves it: two task files a
    /// moment old, then one of them archived and the other written a second
    /// later. The store is completely different and the old accumulator —
    /// `max` with each mtime, `+1` per file, into the same u64 — called both
    /// states `T+2`. The daemon and the panel only ever compare fingerprints
    /// for equality, so a collision is not a near miss: the panel goes on
    /// painting the task that is no longer there, and nothing moves it until
    /// some unrelated write does.
    #[test]
    fn a_task_leaving_never_looks_like_a_task_being_touched() {
        let store = scratch("fp-collision");
        let t = UNIX_EPOCH + Duration::from_secs(1_800_000_000);

        task_file(&store, "a.md", t);
        task_file(&store, "b.md", t);
        let two_files = store.fingerprint();

        fs::remove_file(store.tasks_dir().join("b.md")).unwrap();
        task_file(&store, "a.md", t + Duration::from_secs(1));
        let one_file_a_second_later = store.fingerprint();

        assert_ne!(
            two_files, one_file_a_second_later,
            "archiving one task and writing another read as no change at all"
        );
        let _ = fs::remove_dir_all(&store.root);
    }

    /// A file arriving is a change even when it is older than everything
    /// already there — `wsp unarchive` puts back a task whose mtime is
    /// whatever git wrote, and a store that has grown by one file may not
    /// answer the same as the store that had not.
    #[test]
    fn a_file_arriving_moves_the_fingerprint_even_when_it_is_the_oldest() {
        let store = scratch("fp-count");
        let t = UNIX_EPOCH + Duration::from_secs(1_800_000_000);

        task_file(&store, "a.md", t);
        let alone = store.fingerprint();

        task_file(&store, "b.md", t - Duration::from_secs(3600));
        assert_ne!(alone, store.fingerprint(), "an older file arriving was invisible");

        let _ = fs::remove_dir_all(&store.root);
    }
}
