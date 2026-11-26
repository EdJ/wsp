//! The spoke's on/off switch: `wsp hub disable`, `wsp hub enable` and what
//! `wsp hub status` says of them.
//!
//! Disabling keeps the login (`~/.config/wsp/hubs.json`) and stops this wsp
//! taking part in shared work, cleanly and in this order:
//!
//! 1. **No new shared work.** The switch is written first. From then on the
//!    spoke takes no lease (a claim, `wsp start` or a spawn of a shared row is
//!    refused, naming `wsp hub enable`), advances no shared list, files nothing
//!    new into a shared workspace and sends no message to the hub; core reads
//!    it as standalone ([`wsp_engine::addon::SharedWork::active`] is false).
//! 2. **Agents in flight on shared rows are parked** with `wsp park`'s own
//!    write ([`wsp_engine::cmd_task::park`]), the reason naming the disconnect, and
//!    each row's status before it is kept here so enabling can put it back.
//!    Parking ends nothing that is running: it changes the row's status and
//!    logs why, and a live seat keeps its claim and goes on to the end of what
//!    it is doing (`wsp park` ends the agent record of a seat that is no longer
//!    standing, and nothing else). Private rows are not touched.
//! 3. **The journal is flushed** with one sync turn from this process, so the
//!    parked status and anything not yet sent reach the hub while it answers.
//!    A daemon's sync worker is asked to stand down first (it reads the switch
//!    before every turn, and says in `sync.json` that its link is off), so two
//!    turns never run at once.
//! 4. **Leases go back**: every row lease a claim here holds and every drive
//!    lease is released on the hub, so another user's wsp sees the work free at
//!    once; a released claim keeps its seat binding and loses its token.
//! 5. **The worker stops** taking turns (it idles while the switch is off) and
//!    the cache stays readable, marked not live (`sync.json`'s link is `off`;
//!    `wsp show` says a head is as last synced).
//!
//! A hub that does not answer changes only 3 and 4: the journal is kept, the
//! leases run out on the hub by themselves, and `wsp hub status` says what was
//! not sent and which leases were not given back.
//!
//! Enabling turns the switch back on (the worker's next look is a turn, which
//! replays the journal) and, for each row the disable parked and that is still
//! parked, takes its lease again as `wsp start` does: granted, the lease is
//! stamped on the claim and the row is put back to the status it had; held by
//! another user, it stays parked and the user is told who holds it.
//!
//! The switch lives beside the rest of this spoke's state, in
//! `$WSP_STATE/hub/switch.json`: it is about this wsp's leases, worker and
//! parked rows, which are per state directory, and not about the person's
//! login, which is per home and which disabling leaves alone. No file means
//! on, so a wsp that never disabled (or never logged in) writes nothing.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use wsp_store_contract::{RowId, Token};

use wsp_engine::base::args::Args;
use wsp_engine::model::Status;
use wsp_engine::store::Store;
use crate::{hub_journal, hub_lease, hub_sync};

/// The reason a row parked by the disconnect carries in its log.
pub const REASON: &str = "hub disconnected — wsp hub enable takes its lease again and resumes it";

/// How long `wsp hub disable` waits for a daemon's worker to finish a turn in
/// progress and stand down before it flushes the journal itself.
const WORKER_WAIT: Duration = Duration::from_secs(20);

/// `$WSP_STATE/hub/switch.json`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Switch {
    /// Switched off; no file is on.
    pub off: bool,
    /// When it was last switched, epoch seconds.
    pub at: Option<i64>,
    /// Row -> the status it had before the disconnect parked it.
    pub parked: BTreeMap<String, String>,
    /// Leases the disable could not give back (`drive:<list>` for a drive
    /// lease): they run out on the hub.
    pub not_released: Vec<String>,
    /// Why the hub did not answer the disable, if it did not.
    pub unreachable: Option<String>,
}

impl Switch {
    fn path(store: &Store) -> std::path::PathBuf {
        store.state.join("hub").join("switch.json")
    }

    pub fn read(store: &Store) -> Switch {
        let Ok(text) = std::fs::read_to_string(Switch::path(store)) else { return Switch::default() };
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        Switch {
            off: v["enabled"].as_bool() == Some(false),
            at: v["at"].as_i64(),
            parked: v["parked"]
                .as_object()
                .into_iter()
                .flatten()
                .filter_map(|(r, s)| Some((r.clone(), s.as_str()?.to_string())))
                .collect(),
            not_released: v["not_released"].as_array().into_iter().flatten().filter_map(|r| r.as_str().map(str::to_string)).collect(),
            unreachable: v["unreachable"].as_str().map(str::to_string),
        }
    }

    pub fn write(&self, store: &Store) -> std::io::Result<()> {
        let path = Switch::path(store);
        let dir = path.parent().expect("under hub/");
        std::fs::create_dir_all(dir)?;
        let v = json!({
            "enabled": !self.off,
            "at": self.at,
            "parked": self.parked,
            "not_released": self.not_released,
            "unreachable": self.unreachable,
        });
        let tmp = dir.join(format!(".switch.json.{}.tmp", std::process::id()));
        std::fs::write(&tmp, v.to_string())?;
        std::fs::rename(&tmp, &path)
    }

    /// Read, change and write back under the store's lock.
    fn edit<T>(store: &Store, f: impl FnOnce(&mut Switch) -> T) -> std::io::Result<T> {
        store.locked(|| {
            let mut s = Switch::read(store);
            let out = f(&mut s);
            s.write(store).map(|_| out)
        })
    }
}

/// Whether the connection is switched off here.
pub fn off(store: &Store) -> bool {
    Switch::read(store).off
}

/// What `wsp hub disable` did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Disabled {
    /// It was off already (the steps ran again for what was left).
    pub was_off: bool,
    /// Rows parked now.
    pub parked: Vec<String>,
    /// Why the hub did not answer, if it did not: nothing more was sent.
    pub unreachable: Option<String>,
    /// Journal entries still to send.
    pub unsent: usize,
    /// Leases given back.
    pub released: Vec<String>,
    /// Leases not given back.
    pub not_released: Vec<String>,
}

/// The shared rows a claim here holds that are not finished or parked: the
/// agents in flight on shared work.
fn in_flight(store: &Store) -> Vec<(String, Status)> {
    store
        .claims()
        .keys()
        .filter_map(|row| store.task(row))
        .filter(|t| !t.workspace.is_private() && !matches!(t.status(), Status::Done | Status::Parked))
        .map(|t| (t.id.clone(), t.status()))
        .collect()
}

/// Waits, a bounded while, for a live daemon's worker to stand down: its
/// `sync.json` names its pid and says `off`. Nothing to wait on when no live
/// process other than this one wrote it.
fn worker_stood_down(store: &Store) {
    let read = || -> Value {
        std::fs::read_to_string(store.state.join("hub").join(hub_sync::STATUS_FILE))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    };
    let end = Instant::now() + WORKER_WAIT;
    loop {
        let v = read();
        let pid = v["pid"].as_u64().map(|p| p as u32);
        let running = pid.is_some_and(|p| p != std::process::id() && wsp_engine::gc::alive(p));
        if !running || v["link"].as_str() == Some("off") {
            return;
        }
        if Instant::now() >= end {
            wsp_engine::store::log_line(store, "hub disable: the sync worker did not stand down in time; flushing the journal beside it");
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Disable the connection under the login in `home`, in the order the module's
/// doc gives.
pub fn disable(store: &Store, home: &Path) -> Disabled {
    let mut out = Disabled::default();
    // 1. No new shared work, from this moment, in every process.
    let was = Switch::edit(store, |s| {
        let was = s.off;
        s.off = true;
        s.at = Some(wsp_engine::util::epoch_secs());
        was
    });
    out.was_off = was.unwrap_or(false);
    worker_stood_down(store);

    // 2. Park what is in flight on shared rows, with `wsp park`'s own write.
    for (row, status) in in_flight(store) {
        let args = Args::synth("park", &[row.as_str(), REASON], &[]);
        if wsp_engine::cmd_task::park(store, &args) == 0 {
            let _ = Switch::edit(store, |s| {
                s.parked.insert(row.clone(), status.as_str().to_string());
            });
            out.parked.push(row);
        }
    }

    // 3. Flush the journal while the hub answers.
    let mut worker = hub_sync::Worker::new(home.to_path_buf());
    if let Err(why) = worker.turn(store) {
        out.unreachable = Some(why);
    }

    // 4. Give the leases back.
    if out.unreachable.is_none() {
        match hub_sync::dial(home) {
            Ok(mut client) => release_all(store, &mut client, &mut out),
            Err(why) => out.unreachable = Some(why),
        }
    }
    if out.unreachable.is_some() {
        out.not_released = held_leases(store);
    }

    // 5. The worker stands down here too: `sync.json` says the link is off.
    worker.stand_down(store);
    out.unsent = hub_journal::Journal::of(store).len();
    let _ = Switch::edit(store, |s| {
        s.not_released = out.not_released.clone();
        s.unreachable = out.unreachable.clone();
    });
    out
}

/// Every lease this spoke holds: row leases on the claims, then drive leases.
fn held_leases(store: &Store) -> Vec<String> {
    let mut out: Vec<String> = store.claims().iter().filter(|(_, c)| hub_lease::token_of(c).is_some()).map(|(r, _)| r.clone()).collect();
    out.extend(hub_lease::Book::read(store).drives.keys().map(|l| hub_lease::drive_row(l)));
    out
}

/// Releases every row lease a claim here holds (the claim keeps its binding and
/// loses its token) and every drive lease. A lease the hub says is gone already
/// counts as given back.
fn release_all(store: &Store, client: &mut wsp_client::Client, out: &mut Disabled) {
    for (row, claim) in store.claims() {
        let Some(token) = hub_lease::token_of(&claim) else { continue };
        match client.lease_release(&RowId(row.clone()), token) {
            Ok(()) | Err(wsp_client::Error::Refused(_)) => {
                strip_token(store, &row, token);
                hub_journal::Leased::forget(store, &row);
                out.released.push(row);
            }
            Err(_) => out.not_released.push(row),
        }
    }
    for (list, d) in hub_lease::Book::read(store).drives {
        let row = hub_lease::drive_row(&list);
        match client.lease_release(&RowId(row.clone()), d.token) {
            Ok(()) | Err(wsp_client::Error::Refused(_)) => {
                hub_lease::Book::edit(store, |b| {
                    if b.drives.get(&list).is_some_and(|x| x.token == d.token) {
                        b.drives.remove(&list);
                    }
                });
                out.released.push(row);
            }
            Err(_) => out.not_released.push(row),
        }
    }
}

/// The claim on `row` without its hub token, where it still holds `token`.
fn strip_token(store: &Store, row: &str, token: Token) {
    store.locked(|| {
        let Some(mut claim) = store.claims().get(row).cloned() else { return };
        if hub_lease::token_of(&claim) != Some(token) {
            return;
        }
        if let Some(o) = claim.as_object_mut() {
            o.remove(hub_lease::TOKEN);
        }
        store.set_claim(row, claim);
    });
}

/// What `wsp hub enable` did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Enabled {
    /// It was on already.
    pub was_on: bool,
    /// Rows whose lease was taken again and which are back at their status.
    pub resumed: Vec<String>,
    /// Rows still parked, and why (who holds the lease, or the hub's silence).
    pub still_parked: Vec<(String, String)>,
    /// The journal replayed by this process (no daemon worker was running):
    /// why not, if it did not answer.
    pub replay: Option<Result<(), String>>,
}

/// Enable the connection under the login in `home`, taking back what the
/// disable parked.
pub fn enable(store: &Store, home: &Path) -> Enabled {
    let mut out = Enabled::default();
    let was = Switch::edit(store, |s| {
        let was = !s.off;
        s.off = false;
        s.at = Some(wsp_engine::util::epoch_secs());
        s.not_released.clear();
        s.unreachable = None;
        was
    });
    out.was_on = was.unwrap_or(false);

    for (row, was) in Switch::read(store).parked {
        let Some(mut t) = store.task(&row) else {
            let _ = Switch::edit(store, |s| s.parked.remove(&row));
            continue;
        };
        // Moved on by hand since: no longer the disconnect's to take back.
        if t.status() != Status::Parked {
            let _ = Switch::edit(store, |s| s.parked.remove(&row));
            continue;
        }
        match hub_lease::before_claim(store, &row) {
            Ok(held) => {
                if let Some(held) = held {
                    store.locked(|| {
                        if let Some(mut claim) = store.claims().get(&row).cloned() {
                            hub_lease::stamp(&mut claim, &held);
                            store.set_claim(&row, claim);
                        }
                    });
                }
                let status = Status::parse(&was).unwrap_or(Status::Doing);
                t.set_status(status);
                t.log(&format!("resumed: the hub connection is enabled again (→ {})", status.as_str()));
                t.touch();
                let caller = wsp_engine::cmd_message::Caller::here(store);
                match wsp_engine::cmd_task::write_task(store, &t, "resume", &caller) {
                    Ok(()) => {
                        let _ = Switch::edit(store, |s| s.parked.remove(&row));
                        out.resumed.push(row);
                    }
                    Err(e) => out.still_parked.push((row, format!("the row was not written: {e}"))),
                }
            }
            Err(why) => out.still_parked.push((row, why)),
        }
    }

    // The journal: the daemon's worker replays it on its next look; with no
    // live worker, this process does it now.
    if !worker_running(store) {
        let mut worker = hub_sync::Worker::new(home.to_path_buf());
        out.replay = Some(worker.turn(store).map(|_| ()));
    }
    out
}

/// Whether a live process other than this one keeps `sync.json`: a daemon's worker.
fn worker_running(store: &Store) -> bool {
    std::fs::read_to_string(store.state.join("hub").join(hub_sync::STATUS_FILE))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v["pid"].as_u64())
        .is_some_and(|p| p as u32 != std::process::id() && wsp_engine::gc::alive(p as u32))
}

/// What `wsp hub status` says of the switch, beside the login's lines.
pub fn status_lines(store: &Store) -> Vec<String> {
    let s = Switch::read(store);
    let mut out = vec![match (s.off, s.at) {
        (false, _) => "on    enabled: shared work is taken and synced".to_string(),
        (true, Some(at)) => format!("off   disabled since {}: no shared work is taken (`wsp hub enable`)", wsp_engine::util::iso_at(at)),
        (true, None) => "off   disabled: no shared work is taken (`wsp hub enable`)".to_string(),
    }];
    for (row, was) in &s.parked {
        out.push(format!("parked by the disconnect: {row} (was {was})"));
    }
    let unsent = hub_journal::Journal::of(store).len();
    if unsent > 0 {
        out.push(format!("unsent: {unsent} journal entries wait for the hub"));
    }
    if !s.not_released.is_empty() {
        out.push(format!("not given back: {} (they run out on the hub)", s.not_released.join(" ")));
    }
    if let Some(why) = &s.unreachable {
        out.push(format!("the hub did not answer the disable: {why}"));
    }
    out
}

/// [`status_lines`] as JSON fields.
pub fn status_json(store: &Store) -> Value {
    let s = Switch::read(store);
    json!({
        "enabled": !s.off,
        "since": s.at,
        "parked": s.parked,
        "unsent": hub_journal::Journal::of(store).len(),
        "not_released": s.not_released,
        "unreachable": s.unreachable,
    })
}

/// The doctor's line for a switched-off spoke.
pub fn health_line(store: &Store) -> String {
    let s = Switch::read(store);
    format!(
        "hub connection disabled here (`wsp hub enable`): {} row(s) parked by it, {} journal entries unsent",
        s.parked.len(),
        hub_journal::Journal::of(store).len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spoke::Spoke;
    use std::sync::Arc;

    /// `$WSP_HUB_HOME` pointed at a spoke's home for the test, put back however
    /// it ends. Under the env lock `isolated` holds.
    struct HubHome(Option<std::ffi::OsString>);

    impl HubHome {
        fn at(home: &Path) -> HubHome {
            let was = std::env::var_os(wsp_client::HUB_HOME_ENV);
            std::env::set_var(wsp_client::HUB_HOME_ENV, home);
            HubHome(was)
        }
    }

    impl Drop for HubHome {
        fn drop(&mut self) {
            match self.0.take() {
                Some(h) => std::env::set_var(wsp_client::HUB_HOME_ENV, h),
                None => std::env::remove_var(wsp_client::HUB_HOME_ENV),
            }
        }
    }

    fn row(store: &Store, id: &str, shared: bool, status: Status) {
        let mut t = wsp_engine::model::Task::new(id, id);
        if shared {
            t.workspace = "shared:team".parse().unwrap();
        }
        t.status_raw = status.as_str().into();
        store.save_task(&t).unwrap();
    }

    /// A claim on `row` by seat `seat`, with `held` stamped where given.
    fn claim(store: &Store, row: &str, seat: &str, held: Option<&Held>) {
        let mut c = json!({ "workspace_id": seat, "claimed_at": wsp_engine::util::now_iso() });
        wsp_store::store::stamp_claim_expiry(&mut c);
        if let Some(h) = held {
            hub_lease::stamp(&mut c, h);
        }
        store.set_claim(row, c);
        store.set_binding(seat, json!({ "task_id": row, "pane_id": seat }));
    }

    fn status(store: &Store, id: &str) -> Status {
        store.task(id).unwrap().status()
    }

    use wsp_engine::addon::Held;

    /// A served hub with `ann` and `bo` in `shared:team`, this store ann's
    /// spoke: `s-1` shared, in doing and claimed under ann's lease; `p-1`
    /// private, in doing and claimed; `s-2` shared and in todo, unclaimed.
    struct Rig {
        _addon: wsp_engine::addon::Scoped,
        _hub_home: HubHome,
        running: wsp_hub::serve::Running,
        tokens: BTreeMap<String, String>,
        store: Store,
        home: std::path::PathBuf,
        _env: wsp_engine::util::Isolated,
    }

    fn rig(tag: &str) -> Rig {
        let env = wsp_engine::util::isolated(&format!("hub-switch-{tag}"));
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let dir = env.path("hub");
        std::fs::create_dir_all(&dir).unwrap();
        let (running, tokens) = hub_lease::tests::serve_users(&dir, &["ann", "bo"]);
        let home = hub_lease::tests::login(&dir, &running, &tokens, "ann");
        let hub_home = HubHome::at(&home);
        let addon = wsp_engine::addon::scoped(Arc::new(Spoke::new()));
        row(&store, "s-1", true, Status::Doing);
        row(&store, "p-1", false, Status::Doing);
        row(&store, "s-2", true, Status::Todo);
        let mut ann = hub_lease::tests::dial(&running, &tokens, "ann");
        let held = hub_lease::take(&mut ann, "shared:team", "s-1").unwrap();
        claim(&store, "s-1", "cpd-s1", Some(&held));
        claim(&store, "p-1", "cpd-p1", None);
        Rig { _addon: addon, _hub_home: hub_home, running, tokens, store, home, _env: env }
    }

    /// Done-when: disable parks the shared row in flight and nothing else, with
    /// the reason in its log; flushes; gives the lease back so another user
    /// takes it at once; and from then on the spoke takes no new shared work and
    /// reads as standalone to core.
    #[test]
    fn hub_switch_disable_parks_only_shared_rows_in_flight_flushes_and_gives_the_leases_back() {
        let r = rig("disable");
        let done = disable(&r.store, &r.home);
        assert_eq!(done.parked, vec!["s-1".to_string()], "{done:?}");
        assert_eq!(done.unreachable, None, "{done:?}");
        assert_eq!(done.released, vec!["s-1".to_string()]);
        assert_eq!(done.unsent, 0, "the journal went to the hub");
        assert_eq!(status(&r.store, "s-1"), Status::Parked);
        assert!(r.store.task("s-1").unwrap().body.contains(REASON), "the reason is in its log");
        assert_eq!(status(&r.store, "p-1"), Status::Doing, "private work is untouched");
        assert_eq!(status(&r.store, "s-2"), Status::Todo, "a shared row nobody holds is untouched");
        let c = r.store.claims()["s-1"].clone();
        assert!(hub_lease::token_of(&c).is_none() && c["workspace_id"] == "cpd-s1", "the claim keeps its seat, not its token: {c}");
        assert!(r.store.claims().contains_key("p-1"));
        let mut bo = hub_lease::tests::dial(&r.running, &r.tokens, "bo");
        assert!(hub_lease::take(&mut bo, "shared:team", "s-1").is_ok(), "free to another user at once");

        // Off: no new shared work, and core reads standalone.
        assert!(off(&r.store));
        let spoke = wsp_engine::addon::get();
        assert!(!spoke.active(&r.store));
        let why = spoke.before_claim(&r.store, "s-2").unwrap_err();
        assert!(why.contains("wsp hub enable"), "{why}");
        assert_eq!(spoke.before_claim(&r.store, "p-1"), Ok(None), "private is as ever");
        assert!(spoke.may_file_shared(&r.store).unwrap_err().contains("disabled"));
        let mut w = wsp_engine::model::Worklist::new("run", "run");
        w.workspace = "shared:team".parse().unwrap();
        assert!(matches!(spoke.drive(&r.store, &w), wsp_engine::addon::Drives::NotYet(_)), "no shared list advanced from here");
        let lines = status_lines(&r.store).join("\n");
        assert!(lines.contains("disabled") && lines.contains("s-1 (was doing)"), "{lines}");
        // The login is kept.
        assert!(r.home.join(wsp_client::HUBS_FILE).exists());
        r.running.stop();
    }

    /// Done-when: a disable the hub does not answer still switches off and
    /// parks, keeps the journal, gives nothing back (the leases run out on the
    /// hub), and `wsp hub status` says what was not sent.
    #[test]
    fn hub_switch_disable_with_the_hub_unreachable_parks_keeps_the_journal_and_says_what_was_not_sent() {
        let r = rig("unreachable");
        let addr = r.running.addrs()[0];
        r.running.stop();
        // Nothing answers on the hub's port now.
        let _ = std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(100));
        let done = disable(&r.store, &r.home);
        assert_eq!(done.parked, vec!["s-1".to_string()]);
        assert!(done.unreachable.is_some(), "{done:?}");
        assert!(done.released.is_empty());
        assert_eq!(done.not_released, vec!["s-1".to_string()]);
        assert!(done.unsent >= 1, "the parked row's record waits in the journal: {done:?}");
        assert!(off(&r.store));
        assert_eq!(status(&r.store, "s-1"), Status::Parked);
        assert_eq!(status(&r.store, "p-1"), Status::Doing);
        assert!(hub_lease::token_of(&r.store.claims()["s-1"]).is_some(), "the lease is not given back: it runs out");
        let lines = status_lines(&r.store).join("\n");
        assert!(lines.contains("unsent:") && lines.contains("not given back: s-1") && lines.contains("did not answer"), "{lines}");
        let st: Value = serde_json::from_str(&std::fs::read_to_string(r.store.state.join("hub").join(hub_sync::STATUS_FILE)).unwrap()).unwrap();
        assert_eq!(st["link"], "off", "the cache is marked not live: {st}");
    }

    /// Done-when: enable takes each parked row's lease again; granted, the row
    /// is back at its status with the lease on its claim; held by another user,
    /// it stays parked and the user is told who holds it.
    #[test]
    fn hub_switch_enable_resumes_a_row_whose_lease_it_takes_and_leaves_one_another_user_holds_parked() {
        let r = rig("enable");
        // A second shared row in flight here.
        let mut ann = hub_lease::tests::dial(&r.running, &r.tokens, "ann");
        let held = hub_lease::take(&mut ann, "shared:team", "s-2").unwrap();
        let mut t = r.store.task("s-2").unwrap();
        t.set_status(Status::Review);
        r.store.save_task(&t).unwrap();
        claim(&r.store, "s-2", "cpd-s2", Some(&held));
        let done = disable(&r.store, &r.home);
        assert_eq!(done.parked, vec!["s-1".to_string(), "s-2".to_string()]);
        // While it is off, bo takes s-2.
        let mut bo = hub_lease::tests::dial(&r.running, &r.tokens, "bo");
        hub_lease::take(&mut bo, "shared:team", "s-2").unwrap();

        let on = enable(&r.store, &r.home);
        assert!(!off(&r.store));
        assert_eq!(on.resumed, vec!["s-1".to_string()], "{on:?}");
        assert_eq!(status(&r.store, "s-1"), Status::Doing);
        assert!(hub_lease::token_of(&r.store.claims()["s-1"]).is_some(), "the lease is on the claim again");
        assert_eq!(on.still_parked.len(), 1, "{on:?}");
        assert_eq!(on.still_parked[0].0, "s-2");
        assert!(on.still_parked[0].1.contains("bo"), "who holds it is said: {}", on.still_parked[0].1);
        assert_eq!(status(&r.store, "s-2"), Status::Parked);
        assert!(matches!(on.replay, Some(Ok(()))), "{on:?}");
        assert_eq!(Switch::read(&r.store).parked.keys().cloned().collect::<Vec<_>>(), vec!["s-2".to_string()], "only the one still parked is remembered");
        assert!(wsp_engine::addon::get().active(&r.store));
        r.running.stop();
    }

    /// Done-when: `wsp logout` disables first (parked, leases given back), then
    /// removes the login.
    #[test]
    fn hub_switch_logout_disables_first_then_removes_the_login() {
        let r = rig("logout");
        let args = Args::synth("logout", &[], &[]);
        assert_eq!(crate::cmd_login::logout(&r.store, &args), 0);
        assert!(!r.home.join(wsp_client::HUBS_FILE).exists(), "the login is gone");
        assert!(off(&r.store));
        assert_eq!(status(&r.store, "s-1"), Status::Parked);
        assert_eq!(status(&r.store, "p-1"), Status::Doing);
        assert!(hub_lease::token_of(&r.store.claims()["s-1"]).is_none());
        let mut bo = hub_lease::tests::dial(&r.running, &r.tokens, "bo");
        assert!(hub_lease::take(&mut bo, "shared:team", "s-1").is_ok(), "the lease went back before the login went");
        r.running.stop();
    }
}
