//! Row ids for shared work (the owner's decision on ids): every shared row
//! takes its id from the hub's per-project counter, the one `wsp publish`
//! takes from (`ids.alloc`, past the highest id this store has used), so two
//! people filing into one shared project never pick the same id.
//!
//! - **Connected**: a row filed directly as shared (`wsp add --workspace
//!   shared:x`, a sub-task of a shared row) takes a hub id when it is made,
//!   before anything is written ([`new_row_id`]); a refusal (not a member)
//!   writes nothing.
//! - **Not connected** (the hub does not answer, or the connection is switched
//!   off with a login there): the row takes a local id and is *provisional*:
//!   kept here in `$WSP_STATE/hub/provisional.json` ([`Book`]), stamped with
//!   its shared workspace, never pushed and never leased under that id. A
//!   sub-task of a provisional row is provisional with it. At the next turn
//!   that reaches the hub the sync worker takes hub ids for them and renames
//!   them the way publish does ([`settle`], publish's own
//!   `allocate_map`/`renumber_with`): the old id keeps resolving through
//!   `ids.json`, pieces, sub-tasks' `parent:`, list members, legs, claims and
//!   seat bindings follow, and a claimed row is leased at its new id first.
//! - **Crash-safe**: the map of new ids is written to the book before any row
//!   is renamed, and a turn that finds one finishes that rename with the same
//!   ids rather than asking for more: ids a crash burnt are gaps, and no row is
//!   left provisional.
//!
//! A private row, and a wsp with no login, take the local counter as always.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use wsp_engine::model::WorkspaceId;
use wsp_engine::store::{split_piece_id, Store, INBOX_CODE};

use crate::cmd_publish::{self, Plan};
use crate::hub_client::Client;

/// `$WSP_STATE/hub/provisional.json`: rows filed shared while the hub was not
/// reached, each with its workspace, and the new ids a rename in progress
/// gave them.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Book {
    /// Row (its local id) -> the shared workspace it was filed into.
    pub rows: BTreeMap<String, String>,
    /// Old id -> the hub's id for it, written before the rename starts.
    pub pending: BTreeMap<String, String>,
}

impl Book {
    fn path(store: &Store) -> std::path::PathBuf {
        store.state.join("hub").join("provisional.json")
    }

    pub fn read(store: &Store) -> Book {
        let Ok(text) = std::fs::read_to_string(Book::path(store)) else { return Book::default() };
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let map = |k: &str| -> BTreeMap<String, String> {
            v[k].as_object().into_iter().flatten().filter_map(|(a, b)| Some((a.clone(), b.as_str()?.to_string()))).collect()
        };
        Book { rows: map("rows"), pending: map("pending") }
    }

    fn write(&self, store: &Store) -> std::io::Result<()> {
        let path = Book::path(store);
        if self.rows.is_empty() && self.pending.is_empty() {
            return match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        }
        let dir = path.parent().expect("under hub/");
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!(".provisional.json.{}.tmp", std::process::id()));
        std::fs::write(&tmp, json!({ "rows": self.rows, "pending": self.pending }).to_string())?;
        std::fs::rename(&tmp, &path)
    }

    /// Read, change and write back under the store's lock.
    pub fn edit<T>(store: &Store, f: impl FnOnce(&mut Book) -> T) -> std::io::Result<T> {
        store.locked(|| {
            let mut b = Book::read(store);
            let out = f(&mut b);
            b.write(store).map(|_| out)
        })
    }
}

/// Whether `row` is provisional here: a shared row on a local id.
pub fn is_provisional(store: &Store, row: &str) -> bool {
    Book::read(store).rows.contains_key(row)
}

/// The project code a row of `project` numbers under.
fn code_of(store: &Store, project: Option<&str>) -> String {
    project.and_then(|p| store.project(p)).map(|p| p.code().to_string()).unwrap_or_else(|| INBOX_CODE.to_string())
}

/// The id a new row of `project` filed into `ws` takes ([`wsp_engine::addon::SharedWork::new_row_id`]):
/// `None` for a private one (the local counter), a hub id when the hub answers,
/// a provisional local id when it cannot be asked, and `Err` when it refuses.
pub fn new_row_id(store: &Store, project: Option<&str>, ws: &WorkspaceId, parent: Option<&str>) -> Result<Option<String>, String> {
    if ws.is_private() {
        return Ok(None);
    }
    // A child of a provisional row is provisional with it, renamed in its step.
    if parent.is_some_and(|p| is_provisional(store, p)) {
        return provisional(store, project, ws).map(Some);
    }
    if crate::hub_switch::off(store) {
        return provisional(store, project, ws).map(Some);
    }
    let Some(home) = crate::hub_client::hub_home() else { return provisional(store, project, ws).map(Some) };
    let mut client = match cmd_publish::dial(&home) {
        Ok(c) => c,
        Err(_) => return provisional(store, project, ws).map(Some),
    };
    from_hub(store, &mut client, project, ws)
}

/// A hub id for a new row, reserved here (an empty file at its path, as the
/// local counter leaves one) and the project's counter moved past it.
fn from_hub(store: &Store, client: &mut Client, project: Option<&str>, ws: &WorkspaceId) -> Result<Option<String>, String> {
    let code = code_of(store, project);
    let hub_ws = wsp_objects::WorkspaceId(ws.to_string());
    for _ in 0..8 {
        let id = match cmd_publish::allocate_or(store, client, &hub_ws, &code, 1) {
            Ok(mut got) => got.remove(0),
            Err(e) if e.unreachable() => return provisional(store, project, ws).map(Some),
            Err(e) => return Err(format!("the hub gave no id for a row in {ws}: {}", e.said())),
        };
        std::fs::create_dir_all(store.tasks_dir()).map_err(|e| e.to_string())?;
        match std::fs::OpenOptions::new().write(true).create_new(true).open(store.task_path(&id)) {
            Ok(_) => {
                bump_seq(store, project, &id);
                return Ok(Some(id));
            }
            // Filed here since the hub was asked: ask again, above it.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    Err(format!("the hub kept handing out ids this store already has under {code}"))
}

/// The project's local counter past `id`, so the next private row does not
/// take a number the hub has handed this spoke.
fn bump_seq(store: &Store, project: Option<&str>, id: &str) {
    let Some(mut p) = project.and_then(|p| store.project(p)) else { return };
    let Some(n) = id.rsplit_once('-').and_then(|(_, n)| n.parse::<u32>().ok()) else { return };
    if (p.seq as u64) < n as u64 {
        p.seq = n as _;
        let _ = store.save_project(&p);
    }
}

/// A local id, kept as provisional for `ws`.
fn provisional(store: &Store, project: Option<&str>, ws: &WorkspaceId) -> Result<String, String> {
    let id = store.alloc_task_id(project).map_err(|e| e.to_string())?;
    Book::edit(store, |b| {
        b.rows.insert(id.clone(), ws.to_string());
    })
    .map_err(|e| e.to_string())?;
    Ok(id)
}

/// What a turn's [`settle`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Settled {
    /// Old id -> hub id, rows and pieces.
    pub renamed: Vec<(String, String)>,
    /// Workspaces whose rows stay provisional this turn, and why.
    pub waiting: Vec<(String, String)>,
}

/// The provisional rows of one workspace as a publish plan: the rows parents
/// first, each with its project's code, and the pieces of them.
fn plan_of(store: &Store, rows: &[String]) -> Plan {
    let set: BTreeSet<&str> = rows.iter().map(String::as_str).collect();
    let depth = |id: &str| {
        let (mut d, mut at) = (0usize, id.to_string());
        while let Some(p) = store.task(&at).and_then(|t| t.parent).filter(|p| set.contains(p.as_str())) {
            d += 1;
            at = p;
            if d > 64 {
                break;
            }
        }
        d
    };
    let mut sorted: Vec<&String> = rows.iter().collect();
    sorted.sort_by_key(|id| (depth(id), (*id).clone()));
    let rows = sorted.into_iter().map(|id| (id.clone(), code_of(store, store.task(id).and_then(|t| t.project).as_deref()))).collect();
    let pieces = store
        .tasks()
        .into_iter()
        .filter_map(|t| split_piece_id(&t.id).filter(|(row, _)| set.contains(row)).map(|(row, _)| (t.id.clone(), row.to_string())))
        .collect();
    Plan { list: None, rows, pieces }
}

/// The provisional rows take hub ids and are renamed, workspace by workspace,
/// before a turn pushes anything (the sync worker, connected and switched on).
/// A rename a crash cut short is finished first, with the ids it had.
pub fn settle(store: &Store, client: &mut Client) -> Settled {
    let mut out = Settled::default();
    let book = Book::read(store);
    if book.rows.is_empty() {
        return out;
    }
    let mut by_ws: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (row, ws) in &book.rows {
        // A row gone (removed, archived) is no longer waiting.
        if store.task(row).is_none() && !book.pending.get(row).is_some_and(|new| store.task(new).is_some()) {
            let _ = Book::edit(store, |b| {
                b.rows.remove(row);
                b.pending.remove(row);
            });
            continue;
        }
        by_ws.entry(ws.clone()).or_default().push(row.clone());
    }
    for (ws_name, rows) in by_ws {
        let Ok(ws) = ws_name.parse::<WorkspaceId>() else { continue };
        let plan = plan_of(store, &rows);
        // The ids: those a cut-short rename wrote down, else the hub's now.
        let known: BTreeMap<String, String> = rows.iter().filter_map(|r| Some((r.clone(), book.pending.get(r)?.clone()))).collect();
        let map = if known.len() == rows.len() {
            known
        } else {
            let fresh: Vec<(String, String)> = plan.rows.iter().filter(|(r, _)| !known.contains_key(r)).cloned().collect();
            let ask = Plan { list: None, rows: fresh, pieces: Vec::new() };
            match cmd_publish::allocate_map(store, client, &ws, &ask) {
                Ok(m) => known.into_iter().chain(m).collect(),
                Err(e) => {
                    out.waiting.push((ws_name, e.said()));
                    continue;
                }
            }
        };
        // Written down before any row moves: a crash from here on is finished
        // with these ids, not new ones.
        if let Err(e) = Book::edit(store, |b| b.pending.extend(map.clone())) {
            out.waiting.push((ws_name, e.to_string()));
            continue;
        }
        match cmd_publish::renumber_with(store, client, &ws, &plan, map) {
            Ok(made) => {
                let _ = Book::edit(store, |b| {
                    for r in &rows {
                        b.rows.remove(r);
                        b.pending.remove(r);
                    }
                });
                out.renamed.extend(made);
            }
            Err(why) => out.waiting.push((ws_name, why)),
        }
    }
    out
}

/// What `wsp hub status` says of rows waiting for a hub id.
pub fn status_line(store: &Store) -> Option<String> {
    let rows = Book::read(store).rows;
    (!rows.is_empty()).then(|| {
        format!(
            "waiting for a hub id: {} row(s), on local ids until the next connection ({})",
            rows.len(),
            rows.keys().cloned().collect::<Vec<_>>().join(" ")
        )
    })
}


#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use wsp_engine::base::args::Args;
    use wsp_engine::store::Store;
    use wsp_store_contract::{RowId, Subject};

    use super::*;
    use crate::hub_lease;
    use crate::spoke::Spoke;

    /// `$WSP_HUB_HOME` at `home` while the guard lives, put back after.
    struct HubHome(Option<std::ffi::OsString>);

    impl HubHome {
        fn at(home: &Path) -> HubHome {
            let was = std::env::var_os(crate::hub_client::HUB_HOME_ENV);
            std::env::set_var(crate::hub_client::HUB_HOME_ENV, home);
            HubHome(was)
        }
    }

    impl Drop for HubHome {
        fn drop(&mut self) {
            match self.0.take() {
                Some(h) => std::env::set_var(crate::hub_client::HUB_HOME_ENV, h),
                None => std::env::remove_var(crate::hub_client::HUB_HOME_ENV),
            }
        }
    }

    /// One user's spoke: its store, with project `pp`, and its login home.
    struct Side {
        store: Store,
        home: PathBuf,
        who: String,
    }

    impl Side {
        fn bare(env: &wsp_engine::util::Isolated, who: &str) -> Side {
            let root = env.path(&format!("spoke-{who}"));
            let store = Store::at(root.join("wsp"), root.join("state"));
            store.ensure_dirs().unwrap();
            store.save_project(&wsp_engine::model::Project::new("pp")).unwrap();
            Side { store, home: root.join(format!("home-{who}")), who: who.to_string() }
        }

        fn new(env: &wsp_engine::util::Isolated, hub: &crate::testhub::TestHub, tokens: &BTreeMap<String, String>, who: &str) -> Side {
            let side = Side::bare(env, who);
            side.log_in(hub, tokens);
            side
        }

        fn log_in(&self, hub: &crate::testhub::TestHub, tokens: &BTreeMap<String, String>) {
            hub.login(&self.home, &self.who, &tokens[&self.who]);
        }

        /// Logged in to a hub that does not answer: a port nothing listens on.
        fn log_in_nowhere(&self) {
            use std::os::unix::fs::PermissionsExt;
            let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
            let file = self.home.join(crate::hub_client::HUBS_FILE);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, json!({ "url": format!("tcp://127.0.0.1:{port}"), "token": "t", "user": self.who, "at": "2026-11-23T00:00:00Z" }).to_string()).unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        }

        /// `wsp add <title> -p pp --workspace <ws>` (and `--parent`) on this spoke: the id, or the exit code.
        fn add_in(&self, title: &str, ws: &str, parent: Option<&str>) -> Result<String, i32> {
            let _home = HubHome::at(&self.home);
            let before: Vec<String> = self.store.tasks().into_iter().map(|t| t.id).collect();
            let mut flags = vec![("project", "pp"), ("workspace", ws)];
            if let Some(p) = parent {
                flags.push(("parent", p));
            }
            let code = wsp_engine::cmd_task::add(&self.store, &Args::synth("add", &[title], &flags));
            if code != 0 {
                return Err(code);
            }
            Ok(self.store.tasks().into_iter().map(|t| t.id).find(|id| !before.contains(id)).expect("a new row"))
        }

        fn add(&self, title: &str) -> Result<String, i32> {
            self.add_in(title, "shared:team", None)
        }

        /// `wsp add <title> --workspace shared:team` with no project: a shared row in the inbox.
        fn add_inbox(&self, title: &str) -> String {
            let _home = HubHome::at(&self.home);
            let before: Vec<String> = self.store.tasks().into_iter().map(|t| t.id).collect();
            let code = wsp_engine::cmd_task::add(&self.store, &Args::synth("add", &[title], &[("workspace", "shared:team")]));
            assert_eq!(code, 0, "add to the inbox");
            self.store.tasks().into_iter().map(|t| t.id).find(|id| !before.contains(id)).expect("a new row")
        }

        /// `wsp mv <id> -p pp` on this spoke: the row's id after it.
        fn mv_to_pp(&self, id: &str) -> String {
            let _home = HubHome::at(&self.home);
            assert_eq!(wsp_engine::cmd_task::mv(&self.store, &Args::synth("mv", &[id], &[("project", "pp")])), 0, "mv {id} -p pp");
            self.store.renamed_ids().get(id).cloned().unwrap_or_else(|| id.to_string())
        }

        fn turn(&self) -> Result<crate::hub_sync::Turned, String> {
            let _home = HubHome::at(&self.home);
            crate::hub_sync::Worker::new(self.home.clone()).turn(&self.store)
        }

        fn title(&self, id: &str) -> String {
            self.store.task(id).map(|t| t.title).unwrap_or_default()
        }
    }

    /// The row ids `shared:team` has on the hub, read by a member.
    fn on_hub(hub: &crate::testhub::TestHub, tokens: &BTreeMap<String, String>, who: &str) -> Vec<String> {
        let refs = hub_lease::tests::dial(hub, tokens, who).sync_heads(&wsp_objects::WorkspaceId("shared:team".into())).unwrap();
        refs.heads.keys().filter_map(|s| match s {
            Subject::Row(RowId(id)) => Some(id.clone()),
            _ => None,
        }).collect()
    }

    /// The title the hub's head of row `id` in `shared:team` carries, if it has one.
    fn hub_title(hub: &crate::testhub::TestHub, tokens: &BTreeMap<String, String>, who: &str, id: &str) -> Option<String> {
        let mut c = hub_lease::tests::dial(hub, tokens, who);
        let refs = c.sync_heads(&wsp_objects::WorkspaceId("shared:team".into())).unwrap();
        let head = *refs.heads.get(&Subject::Row(RowId(id.to_string())))?;
        let blob = c.sync_get(vec![head]).unwrap().into_iter().next()?;
        match wsp_objects::decode(&blob.bytes).ok()? {
            wsp_objects::Object::Record { entity: wsp_store_contract::Entity::Row(r), .. } => Some(r.title),
            _ => None,
        }
    }

    /// The gap, and its fix: two spokes filing directly into one shared
    /// project. Before, both took `pp-001` from their local counters and the
    /// second was merged into the first as an edit (its title lost to a
    /// conflict note). Now each takes its id from the hub's counter, and both
    /// rows reach the hub, each whole.
    #[test]
    fn hub_ids_two_spokes_filing_directly_into_one_shared_project_take_different_ids() {
        let Some((hub, tokens)) = hub_lease::tests::serve_users(crate::testhub::Setup::default(), &["ann", "bo"]) else { return };
        let env = wsp_engine::util::isolated("hub-ids-two");
        let _addon = wsp_engine::addon::scoped(Arc::new(Spoke::new()));
        let ann = Side::new(&env, &hub, &tokens, "ann");
        let bo = Side::new(&env, &hub, &tokens, "bo");
        let a = ann.add("ann's work").unwrap();
        let b = bo.add("bo's work").unwrap();
        assert_ne!(a, b, "two spokes filing into one shared project take different ids");
        assert!(!is_provisional(&ann.store, &a) && !is_provisional(&bo.store, &b), "connected: hub ids, not provisional");
        ann.turn().unwrap();
        bo.turn().unwrap();
        ann.turn().unwrap();
        assert_eq!(ann.title(&a), "ann's work");
        assert_eq!(bo.title(&b), "bo's work", "bo's row is bo's, not an edit of ann's");
        assert!(!bo.store.task(&b).unwrap().body.contains("conflict"), "no merge happened");
        let ids = on_hub(&hub, &tokens, "ann");
        assert!(ids.contains(&a) && ids.contains(&b), "{ids:?}");
        // A private row still takes the local counter.
        let p = ann.add_in("mine", "private", None).unwrap();
        assert!(ann.store.task(&p).unwrap().workspace.is_private());
    }

    /// A filing the hub refuses (not a member of the workspace) writes nothing.
    #[test]
    fn hub_ids_a_filing_the_hub_refuses_writes_nothing() {
        let Some((hub, tokens)) = hub_lease::tests::serve_users(crate::testhub::Setup::default(), &["ann"]) else { return };
        hub.apply(crate::testhub::protocol::IdentityCommand::HubWorkspaceAdd { name: crate::testhub::protocol::SharedName("other".into()) });
        let env = wsp_engine::util::isolated("hub-ids-refused");
        let _addon = wsp_engine::addon::scoped(Arc::new(Spoke::new()));
        let ann = Side::new(&env, &hub, &tokens, "ann");
        let before = ann.store.tasks().len();
        assert_eq!(ann.add_in("not mine to file", "shared:other", None), Err(1));
        assert_eq!(ann.store.tasks().len(), before, "nothing written");
        assert!(Book::read(&ann.store).rows.is_empty(), "nothing kept as provisional");
        let files = std::fs::read_dir(ann.store.tasks_dir()).map(|d| d.count()).unwrap_or(0);
        assert_eq!(files, before, "no reservation left behind");
    }

    /// Offline (the hub does not answer): the row is filed on a provisional
    /// local id, marked so in `show` and `ls`, counted in status, never pushed
    /// and never leased; its sub-task is provisional with it. At the next
    /// connection both take hub ids in one step: the old ids still resolve,
    /// the sub-task's `parent:` follows, and the hub has them under the new
    /// ids only.
    #[test]
    fn hub_ids_offline_a_row_is_provisional_then_renamed_at_reconnect_with_its_children() {
        let Some((hub, tokens)) = hub_lease::tests::serve_users(crate::testhub::Setup::default(), &["ann", "bo"]) else { return };
        let env = wsp_engine::util::isolated("hub-ids-offline");
        let _addon = wsp_engine::addon::scoped(Arc::new(Spoke::new()));
        let ann = Side::bare(&env, "ann");
        ann.log_in_nowhere();
        let row = ann.add("filed on the train").unwrap();
        let kid = ann.add_in("its sub-task", "shared:team", Some(&row)).unwrap();
        assert!(is_provisional(&ann.store, &row) && is_provisional(&ann.store, &kid));
        assert!(ann.store.task(&row).unwrap().workspace.to_string() == "shared:team", "filed shared");
        let spoke = Spoke::new();
        use wsp_engine::addon::SharedWork;
        assert_eq!(spoke.marks(&ann.store).get(&row).map(String::as_str), Some("not yet published"));
        assert!(spoke.show_lines(&ann.store, &row).iter().any(|l| l.starts_with("not yet published")));
        assert!(crate::hub_switch::status_lines(&ann.store).iter().any(|l| l.starts_with("waiting for a hub id: 2 row(s)")));
        assert_eq!(spoke.before_claim(&ann.store, &row), Ok(None), "no lease on a local id");
        let subject = Subject::Row(RowId(row.clone()));
        let by = wsp_store_contract::Principal::Person("ann".into());
        assert!(crate::hub_sync::push_record(&ann.store, &subject, wsp_objects::WorkspaceId("shared:team".into()), by, wsp_store_contract::At(0)).is_none());

        // bo files meanwhile: the hub's counter moves on.
        let bo = Side::new(&env, &hub, &tokens, "bo");
        let bos = bo.add("bo's").unwrap();
        bo.turn().unwrap();

        // ann reconnects: the turn renames, then pushes.
        ann.log_in(&hub, &tokens);
        ann.turn().unwrap();
        assert!(Book::read(&ann.store).rows.is_empty() && Book::read(&ann.store).pending.is_empty());
        let ids = ann.store.renamed_ids();
        let (new_row, new_kid) = (ids[&row].clone(), ids[&kid].clone());
        assert_ne!(new_row, bos, "not bo's id");
        assert_eq!(ann.title(&new_row), "filed on the train");
        assert_eq!(ann.store.task(&new_kid).unwrap().parent.as_deref(), Some(new_row.as_str()), "the sub-task follows");
        // The old id is recorded as renamed (`ids.json`), and finds the row
        // while no row of its own has that id; here the hub had given that
        // very number to bo, whose row is now on this spoke under it.
        match ann.store.task(&row) {
            Some(t) => assert_eq!((t.id.as_str(), t.title.as_str()), (bos.as_str(), "bo's"), "the number is bo's now"),
            None => assert_eq!(ann.store.find_task(&row).map(|t| t.id), Some(new_row.clone()), "the old id still finds it"),
        }
        let on = on_hub(&hub, &tokens, "bo");
        assert!(on.contains(&new_row) && on.contains(&new_kid), "{on:?}");
        // Never under its local id: the hub's row of that number, if any, is
        // somebody else's (here bo's, who was given it).
        assert_ne!(hub_title(&hub, &tokens, "bo", &row).as_deref(), Some("filed on the train"), "never under its local id: {on:?}");
        assert_eq!(hub_title(&hub, &tokens, "bo", &new_row).as_deref(), Some("filed on the train"));
    }

    /// Switched off with a login is offline: the row is provisional and a
    /// turn (the disable's own flush) pushes nothing of it; enabled, the next
    /// turn renames and pushes it.
    #[test]
    fn hub_ids_switched_off_files_provisional_and_never_pushes_the_local_id() {
        let Some((hub, tokens)) = hub_lease::tests::serve_users(crate::testhub::Setup::default(), &["ann", "bo"]) else { return };
        let env = wsp_engine::util::isolated("hub-ids-off");
        let _addon = wsp_engine::addon::scoped(Arc::new(Spoke::new()));
        let ann = Side::new(&env, &hub, &tokens, "ann");
        crate::hub_switch::Switch::edit(&ann.store, |s| s.off = true).unwrap();
        let row = ann.add("while off").unwrap();
        assert!(is_provisional(&ann.store, &row));
        ann.turn().unwrap();
        assert!(!on_hub(&hub, &tokens, "bo").contains(&row), "nothing of it was pushed");
        crate::hub_switch::Switch::edit(&ann.store, |s| s.off = false).unwrap();
        ann.turn().unwrap();
        let new = ann.store.renamed_ids().get(&row).cloned().unwrap_or_else(|| row.clone());
        assert!(!is_provisional(&ann.store, &row) && !is_provisional(&ann.store, &new));
        assert!(on_hub(&hub, &tokens, "bo").contains(&new));
    }

    /// Crash-safe: ids taken and written down, then the spoke died before the
    /// rename; and again after the files moved but before the rest. The next
    /// turn finishes each with the ids it had (none taken again), and no row
    /// is left provisional.
    #[test]
    fn hub_ids_a_crash_between_taking_ids_and_renaming_is_finished_with_the_same_ids() {
        let Some((hub, tokens)) = hub_lease::tests::serve_users(crate::testhub::Setup::default(), &["ann"]) else { return };
        let env = wsp_engine::util::isolated("hub-ids-crash");
        let _addon = wsp_engine::addon::scoped(Arc::new(Spoke::new()));
        let ann = Side::bare(&env, "ann");
        ann.log_in_nowhere();
        let one = ann.add("one").unwrap();
        let two = ann.add("two").unwrap();
        ann.log_in(&hub, &tokens);
        let ws: WorkspaceId = "shared:team".parse().unwrap();
        let mut client = hub_lease::tests::dial(&hub, &tokens, "ann");
        let map = cmd_publish::allocate_map(&ann.store, &mut client, &ws, &plan_of(&ann.store, &[one.clone(), two.clone()])).unwrap();
        // Crash 1: the ids written down, no row moved.
        Book::edit(&ann.store, |b| b.pending.extend(map.clone())).unwrap();
        // Crash 2: `two`'s file moved already, the rest not done.
        let part: BTreeMap<String, String> = [(two.clone(), map[&two].clone())].into();
        ann.store.rename_tasks(&part).unwrap();
        ann.turn().unwrap();
        assert_eq!(ann.store.renamed_ids().get(&one), Some(&map[&one]), "the ids written down, not new ones");
        assert_eq!(ann.store.renamed_ids().get(&two), Some(&map[&two]));
        assert_eq!(ann.title(&map[&one]), "one");
        assert_eq!(ann.title(&map[&two]), "two");
        let book = Book::read(&ann.store);
        assert!(book.rows.is_empty() && book.pending.is_empty(), "{book:?}");
        assert!(on_hub(&hub, &tokens, "ann").contains(&map[&two]));
    }

    /// The id the hub's counter hands `who` next under `code` in `shared:team`:
    /// what a row filed there through the hub takes (the test's `add` cannot
    /// name a project: no seats are installed to resolve `-p`, so it files to
    /// the inbox).
    fn hub_hands(hub: &crate::testhub::TestHub, tokens: &BTreeMap<String, String>, who: &str, code: &str) -> String {
        let mut c = hub_lease::tests::dial(hub, tokens, who);
        c.ids_alloc(&wsp_objects::WorkspaceId("shared:team".into()), code, 1, None).unwrap().remove(0)
    }

    /// `wsp mv` filing a shared row out of the inbox into a project gives it
    /// the hub's id, as filing it there would have. bo has taken `pp-001` from
    /// the hub, and ann's local counter for `pp` has never moved, so the local
    /// counter hands ann's row `pp-001` too: two rows, one subject on the hub.
    #[test]
    fn hub_ids_moving_a_shared_inbox_row_into_a_project_takes_a_hub_id() {
        let Some((hub, tokens)) = hub_lease::tests::serve_users(crate::testhub::Setup::default(), &["ann", "bo"]) else { return };
        let env = wsp_engine::util::isolated("hub-ids-mv");
        let _addon = wsp_engine::addon::scoped(Arc::new(Spoke::new()));
        let ann = Side::new(&env, &hub, &tokens, "ann");
        let row = ann.add_inbox("ann's inbox row");
        assert!(!is_provisional(&ann.store, &row));
        let bos = hub_hands(&hub, &tokens, "bo", "pp");
        let new = ann.mv_to_pp(&row);
        assert!(new.starts_with("pp-"), "filed out of the inbox into pp: {new}");
        assert_ne!(new, bos, "the hub gave {bos} to bo; ann's row took it from the local counter");
        assert!(!is_provisional(&ann.store, &new), "connected: a hub id, not provisional");
        assert_eq!(ann.title(&new), "ann's inbox row");
        let after = hub_hands(&hub, &tokens, "bo", "pp");
        assert!(after > new, "the hub's counter moved past ann's id ({new}), now at {after}");
        ann.turn().unwrap();
        assert_eq!(hub_title(&hub, &tokens, "bo", &new).as_deref(), Some("ann's inbox row"));
        // A private inbox row still takes the local counter.
        let home = HubHome::at(&ann.home);
        assert_eq!(wsp_engine::cmd_task::add(&ann.store, &Args::synth("add", &["private"], &[])), 0);
        drop(home);
        let private = ann.store.tasks().into_iter().find(|t| t.title == "private").unwrap().id;
        let moved = ann.mv_to_pp(&private);
        assert!(moved.starts_with("pp-") && ann.store.task(&moved).unwrap().workspace.is_private());
        assert!(!is_provisional(&ann.store, &moved));
    }

    /// Offline, the move gives the row a provisional local id, as filing it
    /// would: never pushed under it, and renamed to a hub id at reconnect.
    #[test]
    fn hub_ids_moving_a_shared_inbox_row_offline_is_provisional_until_reconnect() {
        let Some((hub, tokens)) = hub_lease::tests::serve_users(crate::testhub::Setup::default(), &["ann", "bo"]) else { return };
        let env = wsp_engine::util::isolated("hub-ids-mv-off");
        let _addon = wsp_engine::addon::scoped(Arc::new(Spoke::new()));
        let ann = Side::new(&env, &hub, &tokens, "ann");
        let row = ann.add_inbox("filed in the inbox online");
        ann.log_in_nowhere();
        let new = ann.mv_to_pp(&row);
        assert!(new.starts_with("pp-"), "filed out of the inbox into pp: {new}");
        assert!(is_provisional(&ann.store, &new), "offline: a provisional local id");
        let subject = Subject::Row(RowId(new.clone()));
        let by = wsp_store_contract::Principal::Person("ann".into());
        assert!(crate::hub_sync::push_record(&ann.store, &subject, wsp_objects::WorkspaceId("shared:team".into()), by, wsp_store_contract::At(0)).is_none(), "never pushed under it");
        // bo takes pp's next id meanwhile, through the hub.
        let bos = hub_hands(&hub, &tokens, "bo", "pp");
        ann.log_in(&hub, &tokens);
        ann.turn().unwrap();
        let hub_id = ann.store.renamed_ids().get(&new).cloned().expect("renamed at reconnect");
        assert_ne!(hub_id, bos);
        assert!(!is_provisional(&ann.store, &hub_id) && !is_provisional(&ann.store, &new));
        assert_eq!(ann.title(&hub_id), "filed in the inbox online");
        assert_eq!(hub_title(&hub, &tokens, "bo", &hub_id).as_deref(), Some("filed in the inbox online"));
    }

    /// A claimed provisional row keeps its claim and its seat across the
    /// rename, and is leased at its hub id then.
    #[test]
    fn hub_ids_a_claimed_provisional_row_keeps_its_claim_and_is_leased_at_its_hub_id() {
        let Some((hub, tokens)) = hub_lease::tests::serve_users(crate::testhub::Setup::default(), &["ann", "bo"]) else { return };
        let env = wsp_engine::util::isolated("hub-ids-claimed");
        let _addon = wsp_engine::addon::scoped(Arc::new(Spoke::new()));
        let ann = Side::bare(&env, "ann");
        ann.log_in_nowhere();
        let row = ann.add("claimed offline").unwrap();
        ann.store.set_claim(&row, json!({ "workspace_id": "cpd-a", "claimed_at": wsp_engine::util::now_iso() }));
        ann.store.set_binding("cpd-a", json!({ "task_id": row, "pane_id": "cpd-a" }));
        ann.log_in(&hub, &tokens);
        ann.turn().unwrap();
        let new = ann.store.renamed_ids()[&row].clone();
        let claim = ann.store.claims().get(&new).cloned().expect("the claim follows the row");
        assert!(hub_lease::token_of(&claim).is_some(), "leased at its hub id: {claim}");
        assert_eq!(ann.store.panes_for_task(&new), vec!["cpd-a".to_string()], "the seat's binding follows");
        let mut bo = hub_lease::tests::dial(&hub, &tokens, "bo");
        assert!(hub_lease::take(&mut bo, "shared:team", &new).unwrap_err().contains("ann"));
    }
}
