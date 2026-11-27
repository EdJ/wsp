//! The synced cache (wsp-465, the design's "The synced cache"): what a spoke
//! keeps of every shared workspace its user belongs to.
//!
//! **Whole or a head.** A row that is the spoke's own is kept whole: a file the
//! import door writes (wsp-175), so `ls`, `show`, `wip` and `worklist show` read
//! it as any local row. A row is the spoke's own when it was made here (a file
//! that is not a head), is leased here (a claim of this spoke's), or is
//! allocated to its user (`for:`). Any other row is a **head**: its title,
//! status, holder and allocation, kept in `$WSP_STATE/hub/heads.json`
//! ([`Cache`]) and shown in `ls` as a file with no body (a stub the door never
//! pushes back: [`crate::hub_sync::push_record`] sends nothing for a head).
//! `wsp show <id> --fetch` makes it whole, as does leasing or allocating it:
//! [`classify`] hands the row to the sync whole, which imports the hub's line
//! with the first record written over the stub in one atomic write
//! (`Door::import_over`), and only then drops the cache entry ([`drop_head`]).
//! The stub stays until the whole row replaces it, so a reader (`wsp show`)
//! finds the head or the row at every step, never neither, and a turn the hub
//! cuts short leaves the head as it was.
//!
//! **Why a field of the cache and not of `sync.heads`.** The hub's heads are ids
//! only, and widening the wire's `sync.heads` is the hub's row, not the
//! spoke's. A head is read from the one object a head id names (a single
//! `sync.get`, not its history), and only the head is kept of it.
//!
//! **The stream.** `$WSP_STATE/hub/stream.json` ([`Stream`]) holds the last
//! event id the worker took from the hub's feed and the workspaces it has heard
//! of. A worker that starts within [`GAP`] events of the hub's head resumes
//! from its position; further behind (or never connected) it resumes from the
//! hub's head, and the turn's heads diff (`sync.heads` against what is held)
//! takes the place of the events it skipped: one frame, not a thousand.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde_json::{json, Value};
use crate::hub_client::Client;
use wsp_objects::{Object, ObjectId, Refs};
use wsp_store_contract::{Entity, Principal, Row, RowId, Subject};

use wsp_engine::cmd_message::Caller;
use wsp_engine::cmd_task;
use wsp_engine::door::{task_of_record, user_of, Heads};
use wsp_engine::store::Store;

/// Events behind the hub's head that a worker still replays; past it the
/// heads diff is the cheaper way to catch up.
pub const GAP: u64 = 100;

fn dir(store: &Store) -> PathBuf {
    store.state.join("hub")
}

fn write_json(store: &Store, name: &str, text: String) -> std::io::Result<()> {
    let dir = dir(store);
    std::fs::create_dir_all(&dir)?;
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, dir.join(name))
}

/// `$WSP_STATE/hub/stream.json`: where in the hub's feed the worker is.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Stream {
    /// The last event id taken; none before the first connection.
    pub position: Option<u64>,
    /// The shared workspaces the worker has heard of (`shared:<name>`).
    pub workspaces: BTreeSet<String>,
}

impl Stream {
    pub fn read(store: &Store) -> Stream {
        let v: Value = std::fs::read_to_string(dir(store).join("stream.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Null);
        Stream {
            position: v["position"].as_u64(),
            workspaces: v["workspaces"].as_array().into_iter().flatten().filter_map(|w| w.as_str().map(str::to_string)).collect(),
        }
    }

    pub fn write(&self, store: &Store) -> std::io::Result<()> {
        write_json(store, "stream.json", json!({ "position": self.position, "workspaces": self.workspaces }).to_string())
    }
}

/// Where a feed starts, given the position held and the hub's head now: from
/// the position when it is within [`GAP`] of the head, else from the head, and
/// then the second is true (a heads diff stands in for the events skipped).
pub fn resume(position: Option<u64>, head: u64) -> (u64, bool) {
    match position {
        Some(p) if p <= head && head - p <= GAP => (p, false),
        Some(p) if p > head => (head, true),
        _ => (head, true),
    }
}

/// What a spoke keeps of a row that is not its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    pub workspace: String,
    /// The hub's head object this was read from.
    pub head: String,
    pub title: String,
    pub status: String,
    /// The seat that holds the row, where the record names one.
    pub holder: Option<String>,
    /// The user it is allocated to.
    pub allocation: Option<String>,
    /// The commit its newest landed line names, where it has landed (wsp-468).
    pub landed: Option<String>,
}

impl Head {
    fn to_json(&self) -> Value {
        json!({
            "workspace": self.workspace, "head": self.head, "title": self.title, "status": self.status,
            "holder": self.holder, "allocation": self.allocation, "landed": self.landed,
        })
    }

    fn of_json(v: &Value) -> Option<Head> {
        let text = |k: &str| v[k].as_str().map(str::to_string);
        Some(Head {
            workspace: text("workspace")?,
            head: text("head")?,
            title: text("title")?,
            status: text("status")?,
            holder: text("holder"),
            allocation: text("allocation"),
            landed: text("landed"),
        })
    }
}

/// `$WSP_STATE/hub/heads.json`: the head of each row kept as a head, by row id.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Cache(BTreeMap<String, Head>);

impl Cache {
    pub fn read(store: &Store) -> Cache {
        let text = std::fs::read_to_string(dir(store).join("heads.json")).unwrap_or_default();
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        Cache(v.as_object().into_iter().flatten().filter_map(|(k, h)| Some((k.clone(), Head::of_json(h)?))).collect())
    }

    pub fn write(&self, store: &Store) -> std::io::Result<()> {
        write_json(store, "heads.json", serde_json::to_string_pretty(&self.0.iter().map(|(k, h)| (k.clone(), h.to_json())).collect::<serde_json::Map<_, _>>()).unwrap_or_default())
    }

    pub fn get(&self, row: &str) -> Option<&Head> {
        self.0.get(row)
    }

    pub fn is_head(&self, row: &str) -> bool {
        self.0.contains_key(row)
    }

    pub fn rows(&self) -> impl Iterator<Item = (&String, &Head)> {
        self.0.iter()
    }
}

/// Whether `store` keeps `row` as a head only (the one read `show` and the
/// push make of the cache).
pub fn is_head(store: &Store, row: &str) -> bool {
    Cache::read(store).is_head(row)
}

/// Whether a row is this spoke's own: allocated to `me`, leased here, or a file
/// made here that is not a head.
pub fn is_own(store: &Store, cache: &Cache, me: &str, row: &str, allocation: Option<&str>) -> bool {
    allocation == Some(me)
        || store.claims().contains_key(row)
        || (store.task(row).is_some() && !cache.is_head(row))
}

/// The head `row` is, as of the hub's object `to`.
pub fn head_of(row: &Row, workspace: &str, to: &ObjectId) -> Option<Head> {
    let t = task_of_record(row)?;
    Some(Head {
        workspace: workspace.to_string(),
        head: to.to_string(),
        title: row.title.clone(),
        status: t.status().as_str().to_string(),
        holder: row.seat.as_ref().map(|s| s.0.clone()),
        allocation: t.for_user.clone(),
        landed: wsp_engine::repair::landed_line(&t),
    })
}

fn digest(store: &Store, id: &str) -> Option<ObjectId> {
    std::fs::read(store.task_path(id)).ok().map(|b| ObjectId::of(&b))
}

/// Keep `row` as a head: its stub file through the door's writes, the object
/// `to` as the head synced (so the stub is nothing to push), and the cache.
pub fn keep_head(store: &Store, cache: &mut Cache, row: &Row, workspace: &str, to: ObjectId, by: &Principal) -> Result<(), String> {
    let head = head_of(row, workspace, &to).ok_or_else(|| format!("{} carries neither its text nor its head", row.id.0))?;
    let mut t = task_of_record(row).ok_or_else(|| format!("{} is not a task", row.id.0))?;
    t.body = String::new();
    let caller = Caller::hub(&user_of(by));
    // The cache entry goes down before the stub, so a reader that sees the
    // stub in `ls` finds the entry that says it is a head (`show`, a test).
    let id = t.id.clone();
    let before = cache.0.insert(id.clone(), head);
    let undo = |cache: &mut Cache| {
        match &before {
            Some(h) => cache.0.insert(id.clone(), h.clone()),
            None => cache.0.remove(&id),
        };
        let _ = cache.write(store);
    };
    cache.write(store).map_err(|e| e.to_string())?;
    let written = match store.task(&t.id) {
        Some(_) => cmd_task::write_task(store, &t, "head", &caller),
        None => cmd_task::file_new(store, &mut t, &caller),
    };
    if let Err(e) = written {
        undo(cache);
        return Err(e.to_string());
    }
    let subject = Subject::Row(RowId(t.id.clone()));
    let mut heads = Heads::read(store);
    heads.set(&subject, to, digest(store, &t.id));
    heads.write(store).map_err(|e| e.to_string())
}

/// A head the sync has made whole: its cache entry goes, once the row's file
/// is the whole row (the stub was written over, never removed first).
pub fn drop_head(store: &Store, row: &str) {
    let mut cache = Cache::read(store);
    if cache.0.remove(row).is_some() {
        let _ = cache.write(store);
    }
}

/// What one workspace's classification decided.
#[derive(Debug, Default)]
pub struct Classified {
    /// Subjects the sync takes whole: new own rows (and `forced`).
    pub whole: BTreeSet<Subject>,
    /// Of `whole`, the rows kept here as heads until now: their stubs stay
    /// until the sync writes the whole row over them.
    pub promoting: BTreeSet<Subject>,
    /// Heads written or moved.
    pub heads: usize,
}

/// Sort the hub's heads of `workspace` into whole and head. A subject the
/// spoke has no file for, or holds only as a head at another id, is read from
/// its one head object: own, it goes to the sync whole (and to `promoting` if
/// it was a head, its stub left in place); otherwise its head is kept.
/// `forced` is taken whole as asked (`wsp show --fetch`); `passed` is passed
/// over (rows moved away here, `hub_retire::skip`). Nothing here removes
/// a file.
pub fn classify(
    store: &Store,
    client: &mut Client,
    workspace: &str,
    hub: &Refs,
    me: &str,
    by: &Principal,
    forced: &BTreeSet<Subject>,
    passed: &BTreeSet<Subject>,
) -> Result<Classified, String> {
    let mut cache = Cache::read(store);
    let mut out = Classified::default();
    let mut want: Vec<(Subject, ObjectId)> = Vec::new();
    for (subject, to) in &hub.heads {
        let Subject::Row(id) = subject else { continue };
        // A row this spoke moved to a new id (`hub_retire`): its old subject is
        // being retired, and is never kept as a head here meanwhile.
        if passed.contains(subject) {
            continue;
        }
        if forced.contains(subject) {
            if cache.is_head(&id.0) {
                out.promoting.insert(subject.clone());
            }
            out.whole.insert(subject.clone());
            continue;
        }
        match cache.get(&id.0) {
            Some(h) if h.head == to.to_string() => {
                // Unchanged; leased or allocated since it was kept: whole.
                if is_own(store, &cache, me, &id.0, h.allocation.as_deref()) {
                    out.promoting.insert(subject.clone());
                    out.whole.insert(subject.clone());
                }
            }
            Some(_) => want.push((subject.clone(), *to)),
            None if store.task(&id.0).is_some() => {}
            None => want.push((subject.clone(), *to)),
        }
    }
    if want.is_empty() {
        return Ok(out);
    }
    let blobs = client.sync_get(want.iter().map(|(_, to)| *to).collect()).map_err(|e| e.to_string())?;
    let mut by_id: BTreeMap<ObjectId, Vec<u8>> = BTreeMap::new();
    for b in blobs {
        if wsp_objects::verify(&b.id, &b.bytes).is_ok() {
            by_id.insert(b.id, b.bytes);
        }
    }
    for (subject, to) in want {
        let Some(Object::Record { entity: Entity::Row(row), .. }) =
            by_id.get(&to).and_then(|b| wsp_objects::decode(b).ok())
        else {
            continue;
        };
        let allocation = task_of_record(&row).and_then(|t| t.for_user);
        if is_own(store, &cache, me, &row.id.0, allocation.as_deref()) {
            if cache.is_head(&row.id.0) {
                out.promoting.insert(subject.clone());
            }
            out.whole.insert(subject);
        } else if let Err(why) = keep_head(store, &mut cache, &row, workspace, to, by) {
            wsp_engine::store::log_line(store, &format!("hub-sync: the head of {} was not kept: {why}", row.id.0));
        } else {
            out.heads += 1;
        }
    }
    Ok(out)
}

/// The line `show` prints for a row kept as a head, `None` for any other.
/// Who holds it is the row's facts' (`facts`: `held_by`, `held_on`, the
/// lease as last heard, [`crate::spoke::Spoke::row_facts`]), as `show --json`
/// and the feed say it, not the seat the hub's record names (`Head::holder`).
pub fn show_line(store: &Store, row: &str, facts: Option<&wsp_engine::addon::RowFacts>) -> Option<String> {
    let cache = Cache::read(store);
    let h = cache.get(row)?;
    let holder = match facts.and_then(|f| f.held_by.as_deref().map(|u| (u, f.held_on.as_deref()))) {
        Some((user, Some(machine))) => format!("{user} on {machine}"),
        Some((user, None)) => user.to_string(),
        None => "nobody".to_string(),
    };
    let alloc = h.allocation.as_deref().unwrap_or("nobody");
    Some(format!(
        "head only: {} · {} · held by {holder} · allocated to {alloc} — the hub holds the rest; `wsp show {row} --fetch` brings it whole",
        h.workspace, h.status
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> (wsp_engine::util::Isolated, Store) {
        let env = wsp_engine::util::isolated(tag);
        let store = Store::open();
        store.ensure_dirs().unwrap();
        store.save_project(&wsp_engine::model::Project::new("wsp")).unwrap();
        (env, store)
    }

    fn record(id: &str, title: &str, for_user: Option<&str>) -> Row {
        let mut t = wsp_engine::model::Task::new(title, id);
        t.project = Some("wsp".into());
        t.workspace = "shared:mp".parse().unwrap();
        t.for_user = for_user.map(str::to_string);
        t.body = "## Overview\nthe whole text\n".into();
        let text = t.render();
        wsp_engine::mirror::shared_row(&t, &text).expect("a shared row")
    }

    fn ann() -> Principal {
        Principal::Person("ann".into())
    }

    #[test]
    fn a_feed_resumes_within_the_gap_and_otherwise_from_the_head() {
        assert_eq!(resume(Some(40), 100), (40, false), "close behind: replay");
        assert_eq!(resume(Some(100), 100), (100, false));
        assert_eq!(resume(Some(1), 1001), (1001, true), "1,000 behind: the head, and a heads diff");
        assert_eq!(resume(None, 7), (7, true), "never connected: the head");
        assert_eq!(resume(Some(900), 5), (5, true), "a hub that restarted from less");
    }

    #[test]
    fn the_stream_file_keeps_the_position_and_the_workspaces() {
        let (_env, store) = scratch("hc-stream");
        assert_eq!(Stream::read(&store), Stream::default());
        let s = Stream { position: Some(12), workspaces: BTreeSet::from(["shared:mp".to_string()]) };
        s.write(&store).unwrap();
        assert_eq!(Stream::read(&store), s);
        assert!(store.state.join("hub/stream.json").exists());
    }

    #[test]
    fn a_head_is_a_listed_row_without_its_text_and_is_nothing_to_push() {
        let (_env, store) = scratch("hc-head");
        let row = record("wsp-7", "Someone else's work", Some("bob"));
        let mut cache = Cache::default();
        let to = ObjectId::of(b"the head");
        keep_head(&store, &mut cache, &row, "shared:mp", to, &ann()).unwrap();

        let t = store.task("wsp-7").expect("ls reads it as a row");
        assert_eq!(t.title, "Someone else's work");
        assert!(!t.body.contains("the whole text"), "only the head: {:?}", t.body);
        let h = Cache::read(&store);
        assert_eq!(h.get("wsp-7").map(|h| (h.title.as_str(), h.allocation.as_deref())), Some(("Someone else's work", Some("bob"))));
        assert!(wsp_engine::door::pending(&store).is_empty(), "a head is not pushed back");
        assert!(show_line(&store, "wsp-7", None).unwrap().contains("--fetch"));
        assert!(!is_own(&store, &h, "ann", "wsp-7", Some("bob")), "bob's, not ann's");
        assert!(is_own(&store, &h, "bob", "wsp-7", Some("bob")), "allocated to bob: his");
    }

    /// `wsp show` of a head-only row names its holder as the row's facts do
    /// (`held_by`, `held_on`: the lease the stream last said), not the seat
    /// the record names: a row bo leases on box-b is "held by bo on box-b",
    /// and one nobody is known to hold is "held by nobody", as `held_by` null.
    #[test]
    fn a_head_only_rows_show_line_names_the_holder_its_facts_name() {
        use wsp_engine::addon::SharedWork;
        let (_env, store) = scratch("hc-held");
        let mut cache = Cache::default();
        keep_head(&store, &mut cache, &record("wsp-7", "Bo's work", None), "shared:mp", ObjectId::of(b"h7"), &ann()).unwrap();
        keep_head(&store, &mut cache, &record("wsp-9", "Nobody's", None), "shared:mp", ObjectId::of(b"h9"), &ann()).unwrap();
        crate::hub_holders::follow(
            &store,
            wsp_store_contract::EventKind::LeaseTaken,
            &serde_json::json!({ "workspace": "shared:mp", "row": "wsp-7", "holder": { "person": "bo" }, "token": 3, "expires": 4_000_000_000_000i64, "cause": "take", "machine": { "id": "m-b", "name": "box-b" } }),
        );
        let spoke = crate::spoke::Spoke::new();
        let facts = spoke.row_facts(&store);
        assert_eq!((facts["wsp-7"].held_by.as_deref(), facts["wsp-7"].held_on.as_deref(), facts["wsp-7"].head_only), (Some("bo"), Some("box-b"), true));
        let said = spoke.show_lines(&store, "wsp-7").join("\n");
        assert!(said.contains("head only: shared:mp · todo · held by bo on box-b · allocated to nobody"), "{said}");
        assert_eq!(facts["wsp-9"].held_by, None);
        let said = spoke.show_lines(&store, "wsp-9").join("\n");
        assert!(said.contains("· held by nobody ·"), "{said}");
    }

    /// The whole row is written over the stub by the door, then the cache
    /// entry goes: the file is there throughout, and the synced head is the
    /// record's.
    #[test]
    fn a_head_made_whole_is_written_over_its_stub_and_then_leaves_the_cache() {
        let (_env, store) = scratch("hc-promote");
        let row = record("wsp-8", "Taken on", None);
        let mut cache = Cache::default();
        keep_head(&store, &mut cache, &row, "shared:mp", ObjectId::of(b"h"), &ann()).unwrap();
        assert!(store.task("wsp-8").is_some_and(|t| !t.body.contains("the whole text")));
        let obj = Object::Record {
            workspace: wsp_objects::WorkspaceId("shared:mp".into()),
            subject: Subject::Row(RowId("wsp-8".into())),
            entity: Entity::Row(row),
            parent: None,
            by: ann(),
            at: wsp_store_contract::At(1),
        };
        let door = wsp_engine::door::Door::new(&store, &wsp_engine::door::Unreconciled);
        assert!(door.import(&obj, &ann()).is_err(), "over a stub, a plain import is a divergence");
        door.import_over(&obj, &ann()).expect("written over the stub");
        assert!(store.task("wsp-8").is_some_and(|t| t.body.contains("the whole text")), "whole");
        assert!(Cache::read(&store).is_head("wsp-8"), "the entry stays until the caller drops it");
        drop_head(&store, "wsp-8");
        assert!(!Cache::read(&store).is_head("wsp-8"));
        assert_eq!(Heads::read(&store).head(&Subject::Row(RowId("wsp-8".into()))), Some(&wsp_objects::encode(&obj).0));
        assert!(show_line(&store, "wsp-8", None).is_none());
        assert!(wsp_engine::door::pending(&store).is_empty(), "the import is nothing to push back");
    }

    #[test]
    fn a_row_made_here_is_own_and_one_with_no_file_or_claim_is_not() {
        let (_env, store) = scratch("hc-own");
        let mut t = wsp_engine::model::Task::new("mine", "wsp-9");
        t.project = Some("wsp".into());
        store.save_task(&t).unwrap();
        let cache = Cache::default();
        assert!(is_own(&store, &cache, "ann", "wsp-9", None), "made here");
        assert!(!is_own(&store, &cache, "ann", "wsp-10", None), "no file, no claim");
    }
}
