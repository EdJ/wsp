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
//!
//! **The synced cache** (wsp-465, [`crate::hub_cache`]). The worker also holds
//! a subscription to the hub's stream: a `ref.moved` is a turn at once, and
//! `$WSP_STATE/hub/stream.json` keeps its position. Each turn reads the hub's
//! heads of every workspace the user is a member of and sorts them: a row that
//! is this spoke's own is synced whole as above; any other is kept as a head
//! (title, status, holder, allocation) and listed as a stub nothing pushes. A
//! feed that lost its connection is dropped, not redialled from its last id: the
//! next turn opens one from the hub's head when it is more than
//! [`hub_cache::GAP`] events behind, and the heads diff is the catch-up.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use wsp_api::{Answer, Outcome};
use wsp_client::{Client, Credential, Feed, Item, ObjectSource, SyncError, SyncReport, HUBS_FILE};
use wsp_objects::{Object, ObjectId, Refs, WorkspaceId};
use wsp_store_contract::{At, Entity, EventId, EventKind, Principal, RowId, Subject, UserName};

use crate::door::{user_of, Applied, Door, Heads, Refusal};
use crate::hub_cache::{self, Cache, Stream};
use crate::reconcile::{Lease, Merging, NoLease, Objects};
use crate::store::Store;

/// The record a push sends for `subject`'s row: its file whole, with the
/// synced head as its parent (`None` for a row never synced). `None` where the
/// subject is not a row the spoke has a file for, or the row is private.
pub(crate) fn push_record(store: &Store, subject: &Subject, workspace: WorkspaceId, by: Principal, at: At) -> Option<Object> {
    let Subject::Row(id) = subject else { return None };
    let t = store.task(&id.0)?;
    // A private row never leaves; a row kept as a head only (wsp-465) is the
    // hub's, and its stub is nothing to send back.
    if t.workspace.is_private() || hub_cache::is_head(store, &id.0) {
        return None;
    }
    let text = std::fs::read_to_string(store.task_path(&id.0)).ok()?;
    let row = crate::mirror::shared_row(&t, &text)?;
    let parent = Heads::read(store).head(subject).copied();
    Some(Object::Record { workspace, subject: subject.clone(), entity: Entity::Row(row), parent, by, at })
}

/// The record a push sends for a shared list: its file whole as `body`, with
/// the synced head as parent. `None` for a list this spoke has no file for or
/// one that is private.
pub(crate) fn push_list_record(store: &Store, subject: &Subject, workspace: WorkspaceId, by: Principal, at: At) -> Option<Object> {
    let Subject::List(id) = subject else { return None };
    let w = store.worklist(&id.0)?;
    if w.workspace.is_private() {
        return None;
    }
    let lists = [w];
    let texts = crate::mirror::list_texts_of(store, &lists);
    let entity = crate::mirror::lists(&lists, &store.tasks(), &texts, &BTreeMap::new())
        .into_iter()
        .find(|e| matches!(e, Entity::List(l) if l.id == *id))?;
    let parent = Heads::read(store).head(subject).copied();
    Some(Object::Record { workspace, subject: subject.clone(), entity, parent, by, at })
}

/// The user who published shared list `list`: the `by` of the first record in
/// the chain behind its synced head, as far back as this spoke holds objects
/// (wsp-467: a list nobody is allocated is driven by a spoke of its publisher).
/// `None` for a list never synced.
pub(crate) fn publisher(store: &Store, list: &str) -> Option<String> {
    let subject = Subject::List(wsp_store_contract::ListId(list.to_string()));
    let mut at = *Heads::read(store).head(&subject)?;
    let objects = ObjectDir::of(store);
    let mut by = None;
    let mut seen = BTreeSet::new();
    while seen.insert(at) {
        let Some(Object::Record { by: b, parent, .. }) = Objects::object(&objects, &at) else { break };
        by = Some(user_of(&b));
        match parent {
            Some(p) => at = p,
            None => break,
        }
    }
    by
}

/// The shared workspaces the login's user is a member of, as the hub serves
/// them: the first item of a scoped subscription is a snapshot that holds
/// only the caller's workspaces (wsp-174), and its `workspaces` are the names.
/// Empty where the hub does not answer in time.
fn member_workspaces(client: &Client) -> Vec<WorkspaceId> {
    let mut feed = client.subscribe(None);
    match feed.next_within(DIAL_WAIT) {
        Some(wsp_client::Item::State { state, .. }) => {
            state.workspaces.keys().map(|n| WorkspaceId(format!("shared:{}", n.0))).collect()
        }
        _ => Vec::new(),
    }
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

/// Take the row `id`, kept as a head only, whole from the hub (`wsp show --fetch`).
pub(crate) fn fetch(store: &Store, id: &str) -> Result<(), String> {
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("no HOME")?);
    if !home.join(HUBS_FILE).exists() {
        return Err("not logged in to a hub: `wsp login <url>`".into());
    }
    Worker::new(home).fetch(store, id)
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
    /// Rows of other people's kept as a head (wsp-465).
    pub(crate) heads: usize,
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
    /// The hub's stream for this spoke's workspaces (wsp-465): a wake-up for
    /// the turn, and the position `stream.json` keeps.
    feed: Option<Feed>,
    /// Rows to take whole whatever they are (`wsp show --fetch`).
    forced: BTreeSet<Subject>,
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
            feed: None,
            forced: BTreeSet::new(),
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
            // Between turns: the hub's stream; a moved ref is a turn at once.
            if self.listen(store, POLL) {
                due = Instant::now();
            }
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
                self.renew_leases(store);
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

    /// The hub leases of the claims here whose seats are alive, renewed at most
    /// hourly each (wsp-466, [`crate::hub_lease::renew_due`]), and the drive
    /// leases of the lists this spoke drives (wsp-467).
    fn renew_leases(&mut self, store: &Store) {
        let Some(client) = self.client.as_mut() else { return };
        let now = crate::util::epoch_secs();
        crate::hub_lease::renew_all(store, client, now);
        // And the drive leases of the shared lists this spoke drives (wsp-467).
        crate::hub_lease::renew_drives(store, client, now);
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
            "last": last.map(|t| serde_json::json!({ "sent": t.sent, "moved": t.moved, "adopted": t.adopted, "reconciled": t.reconciled, "heads": t.heads })),
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

    /// The user this connection is, the hub's event head now, and the shared
    /// workspaces the user is a member of (`state`'s identity domain). A hub
    /// that will not say leaves the workspaces to the stream's.
    fn members(&mut self) -> Result<(Vec<String>, u64), String> {
        let client = self.client()?;
        let head = client.head().0;
        let me = user_of(client.by());
        let reply = client.request("state", serde_json::json!({})).map_err(|e| e.to_string())?;
        let Outcome::Ok(Answer::State { state, at }) = reply.outcome else { return Ok((Vec::new(), head)) };
        let user = UserName(me);
        let names = state
            .members
            .iter()
            .filter(|(w, users)| users.contains(&user) && state.workspaces.contains_key(*w))
            .map(|(w, _)| format!("shared:{}", w.0))
            .collect();
        Ok((names, at.0))
    }

    /// Subscribe to the hub's stream where there is no feed: from the position
    /// `stream.json` holds when it is within [`hub_cache::GAP`] of the hub's
    /// head, else from the head (the turn's heads diff stands in for what was
    /// skipped, one frame and not an event per change).
    fn open_feed(&mut self, store: &Store) -> Result<(), String> {
        if self.feed.is_some() {
            return Ok(());
        }
        let (names, head) = self.members()?;
        let mut stream = Stream::read(store);
        stream.workspaces.extend(names);
        let (from, gap) = hub_cache::resume(stream.position, head);
        stream.position = Some(from);
        let _ = stream.write(store);
        if gap {
            crate::store::log_line(store, &format!("hub-sync: the stream resumes at {from}, a heads diff in place of the events missed"));
        }
        let feed = self.client()?.subscribe(Some(EventId(from)));
        self.feed = Some(feed);
        Ok(())
    }

    /// Hear the stream for up to `wait`; whether a ref moved. The position and
    /// the workspaces heard of are kept in `stream.json`.
    fn listen(&mut self, store: &Store, wait: Duration) -> bool {
        let Some(feed) = self.feed.as_mut() else {
            std::thread::sleep(wait);
            return false;
        };
        // A feed that lost its connection would dial again and ask for every
        // event since its last: not the way back from an outage. It is dropped,
        // and the turn that follows opens one through `hub_cache::resume`.
        if feed.why_silent().is_some() || feed.reconnects() > 0 {
            self.feed = None;
            std::thread::sleep(wait);
            return false;
        }
        let end = Instant::now() + wait;
        let mut stream = Stream::read(store);
        let before = stream.clone();
        let mut woke = false;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            match feed.next_within(left) {
                Some(Item::Event(e)) => {
                    if e.kind == EventKind::LeaseExpired {
                        crate::hub_lease::follow_expired(store, &e.payload);
                    }
                    if e.kind == EventKind::RefMoved {
                        woke = true;
                        if let Some(w) = e.payload.get("workspace").and_then(|w| w.as_str()) {
                            stream.workspaces.insert(w.to_string());
                        }
                    }
                }
                Some(Item::State { .. } | Item::Resync { .. } | Item::Resumed { .. }) => woke = true,
                Some(Item::Gone { .. }) => {}
                None => break,
            }
            if Instant::now() >= end || feed.why_silent().is_some() {
                break;
            }
        }
        if let Some(last) = feed.last_id() {
            stream.position = Some(last.0);
        }
        if feed.refused().is_some() {
            self.feed = None;
        }
        if stream != before {
            let _ = stream.write(store);
        }
        woke
    }

    /// Every shared workspace once. `Err` is the hub not answering; a
    /// workspace it refuses is said in the log once and the rest go on.
    fn sync_all(&mut self, store: &Store) -> Result<Turned, String> {
        self.open_feed(store)?;
        let mut workspaces: BTreeMap<String, Vec<crate::model::Task>> =
            crate::mirror::by_shared_workspace(store.tasks()).into_iter().map(|(w, t)| (w.to_string(), t)).collect();
        for w in Stream::read(store).workspaces {
            workspaces.entry(w).or_default();
        }
        for w in store.worklists().iter().filter(|w| !w.workspace.is_private()) {
            workspaces.entry(w.workspace.to_string()).or_default();
        }
        let mut turned = Turned::default();
        // Every shared workspace the login's user belongs to, too: a member
        // with nothing of its own there still takes what the others pushed.
        let by = {
            let c = self.client()?;
            for w in member_workspaces(c) {
                if w.0.starts_with("shared:") {
                    workspaces.entry(w.0).or_default();
                }
            }
            c.by().clone()
        };
        for (ws, tasks) in workspaces {
            self.sync_workspace(store, &ws, &tasks, &by, &mut turned)?;
        }
        Ok(turned)
    }

    /// One shared workspace: the hub's heads sorted into the rows kept whole
    /// and the heads of the rest ([`hub_cache::classify`]), then the sync of the
    /// whole ones.
    fn sync_workspace(
        &mut self,
        store: &Store,
        ws: &str,
        tasks: &[crate::model::Task],
        by: &Principal,
        turned: &mut Turned,
    ) -> Result<(), String> {
        let wid = WorkspaceId(ws.to_string());
        let objects = ObjectDir::of(store);
        let me = user_of(by);
        let forced = self.forced.clone();
        let client = self.client()?;
        let hub = match client.sync_heads(&wid) {
            Ok(r) => r,
            Err(e) if unreachable(&e) => return Err(e.to_string()),
            Err(e) => {
                self.say_once(store, format!("hub-sync: {ws} did not sync: {e}"));
                return Ok(());
            }
        };
        let classified = match hub_cache::classify(store, client, ws, &hub, &me, by, &forced) {
            Ok(c) => c,
            Err(why) => {
                self.say_once(store, format!("hub-sync: the heads of {ws} were not read: {why}"));
                return Ok(());
            }
        };
        turned.heads += classified.heads;
        // The rows this spoke syncs whole: its own files (not heads) and the
        // ones just taken on.
        let cache = Cache::read(store);
        let mut included: BTreeSet<Subject> = classified.whole;
        let heads = Heads::read(store);
        let mut local = Refs::new(wid.clone());
        // What this sync pushes: the record's id and the file it was made of.
        let mut pushing: BTreeMap<Subject, (ObjectId, Option<ObjectId>)> = BTreeMap::new();
        for t in tasks.iter().filter(|t| !cache.is_head(&t.id) && store.task(&t.id).is_some()) {
            let subject = Subject::Row(RowId(t.id.clone()));
            included.insert(subject.clone());
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
        for w in store.worklists().iter().filter(|w| w.workspace.to_string() == ws) {
            let subject = Subject::List(wsp_store_contract::ListId(w.id.clone()));
            included.insert(subject.clone());
            let digest = crate::door::list_digest(store, &w.id);
            let synced = heads.head(&subject).copied();
            if synced.is_some() && heads.file(&subject) == digest {
                local.set(subject, synced.expect("checked"));
                continue;
            }
            let id = match self.minted.get(&subject) {
                Some((d, id)) if *d == digest && objects.has(id) => *id,
                _ => {
                    let Some(obj) = push_list_record(store, &subject, wid.clone(), by.clone(), At(now_ms())) else { continue };
                    let (id, bytes) = wsp_objects::encode(&obj);
                    if let Err(e) = objects.put(&id, &bytes) {
                        self.say_once(store, format!("hub-sync: cannot keep {}'s object: {e}", w.id));
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
        let report = match client.sync_skipping(&wid, &local, &objects, &|s| matches!(s, Subject::Row(_)) && !included.contains(s)) {
            Ok(r) => r,
            Err(SyncError::Request(e)) if unreachable(&e) => return Err(e.to_string()),
            Err(e) => {
                self.say_once(store, format!("hub-sync: {ws} did not sync: {e}"));
                return Ok(());
            }
        };
        self.keep(store, &objects, &heads, &pushing, &report, by, turned);
        Ok(())
    }

    /// `wsp show <id> --fetch` (wsp-465): take a row kept as a head whole, now.
    pub(crate) fn fetch(&mut self, store: &Store, id: &str) -> Result<(), String> {
        let cache = Cache::read(store);
        let head = cache.get(id).ok_or_else(|| format!("{id} is not kept as a head only"))?;
        let ws = head.workspace.clone();
        self.forced.insert(Subject::Row(RowId(id.to_string())));
        let by = self.client()?.by().clone();
        let tasks: Vec<crate::model::Task> = store.tasks().into_iter().filter(|t| t.workspace.to_string() == ws).collect();
        self.sync_workspace(store, &ws, &tasks, &by, &mut Turned::default())?;
        match hub_cache::is_head(store, id) || store.task(id).is_none() {
            true => Err(format!("{id} was not fetched; the hub's answer is in `cycle.log`")),
            false => Ok(()),
        }
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

