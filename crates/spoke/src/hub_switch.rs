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
    // A claim's row, and a row this spoke leased with no claim (`wsp start`).
    let rows: std::collections::BTreeSet<String> = store.claims().keys().cloned().chain(hub_lease::Book::read(store).rows.into_keys()).collect();
    rows.iter()
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
pub fn held_leases(store: &Store) -> Vec<String> {
    let book = hub_lease::Book::read(store);
    let mut rows: std::collections::BTreeSet<String> =
        store.claims().iter().filter(|(_, c)| hub_lease::token_of(c).is_some()).map(|(r, _)| r.clone()).collect();
    rows.extend(book.rows.keys().cloned());
    let mut out: Vec<String> = rows.into_iter().collect();
    out.extend(book.drives.keys().map(|l| hub_lease::drive_row(l)));
    out
}

/// Releases every row lease a claim here holds (the claim keeps its binding and
/// loses its token) and every drive lease. A lease the hub says is gone already
/// counts as given back.
fn release_all(store: &Store, client: &mut crate::hub_client::Client, out: &mut Disabled) {
    for (row, claim) in store.claims() {
        let Some(token) = hub_lease::token_of(&claim) else { continue };
        match client.lease_release(&RowId(row.clone()), token) {
            Ok(()) | Err(crate::hub_client::Error::Refused(_)) => {
                strip_token(store, &row, token);
                hub_journal::Leased::forget(store, &row);
                out.released.push(row);
            }
            Err(_) => out.not_released.push(row),
        }
    }
    // The row leases no claim holds (`wsp start` made none), and any the book
    // has under another token than its claim's.
    for (row, d) in hub_lease::Book::read(store).rows {
        // Already given back with its claim: the hub says it is not held
        // under this token any more, and that counts as given back.
        match client.lease_release(&RowId(row.clone()), d.token) {
            Ok(()) | Err(crate::hub_client::Error::Refused(_)) => {
                hub_lease::forget_row(store, &row, d.token);
                hub_journal::Leased::forget(store, &row);
                if !out.released.contains(&row) {
                    out.released.push(row);
                }
            }
            Err(_) => {
                if !out.not_released.contains(&row) {
                    out.not_released.push(row)
                }
            }
        }
    }
    for (list, d) in hub_lease::Book::read(store).drives {
        let row = hub_lease::drive_row(&list);
        match client.lease_release(&RowId(row.clone()), d.token) {
            Ok(()) | Err(crate::hub_client::Error::Refused(_)) => {
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
    let held = held_leases(store);
    if !held.is_empty() {
        out.push(format!("leases held here: {}", held.join(" ")));
    }
    out.extend(link_lines(store));
    out
}

/// What the sync worker last said of the link that is not an outage: a login
/// the hub refused, and workspaces it would not sync (`sync.json`).
pub fn link_lines(store: &Store) -> Vec<String> {
    let v = sync_status(store);
    let mut out = Vec::new();
    if v["link"].as_str() == Some("refused") {
        out.push(v["why"].as_str().unwrap_or(hub_sync::REFUSED_LOGIN).to_string());
    }
    for (ws, why) in v["refused"].as_object().into_iter().flatten() {
        out.push(format!("not synced: {ws}: {}", why.as_str().unwrap_or("refused")));
    }
    out
}

fn sync_status(store: &Store) -> Value {
    std::fs::read_to_string(store.state.join("hub").join(hub_sync::STATUS_FILE))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// What `wsp hub status` says beside "not logged in" when a logout (or an
/// earlier disable) left rows parked by the disconnect: one line naming them
/// and the way back, and the journal still to send. The on/off switch is not
/// said: with no login there is nothing to be on.
pub fn logged_out_lines(store: &Store) -> Vec<String> {
    let s = Switch::read(store);
    let mut out = Vec::new();
    if !s.parked.is_empty() {
        out.push(format!(
            "parked when this wsp last disconnected: {} — `wsp login`, then `wsp hub enable`, takes their leases again and resumes them",
            s.parked.keys().cloned().collect::<Vec<_>>().join(" ")
        ));
    }
    let unsent = hub_journal::Journal::of(store).len();
    if unsent > 0 {
        out.push(format!("{unsent} journal entries wait to be sent once logged in and enabled"));
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
        "leases": held_leases(store),
        "link": sync_status(store)["link"],
        "refused_login": (sync_status(store)["link"].as_str() == Some("refused")).then(|| sync_status(store)["why"].clone()),
        "not_synced": sync_status(store)["refused"],
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
        assert!(r.home.join(crate::hub_client::HUBS_FILE).exists());
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
        assert!(!r.home.join(crate::hub_client::HUBS_FILE).exists(), "the login is gone");
        assert!(off(&r.store));
        assert_eq!(status(&r.store, "s-1"), Status::Parked);
        assert_eq!(status(&r.store, "p-1"), Status::Doing);
        assert!(hub_lease::token_of(&r.store.claims()["s-1"]).is_none());
        let mut bo = hub_lease::tests::dial(&r.running, &r.tokens, "bo");
        assert!(hub_lease::take(&mut bo, "shared:team", "s-1").is_ok(), "the lease went back before the login went");
        r.running.stop();
    }


    /// `wsp start` on a shared row, as the verb runs it: the lease is taken and
    /// no claim is made.
    fn start(store: &Store, row: &str) -> i32 {
        wsp_engine::cmd_task::set_status(store, &Args::synth("start", &[row], &[]), Status::Doing)
    }

    /// Finding 1: a lease `wsp start` took holds no claim, and is still known
    /// here with its token: status lists it, disable parks the row and gives
    /// the lease back (another user takes it at once), enable takes it again
    /// when it is free, and leaves the row parked naming the holder when not.
    #[test]
    fn hub_switch_a_lease_wsp_start_took_with_no_claim_is_parked_given_back_and_taken_again() {
        let r = rig("start");
        assert_eq!(start(&r.store, "s-2"), 0);
        assert_eq!(status(&r.store, "s-2"), Status::Doing);
        assert!(!r.store.claims().contains_key("s-2"), "`wsp start` makes no claim");
        let token = hub_lease::Book::read(&r.store).rows["s-2"].token;
        assert!(status_lines(&r.store).iter().any(|l| l.starts_with("leases held here:") && l.contains("s-2")), "{:?}", status_lines(&r.store));
        let mut bo = hub_lease::tests::dial(&r.running, &r.tokens, "bo");
        assert!(hub_lease::take(&mut bo, "shared:team", "s-2").unwrap_err().contains("ann"), "ann holds it");

        let done = disable(&r.store, &r.home);
        assert!(done.parked.contains(&"s-2".to_string()), "{done:?}");
        assert!(done.released.contains(&"s-2".to_string()), "{done:?}");
        assert_eq!(status(&r.store, "s-2"), Status::Parked);
        assert!(!hub_lease::Book::read(&r.store).rows.contains_key("s-2"), "the book forgets what it gave back");
        let bos = hub_lease::take(&mut bo, "shared:team", "s-2").expect("free to another user at once");
        // Held by bo: enable leaves it parked, naming bo.
        let on = enable(&r.store, &r.home);
        let still = on.still_parked.iter().find(|(row, _)| row == "s-2").expect("s-2 stays parked");
        assert!(still.1.contains("bo"), "{}", still.1);
        assert_eq!(status(&r.store, "s-2"), Status::Parked);

        // bo lets go; off and on again: taken again, resumed, in the book.
        bo.lease_release(&RowId("s-2".into()), bos.token).unwrap();
        disable(&r.store, &r.home);
        let on = enable(&r.store, &r.home);
        assert!(on.resumed.contains(&"s-2".to_string()), "{on:?}");
        assert_eq!(status(&r.store, "s-2"), Status::Doing);
        assert!(hub_lease::Book::read(&r.store).rows["s-2"].token > token, "a lease again, under a new token");
        assert!(hub_lease::take(&mut bo, "shared:team", "s-2").unwrap_err().contains("ann"));
        r.running.stop();
    }

    /// Finding 1: whatever ends the work lets a `wsp start` lease go, as a
    /// claimed row's end does: `wsp park` as the verb writes it, and a row a
    /// landing closed by another write, at the sync worker's next sweep.
    #[test]
    fn hub_switch_the_end_of_a_started_rows_work_gives_its_lease_back() {
        let r = rig("start-end");
        let mut bo = hub_lease::tests::dial(&r.running, &r.tokens, "bo");
        assert_eq!(start(&r.store, "s-2"), 0);
        assert_eq!(wsp_engine::cmd_task::park(&r.store, &Args::synth("park", &["s-2", "later"], &[])), 0);
        assert!(!hub_lease::Book::read(&r.store).rows.contains_key("s-2"));
        let held = hub_lease::take(&mut bo, "shared:team", "s-2").expect("parked: given back");
        bo.lease_release(&RowId("s-2".into()), held.token).unwrap();

        // Started again, then closed by a write that is no verb's (a landing).
        let mut t = r.store.task("s-2").unwrap();
        t.set_status(Status::Todo);
        r.store.save_task(&t).unwrap();
        assert_eq!(start(&r.store, "s-2"), 0);
        let mut t = r.store.task("s-2").unwrap();
        t.set_status(Status::Done);
        r.store.save_task(&t).unwrap();
        assert_eq!(hub_lease::finished_unheld(&r.store).len(), 1);
        let mut ann = hub_lease::tests::dial(&r.running, &r.tokens, "ann");
        assert_eq!(hub_lease::release_finished(&r.store, &mut ann), 1);
        assert!(hub_lease::take(&mut bo, "shared:team", "s-2").is_ok(), "landed: given back");
        // A claimed row's lease is its claim's, never swept.
        assert!(hub_lease::finished_unheld(&r.store).is_empty());
        r.running.stop();
    }

    /// Finding 2: after `wsp logout` the rows the disconnect parked are still
    /// remembered (a later login and enable resume them), and status says one
    /// thing: not logged in, these are parked, this is the way back. No on/off
    /// line, and exit 1.
    #[test]
    fn hub_switch_after_logout_status_says_not_logged_in_and_what_waits_for_the_next_login() {
        let r = rig("logout-status");
        assert_eq!(crate::cmd_login::logout(&r.store, &Args::synth("logout", &[], &[])), 0);
        assert_eq!(Switch::read(&r.store).parked.keys().cloned().collect::<Vec<_>>(), vec!["s-1".to_string()], "remembered for the next login");
        let lines = logged_out_lines(&r.store).join("\n");
        assert!(lines.contains("s-1") && lines.contains("wsp login") && lines.contains("wsp hub enable"), "{lines}");
        assert!(!lines.contains("disabled since") && !lines.starts_with("off"), "{lines}");
        assert_eq!(crate::cmd_login::hub(&r.store, &Args::synth("hub", &["status"], &[])), 1);
        r.running.stop();
    }

    /// Finding 3: a revoked token is a refused login, not an outage: the
    /// worker's link reads `refused` with the hub's reason in `sync.json` and
    /// `cycle.log`, status says it, `whoami` says it, and the backoff applies.
    #[test]
    fn hub_switch_a_revoked_token_is_a_refused_login_in_the_log_the_status_and_whoami() {
        let r = rig("revoked");
        let root = wsp_store_contract::Principal::Person("root".into());
        r.running
            .hub()
            .apply(
                wsp_store_contract::Command::HubTokenRevoke { id: wsp_store_contract::TokenId("t-ann".into()) },
                wsp_store_contract::IdempotencyKey("revoke-ann".into()),
                root,
            )
            .expect("revoked");
        let mut worker = hub_sync::Worker::new(r.home.clone());
        let why = worker.turn(&r.store).unwrap_err();
        assert!(hub_sync::is_refused_login(&why), "{why}");
        assert!(why.contains("wsp login"), "{why}");
        let st = sync_status(&r.store);
        assert_eq!(st["link"], "refused", "{st}");
        assert!(st["last_failure"]["why"].as_str().is_some_and(hub_sync::is_refused_login), "{st}");
        let log = std::fs::read_to_string(r.store.state.join("cycle.log")).unwrap_or_default();
        assert!(log.contains(hub_sync::REFUSED_LOGIN) && !log.contains("unreachable"), "{log}");
        assert!(link_lines(&r.store).iter().any(|l| hub_sync::is_refused_login(l)), "{:?}", link_lines(&r.store));
        // Said once: a second refused turn adds no line.
        let _ = worker.turn(&r.store);
        let again = std::fs::read_to_string(r.store.state.join("cycle.log")).unwrap_or_default();
        assert_eq!(again.matches(hub_sync::REFUSED_LOGIN).count(), 1, "{again}");
        let url = format!("tcp://{}", r.running.addrs()[0]);
        let said = crate::spoke::whoami_at(&url, crate::hub_client::Credential::User(r.tokens["ann"].clone()), None);
        assert!(matches!(&said, wsp_engine::addon::WhoAmI::Refused(w) if hub_sync::is_refused_login(w)), "{said:?}");
        r.running.stop();
    }

    /// Finding 3: a workspace the hub will not sync for this user (not a
    /// member) is in `sync.json` and status, though the link is up.
    #[test]
    fn hub_switch_a_workspace_the_hub_will_not_sync_shows_in_sync_json_and_status() {
        let r = rig("forbidden");
        let mut t = wsp_engine::model::Task::new("x-1", "x-1");
        t.workspace = "shared:other".parse().unwrap();
        r.store.save_task(&t).unwrap();
        let mut worker = hub_sync::Worker::new(r.home.clone());
        worker.turn(&r.store).expect("the hub answers");
        let st = sync_status(&r.store);
        assert_eq!(st["link"], "up", "{st}");
        assert!(st["refused"]["shared:other"].as_str().is_some(), "{st}");
        assert!(link_lines(&r.store).iter().any(|l| l.contains("not synced: shared:other")), "{:?}", link_lines(&r.store));
        r.running.stop();
    }
}
