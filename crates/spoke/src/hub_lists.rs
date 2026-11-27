//! Shared lists whose slug is already taken here: kept under a local name of
//! their own, so two lists of one slug stay two.
//!
//! **Why.** A list's id is its slug (`wsp worklist new <slug>`), its file is
//! `worklists/<slug>.md`, and the synced heads (`sync-heads.json`) are keyed
//! by subject, `list:<slug>`, whatever the workspace. The hub keeps each
//! workspace's subjects apart, so `run` in `shared:one` and `run` in
//! `shared:two` are two lists there; a spoke that is a member of both had one
//! file for them. The first to arrive was written and the second refused as a
//! divergence on every turn, so it was never seen here; a shared list arriving
//! on a slug a private list holds was refused the same way.
//!
//! **What is done.** Such a list is kept here under `<slug>-<workspace name>`
//! (`run-two`; a number after it if that is taken too), and
//! `$WSP_STATE/hub/lists.json` ([`Names`]) says which workspace and hub id the
//! local name stands for. Records cross between the two names at the sync's
//! edge only: a record from the hub is renamed to the local name before the
//! door writes it ([`to_local`]), one made here is renamed to the hub's id
//! before it is journaled and pushed ([`to_hub`]); the synced heads keep the
//! hub's object ids under the local subject, and the list's drive lease is the
//! hub id's (`drive:<hub id>`). The list that was here first keeps its slug.
//!
//! **One slug made twice in one workspace** (two people making `run` there
//! before either syncs): the hub holds the first pushed; the second spoke's
//! push is stale or diverged, and its list was never sent and shares no
//! object with the hub's. That spoke keeps both: its own goes to the hub under
//! a new id ([`second_of_slug`], `run-<user>`), the hub's comes in under a
//! name of its own ([`arriving`]), and the log says so once. A divergence of
//! a list that was sent before is reconciled as ever.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use wsp_engine::store::Store;
use wsp_objects::Object;
use wsp_store_contract::{Entity, ListId, Subject};

/// `$WSP_STATE/hub/lists.json`: local name -> (workspace, hub id), for the
/// shared lists kept under a name that is not their hub id.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Names(pub BTreeMap<String, (String, String)>);

fn path(store: &Store) -> std::path::PathBuf {
    store.state.join("hub").join("lists.json")
}

impl Names {
    pub fn read(store: &Store) -> Names {
        let v: Value = std::fs::read_to_string(path(store)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
        Names(
            v.as_object()
                .into_iter()
                .flatten()
                .filter_map(|(local, e)| Some((local.clone(), (e["workspace"].as_str()?.to_string(), e["id"].as_str()?.to_string()))))
                .collect(),
        )
    }

    fn write(&self, store: &Store) -> std::io::Result<()> {
        let path = path(store);
        let dir = path.parent().expect("under hub/");
        std::fs::create_dir_all(dir)?;
        let v: serde_json::Map<String, Value> =
            self.0.iter().map(|(l, (w, id))| (l.clone(), json!({ "workspace": w, "id": id }))).collect();
        let tmp = dir.join(format!(".lists.json.{}.tmp", std::process::id()));
        std::fs::write(&tmp, Value::Object(v).to_string())?;
        std::fs::rename(&tmp, &path)
    }

    /// The local name of the hub's list `id` in `ws`, where it has one.
    fn local(&self, ws: &str, id: &str) -> Option<&String> {
        self.0.iter().find(|(_, (w, i))| w == ws && i == id).map(|(l, _)| l)
    }
}

/// The hub's id for the local list `local`: itself, unless it is kept under
/// a name of its own.
pub fn hub_id(store: &Store, local: &str) -> String {
    Names::read(store).0.get(local).map(|(_, id)| id.clone()).unwrap_or_else(|| local.to_string())
}

/// The local subject for the hub's `subject` in `ws` (a row's is itself).
pub fn local_subject(store: &Store, ws: &str, subject: &Subject) -> Subject {
    match subject {
        Subject::List(id) => Subject::List(ListId(Names::read(store).local(ws, &id.0).cloned().unwrap_or_else(|| id.0.clone()))),
        other => other.clone(),
    }
}

/// The hub's subject for the local `subject` (a row's is itself).
pub fn hub_subject(store: &Store, subject: &Subject) -> Subject {
    match subject {
        Subject::List(id) => Subject::List(ListId(hub_id(store, &id.0))),
        other => other.clone(),
    }
}

/// The local name the hub's list `id` in `ws` is kept under, given one now
/// where `id` is taken here by something else: a list of another workspace
/// (private or shared), or a project (one key space with lists).
pub fn arriving(store: &Store, ws: &str, id: &str) -> String {
    let names = Names::read(store);
    if let Some(l) = names.local(ws, id) {
        return l.clone();
    }
    // Taken, too, by a list of this workspace that goes to the hub under
    // another id ([`second_of_slug`]): the hub's list of the slug is another.
    let taken_by_other = store.worklist(id).is_some_and(|w| w.workspace.to_string() != ws)
        || store.project(id).is_some()
        || names.0.get(id).is_some_and(|(w, i)| w == ws && i != id);
    if !taken_by_other {
        return id.to_string();
    }
    let base = format!("{id}-{}", wsp_engine::util::slugify(ws.strip_prefix("shared:").unwrap_or(ws)));
    let free = |cand: &str| !names.0.contains_key(cand) && store.scope_taken(cand, None).is_none() && store.worklist(cand).is_none();
    let local = std::iter::once(base.clone()).chain((2..).map(|n| format!("{base}-{n}"))).find(|c| free(c)).expect("an unbounded search");
    let _ = store.locked(|| {
        let mut names = Names::read(store);
        names.0.insert(local.clone(), (ws.to_string(), id.to_string()));
        names.write(store)
    });
    local
}

/// The local list `local` of `ws`, never sent to the hub, met another list
/// under its slug there (two people made the slug in one workspace before
/// either synced): from now on it goes to the hub under a new id,
/// `<slug>-<me>` (a number after it where that is taken here), recorded in
/// [`Names`]; the hub's list of the slug then arrives here under a name of its
/// own ([`arriving`]). The new id, and what the log is told once.
pub fn second_of_slug(store: &Store, ws: &str, local: &str, me: &str) -> (String, String) {
    let base = format!("{local}-{}", wsp_engine::util::slugify(me));
    let new = store
        .locked(|| {
            let mut names = Names::read(store);
            let taken = |c: &str| names.0.values().any(|(w, i)| w == ws && i == c) || store.worklist(c).is_some() || store.project(c).is_some();
            let new = std::iter::once(base.clone()).chain((2..).map(|n| format!("{base}-{n}"))).find(|c| !taken(c)).expect("an unbounded search");
            names.0.insert(local.to_string(), (ws.to_string(), new.clone()));
            names.write(store).map(|_| new)
        })
        .unwrap_or(base);
    let theirs = format!("{local}-{}", wsp_engine::util::slugify(ws.strip_prefix("shared:").unwrap_or(ws)));
    let said = format!("hub-sync: {local} in {ws} was made on the hub by someone else too before this spoke sent yours; theirs is kept here as {theirs}, and yours goes to the hub as {new}");
    (new, said)
}

/// `body` (a list file) with its frontmatter's `id:` line saying `id`.
fn with_id(body: &str, id: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut fences = 0;
    for line in body.split_inclusive('\n') {
        if line.trim_end() == "---" {
            fences += 1;
        }
        if fences == 1 && line.starts_with("id:") {
            out.push_str(&format!("id: {id}\n"));
        } else {
            out.push_str(line);
        }
    }
    out
}

/// A list record renamed to `id`: its subject, its entity's id and its file's
/// `id:` line. Any other object as it is.
fn renamed(obj: &Object, id: &str) -> Object {
    let mut obj = obj.clone();
    if let Object::Record { subject: subject @ Subject::List(_), entity: Entity::List(list), .. } = &mut obj {
        *subject = Subject::List(ListId(id.to_string()));
        list.id = ListId(id.to_string());
        if let Some(body) = list.fields.get_mut(wsp_engine::mirror::BODY) {
            *body = with_id(body, id);
        }
    }
    obj
}

/// A record of the hub's, as the door is to write it here: a list kept under
/// a local name renamed to it. `local` is that name.
pub fn to_local(obj: &Object, local: &str) -> Object {
    renamed(obj, local)
}

/// A record made here, as the hub is to have it: a list kept under a local
/// name renamed to its hub id.
pub fn to_hub(store: &Store, obj: Object) -> Object {
    match obj.subject() {
        Subject::List(id) => {
            let hub = hub_id(store, &id.0);
            match hub == id.0 {
                true => obj,
                false => renamed(&obj, &hub),
            }
        }
        _ => obj,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_a_slug_another_workspace_holds_is_kept_under_a_name_of_its_own() {
        let (_env, store) = crate::testkit::isolated_store("hub-lists-names");
        let mut w = wsp_engine::model::Worklist::new("run", "one's run");
        w.workspace = "shared:one".parse().unwrap();
        store.save_worklist(&w).unwrap();
        assert_eq!(arriving(&store, "shared:one", "run"), "run", "its own workspace's: the same file");
        assert_eq!(arriving(&store, "shared:two", "run"), "run-two");
        assert_eq!(arriving(&store, "shared:two", "run"), "run-two", "once given, kept");
        assert_eq!(hub_id(&store, "run-two"), "run");
        assert_eq!(hub_id(&store, "run"), "run");
        assert_eq!(local_subject(&store, "shared:two", &Subject::List(ListId("run".into()))), Subject::List(ListId("run-two".into())));
        assert_eq!(local_subject(&store, "shared:one", &Subject::List(ListId("run".into()))), Subject::List(ListId("run".into())));
        // A third workspace whose name gives a taken name: a number after it.
        let mut taken = wsp_engine::model::Worklist::new("run-three", "a private list");
        taken.workspace = wsp_engine::model::WorkspaceId::Private;
        store.save_worklist(&taken).unwrap();
        assert_eq!(arriving(&store, "shared:three", "run"), "run-three-2");
        assert_eq!(with_id("---\nid: run\ntitle: t\n---\n\nid: body\n", "run-two"), "---\nid: run-two\ntitle: t\n---\n\nid: body\n");
    }

    // ---- against a real hub ----

    use std::path::{Path, PathBuf};

    use crate::hub_sync::Worker;
    use crate::testhub::{Setup, TestHub};
    use wsp_engine::model::Worklist;

    /// One user's spoke: its store and its login home.
    fn spoke(dir: &Path, who: &str, hub: &TestHub, tokens: &BTreeMap<String, String>) -> (Store, PathBuf) {
        let store = Store::at(dir.join(who).join("wsp"), dir.join(who).join("state"));
        store.ensure_dirs().unwrap();
        let home = dir.join(who).join("home");
        hub.login(&home, who, &tokens[who]);
        (store, home)
    }

    fn list(store: &Store, id: &str, title: &str, ws: &str) {
        let mut w = Worklist::new(id, title);
        w.workspace = ws.parse().unwrap();
        store.save_worklist(&w).unwrap();
    }

    fn turns(store: &Store, home: &Path, n: usize) {
        for _ in 0..n {
            Worker::new(home.to_path_buf()).turn(store).expect("a turn");
        }
    }

    /// The lists here, as (workspace, title), whatever their local names.
    fn kept(store: &Store) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = store.worklists().into_iter().map(|w| (w.workspace.to_string(), w.title)).collect();
        v.sort();
        v
    }

    /// The title of the hub's head of list `id` in `ws`, read by `who`.
    fn hub_title(hub: &TestHub, tokens: &BTreeMap<String, String>, who: &str, ws: &str, id: &str) -> Option<String> {
        let mut c = hub.client(&tokens[who]);
        let refs = c.sync_heads(&wsp_objects::WorkspaceId(ws.into())).unwrap();
        let head = *refs.heads.get(&Subject::List(ListId(id.into())))?;
        let blob = c.sync_get(vec![head]).unwrap().into_iter().next()?;
        match wsp_objects::decode(&blob.bytes).ok()? {
            Object::Record { entity: Entity::List(l), .. } => wsp_engine::mirror::list_of(&l).map(|w| w.title),
            _ => None,
        }
    }

    /// bo's `run` in `shared:one` and cy's `run` in `shared:two` are two lists
    /// on the hub. ann, a member of both, had one: the first written, the
    /// second refused as a divergence on every turn. Now both are here, each
    /// in its workspace, and an edit to either reaches its own workspace only.
    #[test]
    fn lists_two_workspaces_lists_of_one_slug_stay_two_on_a_spoke_of_both() {
        let Some(hub) = TestHub::start(Setup::default()) else { return };
        let tokens = hub.seed(&["one", "two"], &[("ann", &["one", "two"]), ("bo", &["one"]), ("cy", &["two"])]);
        let env = wsp_engine::util::isolated("hub-lists-two");
        let dir = env.path("spokes");
        let (bo, bo_home) = spoke(&dir, "bo", &hub, &tokens);
        let (cy, cy_home) = spoke(&dir, "cy", &hub, &tokens);
        let (ann, ann_home) = spoke(&dir, "ann", &hub, &tokens);
        list(&bo, "run", "bo's run", "shared:one");
        list(&cy, "run", "cy's run", "shared:two");
        turns(&bo, &bo_home, 1);
        turns(&cy, &cy_home, 1);
        turns(&ann, &ann_home, 2);
        assert_eq!(
            kept(&ann),
            vec![("shared:one".to_string(), "bo's run".to_string()), ("shared:two".to_string(), "cy's run".to_string())],
            "both lists, each in its workspace"
        );
        let log = std::fs::read_to_string(ann.state.join("cycle.log")).unwrap_or_default();
        assert!(!log.contains("was not taken"), "no list refused: {log}");

        // ann edits each; each edit reaches its own workspace and no other.
        for (ws, title) in [("shared:one", "one's run, edited by ann"), ("shared:two", "two's run, edited by ann")] {
            let mut w = ann.worklists().into_iter().find(|w| w.workspace.to_string() == ws).unwrap();
            w.title = title.into();
            ann.save_worklist(&w).unwrap();
        }
        turns(&ann, &ann_home, 2);
        assert_eq!(hub_title(&hub, &tokens, "ann", "shared:one", "run").as_deref(), Some("one's run, edited by ann"));
        assert_eq!(hub_title(&hub, &tokens, "ann", "shared:two", "run").as_deref(), Some("two's run, edited by ann"));
        assert!(hub_title(&hub, &tokens, "ann", "shared:two", "run-two").is_none(), "the local name never reaches the hub");
        turns(&bo, &bo_home, 1);
        turns(&cy, &cy_home, 1);
        assert_eq!(bo.worklist("run").map(|w| w.title).as_deref(), Some("one's run, edited by ann"));
        assert_eq!(cy.worklist("run").map(|w| w.title).as_deref(), Some("two's run, edited by ann"));

        // The aliased list's drive lease is the hub's `drive:run` in two: cy
        // cannot take it while ann's spoke holds it.
        let _addon = wsp_engine::addon::scoped(std::sync::Arc::new(crate::spoke::Spoke::new()));
        let was = std::env::var_os(crate::hub_client::HUB_HOME_ENV);
        std::env::set_var(crate::hub_client::HUB_HOME_ENV, &ann_home);
        let mut w = ann.worklists().into_iter().find(|w| w.workspace.to_string() == "shared:two").unwrap();
        w.for_user = Some("ann".into());
        ann.save_worklist(&w).unwrap();
        let drives = crate::hub_lease::drive(&ann, &w);
        match was {
            Some(h) => std::env::set_var(crate::hub_client::HUB_HOME_ENV, h),
            None => std::env::remove_var(crate::hub_client::HUB_HOME_ENV),
        }
        assert_eq!(drives, crate::hub_lease::Drives::Here, "ann's spoke drives the list allocated to her");
        let mut c = hub.client(&tokens["cy"]);
        let why = crate::hub_lease::take(&mut c, "shared:two", &crate::hub_lease::drive_row("run")).unwrap_err();
        assert!(why.contains("ann"), "cy's take of drive:run is refused, naming ann: {why}");
    }

    /// A shared list arriving on a slug ann's private list holds: the private
    /// list keeps its slug and is never sent; the shared one is here beside it.
    #[test]
    fn lists_a_shared_list_on_a_private_lists_slug_stays_beside_it() {
        let Some(hub) = TestHub::start(Setup::default()) else { return };
        let tokens = hub.seed(&["one"], &[("ann", &["one"]), ("bo", &["one"])]);
        let env = wsp_engine::util::isolated("hub-lists-private");
        let dir = env.path("spokes");
        let (bo, bo_home) = spoke(&dir, "bo", &hub, &tokens);
        let (ann, ann_home) = spoke(&dir, "ann", &hub, &tokens);
        list(&ann, "mine", "ann's private mine", "private");
        list(&bo, "mine", "bo's shared mine", "shared:one");
        turns(&bo, &bo_home, 1);
        turns(&ann, &ann_home, 2);
        assert_eq!(
            kept(&ann),
            vec![("private".to_string(), "ann's private mine".to_string()), ("shared:one".to_string(), "bo's shared mine".to_string())]
        );
        assert!(ann.worklist("mine").is_some_and(|w| w.workspace.is_private()), "the private list keeps its slug");
        assert_eq!(hub_title(&hub, &tokens, "bo", "shared:one", "mine").as_deref(), Some("bo's shared mine"));
    }

    /// Two people make a shared list of one slug in one workspace before
    /// either syncs. The hub holds the first one pushed under the slug; the
    /// second's chain shares nothing with it, so the second spoke takes the
    /// hub's list too and sends its own under a new hub id, saying so once.
    /// Both spokes end with both lists. Either may sync first.
    #[test]
    fn lists_one_slug_made_twice_in_one_workspace_before_either_syncs_ends_as_two_lists_on_both() {
        for first in ["ann", "bo"] {
            let Some(hub) = TestHub::start(Setup::default()) else { return };
            let tokens = hub.seed(&["team"], &[("ann", &["team"]), ("bo", &["team"])]);
            let env = wsp_engine::util::isolated(&format!("hub-lists-same-{first}"));
            let dir = env.path("spokes");
            let (ann, ann_home) = spoke(&dir, "ann", &hub, &tokens);
            let (bo, bo_home) = spoke(&dir, "bo", &hub, &tokens);
            list(&ann, "run", "ann's run", "shared:team");
            list(&bo, "run", "bo's run", "shared:team");
            let (one, one_home, two, two_home, second) = match first {
                "ann" => (&ann, &ann_home, &bo, &bo_home, "bo"),
                _ => (&bo, &bo_home, &ann, &ann_home, "ann"),
            };
            turns(one, one_home, 1);
            turns(two, two_home, 3);
            turns(one, one_home, 2);
            let both = vec![("shared:team".to_string(), "ann's run".to_string()), ("shared:team".to_string(), "bo's run".to_string())];
            assert_eq!(kept(&ann), both, "{first} first: ann has both");
            assert_eq!(kept(&bo), both, "{first} first: bo has both");
            assert_eq!(hub_title(&hub, &tokens, "ann", "shared:team", "run").as_deref(), Some(format!("{first}'s run").as_str()), "the first keeps the slug");
            let new_id = format!("run-{second}");
            assert_eq!(hub_title(&hub, &tokens, "ann", "shared:team", &new_id).as_deref(), Some(format!("{second}'s run").as_str()), "the second under a new hub id");
            let log = std::fs::read_to_string(two.state.join("cycle.log")).unwrap_or_default();
            let said = format!("hub-sync: run in shared:team was made on the hub by someone else too before this spoke sent yours; theirs is kept here as run-team, and yours goes to the hub as {new_id}");
            assert_eq!(log.matches(&said).count(), 1, "said once: {log}");
            assert!(!log.contains("was not reconciled"), "{log}");
        }
    }

    /// A list both spokes had from the hub and both edited is an ordinary
    /// divergence: reconciled as before, never sent under a new id.
    #[test]
    fn lists_an_ordinary_divergence_of_one_list_is_not_taken_for_another_list() {
        let Some(hub) = TestHub::start(Setup::default()) else { return };
        let tokens = hub.seed(&["team"], &[("ann", &["team"]), ("bo", &["team"])]);
        let env = wsp_engine::util::isolated("hub-lists-diverge");
        let dir = env.path("spokes");
        let (ann, ann_home) = spoke(&dir, "ann", &hub, &tokens);
        let (bo, bo_home) = spoke(&dir, "bo", &hub, &tokens);
        list(&ann, "run", "the run", "shared:team");
        turns(&ann, &ann_home, 1);
        turns(&bo, &bo_home, 2);
        assert!(bo.worklist("run").is_some());
        for (store, title) in [(&ann, "ann's edit"), (&bo, "bo's edit")] {
            let mut w = store.worklist("run").unwrap();
            w.title = title.into();
            store.save_worklist(&w).unwrap();
        }
        turns(&ann, &ann_home, 1);
        turns(&bo, &bo_home, 3);
        assert!(Names::read(&bo).0.is_empty(), "bo keeps one list named run: {:?}", Names::read(&bo));
        assert_eq!(bo.worklists().len(), 1);
        assert!(hub_title(&hub, &tokens, "bo", "shared:team", "run-bo").is_none(), "nothing sent under a new id");
        let log = std::fs::read_to_string(bo.state.join("cycle.log")).unwrap_or_default();
        assert!(!log.contains("was made on the hub by someone else too"), "{log}");
    }
}
