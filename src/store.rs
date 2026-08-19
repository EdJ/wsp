//! Durable store (`~/wsp`, git) and ephemeral state (`~/.local/state/wsp`).
//!
//! Every mutation goes through here so that ID allocation, atomic writes and
//! git commits stay in one place — that is the whole reason agents are told to
//! use the CLI rather than editing files.
//!
//! Reading is where the panel's time goes, and [`Store::cached_dir`] is why it
//! no longer does: a process that asks for the tasks twice pays a `stat` per
//! file for the second answer rather than a read and a parse.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::fm;
use crate::model::{Machine, Project, Task, Worklist};
use crate::util;

thread_local! {
    /// How deep we are in nested `locked` calls, so the lock can be taken once
    /// around a whole claim and again by each file it touches.
    static DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// The id prefix for a task filed nowhere.
///
/// Reserved: no project may take it as a slug or a code, because a project that
/// did would share a numbering space with the inbox and the two would hand out
/// the same id. [`Store::code_taken`] is where that is enforced.
pub const INBOX_CODE: &str = "inbox";

/// What a user-typed id came to, or why it came to nothing.
///
/// `Ambiguous` exists because "no such task" was a lie the moment ids stopped
/// being unique per day. A bare `022` matched two open tasks, so the lookup
/// returned nothing and every caller printed `no such task 022` — about a
/// suffix that names two, one of which Ed then spent time reading. Under
/// per-project ids a bare number is ambiguous *more* often, not less, so the
/// answer has to be the candidates rather than a shrug.
pub enum Found {
    One(Box<Task>),
    Ambiguous(Vec<Task>),
    Nothing,
}

impl Found {
    /// The message to print when this is not a single task, ready to hand to
    /// `eprintln!`. `None` when it is one and there is nothing to say.
    pub fn why(&self, needle: &str) -> Option<String> {
        match self {
            Found::One(_) => None,
            Found::Nothing => Some(format!("wsp: no such task `{needle}`")),
            Found::Ambiguous(ts) => {
                let mut s = format!("wsp: `{needle}` names {} tasks:", ts.len());
                for t in ts {
                    let title: String = t.title.chars().take(48).collect();
                    s.push_str(&format!("\n  {}  {}", t.id, title));
                }
                Some(s)
            }
        }
    }
}

pub struct Store {
    pub root: PathBuf,
    pub state: PathBuf,
    /// Files this process has written to the store, waiting for the commit
    /// that names them. See `git_commit`.
    written: RefCell<BTreeSet<PathBuf>>,
    /// The last reading of `tasks/` and `projects/`, kept so a second reading
    /// costs a `stat` per file instead of an `open`, a `read` and a parse.
    /// See [`Store::cached_dir`] for what makes that sound.
    tasks_cache: RefCell<Cache<Task>>,
    projects_cache: RefCell<Cache<Project>>,
}

/// What a file looked like when we last parsed it: modified time in
/// nanoseconds, and length.
///
/// The length is not redundant. mtime alone misses a rewrite that lands inside
/// one timestamp tick, and while APFS keeps nanoseconds and `write_atomic`
/// renames a freshly written temp file into place — so the mtime is always the
/// moment of the write — a filesystem that keeps whole seconds would make that
/// window a second wide. Two facts that would both have to collide are cheap
/// insurance against a panel painting a task whose text has moved on.
type Stamp = (u128, u64);

/// path -> what it was when we read it, and what it parsed to.
type Cache<T> = BTreeMap<PathBuf, (Stamp, T)>;

fn stamp_of(m: &fs::Metadata) -> Option<Stamp> {
    let mtime = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some((mtime.as_nanos(), m.len()))
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
        // The default store is the developer's own, at `~/wsp`, and a test that
        // opens it is reading whoever is working today — which is how two tests
        // of a fake herdr backend came to fail on a laptop being switched on.
        // The argument, and the helper, are in [`crate::util::isolated`];
        // this is the line that makes a test that reads the live store
        // impossible to write by accident rather than merely discouraged. It
        // compiles out of the binary entirely, so no shipped path can tell.
        #[cfg(test)]
        assert!(
            std::env::var_os("WSP_HOME").is_some() && std::env::var_os("WSP_STATE").is_some(),
            "a test opened the live store — take crate::util::isolated() instead"
        );
        let root = match std::env::var_os("WSP_HOME") {
            Some(v) => PathBuf::from(v),
            None => util::home().join("wsp"),
        };
        let state = match std::env::var_os("WSP_STATE") {
            Some(v) => PathBuf::from(v),
            None => Store::default_state(),
        };
        Store::at(root, state)
    }

    pub fn at(root: PathBuf, state: PathBuf) -> Store {
        Store {
            root,
            state,
            written: RefCell::new(BTreeSet::new()),
            tasks_cache: RefCell::new(Cache::new()),
            projects_cache: RefCell::new(Cache::new()),
        }
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
    pub fn archive_projects_dir(&self) -> PathBuf {
        self.root.join("archive/projects")
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

    // ---- reading a directory of records, without re-reading it ------------

    /// Every `.md` in `dir`, parsed — re-parsing only the files whose bytes
    /// have moved since the last call.
    ///
    /// # Why this exists
    ///
    /// The panel is a long-lived process that reads the whole store to draw a
    /// frame, and it draws a lot of frames. Measured 2026-08-17 against the
    /// live store — 285 tasks, 978 KB — a release build spends **8.8 ms** in
    /// `tasks()`, and `read_to_string` is **5.4 ms** of it. `fm::parse` is 1.3
    /// and `Task::from_doc` 0.4: nothing here is a slow function, and there is
    /// no faster parse to write. The cost is the reading.
    ///
    /// It is paid four times a second in the panel somebody is looking at.
    /// Every herdr `pane.updated` marks that panel dirty — two a second per
    /// working agent — and its 250 ms gate then rebuilds the frame from a
    /// fresh reading of all 285 files. That is what "the panel feels laggy"
    /// was, and the focused panel measured 4.1% of a core against 1.2% for the
    /// ones nobody is looking at.
    ///
    /// So: stat instead. A `stat` walk of the same directory is **1.4 ms**,
    /// against 8.8 for reading it, and the store changes about once a minute —
    /// so nearly every one of those readings is asking again for bytes that
    /// have not moved.
    ///
    /// # Why an mtime is trustworthy here
    ///
    /// Every write to the store goes through `write_atomic`, which renames a
    /// freshly written temp file over the target — so the mtime is the moment
    /// of the write, never an inherited one. Nothing in wsp preserves an mtime
    /// across a write.
    ///
    /// The case worth naming is git, because the store is a git repository and
    /// a checkout, a reset or a `wsp migrate` can rewrite the whole tree
    /// underneath a running panel. git does not restore mtimes either: a file
    /// it rewrites gets the time it rewrote it, and a file it leaves alone is
    /// byte-identical to what we parsed. Both answers are the right one.
    ///
    /// A file we cannot `stat` is read, and is not cached — an answer we cannot
    /// check is not one to keep.
    ///
    /// # What it costs
    ///
    /// One `stat` per file on the cold path, which the CLI pays and never gets
    /// back: about 0.9 ms added to a `wsp ls`, against 17 ms for the whole
    /// command including process start. And the parsed records stay resident —
    /// roughly 1 MB for today's store, which the panel was allocating and
    /// freeing on every frame regardless.
    fn cached_dir<T: Clone>(
        dir: &Path,
        cache: &RefCell<Cache<T>>,
        build: impl Fn(&fm::Doc, &str) -> T,
    ) -> Vec<T> {
        let mut out = Vec::new();
        let Ok(entries) = fs::read_dir(dir) else {
            return out;
        };
        let mut cache = cache.borrow_mut();
        let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("md") {
                continue;
            }
            let stem = path.file_stem().and_then(|x| x.to_str()).unwrap_or("").to_string();
            let stamp = e.metadata().ok().as_ref().and_then(stamp_of);

            if let Some(stamp) = stamp {
                if let Some((was, parsed)) = cache.get(&path) {
                    if *was == stamp {
                        out.push(parsed.clone());
                        seen.insert(path);
                        continue;
                    }
                }
            }
            let Ok(text) = fs::read_to_string(&path) else { continue };
            let parsed = build(&fm::parse(&text), &stem);
            out.push(parsed.clone());
            match stamp {
                Some(stamp) => {
                    cache.insert(path.clone(), (stamp, parsed));
                }
                None => {
                    cache.remove(&path);
                }
            }
            seen.insert(path);
        }
        // A task archived or a project removed is a file that will never be
        // asked for again; without this the map only ever grows.
        cache.retain(|p, _| seen.contains(p));
        out
    }

    // ---- projects -------------------------------------------------------

    pub fn projects(&self) -> Vec<Project> {
        let mut out =
            Store::cached_dir(&self.projects_dir(), &self.projects_cache, Project::from_doc);
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

    // ---- worklists --------------------------------------------------------
    //
    // The fifth kind, on the `machines/` pattern and for the same reason: a
    // plan is durable and committed, one file each. No `cached_dir` here,
    // unlike `tasks/` and `projects/` — that cache exists because the panel
    // re-reads 285 files four times a second, and a store holds a handful of
    // worklists read once at a barrier. A cache is worth having where it saves
    // a reading somebody is waiting on, and is otherwise a second copy of the
    // truth.

    pub fn worklists_dir(&self) -> PathBuf {
        self.root.join("worklists")
    }

    pub fn worklist_path(&self, id: &str) -> PathBuf {
        self.worklists_dir().join(format!("{id}.md"))
    }

    /// Every worklist in the store, by slug — `done` ones included, for the
    /// reason a retired machine is still a row: a worklist is a plan, and the
    /// history of a plan is the thing worth keeping about it.
    pub fn worklists(&self) -> Vec<Worklist> {
        let mut out = Vec::new();
        let Ok(entries) = fs::read_dir(self.worklists_dir()) else {
            return out;
        };
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("md") {
                continue;
            }
            let stem = path.file_stem().and_then(|x| x.to_str()).unwrap_or("").to_string();
            if let Ok(text) = fs::read_to_string(&path) {
                out.push(Worklist::from_doc(&fm::parse(&text), &stem));
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    pub fn worklist(&self, id: &str) -> Option<Worklist> {
        let text = fs::read_to_string(self.worklist_path(id)).ok()?;
        Some(Worklist::from_doc(&fm::parse(&text), id))
    }

    #[allow(dead_code)] // the verbs that read it are the next group; see `model`
    pub fn save_worklist(&self, w: &Worklist) -> std::io::Result<()> {
        fs::create_dir_all(self.worklists_dir())?;
        let path = self.worklist_path(&w.id);
        write_atomic(&path, &w.render())?;
        self.wrote(path);
        Ok(())
    }

    /// Whether a name is already spoken for as a **scope** — the key space a
    /// worklist slug and a project id share.
    ///
    /// They share one because `governors.json` is keyed on the scope and a
    /// running worklist takes a seat of its own: routing asks the worklist
    /// first, then the project. Two things answering to one key would send a
    /// hand raised at 3am to whichever the map happened to hold.
    ///
    /// The project half is checked **by id**, and that is the correction the
    /// design needed. [`Store::code_taken`] compares against project *codes*,
    /// and [`Project::code`] falls back to the id only where no code is set —
    /// so a project with an explicit code would have let a slug collide with
    /// its id and pass. Both are asked here: the id because that is the
    /// routing key, and `code_taken` because a slug that is some project's
    /// code, or a prefix ids have already been handed out under, is a name
    /// that means something else to everyone reading it.
    ///
    /// Occupancy only. That a name is *shaped* like a scope is the caller's —
    /// `project add` slugifies what it is given, and a worklist has to be
    /// created the same way, because a key space where one side can name
    /// something the other cannot is not one key space.
    ///
    /// `excluding` is the scope keeping its own name: a rename to what it is
    /// already called is not a collision.
    #[allow(dead_code)] // the verbs that read it are the next group; see `model`
    pub fn scope_taken(&self, scope: &str, excluding: Option<&str>) -> Option<String> {
        if Some(scope) == excluding {
            return None;
        }
        if self.worklist(scope).is_some() {
            return Some(format!("worklist `{scope}` uses it"));
        }
        if self.projects().iter().any(|p| p.id == scope) {
            return Some(format!("project `{scope}` uses it"));
        }
        self.code_taken(scope, excluding)
    }

    // ---- tasks ----------------------------------------------------------

    /// Every task in the store, by id. Re-reads only what has changed since
    /// the last call — see [`Store::cached_dir`], which is where the whole
    /// argument for that lives.
    pub fn tasks(&self) -> Vec<Task> {
        let mut out = Store::cached_dir(&self.tasks_dir(), &self.tasks_cache, Task::from_doc);
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

    /// Resolve a user-typed id: exact, then a retired id, then bare suffix
    /// (`003`), then unique case-insensitive title substring.
    ///
    /// The retired-id step is what keeps three days of git history, every
    /// agent's notes and Ed's own memory working after the renumbering. See
    /// [`Store::renamed_ids`].
    pub fn resolve_task(&self, needle: &str) -> Found {
        if let Some(t) = self.task(needle) {
            return Found::One(Box::new(t));
        }
        if let Some(now) = self.renamed_ids().get(needle) {
            if let Some(t) = self.task(now) {
                return Found::One(Box::new(t));
            }
        }
        let all = self.tasks();
        let open: Vec<&Task> = all.iter().filter(|t| t.status().is_open()).collect();

        // A suffix must land on a `-` boundary. Without that `022` also
        // matched `wsp-1022`, and — worse under the new scheme — `rob` matched
        // every id in a project whose code ended in those letters.
        let by_suffix: Vec<Task> = open
            .iter()
            .filter(|t| t.id == needle || t.id.strip_suffix(needle).is_some_and(|h| h.ends_with('-')))
            .map(|t| (*t).clone())
            .collect();
        match by_suffix.len() {
            1 => return Found::One(Box::new(by_suffix.into_iter().next().expect("just checked"))),
            0 => {}
            _ => return Found::Ambiguous(by_suffix),
        }

        let lower = needle.to_ascii_lowercase();
        let by_title: Vec<Task> = open
            .iter()
            .filter(|t| t.title.to_ascii_lowercase().contains(&lower))
            .map(|t| (*t).clone())
            .collect();
        match by_title.len() {
            1 => Found::One(Box::new(by_title.into_iter().next().expect("just checked"))),
            0 => Found::Nothing,
            _ => Found::Ambiguous(by_title),
        }
    }

    /// The task a needle names, or the message saying why it named none.
    ///
    /// What every command a person types at should use. The `Err` is a ready
    /// sentence, so a caller cannot accidentally flatten "names two tasks, here
    /// they are" back into "no such task" — which is what the store told Ed
    /// when he typed `022`, and is why he read the wrong one.
    pub fn task_or_why(&self, needle: &str) -> Result<Task, String> {
        match self.resolve_task(needle) {
            Found::One(t) => Ok(*t),
            other => Err(other.why(needle).unwrap_or_default()),
        }
    }

    /// [`Store::resolve_task`] for the callers that only want the task, with
    /// an ambiguous match reading as no match.
    ///
    /// Prefer `task_or_why` in anything a person types at: this drops the one
    /// piece of information they need to type something better.
    pub fn find_task(&self, needle: &str) -> Option<Task> {
        match self.resolve_task(needle) {
            Found::One(t) => Some(*t),
            _ => None,
        }
    }

    pub fn save_task(&self, t: &Task) -> std::io::Result<()> {
        fs::create_dir_all(self.tasks_dir())?;
        let path = self.task_path(&t.id);
        write_atomic(&path, &t.render())?;
        self.wrote(path);
        Ok(())
    }

    // ---- renaming, and the ids that were left behind ----------------------
    //
    // An id must never change. Two things break that rule on purpose, and both
    // pay for it here: the migration off dated ids, and a task leaving the
    // inbox for a project whose space it must join.
    //
    // What makes it payable is that the old id keeps resolving for ever.
    // `ids.json` is the record of what became what, it is committed with the
    // store, and `resolve_task` reads it before it gives up. So three days of
    // git history, every note an agent wrote and whatever a person still has in
    // their head go on working — which is also the thing that closes the
    // disagreement Ed accepted when he chose to leave git history unrewritten.

    pub fn ids_path(&self) -> PathBuf {
        self.root.join("ids.json")
    }

    /// Retired id -> the id it became. Never has an entry whose value is itself
    /// a key: see [`Store::rename_tasks`] for why chains are collapsed.
    pub fn renamed_ids(&self) -> BTreeMap<String, String> {
        let Ok(text) = fs::read_to_string(self.ids_path()) else {
            return BTreeMap::new();
        };
        match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(m)) => m
                .into_iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
                .collect(),
            _ => BTreeMap::new(),
        }
    }

    /// Rename tasks wholesale, rewriting every reference to them in one pass.
    ///
    /// One pass rather than one per task, because the alternative is
    /// quadratic: 267 renamings over a 300-file store is 80,000 file reads to
    /// do what a single walk does once.
    ///
    /// Substitution is by token, not by text search. Every maximal run of
    /// `[A-Za-z0-9_-]` is looked up whole, so `wspt-260815-005` is one token
    /// that matches nothing rather than a hit inside a longer word — the same
    /// boundary rule the prose scanner in `cmd_brief` already had to learn —
    /// and no rewritten id can be rewritten again by a later entry in the same
    /// map.
    ///
    /// Returns what it rewrote: (files touched, references rewritten).
    pub fn rename_tasks(&self, map: &BTreeMap<String, String>) -> std::io::Result<(usize, usize)> {
        if map.is_empty() {
            return Ok((0, 0));
        }
        let mut files = 0usize;
        let mut refs = 0usize;

        // The task files themselves move first, so that nothing below is
        // reading a path that is about to stop existing. `id:` inside the file
        // is rewritten by the same substitution as everything else.
        for (from, to) in map {
            let (a, b) = (self.task_path(from), self.task_path(to));
            if a.exists() {
                // An *empty* file at the destination is a reservation that
                // `alloc_task_id` has already won with O_EXCL, and renaming
                // onto it is how the reservation gets filled. Anything with
                // content in it is a real task and a real collision.
                if b.exists() && fs::metadata(&b).map(|m| m.len() > 0).unwrap_or(true) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        format!("{from} cannot become {to}: {to} already exists"),
                    ));
                }
                fs::rename(&a, &b)?;
                self.wrote(a);
                self.wrote(b);
            }
        }
        // The archive holds ids in the same space and is cited by live prose,
        // so it moves too. A `~2` suffix marks a record filed beside an earlier
        // one of the same name and has to survive the move intact.
        if let Ok(months) = fs::read_dir(self.archive_dir()) {
            for m in months.flatten() {
                let Ok(entries) = fs::read_dir(m.path()) else { continue };
                for e in entries.flatten() {
                    let path = e.path();
                    let Some(stem) = path.file_stem().and_then(|x| x.to_str()) else { continue };
                    let (id, tail) = match stem.split_once('~') {
                        Some((id, n)) => (id, format!("~{n}")),
                        None => (stem, String::new()),
                    };
                    if let Some(to) = map.get(id) {
                        let dest = m.path().join(format!("{to}{tail}.md"));
                        fs::rename(&path, &dest)?;
                        self.wrote(path);
                        self.wrote(dest);
                    }
                }
            }
        }

        // Now the references, in everything that can hold one.
        let mut targets: Vec<PathBuf> = Vec::new();
        let collect = |dir: PathBuf, out: &mut Vec<PathBuf>| {
            if let Ok(entries) = fs::read_dir(dir) {
                for e in entries.flatten() {
                    let p = e.path();
                    if p.extension().and_then(|x| x.to_str()) == Some("md") {
                        out.push(p);
                    }
                }
            }
        };
        collect(self.tasks_dir(), &mut targets);
        collect(self.projects_dir(), &mut targets);
        if let Ok(months) = fs::read_dir(self.archive_dir()) {
            for m in months.flatten() {
                collect(m.path(), &mut targets);
            }
        }
        for path in targets {
            let Ok(text) = fs::read_to_string(&path) else { continue };
            let (out, n) = substitute_tokens(&text, map);
            if n > 0 {
                write_atomic(&path, &out)?;
                self.wrote(path);
                files += 1;
                refs += n;
            }
        }

        // Ephemeral state keys tasks by id too — a claim, a binding, a raised
        // hand, the event log. It is not committed, so it is rewritten under
        // the same lock everything else takes rather than with the store.
        // Rewriting the raw text is sound here precisely because an id can hold
        // no JSON metacharacter: there is nothing to escape and nothing to
        // reparse.
        self.locked(|| {
            for name in
                ["bindings.json", "claims.json", "worked.json", "flags.json", "governors.json",
                 "mandates.json", "pins.json", "detail.json", "panel-view.json", "panels.json",
                 "events.jsonl"]
            {
                let path = self.state_file(name);
                let Ok(text) = fs::read_to_string(&path) else { continue };
                let (out, n) = substitute_tokens(&text, map);
                if n > 0 {
                    let _ = write_atomic(&path, &out);
                    files += 1;
                    refs += n;
                }
            }
        });

        // And the record of what became what. Chains are collapsed as they are
        // added: if `inbox-003` already pointed at `data-014` and `data-014` is
        // now becoming something else, `inbox-003` is repointed at the end of
        // the chain rather than at a link in it, so a lookup stays one step and
        // can never land on an id that no longer exists.
        let mut ids = self.renamed_ids();
        for (_, v) in ids.iter_mut() {
            if let Some(end) = map.get(v.as_str()) {
                *v = end.clone();
            }
        }
        for (from, to) in map {
            ids.insert(from.clone(), to.clone());
        }
        ids.retain(|k, v| k != v);
        let json = Value::Object(
            ids.into_iter().map(|(k, v)| (k, Value::String(v))).collect(),
        );
        write_atomic(&self.ids_path(), &serde_json::to_string_pretty(&json).unwrap_or_default())?;
        self.wrote(self.ids_path());

        Ok((files, refs))
    }

    /// Whether a code is already spoken for — by a project, by the inbox, or by
    /// ids that have already been handed out under it.
    ///
    /// The last clause is the one that matters and the one that is easy to miss.
    /// A code freed by a project being renamed is *not* free: the tasks it
    /// numbered still exist and still answer to it, so handing it to a second
    /// project would put two tasks under one name, which is the exact failure
    /// the archive already carries scars from.
    pub fn code_taken(&self, code: &str, excluding: Option<&str>) -> Option<String> {
        if code == INBOX_CODE {
            return Some("the inbox numbers under it".into());
        }
        for p in self.projects() {
            if Some(p.id.as_str()) == excluding {
                continue;
            }
            if p.code() == code {
                return Some(format!("project `{}` uses it", p.id));
            }
        }
        // Ids already handed out under it. Which project they belong to is the
        // whole question: a project reclaiming a code it used before is fine,
        // and a *different* project taking it would put two tasks under one
        // name for ever.
        let prefix = format!("{code}-");
        let held: Vec<Task> = self
            .tasks()
            .into_iter()
            .chain(self.archived_tasks())
            .filter(|t| {
                t.id.strip_prefix(&prefix).is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()))
            })
            .filter(|t| t.project.as_deref() != excluding)
            .collect();
        match held.first() {
            Some(t) => Some(format!(
                "{} already answers to it, in {}",
                t.id,
                t.project.clone().unwrap_or_else(|| "the inbox".into())
            )),
            None => None,
        }
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

    /// Every task the archive holds, read in full.
    ///
    /// The migration needs their `project` and not just their names: an
    /// archived task numbers in the same space as a live one, and giving a live
    /// task a number an archived one already answers to is the collision the
    /// archive exists to have survived.
    pub fn archived_tasks(&self) -> Vec<Task> {
        let mut out = Vec::new();
        let Ok(months) = fs::read_dir(self.archive_dir()) else { return out };
        for m in months.flatten() {
            let Ok(entries) = fs::read_dir(m.path()) else { continue };
            for e in entries.flatten() {
                let path = e.path();
                if path.extension().and_then(|x| x.to_str()) != Some("md") {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(|x| x.to_str()) else { continue };
                let id = stem.split('~').next().unwrap_or(stem).to_string();
                if let Ok(text) = fs::read_to_string(&path) {
                    out.push(Task::from_doc(&fm::parse(&text), &id));
                }
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// The projects the archive holds, each with the file it is in.
    ///
    /// Read when a project cannot be found: "no project matching `batch`" is
    /// the wrong answer when `batch` is sitting in the archive with its
    /// handbook intact, and the person asking is usually asking *because* it
    /// is gone from the live list.
    pub fn archived_projects(&self) -> Vec<(Project, PathBuf)> {
        let mut out = Vec::new();
        let Ok(entries) = fs::read_dir(self.archive_projects_dir()) else { return out };
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("md") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|x| x.to_str()) else { continue };
            // `batch~2` is still `batch`'s record, filed beside an earlier one.
            let id = stem.split('~').next().unwrap_or(stem).to_string();
            if let Ok(text) = fs::read_to_string(&path) {
                out.push((Project::from_doc(&fm::parse(&text), &id), path));
            }
        }
        out.sort_by(|a, b| a.0.id.cmp(&b.0.id));
        out
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

    /// Allocate `<code>-NNN` in the project's own numbering space.
    ///
    /// Two things settle the id, and only one of them is the counter. The
    /// project's `seq` says where to *start* looking, which turns the O(n)
    /// directory scan into an O(1) field read; `O_EXCL` on the file is what
    /// actually decides who gets the number, exactly as it did under the dated
    /// scheme. Keeping those apart is the whole reason the counter is allowed
    /// to live in a file two machines may both be writing: a `seq` that is
    /// stale, missing or hand-edited costs one scan and can never hand the same
    /// id to two tasks.
    ///
    /// The scan is taken lazily — only once the hint has been proved wrong —
    /// so the cost of a fresh clone, whose project files carry a `seq` of 0, is
    /// one wasted `open` rather than one per task the project already holds.
    ///
    /// A task with no project numbers in `inbox`, which has no file to hold a
    /// counter and so always takes the scan. That is affordable because the
    /// inbox is meant to be emptied, and it is the only space whose ids are not
    /// permanent: see [`Store::rename_task`], which renumbers a task into its
    /// project's space when it is finally filed.
    pub fn alloc_task_id(&self, project: Option<&str>) -> std::io::Result<String> {
        fs::create_dir_all(self.tasks_dir())?;
        let proj = project.and_then(|p| self.project(p));
        let code = proj.as_ref().map(|p| p.code().to_string());
        let prefix = format!("{}-", code.as_deref().unwrap_or(INBOX_CODE));

        // Past everything the space has already used, rather than into the
        // first gap. A gap means something was retired, and reusing its number
        // is how one name comes to mean two things.
        let mut n = proj.as_ref().map(|p| p.seq).unwrap_or(0) + 1;
        let mut scanned = proj.is_none();
        if scanned {
            n = self.highest_seq(&prefix) + 1;
        }
        loop {
            let id = format!("{prefix}{n:03}");
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(self.task_path(&id))
            {
                Ok(_) => {
                    if let Some(mut p) = proj {
                        // Written back after the id is won, not before, so a
                        // process that dies mid-allocation leaves a counter
                        // that is behind rather than ahead — behind costs a
                        // scan, ahead silently skips numbers for ever.
                        p.seq = n;
                        let _ = self.save_project(&p);
                    }
                    return Ok(id);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if scanned {
                        n += 1;
                    } else {
                        // The hint was wrong. One scan replaces what would
                        // otherwise be one failed `open` per task in the way.
                        scanned = true;
                        n = self.highest_seq(&prefix) + 1;
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Retire a task to `archive/tasks/YYYY-MM/`, and return the name it took.
    /// A month directory because tasks arrive in their hundreds and carry the
    /// date to file by; see [`Store::file_away`] for the rest.
    pub fn archive_task(&self, t: &Task) -> std::io::Result<String> {
        let month = if t.updated.len() >= 7 { &t.updated[0..7] } else { "unknown" };
        let dir = self.archive_dir().join(month);
        self.file_away(&dir, &t.id, &t.render(), self.task_path(&t.id))
    }

    /// Retire a project to `archive/projects/`, on the same terms as a task.
    ///
    /// A project file is the only place its handbook, its decisions and its
    /// brief exist — tasks carry their own prose out with them under `wsp mv`,
    /// and those three fields have no such escape. Deleting the file was
    /// therefore the one removal in the store that destroyed writing nobody
    /// could get back, and it said nothing while doing it: `wsp project rm
    /// batch` took an eleven-lane table with it and reported success. So the
    /// container is retired rather than deleted, which is what `wsp rm` has
    /// always done for the smaller thing it held.
    ///
    /// No month directory here. Tasks are archived in their hundreds and are
    /// dated by `updated`; projects are few and have no such field, so a flat
    /// directory is both the honest shape and the one you can read with `ls`.
    pub fn archive_project(&self, p: &Project) -> std::io::Result<String> {
        let dir = self.archive_projects_dir();
        let live = self.projects_dir().join(format!("{}.md", p.id));
        self.file_away(&dir, &p.id, &p.render(), live)
    }

    /// Copy a rendered record into `dir` under a name nothing there is using,
    /// remove the live file, and return the name it took.
    ///
    /// Never overwrites. The archive is keyed by id, so an id handed out twice
    /// filed the second task directly on top of the first — which is how four
    /// tasks came to share one archived file, three of them recoverable only
    /// from git. Ids are unique going forward, but an archive that can destroy
    /// the record it exists to keep should not be one bug away from doing it,
    /// so a name already taken gets a `~2` rather than a casualty.
    fn file_away(
        &self,
        dir: &Path,
        id: &str,
        rendered: &str,
        live: PathBuf,
    ) -> std::io::Result<String> {
        fs::create_dir_all(dir)?;

        let mut name = id.to_string();
        for n in 2..100 {
            if !dir.join(format!("{name}.md")).exists() {
                break;
            }
            name = format!("{id}~{n}");
        }
        if dir.join(format!("{name}.md")).exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("archive already holds {id} and 98 renamings of it"),
            ));
        }

        let filed = dir.join(format!("{name}.md"));
        write_atomic(&filed, rendered)?;
        let _ = fs::remove_file(&live);
        // Both halves of the move. A commit holding the arrival without the
        // departure is a store with the record in two places.
        self.wrote(filed);
        self.wrote(live);
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

    // ---- the daemon marker ------------------------------------------------
    //
    // Which process is *the* daemon for this store. It lives beside the
    // bindings and not in `~/wsp`, because the pair of daemons that must not
    // coexist is the pair pointed at one state directory: a sandbox sets
    // `WSP_STATE` and gets its own marker, its own bindings and its own right
    // to a daemon. The argument for what a second one does about it is in
    // [`crate::daemon`].

    /// The pid that last claimed this store, and when it said so.
    pub fn daemon_holder(&self) -> Option<(u32, String)> {
        let v = self.read_json("daemon.json");
        let pid = v.get("pid").and_then(|x| x.as_u64())? as u32;
        let since = v.get("since").and_then(|x| x.as_str()).unwrap_or("").to_string();
        Some((pid, since))
    }

    /// Claim it. Last writer wins, deliberately: two daemons starting in the
    /// same millisecond both read an empty marker, and the one that does not
    /// end up in it stands down at its next tick rather than needing to have
    /// been stopped here.
    pub fn set_daemon_holder(&self, pid: u32) {
        let since = util::now_iso();
        self.update_json("daemon.json", |m| {
            m.insert("pid".into(), json!(pid));
            m.insert("since".into(), json!(since));
        });
    }

    /// Where a store with nothing said about it keeps its state. The daemon
    /// needs it to read another process's environment the way [`Store::open`]
    /// read its own — a `wsp daemon` with no `WSP_STATE` is on this one.
    pub fn default_state() -> PathBuf {
        util::home().join(".local/state/wsp")
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
    // perishable identifier herdr has: it dies with its workspace, and one
    // cascade of `pane.exited` once cleared every binding on this machine at a
    // stroke.
    //
    // A claim says which *workspace* the task is being worked in, keyed on
    // things herdr persists in its own session file: the workspace id, and as a
    // fallback its label and cwd. The fallback is load-bearing in both
    // directions — it finds a workspace rebuilt under a new id, and it is the
    // only thing that can tell a workspace apart from a later one that was
    // handed its id (`robustness-084`). Claims outlive panes; bindings are
    // derived from them and are free to be lost.

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

    // ---- governors --------------------------------------------------------
    //
    // A mandate says a workspace may take work in a project. A governor says a
    // workspace is the *coordination point* for one — where a raised hand goes,
    // and which agent is sequencing rather than working.
    //
    // Keyed on the project rather than on the workspace, which is the one place
    // this differs from every other record beside it. A pin, a mandate and a
    // claim are all facts about a workspace and there can be many per project;
    // there is at most one seat per project, and the question every reader asks
    // is "who governs this?" rather than "what does this workspace govern".
    // Keying it the other way would make the cardinality unenforceable and the
    // lookup a scan, in the one file the panel reads on every tick.
    //
    // State rather than store, for the same reason as a claim: the seat is a
    // live workspace on this host and means nothing tomorrow, while the project
    // it governs is durable and already in git. The hierarchy is committed; the
    // agent sitting in it is not.

    /// project id -> governor record: which workspace holds the seat.
    pub fn governors(&self) -> BTreeMap<String, Value> {
        match self.read_json("governors.json") {
            Value::Object(m) => m.into_iter().collect(),
            _ => BTreeMap::new(),
        }
    }

    pub fn set_governor(&self, project: &str, value: Value) {
        self.update_json("governors.json", |g| {
            g.insert(project.to_string(), value);
        });
    }

    pub fn clear_governor(&self, project: &str) -> bool {
        let mut removed = false;
        self.update_json("governors.json", |g| removed = g.remove(project).is_some());
        removed
    }

    // ---- raised hands ---------------------------------------------------
    //
    // A flag is an agent asking to be looked at: this task, and here is why.
    // It lives here rather than in the task file because it is addressed to a
    // person *now* — the same kind of fact as a claim or a pin, true while
    // somebody is at the machine and meaningless a week later. Writing it into
    // the store would put an interruption into git and leave it in the history
    // of the work for ever; `wsp note` is the verb for the half of it that is
    // worth keeping.

    /// task id -> flag record: what was said, and by which pane.
    pub fn flags(&self) -> BTreeMap<String, Value> {
        match self.read_json("flags.json") {
            Value::Object(m) => m.into_iter().collect(),
            _ => BTreeMap::new(),
        }
    }

    pub fn set_flag(&self, task: &str, value: Value) {
        self.update_json("flags.json", |f| {
            f.insert(task.to_string(), value);
        });
    }

    pub fn clear_flag(&self, task: &str) -> bool {
        let mut removed = false;
        self.update_json("flags.json", |f| removed = f.remove(task).is_some());
        removed
    }

    /// Has anybody raised or lowered a flag since this said otherwise?
    ///
    /// [`Store::fingerprint`] walks `projects/` and `tasks/` and cannot see
    /// this: flags are state, deliberately, and the panel's refetch is gated on
    /// that fingerprint. Without a stamp of its own a raised hand would wait
    /// for whatever else happened to change the store — which on a quiet
    /// machine is nothing at all, and a hand nobody sees is worse than no hand.
    ///
    /// One `stat`, which is why it can sit in the same tick gate. Nanoseconds
    /// for the same reason the fingerprint uses them: raising and lowering
    /// inside one second is two pieces of news. A missing file is zero — the
    /// resting state, and equal to itself.
    pub fn flags_stamp(&self) -> u64 {
        fs::metadata(self.state_file("flags.json"))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
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

    // ---- what a pane is called, at full length ----------------------------
    //
    // A label on the wire is cut to `cmd_agent::LABEL_MAX`, and the rule is
    // right: a herdr sidebar is 26 columns, and a paragraph is not a name. What
    // was wrong is that the cut copy was the *only* copy — the pane label, the
    // workspace name and the `task` token all held 44 characters and the store
    // held nothing longer, so `panel --full`, which has a hundred columns to
    // draw in, had nothing longer to draw.
    //
    // Most names have another home already: a pane holding a task is named
    // after the task, and the task's title is in the store, whole. The one
    // string with nowhere else to live is a sentence from `wsp say`, and this
    // is where it lives — the agent module's ephemeral state, by the decision
    // recorded on t-260816-083.
    //
    // Keyed on the pane, which is exactly as perishable as the label it is the
    // long form of, and holding both halves: `label` is what was put on the
    // wire and `full` is what it was cut from. A reader compares `label`
    // against what the pane is wearing *now* and uses `full` only if they
    // agree, so anything that renames a pane — a person, a claim, a release,
    // another tool — invalidates this by construction rather than by our
    // having remembered to clear it. The failure is then a truncated name,
    // which is what every surface had before, rather than a wrong one.

    /// pane id -> `{ label, full }`
    pub fn said(&self) -> BTreeMap<String, Value> {
        match self.read_json("said.json") {
            Value::Object(m) => m.into_iter().collect(),
            _ => BTreeMap::new(),
        }
    }

    /// Record what a pane's label was cut from. Nothing is stored when the
    /// label is already whole: an entry that says the same as the wire is one
    /// more thing to keep in step for no reading.
    pub fn set_said(&self, pane: &str, label: &str, full: &str) {
        if label == full {
            self.clear_said(pane);
            return;
        }
        self.update_json("said.json", |s| {
            s.insert(pane.to_string(), json!({ "label": label, "full": full }));
        });
    }

    pub fn clear_said(&self, pane: &str) -> bool {
        let mut removed = false;
        self.update_json("said.json", |s| removed = s.remove(pane).is_some());
        removed
    }

    /// Drop the long names of panes that are gone, on the same evidence and
    /// with the same reservation as [`Store::reap_bindings`].
    pub fn reap_said(&self, live_panes: &[String]) -> usize {
        let mut dropped = 0;
        self.update_json("said.json", |s| {
            let before = s.len();
            s.retain(|pane, _| live_panes.iter().any(|p| p == pane));
            dropped = before - s.len();
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

    // ---- the roster ------------------------------------------------------
    //
    // What was running the last time anything looked: one row per agent pane,
    // with the session id that can bring it back. Written by `sync` on the tick
    // that feeds the agents section of the panel, so it is *the census a person
    // last saw* rather than a history — which is the whole of what makes it
    // safe to offer back after a restart.
    //
    // Ed, 2026-08-18, setting the boundary this file exists to enforce: "only
    // resume an agent if the user asks for it, or if it was ACTIVE at the
    // moment of the restart. Do not reload every agent the store has ever
    // seen." `events.jsonl` held 134 session records on the machine that was
    // said on, and a startup that walked those would open dozens of workspaces
    // nobody asked for. The log is still the reader of last resort for one id a
    // person names; it is never the source of a list.
    //
    // Overwritten whole, never appended to. A row that is not in the newest
    // census is not resumable — the agent ended, and an agent that ended is not
    // one a restart interrupted.

    /// The last census: what was running when anything last looked.
    pub fn roster(&self) -> Vec<Value> {
        match self.read_json("resumable.json").get("rows") {
            Some(Value::Array(rows)) => rows.clone(),
            _ => Vec::new(),
        }
    }

    /// Replace it. Silent when the rows have not changed, because this runs on
    /// every daemon tick and an unchanged file rewritten every twenty seconds
    /// is wear for no reader — which is also why the timestamp is beside the
    /// rows rather than on each one, where it would make every tick a change.
    pub fn set_roster(&self, rows: Vec<Value>) -> bool {
        if self.roster() == rows {
            return false;
        }
        self.write_json("resumable.json", &json!({ "at": util::now_iso(), "rows": rows }));
        true
    }

    /// The census a restart interrupted, held back from being overwritten.
    ///
    /// The roster is rewritten on every tick, and the tick after a restart
    /// writes an empty one — so the thing worth offering has a lifetime of
    /// about twenty seconds unless something takes a copy. This is that copy:
    /// written once, when the daemon comes up and finds a census none of whose
    /// agents are running, and cleared the moment a person has answered the
    /// question. It is not a second roster and nothing keeps it current.
    pub fn held(&self) -> Vec<Value> {
        match self.read_json("resume-held.json").get("rows") {
            Some(Value::Array(rows)) => rows.clone(),
            _ => Vec::new(),
        }
    }

    pub fn set_held(&self, rows: Vec<Value>) {
        self.write_json("resume-held.json", &json!({ "at": util::now_iso(), "rows": rows }));
    }

    /// When the held census was taken — the roster's own timestamp, carried
    /// over, because what a person is told is how long ago they were running.
    pub fn held_at(&self) -> String {
        self.read_json("resume-held.json")
            .get("at")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// The question has been answered. Asked once per restart, and a list that
    /// is offered forever is a list nobody reads.
    pub fn clear_held(&self) {
        let _ = fs::remove_file(self.state_file("resume-held.json"));
    }

    /// One row taken off the offer, by the session it names.
    ///
    /// Exact where the liveness filter is inferential: an agent that has just
    /// been resumed is known to be back by the thing that brought it back, and
    /// waiting for it to reappear in a census under the same id is a guess
    /// about somebody else's runtime. Both are wanted — the filter catches the
    /// agent that never died — but this is the one that cannot be wrong.
    pub fn forget_held(&self, session: &str) {
        let rows: Vec<Value> = self
            .held()
            .into_iter()
            .filter(|r| r.get("session").and_then(Value::as_str) != Some(session))
            .collect();
        match rows.is_empty() {
            true => self.clear_held(),
            false => self.write_json(
                "resume-held.json",
                &json!({ "at": self.held_at(), "rows": rows }),
            ),
        }
    }

    /// Every event of one kind, oldest first, as the `data` each carried.
    ///
    /// The event log's first *reader*, and it exists because of what
    /// `render-061` had to decide: how far back a resume reaches. A binding and
    /// a seat each hold one session at a time and are dropped when the pane or
    /// the workspace goes; `events.jsonl` is append-only and holds every
    /// session that has ever been learned. So the record is the truth and this
    /// is the fallback — reached only when the record is gone, and never
    /// allowed to overrule one that is still there, because an id in the log
    /// may have been superseded by a `/clear` the log also recorded.
    ///
    /// Whole-file, and deliberately not indexed. It is 134 lines on the machine
    /// this was written on and it is read when a person types `wsp resume`,
    /// which is not a hot path; the substring test before the parse is what
    /// keeps it from being one if the file grows.
    pub fn events_of(&self, kind: &str) -> Vec<Value> {
        let Ok(text) = fs::read_to_string(self.state_file("events.jsonl")) else {
            return Vec::new();
        };
        let needle = format!("\"kind\":\"{kind}\"");
        text.lines()
            .filter(|l| l.contains(&needle))
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| v.get("kind").and_then(Value::as_str) == Some(kind))
            .filter_map(|v| v.get("data").cloned())
            .collect()
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

/// Replace whole identifier tokens using `map`, returning the text and how many
/// were replaced.
///
/// A token is a maximal run of `[A-Za-z0-9_-]`, which is what makes this safe
/// to run over prose, frontmatter and raw JSON alike. Three properties follow
/// from taking maximal runs, and all three are load-bearing:
///
/// - `wspt-260815-005` is one token, so an id is never matched inside a longer
///   word. `[t-260815-004]` is not, because `[` cannot be in a token.
/// - A token is looked up once and emitted, never re-examined, so one entry in
///   the map can never rewrite what another entry just produced.
/// - An id holds no character that JSON quotes or escapes, so applying this to
///   the raw text of a state file needs no parse and can produce nothing that
///   fails to reparse.
pub fn substitute_tokens(text: &str, map: &BTreeMap<String, String>) -> (String, usize) {
    fn is_tok(c: char) -> bool {
        c.is_ascii_alphanumeric() || c == '-' || c == '_'
    }
    let mut out = String::with_capacity(text.len());
    let mut n = 0usize;
    let mut rest = text;
    while !rest.is_empty() {
        match rest.find(is_tok) {
            None => {
                out.push_str(rest);
                break;
            }
            Some(start) => {
                out.push_str(&rest[..start]);
                let tail = &rest[start..];
                let end = tail.find(|c: char| !is_tok(c)).unwrap_or(tail.len());
                let tok = &tail[..end];
                match map.get(tok) {
                    Some(to) => {
                        out.push_str(to);
                        n += 1;
                    }
                    None => out.push_str(tok),
                }
                rest = &tail[end..];
            }
        }
    }
    (out, n)
}

/// Write via temp file + rename so a reader never sees a half-written task.
pub fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    replace(path, contents, true)
}

/// The same, without waiting for the disk.
///
/// The rename is what a *reader* depends on, and it is here either way: nobody
/// ever sees half a file. The `fsync` is about the machine losing power between
/// the write and the read, and it is the expensive half — on APFS it is
/// milliseconds, which is nothing once a day and a great deal several times a
/// second.
///
/// So: durable for anything a person would have to type again, and this for
/// state that is only true while a process is up and is rewritten by that
/// process within a tick. The surface's last frame is the case it was added
/// for — see `panel::surface_frame`.
pub fn write_atomic_unsynced(path: &Path, contents: &str) -> std::io::Result<()> {
    replace(path, contents, false)
}

fn replace(path: &Path, contents: &str, durable: bool) -> std::io::Result<()> {
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
        if durable {
            f.sync_all()?;
        }
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Group, WorklistStatus};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// The daemon marker is state and not work: a daemon starting must not look
    /// to every panel on the machine like something changed in the store, or
    /// every herdr restart would repaint twenty-two of them for nothing. And it
    /// keys on the state directory, which is what makes a sandbox's daemon and
    /// the live one two daemons rather than a fight — the argument is in
    /// [`crate::daemon`].
    #[test]
    fn the_daemon_marker_says_which_process_holds_which_store() {
        let store = scratch("daemon-marker");
        assert_eq!(store.daemon_holder(), None, "an unclaimed store names nobody");
        let before = store.fingerprint();

        store.set_daemon_holder(22121);
        assert_eq!(store.fingerprint(), before, "a daemon starting is not a change to the work");
        let (pid, since) = store.daemon_holder().expect("the store was claimed");
        assert_eq!(pid, 22121);
        assert!(since.starts_with("20"), "no timestamp to say how long it has held it: {since:?}");

        // Taking it over is a write of the same shape — the previous holder is
        // replaced, not appended to, because there is only ever one.
        store.set_daemon_holder(88994);
        assert_eq!(store.daemon_holder().map(|(p, _)| p), Some(88994));

        // A second store on the same machine has its own, which is how `wsp
        // sandbox` gets a daemon of its own without either of them noticing.
        let sandbox = scratch("daemon-marker-sb");
        assert_eq!(sandbox.daemon_holder(), None, "two stores shared one marker");
    }

    /// A hand raised is state, and the panel's refetch is gated on a
    /// fingerprint that walks `projects/` and `tasks/` — so the stamp is what
    /// stands between a flag and a panel that shows it whenever something else
    /// happens to change. Nothing about the store moves when one is raised,
    /// which is the whole reason this exists.
    #[test]
    fn a_raised_hand_is_visible_without_the_store_moving() {
        let store = scratch("flags");
        assert_eq!(store.flags_stamp(), 0, "nothing written yet is the resting state");
        let before = store.fingerprint();

        store.set_flag("t-1", json!({ "said": "can I take this?", "pane": "w1:p6" }));
        assert_eq!(store.fingerprint(), before, "a flag is not a change to the work");
        let raised = store.flags_stamp();
        assert_ne!(raised, 0, "…and it is a change the panel can see");
        assert_eq!(
            store.flags().get("t-1").and_then(|f| f.get("said")).and_then(|s| s.as_str()),
            Some("can I take this?"),
        );

        // Lowering is news of exactly the same kind. A stamp that only moved
        // on the way up would leave a flag drawn in every panel on the machine
        // until something else changed.
        assert!(store.clear_flag("t-1"));
        assert!(store.flags().is_empty());
        assert!(store.flags_stamp() >= raised, "lowering it is a change too");
        assert!(!store.clear_flag("t-1"), "and there is nothing left to lower");
    }

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

    // ---- worklists --------------------------------------------------------

    /// The fifth kind of record, on disk and back. A plan is durable and
    /// committed for the same reason a project is: what is worth having about
    /// it later is its history, and a list that lived only in state would lose
    /// the one thing that makes it worth writing down.
    #[test]
    fn a_worklist_is_a_record_of_its_own_and_the_queue_survives_the_store() {
        let store = scratch("worklists");
        assert!(store.worklists().is_empty(), "a store with no plans in it");

        let mut w = Worklist::new("batch", "Overnight batch");
        w.set_status(WorklistStatus::Running);
        w.set_groups(&[
            Group { members: vec!["robustness-069".into()], cap: None, stop: "if it does not land clean, stop".into() },
            Group { members: vec!["render-041".into(), "render-068".into()], cap: Some(2), stop: String::new() },
        ]);
        store.save_worklist(&w).unwrap();

        let got = store.worklist("batch").expect("it is where the path says");
        assert_eq!(got.title, "Overnight batch");
        assert_eq!(got.status(), WorklistStatus::Running);
        assert_eq!(got.groups().len(), 2);
        assert_eq!(got.groups()[0].stop, "if it does not land clean, stop");
        assert_eq!(got.groups()[1].members, ["render-041", "render-068"]);
        assert_eq!(store.worklists().len(), 1, "and it is in the list of them");
        assert!(store.worklist("fork").is_none(), "a name nobody has taken is not a list");
    }

    /// A worklist slug and a project id are **one key space**, because
    /// `governors.json` is keyed on the scope and a running worklist takes a
    /// seat of its own — so two things answering to one name would route a
    /// raised hand to whichever the map happened to hold.
    ///
    /// The case in the middle is the one the design got wrong and this test
    /// exists for: `code_taken` compares against a project's *code*, which
    /// falls back to the id only where no code is set. A project with an
    /// explicit code would therefore have let a slug collide with its id and
    /// pass, which is exactly the routing this check is here to protect.
    #[test]
    fn a_worklist_slug_and_a_project_id_cannot_be_the_same_name() {
        let store = scratch("worklist-scope");
        proj(&store, "render", "");
        proj(&store, "strata-prototype", "sp");
        store.save_worklist(&Worklist::new("batch", "Overnight batch")).unwrap();

        assert!(store.scope_taken("render", None).is_some(), "a project id is not free");
        assert!(
            store.scope_taken("strata-prototype", None).is_some(),
            "and its own code being something else does not make its id free — \
             this is what a `code_taken`-only check let through",
        );
        assert!(store.scope_taken("sp", None).is_some(), "a project's code names ids and is not free either");
        assert!(store.scope_taken("batch", None).is_some(), "and the other way: a worklist holds its slug");
        assert!(store.scope_taken("fork", None).is_none(), "a name nobody has is free");
        assert!(
            store.scope_taken("batch", Some("batch")).is_none(),
            "a scope keeping the name it already has is not colliding with itself",
        );
    }

    // ---- per-project ids --------------------------------------------------

    fn proj(store: &Store, id: &str, code: &str) -> Project {
        let mut p = Project::new(id);
        p.code_raw = code.to_string();
        store.save_project(&p).unwrap();
        p
    }

    /// The point of the whole scheme, at the one line where it happens: the id
    /// says which project the task is in, and two projects numbering at the
    /// same time do not share a number.
    #[test]
    fn ids_number_in_their_own_project_and_the_prefix_says_which() {
        let store = scratch("ids-per-project");
        proj(&store, "wsp", "");
        proj(&store, "robustness", "");

        assert_eq!(store.alloc_task_id(Some("wsp")).unwrap(), "wsp-001");
        assert_eq!(store.alloc_task_id(Some("robustness")).unwrap(), "robustness-001");
        assert_eq!(store.alloc_task_id(Some("wsp")).unwrap(), "wsp-002");

        // The incident this exists to prevent: `add` answered 014, `013` was
        // typed, and 013 was a real task in another project. Now it is not.
        assert_eq!(store.alloc_task_id(Some("robustness")).unwrap(), "robustness-002");
        assert!(!store.task_path("robustness-013").exists(), "the slip lands on nothing");
        assert!(!store.task_path("wsp-013").exists());
    }

    /// The counter is a hint and the file is the truth. Every way of getting
    /// the hint wrong has to cost a scan and nothing else — because the hint
    /// lives in a file two machines may both be writing, and that is only
    /// affordable if being wrong is cheap rather than corrupting.
    #[test]
    fn a_wrong_counter_costs_a_scan_and_never_a_duplicate_id() {
        let store = scratch("ids-hint");
        proj(&store, "wsp", "");
        for _ in 0..4 {
            store.alloc_task_id(Some("wsp")).unwrap();
        }
        assert_eq!(store.project("wsp").unwrap().seq, 4, "the hint tracks what was handed out");

        // Behind — a fresh clone, or a counter that never got written back.
        let mut p = store.project("wsp").unwrap();
        p.seq = 0;
        store.save_project(&p).unwrap();
        assert_eq!(store.alloc_task_id(Some("wsp")).unwrap(), "wsp-005");

        // Ahead — a hand-edit, or a write from a machine that got further.
        let mut p = store.project("wsp").unwrap();
        p.seq = 40;
        store.save_project(&p).unwrap();
        assert_eq!(store.alloc_task_id(Some("wsp")).unwrap(), "wsp-041", "trusted, and it is free");

        // Gone entirely. The scan reaches past everything ever used rather
        // than into the first gap, because a gap is a task that was retired
        // and reusing its number is how one name comes to mean two things.
        std::fs::remove_file(store.task_path("wsp-041")).unwrap();
        let mut p = store.project("wsp").unwrap();
        p.seq = 0;
        store.save_project(&p).unwrap();
        assert_eq!(store.alloc_task_id(Some("wsp")).unwrap(), "wsp-006", "the first free one above the scan");
    }

    /// A code is what keeps a descriptive slug from making a long id. Changing
    /// one starts a new space; it never renumbers what is already out.
    #[test]
    fn a_code_gives_the_project_a_shorter_space_without_moving_what_is_out() {
        let store = scratch("ids-code");
        proj(&store, "strata-prototype", "sp");
        let id = store.alloc_task_id(Some("strata-prototype")).unwrap();
        assert_eq!(id, "sp-001");
        let mut t = Task::new("the first one", &id);
        t.project = Some("strata-prototype".into());
        store.save_task(&t).unwrap();

        assert!(store.code_taken("sp", None).is_some(), "another project cannot take it");
        assert!(store.code_taken("sp", Some("strata-prototype")).is_none(), "its owner may keep it");
        assert!(store.code_taken(INBOX_CODE, None).is_some(), "the inbox numbers under it");

        // Freed by its owner, and still not free: the tasks it numbered exist.
        let mut p = store.project("strata-prototype").unwrap();
        p.code_raw = "proto".into();
        p.seq = 0;
        store.save_project(&p).unwrap();
        assert!(
            store.code_taken("sp", None).is_some(),
            "sp-001 still answers to it, so a second project must not have it"
        );
        assert_eq!(store.alloc_task_id(Some("strata-prototype")).unwrap(), "proto-001");
    }

    /// Substitution is by whole token. Three properties hang off that, and all
    /// three have already been got wrong somewhere in this codebase.
    #[test]
    fn references_are_rewritten_by_token_and_never_inside_a_word() {
        let mut map = BTreeMap::new();
        map.insert("t-260815-022".to_string(), "robustness-022".to_string());
        map.insert("robustness-022".to_string(), "nope-999".to_string());

        let (out, n) = substitute_tokens(
            "see t-260815-022 and [t-260815-022], but not wspt-260815-022 or t-260815-0221",
            &map,
        );
        assert_eq!(n, 2, "twice, and not inside the two longer words");
        assert!(out.contains("see robustness-022 and [robustness-022]"));
        assert!(out.contains("wspt-260815-022"), "a longer token is a different token");
        assert!(out.contains("t-260815-0221"));
        assert!(
            !out.contains("nope-999"),
            "what one entry produced must not be rewritten again by another"
        );

        // Raw JSON, which is sound precisely because an id holds nothing that
        // JSON quotes or escapes.
        let (json, _) = substitute_tokens(r#"{"task_id":"t-260815-022"}"#, &map);
        assert_eq!(json, r#"{"task_id":"robustness-022"}"#);
    }

    /// A rename has to take every reference with it, and leave a bridge from
    /// the old id — otherwise three days of git history and every note an
    /// agent wrote stop resolving on the day of the migration.
    #[test]
    fn renaming_carries_the_references_and_leaves_the_old_id_resolving() {
        let store = scratch("ids-rename");
        proj(&store, "data", "");

        let mut parent = Task::new("the parent", "t-260815-024");
        parent.project = Some("data".into());
        parent.body = "## Overview\nleans on t-260815-014\n".into();
        store.save_task(&parent).unwrap();

        let mut kid = Task::new("the child", "t-260815-014");
        kid.project = Some("data".into());
        kid.parent = Some("t-260815-024".into());
        store.save_task(&kid).unwrap();
        store.set_claim("t-260815-024", json!({ "host": "here" }));

        let mut map = BTreeMap::new();
        map.insert("t-260815-024".to_string(), "data-001".to_string());
        map.insert("t-260815-014".to_string(), "data-002".to_string());
        let (_, refs) = store.rename_tasks(&map).unwrap();
        assert!(refs >= 3, "the id field, the parent field and the prose: {refs}");

        assert!(store.task("t-260815-024").is_none(), "the old file is gone");
        let moved = store.task("data-001").expect("under its new name");
        assert_eq!(moved.id, "data-001", "including the id inside the file");
        assert!(moved.body.contains("data-002"), "prose came too: {}", moved.body);
        assert_eq!(
            store.task("data-002").and_then(|t| t.parent),
            Some("data-001".into()),
            "and the parent field, which is what the tree is drawn from"
        );
        assert!(store.claims().contains_key("data-001"), "and the claim, which is ephemeral");

        // The bridge. This is what makes leaving git history unrewritten a
        // decision rather than a dead end.
        assert_eq!(store.renamed_ids().get("t-260815-024"), Some(&"data-001".to_string()));
        assert_eq!(
            store.find_task("t-260815-024").map(|t| t.id),
            Some("data-001".into()),
            "an id from before the migration still resolves"
        );

        // Chains collapse, so a second rename never leaves a lookup pointing
        // at a link that no longer exists.
        let mut again = BTreeMap::new();
        again.insert("data-001".to_string(), "data-009".to_string());
        store.rename_tasks(&again).unwrap();
        assert_eq!(store.renamed_ids().get("t-260815-024"), Some(&"data-009".to_string()));
        assert_eq!(store.find_task("t-260815-024").map(|t| t.id), Some("data-009".into()));
    }

    /// "No such task" was a lie about a suffix that named two, and it is the
    /// lie that cost Ed his afternoon reading the wrong `022`.
    #[test]
    fn a_suffix_that_names_two_tasks_says_so_rather_than_saying_no() {
        let store = scratch("ids-ambiguous");
        for (id, title) in [("wsp-022", "one tree per agent"), ("render-022", "the other one")] {
            let mut t = Task::new(title, id);
            t.project = Some(id.split('-').next().unwrap().into());
            store.save_task(&t).unwrap();
        }
        let mut only = Task::new("alone", "data-005");
        only.project = Some("data".into());
        store.save_task(&only).unwrap();

        let why = store.task_or_why("022").expect_err("two tasks answer to it");
        assert!(why.contains("names 2 tasks"), "{why}");
        assert!(why.contains("wsp-022") && why.contains("render-022"), "and it names them: {why}");
        assert!(!why.contains("no such task"), "which is what it used to say: {why}");

        assert_eq!(store.task_or_why("005").map(|t| t.id), Ok("data-005".into()), "one match still works");
        assert_eq!(store.task_or_why("wsp-022").map(|t| t.id), Ok("wsp-022".into()), "and the whole id always does");

        // A suffix has to land on a `-`, or a project code would match every
        // id in a project whose code merely ended in those letters.
        assert!(store.task_or_why("22").is_err(), "not a boundary");
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

    // ---- not re-reading what has not changed -------------------------------

    /// Put a task file down with a title we can read back and an mtime we
    /// choose, so a test can say "this file changed" or "this file did not"
    /// without waiting on a clock.
    fn titled(store: &Store, id: &str, title: &str, at: SystemTime) {
        let path = store.tasks_dir().join(format!("{id}.md"));
        fs::write(&path, format!("---\nid: {id}\ntitle: {title}\n---\n\n")).unwrap();
        let f = fs::File::options().write(true).open(&path).unwrap();
        f.set_times(fs::FileTimes::new().set_modified(at)).unwrap();
    }

    fn title_of(store: &Store, id: &str) -> String {
        store.tasks().into_iter().find(|t| t.id == id).map(|t| t.title).unwrap_or_default()
    }

    /// The store is a git repository, and a checkout, a reset or a migration
    /// can rewrite every file in it underneath a panel that is running. So the
    /// cache cannot be keyed on "did *we* write it" — it has to be keyed on the
    /// file, and a file somebody else rewrote has to read as changed.
    ///
    /// git is what makes that safe rather than lucky: it does not restore
    /// mtimes, so a file it rewrites carries the moment it rewrote it. This is
    /// that case, with the mtime moved by hand because a test cannot wait a
    /// second for one.
    #[test]
    fn a_task_rewritten_behind_our_back_is_read_again_rather_than_remembered() {
        let store = scratch("cache-rewrite");
        let t = UNIX_EPOCH + Duration::from_secs(1_800_000_000);

        titled(&store, "wsp-001", "as filed", t);
        assert_eq!(title_of(&store, "wsp-001"), "as filed");

        titled(&store, "wsp-001", "as rewritten", t + Duration::from_secs(1));
        assert_eq!(
            title_of(&store, "wsp-001"),
            "as rewritten",
            "a panel would go on painting the old title until something else in the store moved",
        );

        let _ = fs::remove_dir_all(&store.root);
    }

    /// The saving, stated as the only thing a test can see from outside: with
    /// the stamp unchanged, the answer comes from the last parse and the file
    /// is never opened.
    ///
    /// It is asserted by making that visible — the bytes are replaced with a
    /// different title of the same length and the mtime put back — so a store
    /// that had re-read would answer `rewritten` and this would fail. That is
    /// also an honest statement of the window the stamp leaves: a rewrite of
    /// identical length inside one timestamp tick. On the store's own writes it
    /// is not reachable — `write_atomic` renames a fresh temp file into place
    /// and mtimes here are nanoseconds — which is why the pair is enough.
    #[test]
    fn a_task_whose_bytes_have_not_moved_is_not_opened_again() {
        let store = scratch("cache-hit");
        let t = UNIX_EPOCH + Duration::from_secs(1_800_000_000);

        titled(&store, "wsp-001", "aaaaaaaaa", t);
        assert_eq!(title_of(&store, "wsp-001"), "aaaaaaaaa");

        titled(&store, "wsp-001", "bbbbbbbbb", t);
        assert_eq!(
            title_of(&store, "wsp-001"),
            "aaaaaaaaa",
            "the file was opened and parsed again even though nothing about it had changed",
        );

        let _ = fs::remove_dir_all(&store.root);
    }

    /// What is kept is what is there. A task archived or a project removed is a
    /// file nothing will ask for again, and a cache that only ever grew would
    /// be a leak in the one process that never exits — the panel.
    #[test]
    fn a_task_that_has_left_the_store_is_not_kept_alive_by_the_cache() {
        let store = scratch("cache-prune");
        let t = UNIX_EPOCH + Duration::from_secs(1_800_000_000);

        titled(&store, "wsp-001", "stays", t);
        titled(&store, "wsp-002", "goes", t);
        assert_eq!(store.tasks().len(), 2);

        fs::remove_file(store.tasks_dir().join("wsp-002.md")).unwrap();
        assert_eq!(store.tasks().len(), 1, "an archived task went on being drawn");
        assert_eq!(
            store.tasks_cache.borrow().len(),
            1,
            "the cache still holds the file, and would hold every task the store has ever had",
        );

        let _ = fs::remove_dir_all(&store.root);
    }
}
