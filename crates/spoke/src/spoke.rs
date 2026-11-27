//! The hub's spoke as a shared-work add-on: [`Spoke`] answers core's
//! [`SharedWork`] questions with the `hub_*` modules, unchanged.
//!
//! Core never names a module of the spoke; it asks [`wsp_engine::addon::get`], which
//! `main` filled with a [`Spoke`]. Each answer is the standalone one for a
//! private row or list, for a spoke with no login, and, for new shared work,
//! while the hub connection is switched off ([`crate::hub_switch`]): then no
//! lease is taken, no shared list is advanced from here, a row filed shared
//! takes a provisional local id ([`crate::hub_ids`]) and nothing is dialled, while what is kept here (the cache, the
//! journal, who holds what) stays readable.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::Value;

use wsp_engine::addon::{Drives, Elsewhere, Held, Lent, Member, SharedWork, WhoAmI};
use wsp_engine::model::Worklist;
use wsp_engine::store::{log_line, Store};
use crate::{hub_cache, hub_journal, hub_lease, hub_switch, hub_sync};

/// The spoke. Holds only the lines it has said once, so a switched-off list is
/// not said every tick.
#[derive(Default)]
pub struct Spoke {
    said: Mutex<BTreeSet<String>>,
}

impl Spoke {
    pub fn new() -> Spoke {
        Spoke::default()
    }

    fn say_once(&self, store: &Store, line: String) {
        let fresh = self.said.lock().map(|mut s| s.insert(line.clone())).unwrap_or(true);
        if fresh {
            log_line(store, &line);
        }
    }

    fn forget_said(&self, prefix: &str) {
        if let Ok(mut s) = self.said.lock() {
            s.retain(|l| !l.starts_with(prefix));
        }
    }
}

/// The home the login is under (`$WSP_HUB_HOME`, else `$HOME`).
fn home() -> Option<PathBuf> {
    crate::hub_client::hub_home()
}

/// How long a `whoami` dial waits for the hub: a verb writing one line does not wait on it for long.
const WHOAMI_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

/// Whether this machine is logged in to a hub: the login file, or the
/// environment's URL.
pub fn logged_in() -> bool {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    home().is_some_and(|h| crate::hub_client::hub_url_in(env(crate::hub_client::HUB_URL_ENV), &h).is_ok())
}

/// Logged in and switched off: the one state in which the spoke refuses new
/// shared work it would otherwise take.
fn switched_off(store: &Store) -> bool {
    hub_switch::off(store) && logged_in()
}

/// What a refusal says while the connection is off.
pub fn off_said(what: &str) -> String {
    format!("{what}: the hub connection is disabled here, so no shared work is taken (`wsp hub enable` turns it back on)")
}

fn is_shared_row(store: &Store, row: &str) -> bool {
    store.task(row).is_some_and(|t| !t.workspace.is_private())
}

/// `whoami` at the hub `url` with `credential`: the principal it names, a
/// refused login, or no answer, each in words.
pub fn whoami_at(url: &str, credential: crate::hub_client::Credential, ca: Option<&std::path::Path>) -> WhoAmI {
    let mut client = match crate::hub_client::Client::dial_hub_ca(url, credential, ca, WHOAMI_WAIT) {
        Ok(c) => c,
        Err(e) => {
            let why = hub_sync::said_of(&e);
            return match hub_sync::is_refused_login(&why) {
                true => WhoAmI::Refused(why),
                false => WhoAmI::Unanswered(why),
            };
        }
    };
    match client.whoami() {
        Ok((by, server)) => WhoAmI::Known(by, server),
        Err(e) => WhoAmI::Unanswered(e.to_string()),
    }
}

impl SharedWork for Spoke {
    /// Logged in with a login file (as the sync worker starts and a tell
    /// through the hub is sent only with one), and switched on.
    fn active(&self, store: &Store) -> bool {
        home().is_some_and(|h| h.join(crate::hub_client::HUBS_FILE).exists()) && !hub_switch::off(store)
    }

    fn user(&self) -> Option<String> {
        let home = home()?;
        let login = crate::hub_client::hub_login_in(&home).ok().flatten()?;
        Some(login.user).filter(|u| !u.trim().is_empty())
    }

    fn whoami(&self) -> WhoAmI {
        // A test never dials: a developer's own login must not change what it
        // reads. A test of any crate (wsp-544's `under_test`), not this one's alone.
        if wsp_engine::base::under_test() {
            return WhoAmI::NotLoggedIn;
        }
        let Ok(credential) = crate::hub_client::Credential::for_hub() else { return WhoAmI::NotLoggedIn };
        let Ok(url) = crate::hub_client::hub_url() else { return WhoAmI::NotLoggedIn };
        let ca = match crate::hub_client::hub_ca() {
            Ok(ca) => ca,
            Err(e) => return WhoAmI::Unanswered(e.to_string()),
        };
        whoami_at(&url, credential, ca.as_deref())
    }

    /// A login is what filing shared needs. Switched off, a row filed shared
    /// is provisional (a local id, nothing pushed) until the next connection.
    fn may_file_shared(&self, _store: &Store) -> Result<(), String> {
        crate::hub_client::Credential::for_hub().map(|_| ()).map_err(|e| e.to_string())
    }

    /// A member of `workspace`, as far as this spoke knows ([`crate::hub_members`]):
    /// what the hub last said, kept by the worker, or the hub asked now where
    /// that does not say yes and the connection is on. Nothing known (never
    /// connected here) and no hub to ask: yes, and the hub refuses at the sync.
    fn may_file_into(&self, store: &Store, workspace: &wsp_engine::model::WorkspaceId) -> Result<(), String> {
        let Some(user) = self.user() else { return Ok(()) };
        let ask = || {
            if hub_switch::off(store) {
                return None;
            }
            crate::cmd_publish::dial(&home()?).ok()
        };
        crate::hub_members::check(store, workspace, &user, ask)
    }

    fn new_row_id(&self, store: &Store, project: Option<&str>, workspace: &wsp_engine::model::WorkspaceId, parent: Option<&str>) -> Result<Option<String>, String> {
        crate::hub_ids::new_row_id(store, project, workspace, parent)
    }

    fn marks(&self, store: &Store) -> std::collections::BTreeMap<String, String> {
        crate::hub_ids::Book::read(store).rows.into_keys().map(|r| (r, "not yet published".to_string())).collect()
    }

    fn before_claim(&self, store: &Store, row: &str) -> Result<Option<Held>, String> {
        // A provisional row is on a local id the hub does not know: no lease
        // until it has its hub id (the rename leases a claimed row then).
        if crate::hub_ids::is_provisional(store, row) {
            return Ok(None);
        }
        if is_shared_row(store, row) && switched_off(store) {
            return Err(off_said(&format!("{row} is shared")));
        }
        hub_lease::before_claim(store, row)
    }

    fn stamp(&self, claim: &mut Value, held: &Held) {
        hub_lease::stamp(claim, held);
    }

    fn leased(&self, claim: &Value) -> bool {
        hub_lease::token_of(claim).is_some()
    }

    fn let_go(&self, store: &Store, row: &str, claim: &Value) {
        // An agent the disable asked to end at the end of its turn is gone:
        // its row's lease goes back now (whether or not the connection is
        // still off), and the row is said to be parked with its agent ended.
        if hub_switch::agent_gone(store, row, claim) {
            return;
        }
        // Switched off, nothing is dialled: `wsp hub disable` gave back what it
        // could, and a lease it could not runs out on the hub.
        if hub_switch::off(store) {
            return;
        }
        hub_lease::let_go(store, row, claim);
    }

    /// A verb moved `row`'s status: a lease no claim holds (`wsp start`'s) goes
    /// back once the work is over (`done`, `park`, put back). Switched off,
    /// nothing is dialled: disable gave back what it could.
    fn row_moved(&self, store: &Store, row: &str) {
        if hub_switch::off(store) {
            return;
        }
        hub_lease::release_if_finished(store, row);
    }

    fn allocated_elsewhere(&self, store: &Store, row: &str) -> Option<String> {
        crate::cmd_allocate::allocated_elsewhere(store, row)
    }

    /// [`hub_lease::drive`], with who drives the list said in `cycle.log` once
    /// each time it changes (wsp-467). Switched off, a shared list is not
    /// advanced from here, said once.
    fn drive(&self, store: &Store, w: &Worklist) -> Drives {
        if w.workspace.is_private() {
            return Drives::Here;
        }
        let off = format!("{}: not advanced — ", w.id);
        if switched_off(store) {
            let why = "the hub connection is disabled here (`wsp hub enable`)".to_string();
            self.say_once(store, format!("{off}{why}"));
            return Drives::NotYet(why);
        }
        self.forget_said(&off);
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
        if crate::hub_ids::is_provisional(store, row) {
            return Member::Free;
        }
        if is_shared_row(store, row) && switched_off(store) {
            return Member::NotYet(off_said(&format!("{row} is shared")));
        }
        hub_lease::lease_member(store, row)
    }

    fn unused(&self, store: &Store, row: &str, held: &Held) {
        if hub_switch::off(store) {
            return;
        }
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
        if switched_off(store) {
            return Some(Err(off_said(&format!("{row} is kept here as the hub's head only"))));
        }
        Some(hub_sync::fetch(store, row))
    }

    fn show_lines(&self, store: &Store, row: &str) -> Vec<String> {
        let head = hub_cache::show_line(store, row);
        let stale = head.is_some() && switched_off(store);
        let mut out: Vec<String> = head.into_iter().chain(hub_journal::show_line(store, row)).collect();
        if stale {
            out.push("not live: the hub connection is disabled here, so this head is as it was last synced".into());
        }
        if crate::hub_ids::is_provisional(store, row) {
            out.push("not yet published: a local id until the hub gives this row its own, at the next connection; nothing of it is sent before then".into());
        }
        out
    }

    fn elsewhere(&self, store: &Store) -> Elsewhere {
        let drive = hub_lease::Book::read(store);
        let mut rows: BTreeSet<String> = drive.elsewhere.keys().cloned().collect();
        rows.extend(hub_cache::Cache::read(store).rows().map(|(r, _)| r.clone()));
        Elsewhere { rows, lists: drive.driven_by }
    }

    fn isolate(&self, dir: &std::path::Path) -> Vec<(String, String)> {
        vec![(crate::hub_client::HUB_HOME_ENV.to_string(), dir.join("hub").display().to_string())]
    }

    fn send(&self, store: &Store, msg: wsp_engine::addon::Outgoing) -> Result<(String, String), String> {
        if hub_switch::off(store) {
            return Err(off_said("a message for another user's wsp is not sent"));
        }
        let home = home().ok_or("no HOME, so no hub login")?;
        hub_sync::dial(&home).and_then(|mut c| c.msg_send(hub_sync::msg_send(msg)).map_err(|e| e.to_string()))
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
        if switched_off(store) {
            warnings.push(hub_switch::health_line(store));
        }
    }

    /// The hooks the lease follower and the message hand need from above, and
    /// the sync worker (which idles while the connection is switched off).
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

    /// The spoke's half of the drive-lease tests (wsp-467, wsp-486): the same
    /// scenarios against a real hub, asking the real [`Spoke`] directly the
    /// questions the run asks it, and reading what it did to the hub and its
    /// books. The run's half, what `cycle::step` does with these answers, is
    /// in the engine's `cycle` tests, under a scripted add-on that answers as
    /// this one does here.
    mod drive {
    use super::*;
    use wsp_engine::model::{Group, Status, Task, WorklistStatus};

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

    fn set(store: &Store, id: &str, status: Status) {
        let mut t = store.find_task(id).unwrap();
        t.set_status(status);
        t.touch();
        store.save_task(&t).unwrap();
    }

    /// A served hub with `ed` and `jesper` in `shared:team`; this store a spoke
    /// logged in as `me`; three shared rows in `todo` and the `run` list over
    /// them, shared and running.
    /// `None` when no hub is named (the test is skipped, `testhub`).
    fn two_spokes(
        env: &wsp_engine::util::Isolated,
        store: &Store,
        me: &str,
        setup: crate::testhub::Setup,
    ) -> Option<(crate::testhub::TestHub, std::collections::BTreeMap<String, String>, Home)> {
        let dir = env.path("hub");
        std::fs::create_dir_all(&dir).unwrap();
        let (running, tokens) = hub_lease::tests::serve_users(setup, &["ed", "jesper"])?;
        let home = Home::at(&hub_lease::tests::login(&dir, &running, &tokens, me));
        for id in ["wsp-1", "wsp-2", "wsp-3"] {
            let mut t = Task::new(id, id);
            t.workspace = "shared:team".parse().unwrap();
            store.save_task(&t).unwrap();
        }
        let mut w = Worklist::new("run", "run");
        w.set_status(WorklistStatus::Running);
        w.set_groups(&[Group { members: vec!["wsp-1".into(), "wsp-2".into(), "wsp-3".into()], agent: "claude".into(), ..Group::default() }]);
        w.workspace = "shared:team".parse().unwrap();
        store.save_worklist(&w).unwrap();
        Some((running, tokens, home))
    }

    /// The spoke's half of Done-when 1 (wsp-467): Ed's spoke takes the drive
    /// and records itself as driver; of the three members it is leased the two
    /// Jesper's spoke does not hold and is told the third runs on Jesper's wsp,
    /// while it is not done; the drive lease and the two member leases keep
    /// Jesper's spoke off them; a pulled verdict counts at the barrier only
    /// while it holds; and once the drive lease expires, Ed's spoke lets it go
    /// and its next gate takes it again with a higher token.
    #[test]
    fn drive_a_shared_list_is_driven_by_eds_spoke_and_the_member_jespers_spoke_leased_runs_there() {
        let (env, store) = crate::testkit::isolated_store("spoke-drive-two");
        let spoke = Spoke::new();
        // The spoke's own lease code asks core who is logged in
        // (`address::logged_in_user`), which asks the installed add-on.
        let _addon = wsp_engine::addon::scoped(std::sync::Arc::new(Spoke::new()));
        // Short leases, so the drive lease runs out in real time below.
        let Some((running, tokens, _home)) = two_spokes(&env, &store, "ed", crate::testhub::Setup::default().lease_secs(8)) else { return };
        let feed_client = hub_lease::tests::dial(&running, &tokens, "ed");
        let mut feed = feed_client.subscribe(None);
        assert!(matches!(feed.next_within(std::time::Duration::from_secs(10)), Some(crate::hub_client::Item::State { .. })), "subscribed before the leases are taken");
        let mut jesper = hub_lease::tests::dial(&running, &tokens, "jesper");
        hub_lease::take(&mut jesper, "shared:team", "wsp-2").unwrap();
        let w = store.worklist("run").unwrap();

        // What the run's first step asks: the gate, then each member's lease.
        assert_eq!(spoke.drive(&store, &w), Drives::Here, "Ed's spoke drives it");
        assert!(matches!(spoke.lease_member(&store, "wsp-1"), Member::Leased(_)), "the two Ed could lease");
        assert!(matches!(spoke.lease_member(&store, "wsp-2"), Member::Elsewhere(ref u) if u == "jesper"), "never the leased one");
        assert!(matches!(spoke.lease_member(&store, "wsp-3"), Member::Leased(_)));
        assert_eq!(spoke.running_elsewhere(&store, "wsp-2").as_deref(), Some("jesper"));
        assert_eq!(hub_lease::running_elsewhere(&store, "wsp-2").as_deref(), Some("jesper"));
        assert_eq!(spoke.driven_by(&store, &w).as_deref(), Some("ed"));
        assert_eq!(hub_lease::driven_by(&store, &w).as_deref(), Some("ed"));
        assert!(hub_lease::Book::read(&store).drives.contains_key("run"), "Ed's spoke holds drive:run");
        let why = hub_lease::take(&mut jesper, "shared:team", &hub_lease::drive_row("run")).unwrap_err();
        assert!(why.contains("ed"), "Jesper's spoke cannot take the drive: {why}");
        // Ed's spoke holds the two it leased: Jesper cannot take them.
        assert!(hub_lease::take(&mut jesper, "shared:team", "wsp-1").unwrap_err().contains("ed"));

        // Jesper's work arrives through the sync, then his landing: still his,
        // so the run neither reseats nor verifies it.
        set(&store, "wsp-2", Status::Doing);
        assert_eq!(spoke.running_elsewhere(&store, "wsp-2").as_deref(), Some("jesper"), "not reseated: it runs on Jesper's wsp");
        set(&store, "wsp-2", Status::Review);
        assert_eq!(spoke.running_elsewhere(&store, "wsp-2").as_deref(), Some("jesper"), "no verifier on Ed's spoke for Jesper's row");
        assert!(!spoke.verdict_held(&store, "wsp-2"), "and no barrier before its verdict");

        // A verdict that blocks is not counted; one that holds is, as pulled (wsp-486).
        let hold = serde_json::json!({ "commit": "abc1234", "verdict": "holds" });
        hub_lease::take_verdict(&store, "wsp-2", &hold);
        assert!(spoke.verdict_held(&store, "wsp-2"));
        hub_lease::take_verdict(&store, "wsp-2", &serde_json::json!({ "commit": "abc1234", "verdict": "blocks" }));
        assert!(!spoke.verdict_held(&store, "wsp-2"), "a later blocks clears the hold: the barrier waits");
        hub_lease::take_verdict(&store, "wsp-2", &hold);
        assert!(spoke.verdict_held(&store, "wsp-2"), "the barrier opens");
        // A landed member is no longer running elsewhere.
        set(&store, "wsp-2", Status::Done);
        assert_eq!(spoke.running_elsewhere(&store, "wsp-2"), None);

        // The drive lease expires on the hub: Ed's spoke lets it go, and the
        // next gate (any spoke of Ed's) takes it again with a higher token.
        let token = hub_lease::Book::read(&store).drives["run"].token;
        running.pass_ttl();
        let payload = loop {
            match feed.next_within(std::time::Duration::from_secs(10)) {
                Some(crate::hub_client::Item::Event(e)) if e.kind == wsp_store_contract::EventKind::LeaseExpired && e.payload["row"] == "drive:run" => break e.payload,
                Some(_) => continue,
                None => panic!("no lease.expired for drive:run on the stream"),
            }
        };
        assert_eq!(payload["token"], token.0, "the drive lease Ed's spoke held expired");
        assert!(hub_lease::follow_expired(&store, &payload));
        assert!(!hub_lease::Book::read(&store).drives.contains_key("run"));
        assert_eq!(spoke.drive(&store, &w), Drives::Here);
        assert!(hub_lease::Book::read(&store).drives["run"].token > token, "taken again");
        drop(running);
    }

    /// The spoke's half of wsp-486: Jesper's spoke, on a list allocated to Ed,
    /// does not drive it, reads the claim stamped with a hub token as the one
    /// lease it holds, and holds no drive lease.
    #[test]
    fn drive_the_holder_spoke_opens_a_verifier_for_a_member_of_a_list_it_does_not_drive() {
        let (env, store) = crate::testkit::isolated_store("spoke-drive-held");
        let spoke = Spoke::new();
        // The spoke's own lease code asks core who is logged in
        // (`address::logged_in_user`), which asks the installed add-on.
        let _addon = wsp_engine::addon::scoped(std::sync::Arc::new(Spoke::new()));
        let Some((running, _tokens, _home)) = two_spokes(&env, &store, "jesper", crate::testhub::Setup::default()) else { return };
        let mut w = store.worklist("run").unwrap();
        w.for_user = Some("ed".into());
        store.save_worklist(&w).unwrap();
        for id in ["wsp-2", "wsp-3"] {
            set(&store, id, Status::Review);
        }
        store.set_claim("wsp-2", serde_json::json!({ "hub_token": 5, "expires": wsp_engine::util::iso_at(wsp_engine::util::epoch_secs() + 3600) }));
        let before = std::fs::read_to_string(store.worklist_path("run")).unwrap();
        assert_eq!(spoke.drive(&store, &w), Drives::Elsewhere("ed".into()), "not this spoke's to drive");
        assert!(spoke.leased(&store.claims()["wsp-2"]), "only the member held here");
        assert!(store.claims().get("wsp-3").is_none_or(|c| !spoke.leased(c)));
        assert_eq!(spoke.running_elsewhere(&store, "wsp-2"), None, "the member is this spoke's to verify");
        assert!(hub_lease::Book::read(&store).drives.is_empty());
        assert_eq!(std::fs::read_to_string(store.worklist_path("run")).unwrap(), before, "the list untouched");
        drop(running);
    }

    /// The spoke's half of Done-when 2 (wsp-467): with the drive lease held by
    /// Ed's spoke, Jesper's gate says Ed drives it, records that, and takes no
    /// lease; allocated to Ed, it says so without asking the hub.
    #[test]
    fn drive_a_spoke_without_the_drive_lease_never_advances_the_list() {
        let (env, store) = crate::testkit::isolated_store("spoke-drive-none");
        let spoke = Spoke::new();
        // The spoke's own lease code asks core who is logged in
        // (`address::logged_in_user`), which asks the installed add-on.
        let _addon = wsp_engine::addon::scoped(std::sync::Arc::new(Spoke::new()));
        let Some((mut running, tokens, _home)) = two_spokes(&env, &store, "jesper", crate::testhub::Setup::default()) else { return };
        let mut ed = hub_lease::tests::dial(&running, &tokens, "ed");
        hub_lease::take(&mut ed, "shared:team", &hub_lease::drive_row("run")).unwrap();
        let w = store.worklist("run").unwrap();
        let file = || std::fs::read_to_string(store.worklist_path("run")).unwrap();
        let before = file();

        assert_eq!(spoke.drive(&store, &w), Drives::Elsewhere("ed".into()));
        assert_eq!(file(), before, "nothing written to the list");
        assert!(["wsp-1", "wsp-2", "wsp-3"].iter().all(|r| store.find_task(r).unwrap().status() == Status::Todo));
        assert_eq!(hub_lease::driven_by(&store, &w).as_deref(), Some("ed"));
        assert_eq!(spoke.driven_by(&store, &w).as_deref(), Some("ed"));
        assert!(hub_lease::Book::read(&store).drives.is_empty());

        // Allocated to ed: Jesper's spoke does not even ask the hub, which is
        // stopped here to show it.
        let mut w = store.worklist("run").unwrap();
        w.for_user = Some("ed".into());
        store.save_worklist(&w).unwrap();
        let before = file();
        running.stop();
        assert_eq!(hub_lease::drive(&store, &w), hub_lease::Drives::Elsewhere("ed".into()));
        assert_eq!(spoke.drive(&store, &w), Drives::Elsewhere("ed".into()));
        assert_eq!(file(), before);
    }
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
