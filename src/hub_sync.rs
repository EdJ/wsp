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
//!
//! **The offline journal** (wsp-469, [`crate::hub_journal`]). Every record the
//! worker makes is appended to `$WSP_STATE/hub/journal/` first, then pushed, and
//! leaves it once the hub has it. A turn that cannot reach the hub still makes
//! the records of the rows that moved ([`Worker::journal_offline`]); the turn
//! that reaches it again settles what the journal holds by each row's lease
//! ([`Worker::settle`]) before anything of it is pushed.
//!
//! **Messages** (wsp-470, the design's "Message routing and the user
//! primary"). Each turn that reaches the hub also takes the messages the hub
//! queued for this user ([`Worker::take_messages`]): each is handed to the
//! orders machine as a tell to the seat its address names here, from the
//! sending seat named with its user (or `person:<user>`), and acknowledged, so
//! the hub never hands it out again. An `order.queued` for this user on the
//! stream is a turn at once. [`crate::cmd_message`] keeps what was received
//! (`$WSP_STATE/hub/inbox.json`), so a message taken again after a lost ack
//! is acknowledged and not told twice.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use wsp_api::{Answer, Blob, Move, Outcome, RefMove};
use wsp_client::{Client, Credential, Feed, Item, ObjectSource, SyncError, SyncReport, HUBS_FILE};
use wsp_objects::{Object, ObjectId, Refs, WorkspaceId};
use wsp_store_contract::{At, Entity, EventId, EventKind, ListId, Principal, RowId, Subject, Token, UserName};

use crate::door::{user_of, Applied, Door, Heads, Refusal};
use crate::hub_cache::{self, Cache, Stream};
use crate::hub_journal::{self, Entry, Journal, Leased, Side, Sides};
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
    let home = wsp_client::hub_home()?;
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
    let home = wsp_client::hub_home().ok_or("no HOME")?;
    if !home.join(HUBS_FILE).exists() {
        return Err("not logged in to a hub: `wsp login <url>`".into());
    }
    Worker::new(home).fetch(store, id)
}

/// A connection to the hub the login in `home` names (the environment's
/// `WSP_HUB_*` before it), as the worker dials it and a `wsp tell` through the
/// hub does (wsp-470).
pub(crate) fn dial(home: &std::path::Path) -> Result<Client, String> {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let url = wsp_client::hub_url_in(env(wsp_client::HUB_URL_ENV), home).map_err(|e| e.to_string())?;
    let cred = Credential::for_hub_in(env(wsp_client::HUB_TOKEN_ENV), home).map_err(|e| e.to_string())?;
    let ca = wsp_client::hub_ca_in(env(wsp_client::HUB_CA_ENV), home).map_err(|e| e.to_string())?;
    Client::dial_hub_ca(&url, cred, ca.as_deref(), DIAL_WAIT).map_err(|e| e.to_string())
}

/// How a message the hub delivered is handed to a seat here: to `seat`, the
/// dressed `text`, from `from`, under the idempotency `key`; the order's id, or
/// why not.
pub(crate) type Hand<'a> = dyn Fn(&wsp_store_contract::SeatId, &str, &Principal, &str) -> Result<String, String> + 'a;

/// What keeps a tell for a seat's next idle under `WSP_ORDERS=files`: the
/// seats layer's held-tell spool, which this layer cannot reach, so the daemon
/// lends it ([`install_spool`]).
pub(crate) type Spool = fn(&Store, &str, &str);

static SPOOL: std::sync::OnceLock<Spool> = std::sync::OnceLock::new();

/// Lend the worker the held-tell spool (the daemon's `wake::hold_tell`). Set
/// once; a second call is ignored.
pub(crate) fn install_spool(spool: Spool) {
    let _ = SPOOL.set(spool);
}

/// The daemon's [`Hand`]: an order through the orders machine under
/// `WSP_ORDERS=store`; else the seat's held-tell spool, typed at its next
/// idle. With no spool lent, the message stays queued on the hub.
pub(crate) fn hand_to_orders(store: &Store) -> impl Fn(&wsp_store_contract::SeatId, &str, &Principal, &str) -> Result<String, String> + '_ {
    move |seat, text, from, key| match crate::orders::mode() {
        crate::orders::Mode::Store => {
            crate::orders::send_keyed(store, seat, text, from, false, key).map(|o| o.0).map_err(|e| e.to_string())
        }
        crate::orders::Mode::Files => match SPOOL.get() {
            Some(spool) => {
                spool(store, &seat.0, text);
                Ok(format!("held for {}", seat.0))
            }
            None => Err("no held-tell spool in this process to keep it for the seat".into()),
        },
    }
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

/// The status file under `$WSP_STATE/hub/`, which `wsp doctor` reads.
pub(crate) const STATUS_FILE: &str = "sync.json";

/// What `wsp doctor` says of the hub, from `hubs.json` under `home`, the
/// status file the worker keeps, the journal, the claims' leases and the drive
/// book (wsp-194). A wsp that is not logged in says one line and nothing more.
/// A lease within a day of its expiry whose seat `alive` does not know is a
/// warning: nothing is renewing it. `now` is epoch seconds.
pub(crate) fn health(
    store: &Store,
    home: Option<&std::path::Path>,
    now: i64,
    alive: &mut dyn FnMut(&str) -> bool,
    problems: &mut Vec<String>,
    warnings: &mut Vec<String>,
    notes: &mut Vec<String>,
) {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let home = home.unwrap_or_else(|| std::path::Path::new(""));
    let url = match wsp_client::hub_url_in(env(wsp_client::HUB_URL_ENV), home) {
        Ok(u) => u,
        Err(wsp_client::Silent::BadHubsFile(why)) => {
            problems.push(format!("the hub login cannot be read: {why}"));
            return;
        }
        Err(_) => {
            notes.push("not logged in to a hub (standalone)".into());
            return;
        }
    };
    let user = env(wsp_client::HUB_USER_ENV)
        .or_else(|| wsp_client::hub_login_in(home).ok().flatten().map(|l| l.user).filter(|u| !u.is_empty()))
        .unwrap_or_else(|| "(unnamed)".into());
    notes.push(format!("hub {url} as {user}"));

    // The certificate the hub presents now.
    let ca = wsp_client::hub_ca_in(env(wsp_client::HUB_CA_ENV), home).ok().flatten();
    match wsp_client::hub_cert_expiry(&url, ca.as_deref(), Duration::from_secs(2)) {
        Ok(None) => notes.push("hub certificate: none (the hub is dialled in the clear)".into()),
        Ok(Some(at)) => {
            let days = (at - now).div_euclid(86_400);
            let line = format!("hub certificate expires {} (in {days} days)", crate::util::iso_at(at));
            if at - now < 7 * 86_400 {
                warnings.push(line);
            } else {
                notes.push(line);
            }
        }
        Err(why) => warnings.push(format!("hub certificate not read: {why}")),
    }

    // The worker's last word.
    let status: serde_json::Value = std::fs::read_to_string(store.state.join("hub").join(STATUS_FILE))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    match status["last_ok"].as_i64() {
        Some(at) => notes.push(format!("hub last synced {} ({} ago)", crate::util::iso_at(at), crate::util::duration_human((now - at).max(0)))),
        None => notes.push("hub never synced".into()),
    }
    if let Some(at) = status["last_failure"]["at"].as_i64() {
        let why = status["last_failure"]["why"].as_str().unwrap_or("no reason given");
        let line = format!("hub last failure {}: {why}", crate::util::iso_at(at));
        if status["last_ok"].as_i64().is_none_or(|ok| ok < at) {
            warnings.push(line);
        } else {
            notes.push(line);
        }
    }
    notes.push(format!("hub journal: {} entries waiting", Journal::of(store).len()));

    // The row leases the claims here hold, and the drive leases.
    let claims = store.claims();
    let mut held: Vec<(String, i64)> = claims
        .iter()
        .filter(|(_, rec)| rec.get(crate::hub_lease::TOKEN).is_some())
        .filter_map(|(row, rec)| Some((row.clone(), wsp_store::store::record_expires(rec)?)))
        .collect();
    held.sort_by_key(|(_, at)| *at);
    match held.first() {
        None => notes.push("hub leases held: none".into()),
        Some((row, at)) => notes.push(format!("hub leases held: {}, soonest {row} expires {}", held.len(), crate::util::iso_at(*at))),
    }
    for (row, at) in &held {
        if at - now >= 86_400 {
            continue;
        }
        let seat = claims.get(row).and_then(crate::hub_lease::seat_of).unwrap_or("");
        if seat.is_empty() || !alive(seat) {
            warnings.push(format!(
                "hub lease on {row} expires {} and its seat is not alive: nothing renews it",
                crate::util::iso_at(*at)
            ));
        }
    }
    let drives = crate::hub_lease::Book::read(store).drives;
    match drives.iter().min_by_key(|(_, d)| d.expires) {
        None => notes.push("hub drive leases held: none".into()),
        Some((list, d)) => notes.push(format!("hub drive leases held: {}, soonest {list} expires {}", drives.len(), crate::util::iso_at(d.expires))),
    }
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
    /// Rows whose journal went to the hub as a side chain (wsp-469).
    pub(crate) sided: usize,
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
    /// Who the hub last said this connection is: the `by` of a record made offline.
    by: Option<Principal>,
    /// Leases taken again on reconnect for a row no claim here holds: given
    /// back once the row is pushed (wsp-469).
    release_after: Vec<(RowId, Token)>,
    /// Rows whose lease the hub would not say this turn: nothing of them pushed.
    held_back: BTreeSet<Subject>,
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
            by: None,
            release_after: Vec::new(),
            held_back: BTreeSet::new(),
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
                let hand = hand_to_orders(store);
                if let Err(why) = self.take_messages(store, &hand) {
                    self.say_once(store, format!("hub-sync: the messages queued for this spoke were not taken: {why}"));
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
                // Work goes on: what moved is journaled for the reconnect.
                self.journal_offline(store);
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
        crate::hub_lease::push_all(store, client);
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
        // The last good turn and the last failure outlive the turn that wrote
        // them: what `wsp doctor` says of a hub that answered an hour ago.
        let before: serde_json::Value =
            std::fs::read_to_string(dir.join(STATUS_FILE)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let now = crate::util::epoch_secs();
        let last_ok = if matches!(self.link, Link::Up) { Some(serde_json::json!(now)) } else { before.get("last_ok").cloned() };
        let last_failure = match &self.link {
            Link::Down(w) => Some(serde_json::json!({ "at": now, "why": w })),
            _ => before.get("last_failure").cloned(),
        };
        let v = serde_json::json!({
            "last_ok": last_ok,
            "last_failure": last_failure,
            "turns": self.turns,
            "link": self.link.word(),
            "why": match &self.link { Link::Down(w) => Some(w.clone()), _ => None },
            "last": last.map(|t| serde_json::json!({ "sent": t.sent, "moved": t.moved, "adopted": t.adopted, "reconciled": t.reconciled, "heads": t.heads, "sided": t.sided })),
            "journal": Journal::of(store).len(),
        });
        let _ = std::fs::create_dir_all(&dir);
        let tmp = dir.join(format!(".sync.json.{}.tmp", std::process::id()));
        if std::fs::write(&tmp, v.to_string()).is_ok() {
            let _ = std::fs::rename(&tmp, dir.join(STATUS_FILE));
        }
    }

    /// The connection, dialled again where there is none: the login is read
    /// each time, so a new `wsp login` is the next dial's.
    fn client(&mut self) -> Result<&mut Client, String> {
        if self.client.is_none() {
            self.client = Some(dial(&self.home)?);
        }
        Ok(self.client.as_mut().expect("dialled above"))
    }

    /// The messages the hub queued for this user, each handed to its seat
    /// here through `hand` ([`crate::cmd_message::receive`]) and then
    /// acknowledged; how many were told. One whose seat is not here now is
    /// left queued, said once, and tried again next turn; one told before (an
    /// ack that was lost) is acknowledged and not told again.
    pub(crate) fn take_messages(&mut self, store: &Store, hand: &Hand) -> Result<usize, String> {
        let mail = self.client()?.msg_take().map_err(|e| e.to_string())?;
        let mut done = Vec::new();
        let mut told = 0;
        for m in &mail {
            match crate::cmd_message::receive(store, m, hand) {
                Ok(fresh) => {
                    told += usize::from(fresh);
                    done.push(m.id.clone());
                }
                Err(why) => self.say_once(store, format!("hub-sync: {} from {} is not told yet: {why}", m.id, m.from_user)),
            }
        }
        if !done.is_empty() {
            self.client()?.msg_ack(&done).map_err(|e| e.to_string())?;
        }
        Ok(told)
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
        let me = self.by.as_ref().map(user_of);
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
                    // A message queued for this user (wsp-470): take it now.
                    if e.kind == EventKind::OrderQueued && me.is_some() && e.payload.get("user").and_then(|u| u.as_str()) == me.as_deref() {
                        woke = true;
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
        self.by = Some(by.clone());
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
        // What the journal holds from before (an outage, a crash), settled by
        // each row's lease and put in order, before anything new is pushed.
        self.settle(store, ws, &hub, by, turned)?;
        let heads = Heads::read(store);
        let (mut local, pushing, included) = self.pending(store, ws, tasks, by, &objects, &heads, classified.whole);
        for (subject, id) in Sides::read(store).notice_heads(ws) {
            local.set(subject, id);
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
        self.after_push(store, &report)
    }

    /// The records of `ws`'s rows and lists that moved since their synced
    /// heads, each made once per file state, journaled, then kept in the
    /// object dir: the spoke's refs for the sync, what it pushes, and every
    /// subject it syncs whole. A row with a side chain of this spoke's is the
    /// holder's until it is leased here again: its synced head stands.
    #[allow(clippy::too_many_arguments)]
    fn pending(
        &mut self,
        store: &Store,
        ws: &str,
        tasks: &[crate::model::Task],
        by: &Principal,
        objects: &ObjectDir,
        heads: &Heads,
        whole: BTreeSet<Subject>,
    ) -> (Refs, Pushing, BTreeSet<Subject>) {
        let wid = WorkspaceId(ws.to_string());
        let cache = Cache::read(store);
        let journal = Journal::of(store);
        let mut tips: BTreeMap<Subject, ObjectId> =
            journal.by_subject().into_iter().filter_map(|(s, (_, es))| Some((s, es.last()?.id))).collect();
        let sides = Sides::read(store);
        let claims = store.claims();
        let mut leased = Leased::read(store);
        let leased_was = leased.clone();
        let mut included = whole;
        let mut local = Refs::new(wid.clone());
        let mut pushing: BTreeMap<Subject, (ObjectId, Option<ObjectId>)> = BTreeMap::new();
        let mut subjects: Vec<(Subject, Option<ObjectId>)> = Vec::new();
        for t in tasks.iter().filter(|t| !cache.is_head(&t.id) && store.task(&t.id).is_some()) {
            subjects.push((Subject::Row(RowId(t.id.clone())), file_digest(store, &t.id)));
        }
        for w in store.worklists().iter().filter(|w| w.workspace.to_string() == ws) {
            subjects.push((Subject::List(ListId(w.id.clone())), crate::door::list_digest(store, &w.id)));
        }
        for (subject, digest) in subjects {
            included.insert(subject.clone());
            let synced = heads.head(&subject).copied();
            let row = match &subject {
                Subject::Row(r) => Some(r.0.clone()),
                _ => None,
            };
            // The lease the row is held under here, remembered past the claim.
            let claimed = row.as_ref().and_then(|r| claims.get(r)).and_then(|c| c.get(crate::hub_lease::TOKEN)).and_then(|t| t.as_u64());
            if let (Some(r), Some(t)) = (&row, claimed) {
                leased.insert(r.clone(), t);
            }
            let superseded = row.as_ref().and_then(|r| sides.rows.get(r)).is_some_and(|s| s.mine && claimed.is_none());
            if self.held_back.contains(&subject) || superseded || (synced.is_some() && heads.file(&subject) == digest) {
                if let Some(h) = synced {
                    local.set(subject, h);
                }
                continue;
            }
            let id = match self.minted.get(&subject) {
                Some((d, id)) if *d == digest && objects.has(id) => *id,
                _ => {
                    let at = At(now_ms());
                    let made = match row {
                        Some(_) => push_record(store, &subject, wid.clone(), by.clone(), at),
                        None => push_list_record(store, &subject, wid.clone(), by.clone(), at),
                    };
                    let Some(mut obj) = made else { continue };
                    // A journaled record of this very state (a restart lost
                    // `minted`) is the row's id still: no twin on top of it.
                    if let Some((tip, bytes)) = journal.tip_of_entity(&subject, &obj) {
                        if let Err(e) = objects.put(&tip, &bytes) {
                            self.say_once(store, format!("hub-sync: cannot keep {subject:?}'s object: {e}"));
                            continue;
                        }
                        self.minted.insert(subject.clone(), (digest, tip));
                        local.set(subject.clone(), tip);
                        pushing.insert(subject, (tip, digest));
                        continue;
                    }
                    // After what is journaled and not on the hub yet, so the
                    // records of an outage are one line, pushed in order.
                    if let Some(tip) = tips.get(&subject).filter(|t| objects.has(t)) {
                        set_parent(&mut obj, *tip);
                    }
                    let (id, bytes) = wsp_objects::encode(&obj);
                    let token = row.as_ref().and_then(|r| leased.get(r).copied());
                    if let Err(e) = journal.append(&id, &bytes, token).and_then(|_| objects.put(&id, &bytes)) {
                        self.say_once(store, format!("hub-sync: cannot keep {subject:?}'s object: {e}"));
                        continue;
                    }
                    tips.insert(subject.clone(), id);
                    self.minted.insert(subject.clone(), (digest, id));
                    id
                }
            };
            local.set(subject.clone(), id);
            pushing.insert(subject, (id, digest));
        }
        if leased != leased_was {
            Leased::write(store, &leased);
        }
        (local, pushing, included)
    }

    /// A turn the hub did not answer: the records of every shared row and
    /// list that moved are journaled all the same (wsp-469), so the work done
    /// offline is on its way the moment the hub is back.
    pub(crate) fn journal_offline(&mut self, store: &Store) {
        let Some(by) = self.by.clone().or_else(|| login_principal(&self.home)) else { return };
        let objects = ObjectDir::of(store);
        let heads = Heads::read(store);
        let mut workspaces: BTreeMap<String, Vec<crate::model::Task>> =
            crate::mirror::by_shared_workspace(store.tasks()).into_iter().map(|(w, t)| (w.to_string(), t)).collect();
        for w in store.worklists().iter().filter(|w| !w.workspace.is_private()) {
            workspaces.entry(w.workspace.to_string()).or_default();
        }
        for (ws, tasks) in workspaces {
            let _ = self.pending(store, &ws, &tasks, &by, &objects, &heads, BTreeSet::new());
        }
    }

    /// What the journal holds of `ws` from before this turn, row by row by the
    /// lease it was made under (wsp-469, the design's "Leases and offline sync"):
    ///
    /// - renewed under its token: still held, and it fast-forwards as usual;
    /// - refused: the claim here is released as wsp-411 releases an expired
    ///   one ([`crate::hub_lease::follow_expired`]), then the lease is taken
    ///   again: granted (a new token, stamped on a claim still here, else
    ///   given back after the push), it is pushed; held by another user (or
    ///   allocated to one), its objects go to the hub as a side chain.
    ///
    /// Then every entry of `ws` is put, in the journal's order. A side chain's
    /// entries leave the journal at once; the rest leave as their refs move.
    fn settle(&mut self, store: &Store, ws: &str, hub: &Refs, by: &Principal, turned: &mut Turned) -> Result<(), String> {
        let journal = Journal::of(store);
        let me = user_of(by);
        let mut sides = Sides::read(store);
        let sides_were = sides.clone();
        // A row leased here again since its side chain is this spoke's again.
        let claims = store.claims();
        sides.rows.retain(|row, s| {
            let now = claims.get(row).and_then(|c| c.get(crate::hub_lease::TOKEN)).and_then(|t| t.as_u64());
            !(s.mine && now.is_some() && now > s.token)
        });
        self.held_back.clear();
        let left: Vec<(Subject, Vec<Entry>)> =
            journal.by_subject().into_iter().filter(|(_, (w, _))| w.0 == ws).map(|(s, (_, es))| (s, es)).collect();
        let wid = WorkspaceId(ws.to_string());
        let mut aside: Vec<(RowId, String, u64)> = Vec::new();
        for (subject, es) in &left {
            let Subject::Row(row) = subject else { continue };
            let Some(token) = es.iter().rev().find_map(|e| e.token) else { continue };
            let client = self.client()?;
            match client.lease_renew(row, Token(token)) {
                Ok((_, at)) => {
                    if !store.renew_claim(&row.0, (at.0 / 1000) as i64) {
                        // No claim holds it here any more (it ended offline):
                        // the lease goes back once the row is pushed.
                        self.release_after.push((row.clone(), Token(token)));
                    }
                    continue;
                }
                Err(e) if unreachable(&e) => return Err(e.to_string()),
                Err(wsp_client::Error::Refused(_)) => {}
                Err(e) => {
                    self.say_once(store, format!("hub-sync: the lease of {} was not read: {e}; it waits", row.0));
                    self.held_back.insert(subject.clone());
                    continue;
                }
            }
            // Offline past the lease: released here first, as wsp-411 does.
            crate::hub_lease::follow_expired(store, &serde_json::json!({ "row": row.0, "token": token }));
            let client = self.client()?;
            match client.lease_take(&wid, row) {
                Ok((taken, at)) => {
                    let held = crate::hub_lease::Held { token: taken, expires: (at.0 / 1000) as i64 };
                    match store.claims().get(&row.0).cloned() {
                        Some(mut claim) if claim.get(crate::hub_lease::TOKEN).and_then(|t| t.as_u64()) == Some(token) => {
                            crate::hub_lease::stamp(&mut claim, &held);
                            store.set_claim(&row.0, claim);
                        }
                        _ => self.release_after.push((row.clone(), taken)),
                    }
                    crate::store::log_line(
                        store,
                        &format!("hub-sync: {}'s lease ran out while the hub was away; taken again (token {}) and pushed", row.0, taken.0),
                    );
                }
                Err(wsp_client::Error::Refused(wsp_store_contract::Refusal::Held { holder, .. })) => {
                    aside.push((row.clone(), user_of(&holder), token));
                }
                Err(wsp_client::Error::Refused(wsp_store_contract::Refusal::Forbidden { .. })) => {
                    let holder = hub_cache::Cache::read(store).get(&row.0).and_then(|h| h.allocation.clone());
                    aside.push((row.clone(), holder.unwrap_or_else(|| "another user".into()), token));
                }
                Err(e) if unreachable(&e) => return Err(e.to_string()),
                Err(e) => {
                    self.say_once(store, format!("hub-sync: the lease of {} was not taken again: {e}; it waits", row.0));
                    self.held_back.insert(subject.clone());
                }
            }
        }
        // The journal of `ws` to the hub, in order; a put is idempotent by hash.
        let mut entries: Vec<Entry> = left.iter().flat_map(|(_, es)| es.iter().cloned()).collect();
        entries.sort_by_key(|e| e.seq);
        if !entries.is_empty() {
            let client = self.client()?;
            match journal.replay(&entries, &mut |blobs| client.sync_put(blobs)) {
                Ok(n) => turned.sent += n,
                Err(e) if unreachable(&e) => return Err(e.to_string()),
                Err(e) => {
                    self.say_once(store, format!("hub-sync: the journal of {ws} was not put: {e}"));
                    return Ok(());
                }
            }
        }
        // Side chains: on the hub now, under no ref. The file here follows the
        // holder's line from the synced head, which the sync then adopts, so
        // the old holder's verdict and landing go no further.
        for (row, holder, token) in aside {
            let subject = Subject::Row(row.clone());
            let Some((_, es)) = left.iter().find(|(s, _)| *s == subject) else { continue };
            let Some(tip) = es.last().map(|e| e.id) else { continue };
            let side = Side {
                workspace: ws.to_string(),
                row: row.0.clone(),
                tip: tip.hex(),
                holder,
                old: me.clone(),
                mine: true,
                token: Some(token),
                at: crate::util::iso_at(crate::util::epoch_secs()),
            };
            match self.move_notice(store, &wid, hub, &side, by) {
                Ok(Some(id)) => {
                    sides.notices.entry(ws.to_string()).or_default().insert(row.0.clone(), id.hex());
                }
                Ok(None) => {}
                Err(e) => return Err(e),
            }
            let mut heads = Heads::read(store);
            if let Some(synced) = heads.head(&subject).copied() {
                heads.set(&subject, synced, file_digest(store, &row.0));
                if let Err(e) = heads.write(store) {
                    self.say_once(store, format!("hub-sync: cannot write the synced heads: {e}"));
                }
            }
            self.minted.remove(&subject);
            for e in es {
                journal.remove(e);
            }
            Leased::forget(store, &row.0);
            let words = hub_journal::said(&side);
            crate::store::log_line(store, &format!("hub-sync: side chain: {words}"));
            crate::verification::tell_governor(store, &row.0, &words);
            sides.rows.insert(row.0.clone(), side);
            turned.sided += 1;
        }
        if sides != sides_were {
            if let Err(e) = sides.write(store) {
                self.say_once(store, format!("hub-sync: cannot write side.json: {e}"));
            }
        }
        Ok(())
    }

    /// The notice of a side chain, journaled, put and moved under its own
    /// subject: the holder's spoke takes it on its next sync. Its id where the
    /// hub moved to it; `None` where another notice moved first (said once).
    fn move_notice(&mut self, store: &Store, wid: &WorkspaceId, hub: &Refs, side: &Side, by: &Principal) -> Result<Option<ObjectId>, String> {
        let subject = hub_journal::notice_subject(&side.row);
        let parent = hub.get(&subject).copied();
        let obj = hub_journal::notice(side, parent, by.clone(), At(now_ms()));
        let (id, bytes) = wsp_objects::encode(&obj);
        let journal = Journal::of(store);
        let _ = journal.append(&id, &bytes, None);
        let _ = ObjectDir::of(store).put(&id, &bytes);
        let client = self.client()?;
        let put = client.sync_put(vec![Blob { id, bytes }]).and_then(|_| {
            client.refs_move(RefMove { workspace: wid.clone(), subject: subject.clone(), from: parent, to: id })
        });
        let out = match put {
            Ok(Move::Moved { .. }) => Some(id),
            Ok(Move::Stale { .. }) => {
                self.say_once(store, format!("hub-sync: the notice of {}'s side chain met another; the flag here stands", side.row));
                None
            }
            Err(e) if unreachable(&e) => return Err(e.to_string()),
            Err(e) => {
                self.say_once(store, format!("hub-sync: the notice of {}'s side chain was not sent: {e}", side.row));
                None
            }
        };
        if let Some(e) = journal.entries().into_iter().find(|e| e.id == id) {
            journal.remove(&e);
        }
        Ok(out)
    }

    /// A notice the hub holds for `row`: kept as taken, and where this spoke's
    /// user holds the row (or wrote the side chain), the flag, a line in the
    /// log and a tell to the row's governor, once per side chain.
    #[allow(clippy::too_many_arguments)]
    fn take_notice(&mut self, store: &Store, objects: &ObjectDir, sides: &mut Sides, ws: &str, row: &str, head: ObjectId, by: &Principal) {
        let Some(obj) = Objects::object(objects, &head) else { return };
        sides.notices.entry(ws.to_string()).or_default().insert(row.to_string(), head.hex());
        let me = user_of(by);
        let Some(side) = hub_journal::side_of_notice(&obj, &me) else { return };
        if side.holder != me && !side.mine {
            return;
        }
        if sides.rows.get(row).is_some_and(|s| s.tip == side.tip) {
            return;
        }
        let words = hub_journal::said(&side);
        crate::store::log_line(store, &format!("hub-sync: side chain: {words}"));
        crate::verification::tell_governor(store, row, &words);
        sides.rows.insert(row.to_string(), side);
    }

    /// After a sync: what the hub took leaves the journal (a move, or a stale
    /// or diverged head whose objects are put before reconcile writes the
    /// merge), and a lease taken only to push goes back.
    fn after_push(&mut self, store: &Store, report: &SyncReport) -> Result<(), String> {
        let journal = Journal::of(store);
        let by_subject = journal.by_subject();
        let mut unput: Vec<Entry> = Vec::new();
        for s in report.moved.iter().chain(report.stale.iter().map(|st| &st.subject)).chain(&report.diverged) {
            let Some((_, es)) = by_subject.get(s) else { continue };
            if report.diverged.contains(s) {
                unput.extend(es.iter().cloned());
            }
            for e in es {
                if !report.diverged.contains(s) {
                    journal.remove(e);
                }
            }
        }
        if !unput.is_empty() {
            unput.sort_by_key(|e| e.seq);
            let client = self.client()?;
            match journal.replay(&unput, &mut |blobs| client.sync_put(blobs)) {
                Ok(_) => unput.iter().for_each(|e| journal.remove(e)),
                Err(e) if unreachable(&e) => return Err(e.to_string()),
                Err(e) => self.say_once(store, format!("hub-sync: diverged objects were not put: {e}")),
            }
        }
        for (row, token) in std::mem::take(&mut self.release_after) {
            let client = self.client()?;
            match client.lease_release(&row, token) {
                Ok(()) => Leased::forget(store, &row.0),
                Err(e) if unreachable(&e) => {
                    self.release_after.push((row, token));
                    return Err(e.to_string());
                }
                Err(_) => Leased::forget(store, &row.0),
            }
        }
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
        let mut sides = Sides::read(store);
        let sides_were = sides.clone();
        for s in &report.adopted {
            let Some(head) = report.refs.get(s).copied() else { continue };
            // A side chain's notice (wsp-469): a flag here, never a file.
            if let Some(row) = hub_journal::notice_row(s) {
                self.take_notice(store, objects, &mut sides, &report.refs.workspace.0, row, head, by);
                continue;
            }
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
            // A row's verdicts and landings (wsp-468) write no file: a landing
            // fetches the remote it was pushed to, and the head is kept so the
            // next turn walks only what is new.
            if matches!(s, Subject::Lease(_)) {
                for obj in line.iter().rev() {
                    if let Object::Event { kind: EventKind::RowLanded, payload, .. } = obj {
                        crate::hub_lease::fetch_landed(store, payload);
                    }
                    if let (Object::Event { kind: EventKind::RowReviewed, payload, .. }, Subject::Lease(name)) = (obj, s) {
                        if let Some(row) = name.0.strip_prefix("row:") {
                            crate::hub_lease::take_verdict(store, row, payload);
                        }
                    }
                }
                let mut heads = Heads::read(store);
                heads.set(s, head, None);
                if let Err(e) = heads.write(store) {
                    self.say_once(store, format!("hub-sync: cannot write the synced heads: {e}"));
                }
                continue;
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
        if sides != sides_were {
            if let Err(e) = sides.write(store) {
                self.say_once(store, format!("hub-sync: cannot write side.json: {e}"));
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

/// What a sync pushes: per subject, the record's id and the file digest it was made of.
type Pushing = BTreeMap<Subject, (ObjectId, Option<ObjectId>)>;

/// Makes `obj` follow `to`.
fn set_parent(obj: &mut Object, to: ObjectId) {
    match obj {
        Object::Record { parent, .. } | Object::Event { parent, .. } => *parent = Some(to),
    }
}

/// The principal the login names, for a record made before the hub ever said
/// who this connection is.
fn login_principal(home: &std::path::Path) -> Option<Principal> {
    let login = wsp_client::hub_login_in(home).ok().flatten()?;
    (!login.user.is_empty()).then(|| Principal::Person(login.user.as_str().into()))
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


#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use wsp_store_contract::{Command as Cmd, IdempotencyKey, SharedName, TokenId};

    /// wsp-484: a worker that lost `minted` (the daemon restarted with the
    /// journal not yet pushed) takes the journaled record for the unchanged
    /// row as the row's id; it makes no twin of it.
    #[test]
    fn a_restarted_worker_reuses_the_journaled_record_of_an_unchanged_row() {
        let env = crate::util::isolated("hub-sync-restart");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let dir = std::env::temp_dir().join(format!("wsp-484-restart-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let home = dir.join("home");
        let data = dir.join("hub");
        std::fs::create_dir_all(&home).unwrap();

        // The hub: ann a member of `mp`, with a token.
        let token = {
            let hub = wsp_hub::Hub::open(&data).unwrap();
            let apply = |cmd: Cmd, by: &Principal| {
                hub.apply(cmd.clone(), IdempotencyKey(format!("{cmd:?}")), by.clone()).expect("the identity machine takes it");
            };
            let root = Principal::Person("root".into());
            let mp = SharedName("mp".into());
            apply(Cmd::HubUserAdd { name: UserName("root".into()), admin: true }, &Principal::Wsp("wsp-hub".into()));
            apply(Cmd::HubWorkspaceAdd { name: mp.clone() }, &root);
            let (secret, hash) = wsp_machines::token::mint();
            apply(Cmd::HubUserAdd { name: UserName("ann".into()), admin: false }, &root);
            apply(Cmd::HubMemberSet { workspace: mp, user: UserName("ann".into()), member: true }, &root);
            apply(Cmd::HubTokenIssue { id: TokenId("t-ann".into()), user: UserName("ann".into()), hash }, &root);
            secret
        };
        let login = |addr: std::net::SocketAddr| {
            let hubs = home.join(HUBS_FILE);
            std::fs::create_dir_all(hubs.parent().unwrap()).unwrap();
            let v = serde_json::json!({ "url": format!("tcp://{addr}"), "token": token, "user": "ann", "at": "2026-11-23T00:00:00Z" });
            std::fs::write(&hubs, v.to_string()).unwrap();
            std::fs::set_permissions(&hubs, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
        };

        // A shared row, and the hub away: a port nobody listens on.
        let mut t = crate::model::Task::new("a shared row", "wsp-484-r1");
        t.project = Some("wsp".into());
        t.workspace = "shared:mp".parse().unwrap();
        store.save_task(&t).unwrap();
        let subject = Subject::Row(RowId(t.id.clone()));
        let gone = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        login(gone);

        let mut first = Worker::new(home.clone());
        assert!(first.turn(&store).is_err(), "the hub is away");
        let journal = Journal::of(&store);
        let held = |j: &Journal| j.by_subject().get(&subject).map_or(0, |(_, es)| es.len());
        assert_eq!(held(&journal), 1, "the outage journals the row's record once");
        let id = journal.by_subject()[&subject].1[0].id;
        drop(first);

        // The daemon restarts; the hub is back; the file has not changed.
        let cfg = wsp_hub::Config {
            data_dir: data.clone(),
            bind: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            port: 0,
            peers: vec!["127.0.0.0/8".into()],
            tls: None,
            name: "wsp-hub".into(),
            lease_days: wsp_hub::DEFAULT_LEASE_DAYS,
        };
        let running = wsp_hub::serve::start(&cfg).expect("the hub starts");
        login(running.addrs()[0]);
        let mut second = Worker::new(home.clone());
        assert!(second.minted.is_empty(), "a fresh worker remembers nothing");
        second.turn(&store).expect("the hub answers");

        // The hub's line for the row: one new record, the journaled one.
        let hub = running.hub();
        let head = hub.heads(&wsp_api::WorkspaceId("shared:mp".into())).unwrap().get(&subject).copied();
        assert_eq!(head, Some(id), "the hub's head is the journaled record");
        let mut records = 0;
        let mut stack = vec![hub.objects().root().to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
                if e.path().is_dir() {
                    stack.push(e.path());
                } else if let Some(o) = std::fs::read(e.path()).ok().and_then(|b| wsp_objects::decode(&b).ok()) {
                    records += usize::from(o.subject() == &subject);
                }
            }
        }
        assert_eq!(records, 1, "the row's line has one record, not two");
        assert!(held(&journal) <= 1 && journal.by_subject().get(&subject).is_none_or(|(_, es)| es.iter().all(|e| e.id == id)), "no twin in the journal");
        running.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- messages through the hub (wsp-470) ----

    /// A served hub whose users `ed` and `jesper` are members of `mp`; their tokens.
    fn message_hub(dir: &std::path::Path) -> (wsp_hub::serve::Running, BTreeMap<String, String>) {
        let data = dir.join("hub");
        let mut tokens = BTreeMap::new();
        {
            let hub = wsp_hub::Hub::open(&data).unwrap();
            let apply = |cmd: Cmd, by: &Principal| {
                hub.apply(cmd.clone(), IdempotencyKey(format!("{cmd:?}")), by.clone()).expect("the identity machine takes it");
            };
            let root = Principal::Person("root".into());
            apply(Cmd::HubUserAdd { name: UserName("root".into()), admin: true }, &Principal::Wsp("wsp-hub".into()));
            apply(Cmd::HubWorkspaceAdd { name: SharedName("mp".into()) }, &root);
            for user in ["ed", "jesper"] {
                let (secret, hash) = wsp_machines::token::mint();
                apply(Cmd::HubUserAdd { name: UserName(user.into()), admin: false }, &root);
                apply(Cmd::HubMemberSet { workspace: SharedName("mp".into()), user: UserName(user.into()), member: true }, &root);
                apply(Cmd::HubTokenIssue { id: TokenId(format!("t-{user}")), user: UserName(user.into()), hash }, &root);
                tokens.insert(user.to_string(), secret);
            }
        }
        let cfg = wsp_hub::Config {
            data_dir: data,
            bind: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            port: 0,
            peers: vec!["127.0.0.0/8".into()],
            tls: None,
            name: "wsp-hub".into(),
            lease_days: wsp_hub::DEFAULT_LEASE_DAYS,
        };
        (wsp_hub::serve::start(&cfg).expect("the hub starts"), tokens)
    }

    /// `home` logged in to the hub at `addr` as `user`, as `wsp login` writes it.
    fn log_in(home: &std::path::Path, addr: std::net::SocketAddr, token: &str, user: &str) {
        let hubs = home.join(HUBS_FILE);
        std::fs::create_dir_all(hubs.parent().unwrap()).unwrap();
        let v = serde_json::json!({ "url": format!("tcp://{addr}"), "token": token, "user": user, "at": "2026-11-23T00:00:00Z" });
        std::fs::write(&hubs, v.to_string()).unwrap();
        std::fs::set_permissions(&hubs, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    }

    /// A store at `dir` with row `row` in `shared:mp`, claimed, and seat `pane` bound to it.
    fn spoke_with_seat(dir: &std::path::Path, row: &str, pane: &str) -> Store {
        let store = Store::at(dir.join("wsp"), dir.join("state"));
        store.ensure_dirs().unwrap();
        let mut t = crate::model::Task::new(&format!("row {row}"), row);
        t.project = Some("wsp".into());
        t.workspace = "shared:mp".parse().unwrap();
        store.save_task(&t).unwrap();
        store.set_claim(row, serde_json::json!({ "workspace_id": "w1" }));
        store.set_binding(pane, serde_json::json!({ "task_id": row, "pane_id": pane }));
        store
    }

    /// The orders a seat has in `db`, as (from, text).
    fn orders_of(db: &wsp_server::SharedStore, seat: &str) -> Vec<(Principal, String)> {
        db.orders_for(&wsp_store_contract::SeatId(seat.into()), &[]).unwrap().into_iter().map(|o| (o.from, o.text)).collect()
    }

    /// wsp-470's two spokes: a seat on Ed's spoke tells `seat:j-1`, a row
    /// Jesper's spoke holds the hub lease of; Jesper's worker takes it and
    /// hands it to his orders machine as a tell to the seat on j-1 from Ed's
    /// seat named with its user; that seat's reply, `--re` the hub's id, goes
    /// back through the hub and Ed's worker tells it to the seat on e-9.
    #[test]
    fn a_tell_to_a_row_another_users_spoke_holds_is_told_there_and_its_reply_comes_back() {
        let env = crate::util::isolated("hub-msg-two-spokes");
        let dir = env.home().parent().unwrap().join("two-spokes");
        let _ = std::fs::remove_dir_all(&dir);
        let (running, tokens) = message_hub(&dir);
        let addr = running.addrs()[0];

        // Jesper's spoke: the seat cpd-j1 on j-1, whose hub lease is his.
        let jesper = spoke_with_seat(&dir.join("jesper"), "j-1", "cpd-j1");
        let jesper_home = dir.join("jesper/home");
        log_in(&jesper_home, addr, &tokens["jesper"], "jesper");
        dial(&jesper_home).unwrap().lease_take(&WorkspaceId("shared:mp".into()), &RowId("j-1".into())).unwrap();
        let jesper_db = crate::orders::tests::db(&dir.join("jesper/orders"), wsp_server::Mode::Owned);
        crate::orders::tests::seat(&jesper_db, "cpd-j1", wsp_store_contract::SeatState::Idle);

        // Ed's spoke: the seat cpd-e1 on e-9; the hub said j-1 is another user's.
        let ed = spoke_with_seat(&dir.join("ed"), "e-9", "cpd-e1");
        let ed_home = dir.join("ed/home");
        log_in(&ed_home, addr, &tokens["ed"], "ed");
        crate::hub_lease::Book::edit(&ed, |b| b.elsewhere.insert("j-1".into(), "jesper".into()));
        let ed_db = crate::orders::tests::db(&dir.join("ed/orders"), wsp_server::Mode::Owned);
        crate::orders::tests::seat(&ed_db, "cpd-e1", wsp_store_contract::SeatState::Idle);

        // Ed's seat tells seat:j-1.
        let book = crate::address::Book::read(&ed);
        let sent = crate::cmd_message::hub_tell(&ed, &ed_home, &book, "seat:j-1", "is j-1 landed?", None, "seat:e-9", Some("cpd-e1".into()));
        let (id, user, to) = sent.expect("j-1 is another user's: through the hub").expect("the hub takes it");
        assert_eq!((user.as_str(), to.as_str()), ("jesper", "seat:j-1"));

        // Jesper's worker takes it: a tell to the seat on j-1, from Ed's seat named with Ed.
        let hand_j = |seat: &wsp_store_contract::SeatId, text: &str, from: &Principal, key: &str| {
            crate::orders::send_keyed_in(&jesper_db, seat, text, from, false, key).map(|o| o.0).map_err(|e| e.to_string())
        };
        let mut jesper_worker = Worker::new(jesper_home.clone());
        assert_eq!(jesper_worker.take_messages(&jesper, &hand_j).unwrap(), 1);
        let told = orders_of(&jesper_db, "cpd-j1");
        assert_eq!(told.len(), 1, "one order: {told:?}");
        assert_eq!(told[0].0, Principal::Seat(wsp_store_contract::SeatId("cpd-e1@ed".into())), "from the sending seat, named with its user");
        assert!(told[0].1.starts_with("seat:e-9@ed says: is j-1 landed?"), "{}", told[0].1);
        assert!(told[0].1.ends_with(&format!("reply: wsp tell seat:e-9 --re {id} -]")), "{}", told[0].1);
        assert_eq!(jesper_worker.take_messages(&jesper, &hand_j).unwrap(), 0, "acknowledged: not handed out again");
        assert_eq!(orders_of(&jesper_db, "cpd-j1").len(), 1);

        // The seat there replies as its footer says: seat:e-9 is nothing on Jesper's spoke; the --re routes it.
        let book = crate::address::Book::read(&jesper);
        assert!(crate::address::resolve(&crate::address::parse("seat:e-9").unwrap(), &book).is_err(), "no e-9 here");
        let back = crate::cmd_message::hub_tell(&jesper, &jesper_home, &book, "seat:e-9", "landed at abc123", Some(&id), "seat:j-1", Some("cpd-j1".into()));
        let (_, user, _) = back.expect("a reply to a hub message: through the hub").expect("the hub takes it");
        assert_eq!(user, "ed");

        let hand_e = |seat: &wsp_store_contract::SeatId, text: &str, from: &Principal, key: &str| {
            crate::orders::send_keyed_in(&ed_db, seat, text, from, false, key).map(|o| o.0).map_err(|e| e.to_string())
        };
        assert_eq!(Worker::new(ed_home.clone()).take_messages(&ed, &hand_e).unwrap(), 1);
        let replies = orders_of(&ed_db, "cpd-e1");
        assert_eq!(replies.len(), 1, "{replies:?}");
        assert_eq!(replies[0].0, Principal::Seat(wsp_store_contract::SeatId("cpd-j1@jesper".into())));
        assert!(replies[0].1.starts_with("seat:j-1@jesper says: landed at abc123"), "{}", replies[0].1);
        running.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// wsp-470: a message queued while the receiving spoke is away is told
    /// once it reaches the hub again, exactly once: a turn that took it and
    /// lost its ack tells it no second time, and a worker after a restart takes
    /// nothing.
    #[test]
    fn a_message_to_an_offline_spoke_is_told_on_reconnect_exactly_once() {
        let env = crate::util::isolated("hub-msg-offline");
        let dir = env.home().parent().unwrap().join("offline");
        let _ = std::fs::remove_dir_all(&dir);
        let (running, tokens) = message_hub(&dir);
        let addr = running.addrs()[0];
        let jesper = spoke_with_seat(&dir.join("jesper"), "j-1", "cpd-j1");
        let jesper_home = dir.join("jesper/home");
        // Jesper's spoke is away: his login points at a port nobody listens on.
        let gone = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        log_in(&jesper_home, gone, &tokens["jesper"], "jesper");
        let db = crate::orders::tests::db(&dir.join("jesper/orders"), wsp_server::Mode::Owned);
        crate::orders::tests::seat(&db, "cpd-j1", wsp_store_contract::SeatState::Idle);
        let hand = |seat: &wsp_store_contract::SeatId, text: &str, from: &Principal, key: &str| {
            crate::orders::send_keyed_in(&db, seat, text, from, false, key).map(|o| o.0).map_err(|e| e.to_string())
        };
        let mut worker = Worker::new(jesper_home.clone());
        assert!(worker.take_messages(&jesper, &hand).is_err(), "the hub is out of reach");

        // Meanwhile Ed tells Jesper's primary through the hub, from a shell.
        let ed_home = dir.join("ed/home");
        log_in(&ed_home, addr, &tokens["ed"], "ed");
        let ed = Store::at(dir.join("ed/wsp"), dir.join("ed/state"));
        ed.ensure_dirs().unwrap();
        let book = crate::address::Book { login: Some("ed".into()), ..crate::address::Book::default() };
        let sent = crate::cmd_message::hub_tell(&ed, &ed_home, &book, "primary:jesper", "call me", None, "person:ed", None);
        let (id, ..) = sent.expect("another user's primary").expect("queued while Jesper is away");

        // Jesper's primary is the seat cpd-j1 here; Jesper reconnects.
        crate::cmd_message::tests_govern(&jesper, "primary", "cpd-j1");
        log_in(&jesper_home, addr, &tokens["jesper"], "jesper");
        let mut worker = Worker::new(jesper_home.clone());
        // The first take tells it, and its ack is lost: the hub hands it out again.
        let mail = dial(&jesper_home).unwrap().msg_take().unwrap();
        assert_eq!(mail.len(), 1);
        assert!(crate::cmd_message::receive(&jesper, &mail[0], &hand).unwrap(), "told on reconnect");
        assert_eq!(worker.take_messages(&jesper, &hand).unwrap(), 0, "taken again after the lost ack, not told again");
        let told = orders_of(&db, "cpd-j1");
        assert_eq!(told.len(), 1, "exactly once: {told:?}");
        assert_eq!(told[0].0, Principal::Person("ed".into()), "from a shell: the user");
        assert!(told[0].1.contains(&format!("reply: wsp tell primary:ed --re {id} -]")), "{}", told[0].1);
        assert!(Worker::new(jesper_home.clone()).take_messages(&jesper, &hand).unwrap() == 0);
        assert!(dial(&jesper_home).unwrap().msg_take().unwrap().is_empty(), "acknowledged on the hub");
        assert_eq!(orders_of(&db, "cpd-j1").len(), 1);
        running.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// wsp-470: an address this spoke resolves itself never reaches the hub:
    /// with a login, `seat:`, `governor:`, `primary`, `primary:<me>` and a
    /// failed local address dial nothing, and not a byte reaches the hub's
    /// address; a remote one does dial it.
    #[test]
    fn a_local_address_never_reaches_the_hub() {
        use std::io::Read;
        let env = crate::util::isolated("hub-msg-local");
        let dir = env.home().parent().unwrap().join("local");
        let _ = std::fs::remove_dir_all(&dir);
        let store = spoke_with_seat(&dir.join("ed"), "e-9", "cpd-e1");
        crate::cmd_message::tests_govern(&store, "primary", "cpd-p");
        let home = dir.join("ed/home");
        // The "hub": a listener that keeps every byte sent to it.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        log_in(&home, listener.local_addr().unwrap(), "secret", "ed");
        let wire = std::sync::Arc::new(std::sync::Mutex::new((0usize, Vec::<u8>::new())));
        let w = wire.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut c) = conn else { continue };
                c.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
                let mut buf = [0u8; 4096];
                let n = c.read(&mut buf).unwrap_or(0);
                let mut w = w.lock().unwrap();
                w.0 += 1;
                w.1.extend_from_slice(&buf[..n]);
            }
        });
        let mut book = crate::address::Book::read(&store);
        book.login = Some("ed".into());
        book.remote_rows.insert("j-1".into());
        for local in ["seat:e-9", "primary", "primary:ed", "seat:nope", "governor:nobody", "cpd-e1"] {
            let sent = crate::cmd_message::hub_tell(&store, &home, &book, local, "hello", None, "seat:e-9", None);
            assert!(sent.is_none(), "{local} is this spoke's");
        }
        // A `--re` naming no message the hub delivered here is no reason to dial it.
        assert!(crate::cmd_message::hub_tell(&store, &home, &book, "seat:e-9", "hi", Some("m-0123"), "seat:e-9", None).is_none());
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(wire.lock().unwrap().0, 0, "no connection, no frame");

        // The control: another user's row dials the hub's address.
        let sent = crate::cmd_message::hub_tell(&store, &home, &book, "seat:j-1", "hello", None, "seat:e-9", None);
        assert!(sent.is_some_and(|r| r.is_err()), "through the hub, which this listener is not");
        let w = wire.lock().unwrap();
        assert_eq!(w.0, 1, "one connection");
        assert!(String::from_utf8_lossy(&w.1).contains("\"type\":\"hello\""), "a hello frame: {}", String::from_utf8_lossy(&w.1));
        drop(w);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// wsp-194: what `health` says, as (problems, warnings, notes).
    fn said(store: &Store, home: &std::path::Path, now: i64, alive: bool) -> (Vec<String>, Vec<String>, Vec<String>) {
        let (mut p, mut w, mut n) = (Vec::new(), Vec::new(), Vec::new());
        health(store, Some(home), now, &mut |_| alive, &mut p, &mut w, &mut n);
        (p, w, n)
    }

    /// A wsp with no login says the one line and nothing about a hub.
    #[test]
    fn doctor_says_one_line_for_a_standalone_wsp() {
        let env = crate::util::isolated("hub-doctor-standalone");
        let dir = env.home().parent().unwrap().join("doc-standalone");
        let _ = std::fs::remove_dir_all(&dir);
        let store = spoke_with_seat(&dir.join("s"), "s-1", "cpd-s1");
        let (p, w, n) = said(&store, &dir.join("nohome"), 1_800_000_000, true);
        assert!(p.is_empty() && w.is_empty(), "{p:?} {w:?}");
        assert_eq!(n, vec!["not logged in to a hub (standalone)".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Logged in: the URL and user, the last sync and the journal's length;
    /// and a lease a day from its end with no seat is a warning (a live seat
    /// or a far-off expiry is not).
    #[test]
    fn doctor_names_the_hub_the_last_sync_the_journal_and_a_lease_nothing_renews() {
        let env = crate::util::isolated("hub-doctor-in");
        let dir = env.home().parent().unwrap().join("doc-in");
        let _ = std::fs::remove_dir_all(&dir);
        let store = spoke_with_seat(&dir.join("s"), "s-1", "cpd-s1");
        let home = dir.join("s/home");
        log_in(&home, "127.0.0.1:9".parse().unwrap(), "secret", "ann");
        let now = crate::util::epoch_secs();
        std::fs::create_dir_all(store.state.join("hub")).unwrap();
        std::fs::write(
            store.state.join("hub").join(STATUS_FILE),
            serde_json::json!({
                "last_ok": now - 120,
                "last_failure": { "at": now - 600, "why": "the hub is gone" },
                "journal": 0,
            })
            .to_string(),
        )
        .unwrap();
        store.set_claim(
            "s-1",
            serde_json::json!({ "workspace_id": "w1", "hub_token": 7, "expires": crate::util::iso_at(now + 3600), "ttl_s": 86400 }),
        );

        let (p, w, n) = said(&store, &home, now, false);
        let text = n.join("\n");
        assert!(p.is_empty(), "{p:?}");
        assert!(text.contains("hub tcp://127.0.0.1:9 as ann"), "{text}");
        assert!(text.contains("hub last synced") && text.contains("2m"), "{text}");
        assert!(text.contains("hub journal: 0 entries"), "{text}");
        assert!(text.contains("hub leases held: 1, soonest s-1 expires"), "{text}");
        assert!(text.contains("hub drive leases held: none"), "{text}");
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("hub lease on s-1") && w[0].contains("not alive"), "{w:?}");

        // The same lease with its seat alive, or more than a day from its end, warns of nothing.
        assert!(said(&store, &home, now, true).1.is_empty());
        assert!(said(&store, &home, now - 100_000, false).1.is_empty());

        // A failure after the last good turn is a warning naming the reason.
        std::fs::write(
            store.state.join("hub").join(STATUS_FILE),
            serde_json::json!({ "last_ok": now - 600, "last_failure": { "at": now - 60, "why": "the hub is gone" } }).to_string(),
        )
        .unwrap();
        let (_, w, _) = said(&store, &home, now, true);
        assert!(w.iter().any(|l| l.contains("the hub is gone")), "{w:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
