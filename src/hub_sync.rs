//! A spoke's sync with the hub, on the files' side (wsp-176): the record a
//! push sends for a pending row, and what follows a `stale`.
//!
//! A push sends a row's file as a record whose parent is the head the spoke
//! last synced ([`push_record`]); the hub moves its ref only from that head
//! (wsp-459). Where the hub has moved since, the move comes back `stale` with
//! the hub's head, and [`after_stale`] takes that head through the door with
//! [`Merging`]: the merge is written to the file, the hub's head becomes the
//! synced one, and the next [`push_record`] is the merge as a new object whose
//! parent is theirs, pushed as usual.
//!
//! Who runs these is the spoke's sync worker (wsp-464, wsp-469); the lease
//! that lets a spoke move a row's status is wsp-466's, passed in as a [`Lease`].
//!
//! **The worker** (wsp-464, the design's "Standalone and spoke mode"). A daemon
//! whose home holds `~/.config/wsp/hubs.json` starts one thread, [`THREAD`]
//! ([`start`]); without the file nothing starts and nothing is written. Each
//! turn (every [`TURN`], [`TURN_ENV`] to change it, and at once when a task
//! file changes) it takes every task of a shared workspace
//! ([`crate::mirror::by_shared_workspace`]), makes a record of each one whose
//! file moved since its synced head ([`push_record`], the mirror's projection),
//! encodes it, keeps it in `$WSP_STATE/hub/objects/`, and syncs the workspace
//! through [`wsp_client::Client::sync`] from the synced heads ([`Heads`]). A
//! private task is in no workspace it syncs, and is never projected.
//!
//! What comes back: a moved head is the synced one; the hub's objects are kept
//! and an adopted head goes through the door, oldest first; a `stale` move or a
//! diverged head goes to wsp-176's reconcile ([`after_stale`]), never over the
//! hub. An unreachable hub is a state ([`Link`]): one line in `cycle.log` when
//! it changes, and another try after a wait that doubles from [`BACKOFF_MIN`]
//! to [`BACKOFF_MAX`]. `$WSP_STATE/hub/sync.json` says how many turns ran and
//! where the link stands.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use wsp_client::{Client, Credential, ObjectSource, SyncError, SyncReport, HUBS_FILE};
use wsp_objects::{Object, ObjectId, Refs, WorkspaceId};
use wsp_store_contract::{At, Entity, Principal, RowId, Subject};

use crate::door::{Applied, Door, Heads, Refusal};
use crate::reconcile::{Lease, Merging, NoLease, Objects};
use crate::store::Store;

/// The record a push sends for `subject`'s row: its file whole, with the
/// synced head as its parent (`None` for a row never synced). `None` where the
/// subject is not a row the spoke has a file for, or the row is private.
pub(crate) fn push_record(store: &Store, subject: &Subject, workspace: WorkspaceId, by: Principal, at: At) -> Option<Object> {
    let Subject::Row(id) = subject else { return None };
    let t = store.task(&id.0)?;
    if t.workspace.is_private() {
        return None;
    }
    let text = std::fs::read_to_string(store.task_path(&id.0)).ok()?;
    let row = crate::mirror::shared_row(&t, &text)?;
    let parent = Heads::read(store).head(subject).copied();
    Some(Object::Record { workspace, subject: subject.clone(), entity: Entity::Row(row), parent, by, at })
}

/// A push refused `stale`: the hub's head for the subject, read from
/// `objects` (what the sync fetched), merged into the file by the door.
pub(crate) fn after_stale(
    store: &Store,
    stale: &wsp_client::Stale,
    objects: &dyn Objects,
    lease: &dyn Lease,
    by: &Principal,
) -> Result<Applied, Refusal> {
    let head = stale.head.ok_or_else(|| Refusal::NotYet("a stale with no head on the hub".into()))?;
    let theirs = objects.object(&head).ok_or_else(|| Refusal::NotYet(format!("the hub's head {} was not fetched", head.hex())))?;
    let merging = Merging { objects, lease };
    Door::new(store, &merging).import(&theirs, by)
}

/// The worker's thread name.
pub(crate) const THREAD: &str = "hub-sync";

/// How often a turn runs with nothing changed.
pub(crate) const TURN: Duration = Duration::from_secs(10);

/// Milliseconds between turns, for a test that cannot wait ten seconds.
pub(crate) const TURN_ENV: &str = "WSP_HUB_SYNC_MS";

/// The first wait after the hub did not answer, doubled per miss up to [`BACKOFF_MAX`].
pub(crate) const BACKOFF_MIN: Duration = Duration::from_secs(1);
pub(crate) const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// How often the task files are looked at for a change between turns.
const POLL: Duration = Duration::from_millis(250);

/// How long a dial waits for the hub.
const DIAL_WAIT: Duration = Duration::from_secs(3);

/// Start the worker when `home` holds a login; `None` (no thread, no file)
/// without one.
pub(crate) fn start(store: &Store) -> Option<std::thread::JoinHandle<()>> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    if !home.join(HUBS_FILE).exists() {
        return None;
    }
    let turn = std::env::var(TURN_ENV).ok().and_then(|v| v.trim().parse().ok()).map_or(TURN, Duration::from_millis);
    let (root, state) = (store.root.clone(), store.state.clone());
    std::thread::Builder::new()
        .name(THREAD.into())
        .spawn(move || {
            let store = Store::at(root, state);
            Worker::new(home).run(&store, turn)
        })
        .ok()
}

/// Where the hub stands for this spoke.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Link {
    /// Not dialled yet (nothing shared to sync, or the first turn to come).
    Unknown,
    Up,
    /// It did not answer; why, as the dial or the request said.
    Down(String),
}

impl Link {
    fn word(&self) -> &'static str {
        match self {
            Link::Unknown => "unknown",
            Link::Up => "up",
            Link::Down(_) => "down",
        }
    }
}

/// The objects this spoke holds for the hub: `$WSP_STATE/hub/objects/<hex>`,
/// each the canonical bytes its id hashes, written once and never re-encoded.
pub(crate) struct ObjectDir(PathBuf);

impl ObjectDir {
    pub(crate) fn of(store: &Store) -> ObjectDir {
        ObjectDir(store.state.join("hub").join("objects"))
    }

    fn path(&self, id: &ObjectId) -> PathBuf {
        self.0.join(id.hex())
    }

    pub(crate) fn has(&self, id: &ObjectId) -> bool {
        self.path(id).exists()
    }

    pub(crate) fn put(&self, id: &ObjectId, bytes: &[u8]) -> std::io::Result<()> {
        let path = self.path(id);
        if path.exists() {
            return Ok(());
        }
        std::fs::create_dir_all(&self.0)?;
        let tmp = self.0.join(format!(".{}.{}.tmp", id.hex(), std::process::id()));
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &path)
    }
}

impl ObjectSource for ObjectDir {
    fn object(&self, id: &ObjectId) -> Option<Vec<u8>> {
        std::fs::read(self.path(id)).ok()
    }
}

impl Objects for ObjectDir {
    fn object(&self, id: &ObjectId) -> Option<Object> {
        wsp_objects::decode(&std::fs::read(self.path(id)).ok()?).ok()
    }
}

/// The SHA-256 of a row's file as it is now; `None` for no file.
fn file_digest(store: &Store, id: &str) -> Option<ObjectId> {
    std::fs::read(store.task_path(id)).ok().map(|b| ObjectId::of(&b))
}

/// What the task files are now, cheaply: each file's name, size and mtime.
fn fingerprint(store: &Store) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    if let Ok(dir) = std::fs::read_dir(store.tasks_dir()) {
        let mut seen: Vec<(String, u64, Option<std::time::SystemTime>)> = dir
            .filter_map(|e| {
                let e = e.ok()?;
                let m = e.metadata().ok()?;
                Some((e.file_name().to_string_lossy().into_owned(), m.len(), m.modified().ok()))
            })
            .collect();
        seen.sort();
        seen.hash(&mut h);
    }
    h.finish()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// A request that failed because the hub is not there, as against one it refused.
fn unreachable(e: &wsp_client::Error) -> bool {
    matches!(e, wsp_client::Error::Timeout | wsp_client::Error::Lost(_) | wsp_client::Error::Silent(_))
}

/// One turn's tally.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Turned {
    /// Objects sent to the hub.
    pub(crate) sent: usize,
    /// Heads the hub moved to this spoke's.
    pub(crate) moved: usize,
    /// Heads taken from the hub through the door.
    pub(crate) adopted: usize,
    /// Moves refused `stale` and heads that diverged, handed to reconcile.
    pub(crate) reconciled: usize,
}

/// The sync worker: its connection, where the link stands, and what it said.
pub(crate) struct Worker {
    home: PathBuf,
    client: Option<Client>,
    pub(crate) link: Link,
    backoff: Duration,
    retry_at: Option<Instant>,
    /// Per subject, the file digest a record was made for and its id, so a row
    /// still pending next turn sends the same object, not a new one per turn.
    minted: BTreeMap<Subject, (Option<ObjectId>, ObjectId)>,
    /// Lines already in the log, so a refusal that repeats each turn is said once.
    said: BTreeSet<String>,
    pub(crate) turns: u64,
}

impl Worker {
    pub(crate) fn new(home: PathBuf) -> Worker {
        Worker {
            home,
            client: None,
            link: Link::Unknown,
            backoff: BACKOFF_MIN,
            retry_at: None,
            minted: BTreeMap::new(),
            said: BTreeSet::new(),
            turns: 0,
        }
    }

    /// Turns until the process ends: one every `turn`, one at once when a task
    /// file changes, and while the hub is down one per backoff.
    fn run(mut self, store: &Store, turn: Duration) {
        let mut seen = fingerprint(store);
        let mut due = Instant::now();
        loop {
            let now = Instant::now();
            let ready = match self.retry_at {
                Some(at) => now >= at,
                None => now >= due || fingerprint(store) != seen,
            };
            if ready {
                seen = fingerprint(store);
                let _ = self.turn(store);
                due = Instant::now() + turn;
            }
            std::thread::sleep(POLL);
        }
    }

    /// One turn: sync every shared workspace, then move the link's state.
    pub(crate) fn turn(&mut self, store: &Store) -> Result<Turned, String> {
        self.turns += 1;
        let out = self.sync_all(store);
        match &out {
            Ok(_) => {
                match &self.link {
                    Link::Down(_) => crate::store::log_line(store, &format!("hub-sync: the hub at {} answers again", self.url())),
                    Link::Unknown if self.client.is_some() => {
                        crate::store::log_line(store, &format!("hub-sync: syncing with the hub at {}", self.url()))
                    }
                    _ => {}
                }
                if self.client.is_some() {
                    self.link = Link::Up;
                }
                self.backoff = BACKOFF_MIN;
                self.retry_at = None;
            }
            Err(why) => {
                self.client = None;
                if !matches!(self.link, Link::Down(_)) {
                    crate::store::log_line(
                        store,
                        &format!(
                            "hub-sync: the hub at {} is unreachable ({why}); trying again from {}s to {}s apart",
                            self.url(),
                            BACKOFF_MIN.as_secs(),
                            BACKOFF_MAX.as_secs()
                        ),
                    );
                }
                self.link = Link::Down(why.clone());
                self.retry_at = Some(Instant::now() + self.backoff);
                self.backoff = (self.backoff * 2).min(BACKOFF_MAX);
            }
        }
        self.write_status(store, out.as_ref().ok());
        out
    }

    fn url(&self) -> String {
        wsp_client::hub_url_in(None, &self.home).unwrap_or_else(|_| "(no url)".into())
    }

    fn say_once(&mut self, store: &Store, line: String) {
        if self.said.insert(line.clone()) {
            crate::store::log_line(store, &line);
        }
    }

    /// `$WSP_STATE/hub/sync.json`: turns run, the link, and the last turn's tally.
    fn write_status(&self, store: &Store, last: Option<&Turned>) {
        let dir = store.state.join("hub");
        let v = serde_json::json!({
            "turns": self.turns,
            "link": self.link.word(),
            "why": match &self.link { Link::Down(w) => Some(w.clone()), _ => None },
            "last": last.map(|t| serde_json::json!({ "sent": t.sent, "moved": t.moved, "adopted": t.adopted, "reconciled": t.reconciled })),
        });
        let _ = std::fs::create_dir_all(&dir);
        let tmp = dir.join(format!(".sync.json.{}.tmp", std::process::id()));
        if std::fs::write(&tmp, v.to_string()).is_ok() {
            let _ = std::fs::rename(&tmp, dir.join("sync.json"));
        }
    }

    /// The connection, dialled again where there is none: the login is read
    /// each time, so a new `wsp login` is the next dial's.
    fn client(&mut self) -> Result<&mut Client, String> {
        if self.client.is_none() {
            let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
            let url = wsp_client::hub_url_in(env(wsp_client::HUB_URL_ENV), &self.home).map_err(|e| e.to_string())?;
            let cred = Credential::for_hub_in(env(wsp_client::HUB_TOKEN_ENV), &self.home).map_err(|e| e.to_string())?;
            let ca = wsp_client::hub_ca_in(env(wsp_client::HUB_CA_ENV), &self.home).map_err(|e| e.to_string())?;
            let c = Client::dial_hub_ca(&url, cred, ca.as_deref(), DIAL_WAIT).map_err(|e| e.to_string())?;
            self.client = Some(c);
        }
        Ok(self.client.as_mut().expect("dialled above"))
    }

    /// Every shared workspace once. `Err` is the hub not answering; a
    /// workspace it refuses is said in the log once and the rest go on.
    fn sync_all(&mut self, store: &Store) -> Result<Turned, String> {
        let workspaces = crate::mirror::by_shared_workspace(store.tasks());
        let mut turned = Turned::default();
        if workspaces.is_empty() {
            return Ok(turned);
        }
        let by = self.client()?.by().clone();
        let objects = ObjectDir::of(store);
        for (ws, tasks) in workspaces {
            let wid = WorkspaceId(ws.to_string());
            let heads = Heads::read(store);
            let mut local = Refs::new(wid.clone());
            // What this sync pushes: the record's id and the file it was made of.
            let mut pushing: BTreeMap<Subject, (ObjectId, Option<ObjectId>)> = BTreeMap::new();
            for t in &tasks {
                let subject = Subject::Row(RowId(t.id.clone()));
                let digest = file_digest(store, &t.id);
                let synced = heads.head(&subject).copied();
                if synced.is_some() && heads.file(&subject) == digest {
                    local.set(subject, synced.expect("checked"));
                    continue;
                }
                let id = match self.minted.get(&subject) {
                    Some((d, id)) if *d == digest && objects.has(id) => *id,
                    _ => {
                        let Some(obj) = push_record(store, &subject, wid.clone(), by.clone(), At(now_ms())) else { continue };
                        let (id, bytes) = wsp_objects::encode(&obj);
                        if let Err(e) = objects.put(&id, &bytes) {
                            self.say_once(store, format!("hub-sync: cannot keep {}'s object: {e}", t.id));
                            continue;
                        }
                        self.minted.insert(subject.clone(), (digest, id));
                        id
                    }
                };
                local.set(subject.clone(), id);
                pushing.insert(subject, (id, digest));
            }
            let client = self.client()?;
            let report = match client.sync(&wid, &local, &objects) {
                Ok(r) => r,
                Err(SyncError::Request(e)) if unreachable(&e) => return Err(e.to_string()),
                Err(e) => {
                    self.say_once(store, format!("hub-sync: {ws} did not sync: {e}"));
                    continue;
                }
            };
            self.keep(store, &objects, &heads, &pushing, &report, &by, &mut turned);
        }
        Ok(turned)
    }

    /// What a sync's report leaves in the store: the moved heads synced, the
    /// hub's objects kept, its heads through the door, the rest reconciled.
    #[allow(clippy::too_many_arguments)]
    fn keep(
        &mut self,
        store: &Store,
        objects: &ObjectDir,
        before: &Heads,
        pushing: &BTreeMap<Subject, (ObjectId, Option<ObjectId>)>,
        report: &SyncReport,
        by: &Principal,
        turned: &mut Turned,
    ) {
        turned.sent += report.sent;
        if !report.moved.is_empty() {
            let mut heads = Heads::read(store);
            for s in &report.moved {
                if let Some((id, digest)) = pushing.get(s) {
                    heads.set(s, *id, *digest);
                    self.minted.remove(s);
                    turned.moved += 1;
                }
            }
            if let Err(e) = heads.write(store) {
                self.say_once(store, format!("hub-sync: cannot write the synced heads: {e}"));
            }
        }
        for blob in &report.received {
            if let Err(e) = objects.put(&blob.id, &blob.bytes) {
                self.say_once(store, format!("hub-sync: cannot keep the hub's object {}: {e}", blob.id.hex()));
            }
        }
        let merging = Merging { objects, lease: &NoLease };
        let door = Door::new(store, &merging);
        for s in &report.adopted {
            let Some(head) = report.refs.get(s).copied() else { continue };
            // The hub's line from the head this spoke synced, oldest first, so
            // each import follows the one before it.
            let mut line = Vec::new();
            let mut at = Some(head);
            while let Some(id) = at {
                if Some(&id) == before.head(s) {
                    break;
                }
                let Some(obj) = Objects::object(objects, &id) else { break };
                at = obj.parent().copied();
                line.push(obj);
            }
            for obj in line.iter().rev() {
                match door.import(obj, by) {
                    Ok(_) => turned.adopted += 1,
                    Err(r) => {
                        self.say_once(store, format!("hub-sync: the hub's {s:?} was not taken: {r:?}"));
                        break;
                    }
                }
            }
        }
        // A move refused `stale`, and a head both sides wrote: the hub's head
        // through reconcile, which writes the merge; the next turn pushes it.
        let mut stale: Vec<wsp_client::Stale> = report.stale.clone();
        for s in &report.diverged {
            if let Some(head) = hub_head_in(&report.received, s) {
                stale.push(wsp_client::Stale { subject: s.clone(), from: before.head(s).copied(), head: Some(head) });
            }
        }
        for st in &stale {
            self.minted.remove(&st.subject);
            match after_stale(store, st, objects, &NoLease, by) {
                Ok(_) => turned.reconciled += 1,
                Err(r) => self.say_once(store, format!("hub-sync: {:?} is stale and was not reconciled: {r:?}", st.subject)),
            }
        }
    }
}

/// The hub's head for `subject` among what a sync fetched: its object that no
/// other fetched object of the subject names as parent.
fn hub_head_in(received: &[wsp_api::Blob], subject: &Subject) -> Option<ObjectId> {
    let of: Vec<(ObjectId, Option<ObjectId>)> = received
        .iter()
        .filter_map(|b| wsp_objects::decode(&b.bytes).ok().map(|o| (b.id, o)))
        .filter(|(_, o)| o.subject() == subject)
        .map(|(id, o)| (id, o.parent().copied()))
        .collect();
    let parents: BTreeSet<ObjectId> = of.iter().filter_map(|(_, p)| *p).collect();
    of.iter().map(|(id, _)| *id).find(|id| !parents.contains(id))
}

