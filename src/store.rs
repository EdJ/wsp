//! Durable store (`~/wsp`, git) and ephemeral state (`~/.local/state/wsp`).
//!
//! Every mutation goes through here so that ID allocation, atomic writes and
//! git commits stay in one place — that is the whole reason agents are told to
//! use the CLI rather than editing files.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::fm;
use crate::model::{Project, Task};
use crate::util;

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

    /// Allocate `t-YYMMDD-NNN`, reserving the file with O_EXCL so two agents
    /// adding a task in the same second cannot collide.
    pub fn alloc_task_id(&self) -> std::io::Result<String> {
        fs::create_dir_all(self.tasks_dir())?;
        let stamp = util::today_stamp();
        let prefix = format!("t-{stamp}-");
        let mut n = 1;
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

    pub fn archive_task(&self, t: &Task) -> std::io::Result<()> {
        let month = if t.updated.len() >= 7 { &t.updated[0..7] } else { "unknown" };
        let dir = self.archive_dir().join(month);
        fs::create_dir_all(&dir)?;
        write_atomic(&dir.join(format!("{}.md", t.id)), &t.render())?;
        let _ = fs::remove_file(self.task_path(&t.id));
        Ok(())
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
        let mut b = self.bindings();
        b.insert(pane.to_string(), value);
        self.write_json("bindings.json", &Value::Object(b.into_iter().collect()));
    }

    pub fn clear_binding(&self, pane: &str) -> bool {
        let mut b = self.bindings();
        let removed = b.remove(pane).is_some();
        if removed {
            self.write_json("bindings.json", &Value::Object(b.into_iter().collect()));
        }
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

    pub fn set_claim(&self, task: &str, value: Value) {
        let mut c = self.claims();
        c.insert(task.to_string(), value);
        self.write_json("claims.json", &Value::Object(c.into_iter().collect()));
    }

    pub fn clear_claim(&self, task: &str) -> bool {
        let mut c = self.claims();
        let removed = c.remove(task).is_some();
        if removed {
            self.write_json("claims.json", &Value::Object(c.into_iter().collect()));
        }
        removed
    }

    /// Drop claims whose task no longer exists. A claim on a removed task is
    /// the only way this file grows without bound.
    pub fn reap_claims(&self, live_tasks: &[String]) -> usize {
        let c = self.claims();
        let keep: BTreeMap<String, Value> = c
            .iter()
            .filter(|(task, _)| live_tasks.iter().any(|t| t == *task))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let dropped = c.len() - keep.len();
        if dropped > 0 {
            self.write_json("claims.json", &Value::Object(keep.into_iter().collect()));
        }
        dropped
    }

    pub fn reap_bindings(&self, live_panes: &[String]) -> usize {
        let b = self.bindings();
        let keep: BTreeMap<String, Value> = b
            .iter()
            .filter(|(pane, _)| live_panes.iter().any(|p| p == *pane))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let dropped = b.len() - keep.len();
        if dropped > 0 {
            self.write_json("bindings.json", &Value::Object(keep.into_iter().collect()));
        }
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
        let mut p = self.pins();
        p.insert(workspace.to_string(), project.to_string());
        let obj: serde_json::Map<String, Value> =
            p.into_iter().map(|(k, v)| (k, Value::String(v))).collect();
        self.write_json("pins.json", &Value::Object(obj));
    }

    pub fn clear_pin(&self, workspace: &str) -> bool {
        let mut p = self.pins();
        let removed = p.remove(workspace).is_some();
        if removed {
            let obj: serde_json::Map<String, Value> =
                p.into_iter().map(|(k, v)| (k, Value::String(v))).collect();
            self.write_json("pins.json", &Value::Object(obj));
        }
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
