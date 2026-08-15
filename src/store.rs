//! Durable store (`~/wsp`, git) and ephemeral state (`~/.local/state/wsp`).
//!
//! Every mutation goes through here so that ID allocation, atomic writes and
//! git commits stay in one place — that is the whole reason agents are told to
//! use the CLI rather than editing files.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::fm;
use crate::model::{Project, Task};
use crate::util;

thread_local! {
    /// How deep we are in nested `locked` calls, so the lock can be taken once
    /// around a whole claim and again by each file it touches.
    static DEPTH: Cell<usize> = const { Cell::new(0) };
}

pub struct Store {
    pub root: PathBuf,
    pub state: PathBuf,
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
        Store { root, state }
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
        write_atomic(&self.projects_dir().join(format!("{}.md", p.id)), &p.render())
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
        write_atomic(&self.task_path(&t.id), &t.render())
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

        write_atomic(&dir.join(format!("{name}.md")), &t.render())?;
        let _ = fs::remove_file(self.task_path(&t.id));
        Ok(name)
    }

    /// Newest mtime across the store; the daemon polls this to notice
    /// out-of-band edits without a filesystem-watch dependency.
    pub fn fingerprint(&self) -> u64 {
        let mut newest = 0u64;
        for dir in [self.projects_dir(), self.tasks_dir()] {
            let Ok(entries) = fs::read_dir(dir) else { continue };
            for e in entries.flatten() {
                if let Ok(meta) = e.metadata() {
                    if let Ok(m) = meta.modified() {
                        if let Ok(d) = m.duration_since(std::time::UNIX_EPOCH) {
                            newest = newest.max(d.as_secs());
                        }
                    }
                }
                newest = newest.wrapping_add(1); // count files too
            }
        }
        newest
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

    pub fn git_commit(&self, msg: &str) {
        if std::env::var_os("WSP_NO_COMMIT").is_some() || !self.root.join(".git").exists() {
            return;
        }
        use std::process::{Command, Stdio};
        let run = |args: &[&str]| {
            let _ = Command::new("git")
                .arg("-C")
                .arg(&self.root)
                .args(args)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        };
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", msg]);
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
