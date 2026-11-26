//! The hub's spoke as a shared-work add-on: [`Spoke`] answers core's
//! [`SharedWork`] questions with the `hub_*` modules, unchanged.
//!
//! Core never names a module of the spoke; it asks [`crate::addon::get`], which
//! `main` filled with a [`Spoke`]. Each answer is the standalone one for a
//! private row or list and for a spoke with no login.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::Value;

use crate::addon::{Drives, Elsewhere, Held, Lent, Member, SharedWork};
use crate::model::Worklist;
use crate::store::{log_line, Store};
use crate::{hub_cache, hub_journal, hub_lease, hub_sync};

/// The spoke.
#[derive(Default)]
pub struct Spoke;

impl Spoke {
    pub fn new() -> Spoke {
        Spoke
    }
}

/// The home the login is under (`$WSP_HUB_HOME`, else `$HOME`).
fn home() -> Option<PathBuf> {
    wsp_client::hub_home()
}

/// Whether this machine is logged in to a hub: the login file, or the
/// environment's URL.
pub fn logged_in() -> bool {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    home().is_some_and(|h| wsp_client::hub_url_in(env(wsp_client::HUB_URL_ENV), &h).is_ok())
}

impl SharedWork for Spoke {
    /// Logged in with a login file, as the sync worker starts and a tell
    /// through the hub is sent only with one.
    fn active(&self, _store: &Store) -> bool {
        home().is_some_and(|h| h.join(wsp_client::HUBS_FILE).exists())
    }

    fn user(&self) -> Option<String> {
        let home = home()?;
        let login = wsp_client::hub_login_in(&home).ok().flatten()?;
        Some(login.user).filter(|u| !u.trim().is_empty())
    }

    fn may_file_shared(&self, _store: &Store) -> Result<(), String> {
        wsp_client::Credential::for_hub().map(|_| ()).map_err(|e| e.to_string())
    }

    fn before_claim(&self, store: &Store, row: &str) -> Result<Option<Held>, String> {
        hub_lease::before_claim(store, row)
    }

    fn stamp(&self, claim: &mut Value, held: &Held) {
        hub_lease::stamp(claim, held);
    }

    fn leased(&self, claim: &Value) -> bool {
        hub_lease::token_of(claim).is_some()
    }

    fn let_go(&self, _store: &Store, row: &str, claim: &Value) {
        hub_lease::let_go(row, claim);
    }

    fn allocated_elsewhere(&self, store: &Store, row: &str) -> Option<String> {
        crate::cmd_allocate::allocated_elsewhere(store, row)
    }

    /// [`hub_lease::drive`], with who drives the list said in `cycle.log` once
    /// each time it changes (wsp-467).
    fn drive(&self, store: &Store, w: &Worklist) -> Drives {
        if w.workspace.is_private() {
            return Drives::Here;
        }
        let drives = hub_lease::drive(store, w);
        let (user, line) = match &drives {
            Drives::Here => match self.user() {
                Some(me) => (me.clone(), format!("{}: driven here, by {me}'s wsp", w.id)),
                None => return drives,
            },
            Drives::Elsewhere(u) => (u.clone(), format!("{}: driven by {u} — this spoke does not advance it", w.id)),
            Drives::NotYet(why) => {
                log_line(store, &format!("{}: not advanced — the drive lease was not taken: {why}", w.id));
                return drives;
            }
        };
        let changed = hub_lease::Book::edit(store, |b| b.driven_by.insert(w.id.clone(), user.clone()) != Some(user));
        if changed {
            log_line(store, &line);
        }
        drives
    }

    fn driven_by(&self, store: &Store, w: &Worklist) -> Option<String> {
        hub_lease::driven_by(store, w)
    }

    fn lease_member(&self, store: &Store, row: &str) -> Member {
        hub_lease::lease_member(store, row)
    }

    fn unused(&self, store: &Store, row: &str, held: &Held) {
        hub_lease::unused(store, row, held);
    }

    fn running_elsewhere(&self, store: &Store, row: &str) -> Option<String> {
        hub_lease::running_elsewhere(store, row)
    }

    fn verdict_held(&self, store: &Store, row: &str) -> bool {
        hub_lease::verdict_held(store, row)
    }

    fn refusal(&self, store: &Store, row: &str, verb: &str) -> Option<String> {
        hub_journal::refusal(store, row, verb)
    }

    fn fetch(&self, store: &Store, row: &str) -> Option<Result<(), String>> {
        if !hub_cache::is_head(store, row) {
            return None;
        }
        Some(hub_sync::fetch(store, row))
    }

    fn show_lines(&self, store: &Store, row: &str) -> Vec<String> {
        hub_cache::show_line(store, row).into_iter().chain(hub_journal::show_line(store, row)).collect()
    }

    fn elsewhere(&self, store: &Store) -> Elsewhere {
        let drive = hub_lease::Book::read(store);
        let mut rows: BTreeSet<String> = drive.elsewhere.keys().cloned().collect();
        rows.extend(hub_cache::Cache::read(store).rows().map(|(r, _)| r.clone()));
        Elsewhere { rows, lists: drive.driven_by }
    }

    fn send(&self, _store: &Store, msg: wsp_api::MsgSend) -> Result<(String, String), String> {
        let home = home().ok_or("no HOME, so no hub login")?;
        hub_sync::dial(&home).and_then(|mut c| c.msg_send(msg).map_err(|e| e.to_string()))
    }

    fn health(
        &self,
        store: &Store,
        now: i64,
        alive: &mut dyn FnMut(&str) -> bool,
        problems: &mut Vec<String>,
        warnings: &mut Vec<String>,
        notes: &mut Vec<String>,
    ) {
        hub_sync::health(store, home().as_deref(), now, alive, problems, warnings, notes);
    }

    /// The hooks the lease follower and the message hand need from above, and
    /// the sync worker.
    fn start(&self, store: &Store, lent: Lent) -> Option<std::thread::JoinHandle<()>> {
        hub_lease::install(lent.release, lent.alive);
        hub_sync::install_spool(lent.spool);
        hub_sync::start(store)
    }
}

#[cfg(test)]
mod tests {
    //! The drive lease through the run (wsp-467, wsp-486), moved here from
    //! `cycle`'s tests: the run asks the add-on, so these install the spoke on
    //! their thread and drive a real hub.
    use super::*;
    use crate::cycle::tests::{checks, hold_all, list, scratch, set, spawned, Blind};
    use crate::cycle::step;
    use crate::model::{Status, Task};
    use std::sync::Arc;

    /// The spoke as this thread's add-on, for as long as the guard lives.
    fn spoke() -> crate::addon::Scoped {
        crate::addon::scoped(Arc::new(Spoke::new()))
    }

    // ---- the drive lease (wsp-467) ------------------------------------------------

    /// `HOME` pointed at a spoke's login for the test, put back however it ends.
    struct Home(Option<std::ffi::OsString>);

    impl Home {
        fn at(home: &std::path::Path) -> Home {
            let was = std::env::var_os("HOME");
            std::env::set_var("HOME", home);
            Home(was)
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            match self.0.take() {
                Some(h) => std::env::set_var("HOME", h),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    /// A served hub with `ed` and `jesper` in `shared:team`; this store a spoke
    /// logged in as `me`; three shared rows in `todo` and the `run` list over
    /// them, shared.
    fn two_spokes(env: &crate::util::Isolated, store: &Store, me: &str) -> (wsp_hub::serve::Running, std::collections::BTreeMap<String, String>, Home) {
        let dir = env.path("hub");
        std::fs::create_dir_all(&dir).unwrap();
        let (running, tokens) = hub_lease::tests::serve_users(&dir, &["ed", "jesper"]);
        let home = Home::at(&hub_lease::tests::login(&dir, &running, &tokens, me));
        for id in ["wsp-1", "wsp-2", "wsp-3"] {
            let mut t = Task::new(id, id);
            t.workspace = "shared:team".parse().unwrap();
            store.save_task(&t).unwrap();
        }
        list(store, &[(&["wsp-1", "wsp-2", "wsp-3"], "claude")]);
        let mut w = store.worklist("run").unwrap();
        w.workspace = "shared:team".parse().unwrap();
        store.save_worklist(&w).unwrap();
        (running, tokens, home)
    }

    /// Done-when 1 (wsp-467): Ed's spoke drives a shared list of three rows;
    /// Jesper's spoke leases one; Ed's run spawns the other two and never the
    /// leased one, neither reseats nor verifies it, and the barrier opens once
    /// Jesper's verdict and landing arrive. The drive lease holds Jesper's spoke
    /// off the list, and once it expires a spoke of Ed's takes it again.
    #[test]
    fn drive_a_shared_list_is_driven_by_eds_spoke_and_the_member_jespers_spoke_leased_runs_there() {
        let (env, store) = scratch("drive-two");
        let _addon = spoke();
        let (running, tokens, _home) = two_spokes(&env, &store, "ed");
        let mut jesper = hub_lease::tests::dial(&running, &tokens, "jesper");
        hub_lease::take(&mut jesper, "shared:team", "wsp-2").unwrap();
        let w = store.worklist("run").unwrap();

        step(&store, &w, &Blind);
        assert_eq!(spawned(), vec![("wsp-1".into(), "claude".into()), ("wsp-3".into(), "claude".into())], "the two Ed could lease");
        assert_eq!(store.find_task("wsp-2").unwrap().status(), Status::Todo, "never the leased one");
        assert_eq!(hub_lease::running_elsewhere(&store, "wsp-2").as_deref(), Some("jesper"));
        assert_eq!(hub_lease::driven_by(&store, &w).as_deref(), Some("ed"));
        assert!(hub_lease::Book::read(&store).drives.contains_key("run"), "Ed's spoke holds drive:run");
        let why = hub_lease::take(&mut jesper, "shared:team", &hub_lease::drive_row("run")).unwrap_err();
        assert!(why.contains("ed"), "Jesper's spoke cannot take the drive: {why}");
        // Ed's spoke holds the two it started: Jesper cannot take them.
        assert!(hub_lease::take(&mut jesper, "shared:team", "wsp-1").unwrap_err().contains("ed"));

        // Jesper's work arrives through the sync: `doing`, with no seat here.
        set(&store, "wsp-2", Status::Doing);
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "not reseated: it runs on Jesper's wsp");

        // Ed's two land and are verified here; Jesper's is not.
        set(&store, "wsp-1", Status::Review);
        set(&store, "wsp-3", Status::Review);
        step(&store, &w, &Blind);
        assert_eq!(spawned(), vec![("wsp-1 · verifying".into(), "claude".into()), ("wsp-3 · verifying".into(), "claude".into())]);
        hold_all(&store);
        step(&store, &w, &Blind);
        assert!(checks(&store, 1).is_empty(), "the barrier waits for Jesper's landing");

        // Jesper's landing arrives, and then the verdict his spoke's verifier wrote.
        set(&store, "wsp-2", Status::Review);
        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "no verifier on Ed's spoke for Jesper's row");
        assert!(checks(&store, 1).is_empty(), "and no barrier before its verdict");
        // A verdict that blocks is not counted; one that holds is, as pulled (wsp-486).
        let hold = serde_json::json!({ "commit": "abc1234", "verdict": "holds" });
        hub_lease::take_verdict(&store, "wsp-2", &hold);
        hub_lease::take_verdict(&store, "wsp-2", &serde_json::json!({ "commit": "abc1234", "verdict": "blocks" }));
        step(&store, &w, &Blind);
        assert!(checks(&store, 1).is_empty(), "a later blocks clears the hold: the barrier waits");
        hub_lease::take_verdict(&store, "wsp-2", &hold);
        step(&store, &w, &Blind);
        assert_eq!(checks(&store, 1).len(), 1, "the barrier opens");
        assert_eq!(spawned(), vec![("run · barrier 1".into(), "claude".into())]);

        // The drive lease expires on the hub: Ed's spoke lets it go, and the
        // next step (any spoke of Ed's) takes it again with a higher token.
        let token = hub_lease::Book::read(&store).drives["run"].token;
        running.hub().clock().advance(std::time::Duration::from_secs(8 * 86_400));
        let expired = running.hub().expire_leases().unwrap();
        let drive = expired.iter().find(|e| e.row.0 == "drive:run").expect("drive:run expired");
        let payload = serde_json::json!({ "row": drive.row.0, "token": drive.token.0 });
        assert!(hub_lease::follow_expired(&store, &payload));
        assert!(!hub_lease::Book::read(&store).drives.contains_key("run"));
        step(&store, &w, &Blind);
        assert!(hub_lease::Book::read(&store).drives["run"].token > token, "taken again");
        running.stop();
    }

    /// wsp-486: Jesper's spoke, which does not drive Ed's list, opens the
    /// verifier of the member it holds, and of no other.
    #[test]
    fn drive_the_holder_spoke_opens_a_verifier_for_a_member_of_a_list_it_does_not_drive() {
        let (env, store) = scratch("drive-held");
        let _addon = spoke();
        let (running, _tokens, _home) = two_spokes(&env, &store, "jesper");
        let mut w = store.worklist("run").unwrap();
        w.for_user = Some("ed".into());
        store.save_worklist(&w).unwrap();
        for id in ["wsp-2", "wsp-3"] {
            set(&store, id, Status::Review);
        }
        store.set_claim("wsp-2", serde_json::json!({ "hub_token": 5, "expires": crate::util::iso_at(crate::util::epoch_secs() + 3600) }));
        let before = std::fs::read_to_string(store.worklist_path("run")).unwrap();
        step(&store, &w, &Blind);
        assert_eq!(spawned(), vec![("wsp-2 · verifying".into(), "claude".into())], "only the member held here");
        assert_eq!(std::fs::read_to_string(store.worklist_path("run")).unwrap(), before, "the list untouched");
        running.stop();
    }

    /// Done-when 2 (wsp-467): a spoke without the drive lease never advances
    /// the list — no spawn, no list change — whether another user's spoke holds
    /// the lease or the list is allocated to another user.
    #[test]
    fn drive_a_spoke_without_the_drive_lease_never_advances_the_list() {
        let (env, store) = scratch("drive-none");
        let _addon = spoke();
        let (running, tokens, _home) = two_spokes(&env, &store, "jesper");
        let mut ed = hub_lease::tests::dial(&running, &tokens, "ed");
        hub_lease::take(&mut ed, "shared:team", &hub_lease::drive_row("run")).unwrap();
        let w = store.worklist("run").unwrap();
        let file = || std::fs::read_to_string(store.worklist_path("run")).unwrap();
        let before = file();

        step(&store, &w, &Blind);
        assert!(spawned().is_empty(), "nothing spawned");
        assert_eq!(file(), before, "nothing written to the list");
        assert!(["wsp-1", "wsp-2", "wsp-3"].iter().all(|r| store.find_task(r).unwrap().status() == Status::Todo));
        assert_eq!(hub_lease::driven_by(&store, &w).as_deref(), Some("ed"));
        assert!(hub_lease::Book::read(&store).drives.is_empty());

        // Allocated to ed: Jesper's spoke does not even ask the hub.
        let mut w = store.worklist("run").unwrap();
        w.for_user = Some("ed".into());
        store.save_worklist(&w).unwrap();
        let before = file();
        assert_eq!(hub_lease::drive(&store, &w), hub_lease::Drives::Elsewhere("ed".into()));
        step(&store, &w, &Blind);
        assert!(spawned().is_empty());
        assert_eq!(file(), before);
        running.stop();
    }

    /// The spoke's half of `verification`'s side-chain test (wsp-469): a row
    /// its journal side-chained while offline, now another user's, is refused
    /// for a verdict and a landing in words that name the holder; with the side
    /// chain gone it is not.
    #[test]
    fn spoke_refuses_a_verdict_and_a_landing_on_a_row_its_journal_side_chained() {
        let (_env, store) = crate::testkit::isolated_store("spoke-side-chained");
        let mut sides = hub_journal::Sides::default();
        sides.rows.insert(
            "m-1".into(),
            hub_journal::Side {
                workspace: "ws-1".into(),
                row: "m-1".into(),
                tip: "sha256:0123456789abcdef0123456789abcdef".into(),
                holder: "bo".into(),
                old: "ann".into(),
                mine: true,
                token: Some(1),
                at: "2026-11-23T10:00:00Z".into(),
            },
        );
        sides.write(&store).unwrap();
        let spoke = Spoke::new();
        let why = spoke.refusal(&store, "m-1", "verified").expect("the row is refused");
        assert!(why.contains("`wsp verified` is refused here") && why.contains("bo"), "{why}");
        assert!(spoke.refusal(&store, "m-1", "land").is_some_and(|w| w.contains("`wsp land`")));
        assert_eq!(spoke.refusal(&store, "m-2", "land"), None, "another row is not");
        std::fs::remove_file(store.state.join("hub").join("side.json")).unwrap();
        assert_eq!(spoke.refusal(&store, "m-1", "land"), None);
    }
}
