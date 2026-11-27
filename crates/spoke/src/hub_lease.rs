//! The spoke's side of the hub's row leases (wsp-466).
//!
//! A row in a shared workspace is moved by one user's seat at a time, and the
//! hub says which: `lease.take` before the claim (`wsp claim`, so a spawn, and
//! `wsp start`) answers a fencing token and an expiry, or refuses `held` naming
//! the user whose spoke has it, or `forbidden` for a row allocated to another
//! user. The token and the hub's expiry are written into the local claim
//! (`hub_token`, `expires`: wsp-409), so the local expiry pass and the hub agree.
//!
//! **Renewal** (wsp-410's rule): the holder seat is alive and the claim has been
//! as it is for an hour. [`renew_due`] sends `lease.renew` and moves `expires` to
//! the hub's answer; the daemon's own renewal leaves a claim with a `hub_token`
//! alone, since only the hub's word extends it.
//!
//! **Expiry**: the hub releases a lease past its TTL and announces
//! `lease.expired`; [`follow_expired`] is the spoke that held it letting go the
//! way `wsp despawn --lost` does (`cmd_spawn::release_lost`, lent by the daemon), the
//! checkpoint kept in the tree. A renewal the hub refuses as gone is the same.
//!
//! **Verdicts and landings** (wsp-468). A shared row's verdict (its ready line)
//! and its landing (its `wsp: landed` line) go to the hub as `row.reviewed` and
//! `row.landed` event objects on the row's lease subject, each carrying the
//! claim's `hub_token` ([`push_verdict`], run each turn by [`push_all`]). The
//! hub refuses `fenced` one under a token that is no longer the row's current
//! one (the lease expired or was taken since): the spoke then lets go of the
//! claim as for a lost lease, and the row stays as its new holder left it. What
//! was pushed is kept in the claim (`hub_pushed`, `<kind>:<commit>`), so a turn
//! sends each once.
//!
//! **The landing is a push** (wsp-468). A shared row lands through the
//! project's shared git remote (`remote:`, `origin` by default): the hold's
//! land pushes the trunk there fast-forward only, and a push that loses the
//! race fetches, rebases, reruns what the rebase can affect and tries again,
//! at most three more times, before the row is blocked naming the race
//! (`cmd_checkout::land_verdict`). The landed line then says `pushed to
//! <remote>`, and the landing object carries the commit on the remote
//! (`remote_commit`, beside `remote` and `project`). A spoke that imports a
//! `row.landed` object fetches its own clone's remote ([`fetch_landed`]), so
//! the commit it names is there to read.
//!
//! A spoke that is not logged in to a hub has no lease to take, and a private
//! row has none either: both are the standalone case and change nothing.
//!
//! **The drive lease** (wsp-467, decision d5). A shared list is advanced by one
//! spoke, the one holding the hub lease `drive:<list>` ([`drive`]), which only a
//! spoke of the list's allocated user, else of its publisher, takes; any other
//! spoke's run leaves the list as it is. The driver leases each member before it
//! starts it ([`lease_member`]): one another user's spoke holds is recorded as
//! running there ([`running_elsewhere`]), and the run neither starts, reseats nor
//! verifies it, its landing and verdict arriving with the row. The hub-sync
//! worker renews the drive while the daemon ticks ([`renew_drives`]); on
//! `lease.expired` it leaves the book and the next step takes it again.
//! `$WSP_STATE/hub/drive.json` ([`Book`]) keeps all three.
//!
//! The hub keys a lease on its holder's user, so two spokes of one user both
//! read as its holder: the drive is told apart between them only by which one
//! holds it in its book.

use std::path::Path;

use serde_json::{json, Value};
use crate::hub_client::{self, Client, Error as HubError};
use wsp_hub_protocol::{Blob, Code, Move, RefMove, FENCE_TOKEN};
use wsp_objects::{Object, ObjectId, WorkspaceId as HubWorkspace};
use wsp_store_contract::{At, EventKind, LeaseName, Principal, Refusal, RowId, Subject, Token};

use wsp_engine::store::Store;
use wsp_engine::util;

/// The claim record's field holding the hub's fencing token.
pub const TOKEN: &str = "hub_token";

/// What the daemon, which sits above the seats, lends this module: ending a
/// claim the way `wsp despawn --lost` does, and whether a seat is alive.
pub type Release = fn(&Store, &str, &str) -> Result<(), String>;
pub type Alive = fn(&Store, &str, &mut Option<Option<Vec<String>>>) -> bool;

static HOOKS: std::sync::OnceLock<(Release, Alive)> = std::sync::OnceLock::new();

/// Installs [`Release`] and [`Alive`]; the first install stands. Nothing is
/// renewed or followed in a process that has not (a command at a shell).
pub fn install(release: Release, alive: Alive) {
    let _ = HOOKS.set((release, alive));
}

/// A lease the hub granted: its token and when it runs out, in epoch seconds.
/// The type crosses to core, so it is the add-on interface's.
pub use wsp_engine::addon::Held;

fn who(p: &Principal) -> String {
    match p {
        Principal::Person(u) => u.to_string(),
        other => wsp_hub_protocol::principal::text(other),
    }
}

/// What a refused or failed lease verb says. `me` is who the connection is:
/// a lease the hub says `me` holds is held by this user on another of their
/// machines (the hub keys a lease by user and machine), which the hub's text
/// names ([`other_machine`]).
fn say(row: &str, e: HubError, me: &Principal) -> String {
    match e {
        HubError::Refused(Refusal::Held { holder, .. }, text) if holder == *me => match other_machine(&text) {
            Some(machine) => format!(
                "{row} is leased by you on your other machine {machine}, not this one: release it there, or let its lease run out"
            ),
            None => format!("{row} is leased by you on another machine of yours, not this one: {text}"),
        },
        HubError::Refused(Refusal::Held { holder, .. }, _) => {
            format!("{row} is leased by {}: their spoke holds it until it lets go or the lease runs out", who(&holder))
        }
        HubError::Refused(Refusal::Forbidden { .. }, _) => format!("{row} is forbidden to you: it is allocated to another user"),
        other => format!("the hub would not lease {row}: {other}"),
    }
}

/// The machine a hub's `held` text names for a lease its own user holds
/// elsewhere: the hub writes "… is held by <user> on the machine <name>, not
/// this one: …" (`wsp-hub` `leases.rs` `held_elsewhere`). None for any other
/// text (a hub that keys leases by user alone says no machine).
pub fn other_machine(text: &str) -> Option<String> {
    const ON: &str = " on the machine ";
    const NOT: &str = ", not this one:";
    let from = text.find(ON)? + ON.len();
    let to = text[from..].rfind(NOT)? + from;
    Some(text[from..to].to_string()).filter(|m| !m.is_empty())
}

/// `lease.take` for `row` in `workspace`, the refusals said in words.
pub fn take(client: &mut Client, workspace: &str, row: &str) -> Result<Held, String> {
    match client.lease_take(&HubWorkspace(workspace.to_string()), &RowId(row.to_string())) {
        Ok((token, at)) => Ok(Held { token, expires: (at.0 / 1000) as i64 }),
        Err(e) => {
            let me = client.by().clone();
            Err(say(row, e, &me))
        }
    }
}

/// Whether this machine is logged in to a hub (the login file or the env).
fn logged_in(home: &Path) -> bool {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    hub_client::hub_url_in(env(hub_client::HUB_URL_ENV), home).is_ok()
}

fn home() -> Option<std::path::PathBuf> {
    hub_client::hub_home()
}

/// The lease a claim on `row` (or a `wsp start`) needs, taken before anything
/// is written: `None` for a private row or a spoke with no login, `Err` for a
/// refusal (the claim is then not made) or a hub that does not answer. A lease
/// taken is kept in the book ([`Book::rows`]) with its token, whether or not a
/// claim will hold it: `wsp start` makes none, and a lease no record here
/// knows could never be given back.
pub fn before_claim(store: &Store, row: &str) -> Result<Option<Held>, String> {
    let held = take_for(store, row)?;
    if let Some(h) = &held {
        let now = util::epoch_secs();
        Book::edit(store, |b| {
            b.rows.insert(row.to_string(), Drive { token: h.token, expires: h.expires, ttl: (h.expires - now).max(0) });
        });
    }
    Ok(held)
}

fn take_for(store: &Store, row: &str) -> Result<Option<Held>, String> {
    let Some(t) = store.task(row) else { return Ok(None) };
    if t.workspace.is_private() {
        return Ok(None);
    }
    // Said here first, without a round trip (wsp-416).
    if let Some(user) = crate::cmd_allocate::allocated_elsewhere(store, &t.id) {
        return Err(crate::cmd_allocate::elsewhere_said(&t.id, &user));
    }
    let Some(home) = home().filter(|h| logged_in(h)) else { return Ok(None) };
    let mut client = crate::cmd_publish::dial(&home)?;
    take(&mut client, &t.workspace.to_string(), &t.id).map(Some)
}

/// Writes the lease into a claim record: the token, and the hub's expiry in
/// place of the local one.
pub fn stamp(claim: &mut Value, held: &Held) {
    let ttl = (held.expires - util::epoch_secs()).max(0);
    if let Some(o) = claim.as_object_mut() {
        o.insert(TOKEN.into(), json!(held.token.0));
        o.insert("expires".into(), json!(util::iso_at(held.expires)));
        o.insert("ttl_s".into(), json!(ttl));
    }
}

pub fn token_of(rec: &Value) -> Option<Token> {
    rec.get(TOKEN).and_then(|t| t.as_u64()).map(Token)
}

pub fn seat_of(rec: &Value) -> Option<&str> {
    ["workspace_id", "workspace", "workspace_label"]
        .iter()
        .find_map(|k| rec.get(k).and_then(|x| x.as_str()).filter(|s| !s.is_empty()))
}

/// The claim on `row` lets go: its lease goes back to the hub, best effort (a
/// lease the hub cannot be told of runs out on its own). Called as the claim
/// ends, so a row released by `despawn`, `done` or a hand-off is free to the
/// next user and not held for a week. The book forgets it.
pub fn let_go(store: &Store, row: &str, claim: &Value) {
    let Some(token) = token_of(claim) else { return };
    forget_row(store, row, token);
    let Some(home) = home().filter(|h| logged_in(h)) else { return };
    let Ok(mut client) = crate::cmd_publish::dial(&home) else { return };
    let _ = client.lease_release(&RowId(row.to_string()), token);
}

/// [`renew_due`] with the daemon's reading of which seats are alive.
pub fn renew_all(store: &Store, client: &mut Client, now: i64) -> Renewed {
    let Some((_, alive)) = HOOKS.get() else { return Renewed::default() };
    let mut seen = None;
    renew_due(store, client, now, &mut |seat| alive(store, seat, &mut seen))
}

/// What one renewal pass did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Renewed {
    /// Claims whose lease the hub renewed.
    pub renewed: usize,
    /// Rows whose lease the hub said was gone, and which were released.
    pub lost: Vec<String>,
}

/// The renewal pass: every claim with a hub lease, whose holder seat `alive`
/// says is alive and whose lease was last set over an hour ago (a short TTL:
/// half of it), is renewed on the hub, and its `expires` moves to the hub's.
/// A lease the hub says is gone or another's releases the claim.
pub fn renew_due(store: &Store, client: &mut Client, now: i64, alive: &mut dyn FnMut(&str) -> bool) -> Renewed {
    let mut out = Renewed::default();
    let due: Vec<(String, Token, String)> = store
        .claims()
        .iter()
        .filter_map(|(row, rec)| {
            let token = token_of(rec)?;
            let at = wsp_store::store::record_expires(rec)?;
            let ttl = rec.get("ttl_s").and_then(|t| t.as_i64()).filter(|t| *t > 0)?;
            let margin = ttl.min(3600).min(ttl / 2).max(0);
            (at - now < ttl - margin).then(|| (row.clone(), token, seat_of(rec).unwrap_or("").to_string()))
        })
        .collect();
    for (row, token, seat) in due {
        if seat.is_empty() || !alive(&seat) {
            continue;
        }
        match client.lease_renew(&RowId(row.clone()), token) {
            Ok((_, at)) => {
                if store.renew_claim(&row, (at.0 / 1000) as i64) {
                    out.renewed += 1;
                }
            }
            Err(HubError::Refused(r, text)) => {
                let why = format!("hub lease gone: renewing it was refused ({})", crate::hub_client::words::refusal(&r, &text));
                if drop_claim(store, &row, token, &why) {
                    out.lost.push(row);
                }
            }
            // The hub not answering is not the lease gone: try again next turn.
            Err(_) => {}
        }
    }
    out
}

/// The claim record's field holding what was last pushed for the row.
pub const PUSHED: &str = "hub_pushed";

/// Why a verdict or a landing did not reach the hub.
#[derive(Debug, PartialEq, Eq)]
pub enum PushError {
    /// The hub refused it `fenced`: the lease token is no longer the row's.
    Fenced(String),
    /// The hub did not take it, or did not answer: try again next turn.
    Failed(String),
}

fn failed(e: HubError) -> PushError {
    match e {
        HubError::Failed(f) if f.code == Code::Fenced => PushError::Fenced(f.text),
        other => PushError::Failed(other.to_string()),
    }
}

/// Pushes the verdict (`RowReviewed`) or the landing (`RowLanded`) of `row`,
/// `payload` its fields (`commit`, and for a pushed landing the remote's), under `token`: the object, then the move of the row's
/// lease subject from the head the hub has (a stale move is read again, three
/// times at most).
pub fn push_verdict(
    client: &mut Client,
    workspace: &str,
    row: &str,
    kind: EventKind,
    payload: &Value,
    token: Token,
) -> Result<(), PushError> {
    let (wid, id) = (HubWorkspace(workspace.to_string()), RowId(row.to_string()));
    let subject = fence_subject(&id);
    for _ in 0..3 {
        let from = client.sync_heads(&wid).map_err(failed)?.get(&subject).copied();
        let at = At((util::epoch_secs().max(0) as u64) * 1000);
        let object = fenced_event(wid.clone(), &id, kind, payload.clone(), token, from, client.by().clone(), at);
        let (to, bytes) = wsp_objects::encode(&object);
        client.sync_put(vec![Blob { id: to, bytes }]).map_err(failed)?;
        match client.refs_move(RefMove { workspace: wid.clone(), subject: subject.clone(), from, to }).map_err(failed)? {
            Move::Moved { .. } => return Ok(()),
            Move::Stale { .. } => continue,
        }
    }
    Err(PushError::Failed(format!("{row}: the hub's head kept moving")))
}

/// The subject a row's verdicts and landings are chained on: the row's lease.
/// (The hub holds a verdict or a landing to the row's current lease token.)
pub fn fence_subject(row: &RowId) -> Subject {
    Subject::Lease(LeaseName(format!("row:{}", row.0)))
}

/// A verdict (`row.reviewed`) or a landing (`row.landed`) of `row` as an
/// object under the lease `token`: `payload`'s fields (the verdict, the commit
/// on the remote) with the token added. `parent` is the head of
/// [`fence_subject`] the writer saw.
#[allow(clippy::too_many_arguments)]
pub fn fenced_event(
    workspace: HubWorkspace,
    row: &RowId,
    kind: EventKind,
    payload: Value,
    token: Token,
    parent: Option<ObjectId>,
    by: Principal,
    at: At,
) -> Object {
    let mut payload = match payload {
        Value::Object(o) => Value::Object(o),
        Value::Null => json!({}),
        other => json!({ "value": other }),
    };
    payload[FENCE_TOKEN] = json!(token.0);
    Object::Event { workspace, subject: fence_subject(row), kind, payload, parent, by, at }
}

/// What a pass of [`push_all`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pushed {
    /// Verdicts and landings the hub took.
    pub sent: usize,
    /// Rows whose token the hub refused `fenced`, and which were released.
    pub fenced: Vec<String>,
}

/// The verdict or landing each claim with a hub lease is owed to the hub, and
/// has not had: a landed line names the landing, else a ready line the verdict.
pub fn push_all(store: &Store, client: &mut Client) -> Pushed {
    let mut out = Pushed::default();
    for (row, claim) in store.claims() {
        let Some(token) = token_of(&claim) else { continue };
        let Some(t) = store.task(&row).filter(|t| !t.workspace.is_private()) else { continue };
        // The verifier's hold on a member this spoke holds goes first (wsp-486).
        if let Some((commit, key, word)) = held_verdict(&t, &claim) {
            let pushed = |c: &Value| c.get("hub_verdict").and_then(|p| p.as_str()) == Some(key.as_str());
            if !pushed(&claim) {
                let payload = json!({ "commit": commit, "verdict": word });
                if push_verdict(client, &t.workspace.to_string(), &row, EventKind::RowReviewed, &payload, token).is_ok() {
                    if let Some(mut now) = store.claims().get(&row).cloned().filter(|c| token_of(c) == Some(token)) {
                        now["hub_verdict"] = json!(key);
                        store.set_claim(&row, now);
                    }
                    out.sent += 1;
                }
            }
        }
        let (kind, commit) = match (wsp_engine::repair::landed_line(&t), wsp_engine::repair::ready(&t)) {
            (Some(sha), _) => (EventKind::RowLanded, sha),
            (None, Some(sha)) => (EventKind::RowReviewed, sha),
            (None, None) => continue,
        };
        let key = format!("{}:{commit}", if kind == EventKind::RowLanded { "landed" } else { "reviewed" });
        if claim.get(PUSHED).and_then(|p| p.as_str()) == Some(key.as_str()) {
            continue;
        }
        match push_verdict(client, &t.workspace.to_string(), &row, kind, &payload_of(&t, kind, &commit), token) {
            Ok(()) => {
                // The claim as it is now, in case it moved while the hub answered.
                if let Some(mut now) = store.claims().get(&row).cloned().filter(|c| token_of(c) == Some(token)) {
                    now[PUSHED] = json!(key);
                    store.set_claim(&row, now);
                }
                out.sent += 1;
            }
            Err(PushError::Fenced(text)) => {
                let why = format!("hub lease lost: a {} was refused fenced ({text})", key.split(':').next().unwrap_or("verdict"));
                if drop_claim(store, &row, token, &why) {
                    out.fenced.push(row);
                }
            }
            // The hub not answering is not a verdict lost: try again next turn.
            Err(PushError::Failed(why)) => wsp_store::store::log_line(store, &format!("hub lease: {row}: {why}")),
        }
    }
    out
}

/// The verdict of a member this spoke holds a verifier's pass for, owed to the
/// hub and not yet sent (wsp-486): `(commit, key, word)` for the newest pass
/// holding, or blocking a member whose earlier hold was pushed (it overrides it).
fn held_verdict(t: &wsp_engine::model::Task, claim: &Value) -> Option<(String, String, &'static str)> {
    let tasks = [t.clone()];
    let commit = wsp_engine::repair::commit_of(t)?;
    if wsp_engine::verification::verified(&tasks, &t.id) {
        return Some((commit.clone(), format!("verdict:{commit}"), "holds"));
    }
    let earlier = claim.get("hub_verdict").and_then(|p| p.as_str()).is_some_and(|k| k.starts_with("verdict:") && !k.ends_with(":blocks"));
    if earlier && wsp_engine::verification::blocked(&tasks, &t.id).is_some() {
        return Some((commit.clone(), format!("verdict:{commit}:blocks"), "blocks"));
    }
    None
}

/// Whether the barrier may count `row` verified by the verdict its holder's
/// spoke pushed (wsp-486): only for a member held elsewhere.
pub fn verdict_held(store: &Store, row: &str) -> bool {
    let book = Book::read(store);
    if !book.elsewhere.contains_key(row) {
        return false;
    }
    let Some(verdict) = book.verdicts.get(row) else { return false };
    // A verdict is for the commit it names: a member whose current commit
    // moved on since is not verified by it.
    match store.task(row).and_then(|t| wsp_engine::repair::commit_of(&t)) {
        Some(now) => now.starts_with(verdict.as_str()) || verdict.starts_with(now.as_str()),
        None => true,
    }
}

/// Takes in a `row.reviewed` object read from the hub's line for `row`: one
/// carrying `verdict: holds` is the holder's verifier's verdict (wsp-486).
pub fn take_verdict(store: &Store, row: &str, payload: &Value) {
    if payload["verdict"].as_str() == Some("blocks") {
        Book::edit(store, |b| b.verdicts.remove(row));
        return;
    }
    if payload["verdict"].as_str() != Some("holds") {
        return;
    }
    let Some(commit) = payload["commit"].as_str() else { return };
    Book::edit(store, |b| b.verdicts.insert(row.to_string(), commit.to_string()));
}

/// What a verdict or landing object of `t` says: the `commit`; for a landing,
/// the row's `project`, and where the landed line says it was pushed, the
/// `remote` and the commit on it (`remote_commit`, the same commit: the push
/// is fast-forward only).
fn payload_of(t: &wsp_engine::model::Task, kind: EventKind, commit: &str) -> Value {
    let mut payload = json!({ "commit": commit });
    if kind != EventKind::RowLanded {
        return payload;
    }
    if let Some(project) = wsp_engine::model::project_of(t) {
        payload["project"] = json!(project);
    }
    if let Some(remote) = pushed_to(t) {
        payload["remote"] = json!(remote);
        payload["remote_commit"] = json!(commit);
    }
    payload
}

/// The remote the newest landed line of `t` names (`· pushed to <remote>`).
fn pushed_to(t: &wsp_engine::model::Task) -> Option<String> {
    let log = t.section("Log")?;
    let line = log.lines().filter(|l| l.contains(wsp_store::log::LANDED)).last()?;
    let (_, after) = line.split_once(&format!(" · {} ", wsp_engine::cmd_checkout::PUSHED_TO))?;
    after.split_whitespace().next().map(str::to_string)
}

/// A `row.landed` object this spoke imports, `payload` its fields: where the
/// landing was pushed (it names a `remote_commit`), each root of the row's
/// project here fetches the project's own remote, as its clone names it. The
/// root that fetched, or `None` (a landing never pushed, a project this spoke
/// does not have, a fetch that failed: said in `cycle.log`).
pub fn fetch_landed(store: &Store, payload: &Value) -> Option<std::path::PathBuf> {
    let commit = payload.get("remote_commit")?.as_str()?;
    let project = payload.get("project").and_then(Value::as_str).and_then(|p| store.project(p))?;
    let remote = project.remote().to_string();
    for root in &project.roots {
        let dir = util::expand(root);
        if !dir.join(".git").exists() {
            continue;
        }
        match wsp_engine::cmd_verify::git_ok(&dir, &["fetch", "--quiet", &remote]) {
            Ok(()) => return Some(dir),
            Err(e) => wsp_store::store::log_line(
                store,
                &format!("hub-sync: a landing of {commit} came in and {} did not fetch {remote}: {e}", util::contract(&dir)),
            ),
        }
    }
    None
}

/// The spoke that held a lease lets go of its claim: what `wsp despawn --lost`
/// does, without telling the hub (it released the lease). Only a claim whose
/// `hub_token` is `token` is touched.
fn drop_claim(store: &Store, row: &str, token: Token, why: &str) -> bool {
    let Some((release, _)) = HOOKS.get() else { return false };
    let Some(mut claim) = store.claims().get(row).cloned() else { return false };
    if token_of(&claim) != Some(token) {
        return false;
    }
    if let Some(o) = claim.as_object_mut() {
        o.remove(TOKEN);
    }
    store.set_claim(row, claim);
    match release(store, row, why) {
        Ok(()) => {
            wsp_engine::verification::tell_governor(store, row, &format!("{row}: {why}. It is released and back to todo; the last checkpoint is in the seat's worktree."));
            true
        }
        Err(e) => {
            wsp_store::store::log_line(store, &format!("hub lease: {row}: {e}"));
            false
        }
    }
}

/// A `lease.expired` the hub announced, `payload` its `{workspace,row,holder,token}`:
/// where the claim here holds that token it is released. Whether it was. The
/// book forgets the row lease either way.
pub fn follow_expired(store: &Store, payload: &Value) -> bool {
    follow_expired_by(store, payload, None)
}

/// What the holder's spoke does with a lease event of the stream: a
/// `lease.released` by an admin (cause `admin`, `released_by`) is noted in
/// `admin_released` by row and token, and the `lease.expired` that follows
/// it (cause `admin_released`), as any `lease.expired`, lets the claim go
/// ([`follow_expired_by`]), naming the admin. Whether a claim was let go.
pub fn follow_lease_event(store: &Store, kind: EventKind, payload: &Value, admin_released: &mut std::collections::BTreeMap<(String, u64), String>) -> bool {
    let key = || (payload["row"].as_str().unwrap_or_default().to_string(), payload["token"].as_u64().unwrap_or_default());
    match kind {
        EventKind::LeaseReleased if payload["cause"] == "admin" => {
            if let Some(by) = payload["released_by"].as_str() {
                admin_released.insert(key(), by.to_string());
            }
            false
        }
        EventKind::LeaseExpired => {
            let by = admin_released.remove(&key());
            follow_expired_by(store, payload, by.as_deref())
        }
        _ => false,
    }
}

/// [`follow_expired`], for a `lease.expired` that may follow an admin's
/// release (cause `admin_released`): `released_by` is the admin the
/// `lease.released` before it named (`person:ann`). The claim is let go
/// exactly as on an expiry; the line it leaves names the admin instead.
pub fn follow_expired_by(store: &Store, payload: &Value, released_by: Option<&str>) -> bool {
    let (Some(row), Some(token)) = (payload["row"].as_str(), payload["token"].as_u64()) else { return false };
    let admin = (payload["cause"] == "admin_released").then(|| {
        let by = released_by.unwrap_or("an admin");
        by.strip_prefix("person:").map(|u| format!("the hub admin {u}")).unwrap_or_else(|| by.to_string())
    });
    if let Some(list) = row.strip_prefix(DRIVE) {
        let why = match &admin {
            Some(by) => format!("{by} released it on the hub (token {token})"),
            None => format!("it expired on the hub (token {token})"),
        };
        return drop_drive(store, list, Token(token), &why);
    }
    forget_row(store, row, Token(token));
    let why = match &admin {
        Some(by) => format!("lease released on the hub by {by}: {row}, token {token}"),
        None => format!("lease expired on the hub: {row}, token {token}"),
    };
    drop_claim(store, row, Token(token), &why)
}

// ---- the drive lease (wsp-467) ----------------------------------------------------

/// The hub lease a shared list's driver holds: `drive:<list>`.
pub fn drive_row(list: &str) -> String {
    format!("{DRIVE}{list}")
}

const DRIVE: &str = "drive:";

/// A drive lease this spoke holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Drive {
    pub token: Token,
    /// When the hub said it runs out, in epoch seconds.
    pub expires: i64,
    /// The TTL it was taken or last renewed with, for the renewal's margin.
    pub ttl: i64,
}

/// `$WSP_STATE/hub/drive.json`: the drive leases this spoke holds, the members
/// the hub said another user's spoke holds, and who this spoke last read as
/// each shared list's driver (so a change is said once, not every tick).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Book {
    pub drives: std::collections::BTreeMap<String, Drive>,
    /// Row → the user whose spoke holds its lease.
    pub elsewhere: std::collections::BTreeMap<String, String>,
    /// List → the user it was last read as driven by.
    pub driven_by: std::collections::BTreeMap<String, String>,
    /// Row → the commit a verifier held, as the spoke holding the row pushed it
    /// (wsp-486): what the driver's barrier reads for a member held elsewhere.
    pub verdicts: std::collections::BTreeMap<String, String>,
    /// Row → every row lease this spoke took ([`before_claim`]), held by a
    /// claim or not (`wsp start` makes none): what disable gives back and
    /// status lists. A claim's lease is renewed with the claim; one no claim
    /// holds is not renewed (nothing here keeps a seat on it) and runs its
    /// TTL, unless the work ends first ([`release_finished`]).
    pub rows: std::collections::BTreeMap<String, Drive>,
}

impl Book {
    fn path(store: &Store) -> std::path::PathBuf {
        store.state.join("hub").join("drive.json")
    }

    pub fn read(store: &Store) -> Book {
        let text = std::fs::read_to_string(Book::path(store)).unwrap_or_default();
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let strings = |k: &str| {
            v[k].as_object()
                .into_iter()
                .flatten()
                .filter_map(|(r, u)| Some((r.clone(), u.as_str()?.to_string())))
                .collect()
        };
        let leases = |k: &str| {
            v[k].as_object()
                .into_iter()
                .flatten()
                .filter_map(|(l, d)| {
                    Some((l.clone(), Drive { token: Token(d["token"].as_u64()?), expires: d["expires"].as_i64()?, ttl: d["ttl"].as_i64().unwrap_or(0) }))
                })
                .collect()
        };
        Book { drives: leases("drives"), elsewhere: strings("elsewhere"), driven_by: strings("driven_by"), verdicts: strings("verdicts"), rows: leases("rows") }
    }

    fn write(&self, store: &Store) {
        let leases = |m: &std::collections::BTreeMap<String, Drive>| -> serde_json::Map<String, Value> {
            m.iter().map(|(l, d)| (l.clone(), json!({ "token": d.token.0, "expires": d.expires, "ttl": d.ttl }))).collect()
        };
        let mut v = json!({ "drives": leases(&self.drives), "elsewhere": self.elsewhere, "driven_by": self.driven_by, "verdicts": self.verdicts });
        if !self.rows.is_empty() {
            v["rows"] = Value::Object(leases(&self.rows));
        }
        let path = Book::path(store);
        let Some(dir) = path.parent() else { return };
        let _ = std::fs::create_dir_all(dir);
        let tmp = dir.join(format!(".drive.json.{}.tmp", std::process::id()));
        if std::fs::write(&tmp, v.to_string()).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    /// Read, change and write it back under the store's lock; what `f` answered.
    pub fn edit<T>(store: &Store, f: impl FnOnce(&mut Book) -> T) -> T {
        store.locked(|| {
            let mut book = Book::read(store);
            let before = book.clone();
            let out = f(&mut book);
            if book != before {
                book.write(store);
            }
            out
        })
    }
}

/// What the gate before advancing a shared list answers: `Here` (this spoke
/// holds the drive lease, the list is private, or there is no hub),
/// `Elsewhere(user)` or `NotYet(why)`. The add-on interface's type.
pub use wsp_engine::addon::Drives;

/// The user a shared list's drive lease belongs to: its allocated user, else
/// its publisher (the first record of it this spoke holds), else `None` (a
/// list published here and not yet synced is this spoke's user's).
pub fn driver_of(store: &Store, w: &wsp_engine::model::Worklist) -> Option<String> {
    w.for_user.clone().or_else(|| crate::hub_sync::publisher(store, &w.id))
}

/// The gate before the run advances `w` (wsp-467, decision d5): a shared list
/// moves only on the spoke holding `drive:<list>`, which only a spoke of
/// [`driver_of`]'s user takes. One held and unexpired here is used as it is
/// (the hub-sync worker renews it); otherwise it is taken now.
pub fn drive(store: &Store, w: &wsp_engine::model::Worklist) -> Drives {
    if w.workspace.is_private() {
        return Drives::Here;
    }
    let Some(home) = home().filter(|h| logged_in(h)) else { return Drives::Here };
    let Some(me) = wsp_engine::address::logged_in_user() else { return Drives::Here };
    let driver = driver_of(store, w).unwrap_or_else(|| me.clone());
    if driver != me {
        return Drives::Elsewhere(driver);
    }
    let now = util::epoch_secs();
    if Book::read(store).drives.get(&w.id).is_some_and(|d| d.expires > now) {
        return Drives::Here;
    }
    let mut client = match crate::cmd_publish::dial(&home) {
        Ok(c) => c,
        Err(e) => return Drives::NotYet(e),
    };
    // The hub's name: a list kept here under a name of its own goes by its
    // hub id there (`hub_lists`).
    let row = drive_row(&crate::hub_lists::hub_id(store, &w.id));
    match client.lease_take(&HubWorkspace(w.workspace.to_string()), &RowId(row.clone())) {
        Ok((token, at)) => {
            let expires = (at.0 / 1000) as i64;
            Book::edit(store, |b| {
                b.drives.insert(w.id.clone(), Drive { token, expires, ttl: (expires - now).max(0) });
            });
            Drives::Here
        }
        Err(HubError::Refused(Refusal::Held { holder, .. }, _)) => Drives::Elsewhere(who(&holder)),
        Err(HubError::Refused(Refusal::Forbidden { .. }, _)) => Drives::Elsewhere(driver),
        Err(e) => Drives::NotYet(format!("the hub would not lease {row}: {e}")),
    }
}

/// What the run does about one member of a list it drives, before it starts
/// it: `Free` (a private row, or no hub), `Leased` (the spawn's claim takes it
/// again; the same holder keeps its token), `Elsewhere(user)` (it runs there,
/// and the barrier waits for its landing) or `NotYet(why)`. The add-on
/// interface's type.
pub use wsp_engine::addon::Member;

/// The driver leases `row` before spawning it (wsp-467). A refusal `held`
/// records the holder, so the run neither starts, reseats nor verifies the
/// member here ([`running_elsewhere`]).
pub fn lease_member(store: &Store, row: &str) -> Member {
    let Some(t) = store.task(row) else { return Member::Free };
    if t.workspace.is_private() {
        return Member::Free;
    }
    let Some(home) = home().filter(|h| logged_in(h)) else { return Member::Free };
    let mut client = match crate::cmd_publish::dial(&home) {
        Ok(c) => c,
        Err(e) => return Member::NotYet(e),
    };
    match client.lease_take(&HubWorkspace(t.workspace.to_string()), &RowId(row.to_string())) {
        Ok((token, at)) => {
            Book::edit(store, |b| b.elsewhere.remove(row));
            Member::Leased(Held { token, expires: (at.0 / 1000) as i64 })
        }
        Err(HubError::Refused(Refusal::Held { holder, .. }, _)) => {
            let user = who(&holder);
            Book::edit(store, |b| b.elsewhere.insert(row.to_string(), user.clone()));
            Member::Elsewhere(user)
        }
        Err(HubError::Refused(Refusal::Forbidden { .. }, _)) => {
            Member::Elsewhere(crate::cmd_allocate::allocated_to(store, row).unwrap_or_else(|| "another user".into()))
        }
        Err(e) => {
            let me = client.by().clone();
            Member::NotYet(say(row, e, &me))
        }
    }
}

/// A member lease the run took and did not use (no start, or a start that
/// made no claim) goes back to the hub at once, not in a week.
pub fn unused(store: &Store, row: &str, held: &Held) {
    if store.claims().contains_key(row) {
        return;
    }
    let Some(home) = home().filter(|h| logged_in(h)) else { return };
    let Ok(mut client) = crate::cmd_publish::dial(&home) else { return };
    let _ = client.lease_release(&RowId(row.to_string()), held.token);
}

/// The user whose spoke the hub last said holds `row`, when it is not yet
/// landed: the member-held branch of the run, and `worklist show`'s
/// `running on <user>'s wsp`.
pub fn running_elsewhere(store: &Store, row: &str) -> Option<String> {
    let user = Book::read(store).elsewhere.get(row).cloned()?;
    let done = store.find_task(row).is_none_or(|t| t.status() == wsp_engine::model::Status::Done);
    (!done).then_some(user)
}

/// Who `worklist show` says drives `w`: the user this spoke last read it as
/// driven by, else the one whose spoke may. `None` for a private list.
pub fn driven_by(store: &Store, w: &wsp_engine::model::Worklist) -> Option<String> {
    if w.workspace.is_private() {
        return None;
    }
    Book::read(store)
        .driven_by
        .get(&w.id)
        .cloned()
        .or_else(|| driver_of(store, w))
        .or_else(wsp_engine::address::logged_in_user)
}

/// The drive leases this spoke holds, renewed while the daemon ticks: each on
/// [`renew_due`]'s margin. A list no longer running here (done, gone, made
/// private) lets its lease go; one the hub refuses is dropped from the book,
/// so the next step takes it again or reads who has it.
pub fn renew_drives(store: &Store, client: &mut Client, now: i64) -> usize {
    let mut renewed = 0;
    for (list, d) in Book::read(store).drives {
        let row = RowId(drive_row(&crate::hub_lists::hub_id(store, &list)));
        let running = store
            .worklist(&list)
            .is_some_and(|w| !w.workspace.is_private() && w.status() != wsp_engine::model::WorklistStatus::Done);
        if !running {
            let _ = client.lease_release(&row, d.token);
            Book::edit(store, |b| b.drives.remove(&list));
            continue;
        }
        let margin = d.ttl.min(3600).min(d.ttl / 2).max(0);
        if d.ttl <= 0 || d.expires - now >= d.ttl - margin {
            continue;
        }
        match client.lease_renew(&row, d.token) {
            Ok((_, at)) => {
                let expires = (at.0 / 1000) as i64;
                Book::edit(store, |b| {
                    if let Some(x) = b.drives.get_mut(&list).filter(|x| x.token == d.token) {
                        x.expires = expires;
                        x.ttl = (expires - now).max(0);
                    }
                });
                renewed += 1;
            }
            Err(HubError::Refused(r, text)) => {
                drop_drive(store, &list, d.token, &format!("renewing it was refused ({})", crate::hub_client::words::refusal(&r, &text)));
            }
            Err(_) => {}
        }
    }
    renewed
}

/// The drive lease on `list` with `token` is gone: out of the book, said once.
fn drop_drive(store: &Store, list: &str, token: Token, why: &str) -> bool {
    let dropped = Book::edit(store, |b| match b.drives.get(list) {
        Some(d) if d.token == token => {
            b.drives.remove(list);
            b.driven_by.remove(list);
            true
        }
        _ => false,
    });
    if dropped {
        wsp_store::store::log_line(store, &format!("{list}: the drive lease is gone: {why}; another spoke of its user may take it"));
    }
    dropped
}

// ---- row leases no claim holds -----------------------------------------------

/// The book forgets `row`'s lease, where it is the one with `token`.
pub fn forget_row(store: &Store, row: &str, token: Token) {
    Book::edit(store, |b| {
        if b.rows.get(row).is_some_and(|d| d.token == token) {
            b.rows.remove(row);
        }
    });
}

/// Whether `row`'s work here is over as far as its lease goes: done, parked,
/// put back (`todo`, `inbox`) or gone.
fn finished(store: &Store, row: &str) -> bool {
    store.find_task(row).is_none_or(|t| {
        matches!(t.status(), wsp_engine::model::Status::Done | wsp_engine::model::Status::Parked | wsp_engine::model::Status::Todo | wsp_engine::model::Status::Inbox)
    })
}

/// The row leases in the book that no claim holds and whose work is over
/// (`done`, `park`, a landing, a row put back): what [`release_finished`] gives
/// back. A claim's lease goes back when its claim ends ([`let_go`]), as before.
pub fn finished_unheld(store: &Store) -> Vec<(String, Token)> {
    let claims = store.claims();
    Book::read(store)
        .rows
        .into_iter()
        .filter(|(row, d)| claims.get(row).and_then(token_of) != Some(d.token))
        .filter(|(row, _)| finished(store, row))
        .map(|(row, d)| (row, d.token))
        .collect()
}

/// Gives back each lease [`finished_unheld`] names (a lease the hub says is
/// gone counts as given back) and forgets it; how many. Run by the sync
/// worker each turn, and by the spoke as a verb moves a row's status.
pub fn release_finished(store: &Store, client: &mut Client) -> usize {
    let mut n = 0;
    for (row, token) in finished_unheld(store) {
        match client.lease_release(&RowId(row.clone()), token) {
            Ok(()) | Err(HubError::Refused(..)) => {
                forget_row(store, &row, token);
                n += 1;
            }
            Err(_) => {}
        }
    }
    n
}

/// [`release_finished`] for `row` alone, dialling the login's hub: a verb
/// moved its status. Nothing is dialled when there is nothing to give back.
pub fn release_if_finished(store: &Store, row: &str) {
    let Some((_, token)) = finished_unheld(store).into_iter().find(|(r, _)| r == row) else { return };
    let Some(home) = home().filter(|h| logged_in(h)) else { return };
    let Ok(mut client) = crate::cmd_publish::dial(&home) else { return };
    if matches!(client.lease_release(&RowId(row.to_string()), token), Ok(()) | Err(HubError::Refused(..))) {
        forget_row(store, row, token);
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::time::Duration;
    use crate::hub_client::{Client, Item};
    use crate::testhub::{Setup, TestHub};
    use wsp_store_contract::{At, Entity, EventKind, ProjectId, Row, RowState};

    const WAIT: Duration = Duration::from_secs(10);

    /// The lease TTL of a hub whose leases a test lets run out, in real time.
    pub const SHORT_LEASE: u64 = 4;

    /// A hub (no TLS, loopback) with `ann`, `bo` members of `shared:team`;
    /// `None` when no hub is named (the test is skipped, `testhub`).
    fn serve(setup: Setup) -> Option<(TestHub, BTreeMap<String, String>)> {
        serve_users(setup, &["ann", "bo"])
    }

    /// A hub with `users` members of `shared:team`, and their tokens.
    pub fn serve_users(setup: Setup, users: &[&str]) -> Option<(TestHub, BTreeMap<String, String>)> {
        let hub = TestHub::start(setup)?;
        let tokens = hub.seed_members(&["team"], users);
        Some((hub, tokens))
    }

    pub fn dial(hub: &TestHub, tokens: &BTreeMap<String, String>, who: &str) -> Client {
        hub.client(&tokens[who])
    }

    /// A double for the root's [`Release`]: what a spoke is left with once a
    /// seat is lost. Seats' own release is above the engine.
    pub fn lose(store: &Store, row: &str, why: &str) -> Result<(), String> {
        for seat in store.panes_for_task(row) {
            store.clear_binding(&seat);
        }
        store.clear_claim(row);
        let mut t = store.task(row).ok_or_else(|| format!("no row {row}"))?;
        t.set_status(wsp_engine::model::Status::Todo);
        t.log(why);
        store.save_task(&t).map_err(|e| e.to_string())
    }

    fn shared_row(store: &Store, id: &str) {
        let mut t = wsp_store::model::Task::new(id, id);
        t.status_raw = "doing".into();
        t.workspace = "shared:team".parse().unwrap();
        store.save_task(&t).unwrap();
    }

    /// An admin's release (the hub's b5c19f9) reaches the holder's spoke as
    /// `lease.released` (cause `admin`, `released_by`) then `lease.expired`
    /// (cause `admin_released`): the spoke lets its claim go exactly as on an
    /// expiry, and the line it leaves names the admin, not an expiry.
    #[test]
    fn hub_lease_an_admins_release_lets_the_holders_claim_go_as_an_expiry_naming_the_admin() {
        install(lose, |_, _, _| false);
        let _env = wsp_engine::util::isolated("hub-lease-admin-release");
        std::env::remove_var("WSP_CLAIM_TTL");
        let Some((running, tokens)) = serve(Setup::default()) else { return };
        // The hub's admin `root` is a member of team (`seed`), so it may release there.
        let mut ann = dial(&running, &tokens, "ann");
        let mut feed = dial(&running, &tokens, "ann").subscribe(None);
        assert!(matches!(feed.next_within(WAIT), Some(Item::State { .. })));
        let store = Store::open();
        store.ensure_dirs().unwrap();
        shared_row(&store, "wsp-1");
        let held = take(&mut ann, "shared:team", "wsp-1").unwrap();
        let mut claim = json!({ "workspace_id": "cpd-a", "claimed_at": util::now_iso() });
        stamp(&mut claim, &held);
        store.set_claim("wsp-1", claim);
        store.set_binding("cpd-a", json!({ "task_id": "wsp-1", "pane_id": "cpd-a" }));

        running.admin().lease_release_admin(&wsp_objects::WorkspaceId("shared:team".into()), &RowId("wsp-1".into()), held.token).unwrap();
        let mut noted = BTreeMap::new();
        let mut heard = Vec::new();
        let dropped = loop {
            match feed.next_within(WAIT) {
                Some(Item::Event(e)) if matches!(e.kind, EventKind::LeaseReleased | EventKind::LeaseExpired) => {
                    heard.push((e.kind, e.payload["cause"].clone()));
                    let let_go = follow_lease_event(&store, e.kind, &e.payload, &mut noted);
                    if e.kind == EventKind::LeaseExpired {
                        break let_go;
                    }
                }
                Some(_) => continue,
                None => panic!("no lease.expired on the stream after the admin's release: {heard:?}"),
            }
        };
        assert_eq!(heard, vec![(EventKind::LeaseReleased, json!("admin")), (EventKind::LeaseExpired, json!("admin_released"))]);
        assert!(dropped, "the claim was let go");
        assert!(!store.claims().contains_key("wsp-1"), "the claim is released, as on an expiry");
        assert!(store.bindings().get("cpd-a").is_none());
        let row = store.task("wsp-1").unwrap();
        assert_eq!(row.status(), wsp_engine::model::Status::Todo);
        let log = row.section("Log").unwrap_or_default();
        assert!(log.contains(&format!("lease released on the hub by the hub admin root: wsp-1, token {}", held.token.0)), "{log}");
        assert!(!log.contains("lease expired on the hub"), "not called an expiry: {log}");
        assert!(noted.is_empty(), "what the release noted is used up");
    }

    /// Done-when 1 (wsp-466): A leases a row; B's claim on it is refused naming
    /// A's user; A's live seat renews, a dead one does not; with the hub's clock
    /// past the TTL the lease is released and announced, A's spoke releases as
    /// `despawn --lost` does, and B takes it with a higher token.
    #[test]
    fn hub_lease_two_spokes_a_held_row_is_refused_renewed_while_alive_and_follows_the_hub_on_expiry() {
        install(lose, |_, _, _| false);
        let _env = wsp_engine::util::isolated("hub-lease-two");
        std::env::remove_var("WSP_CLAIM_TTL");
        let dir = std::env::temp_dir().join(format!("wsp-466-two-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let Some((running, tokens)) = serve(Setup::default().lease_secs(SHORT_LEASE)) else { return };
        let (mut ann, mut bo) = (dial(&running, &tokens, "ann"), dial(&running, &tokens, "bo"));
        let mut feed = dial(&running, &tokens, "bo").subscribe(None);
        assert!(matches!(feed.next_within(WAIT), Some(Item::State { .. })));

        // A's spoke: a shared row in doing, its claim stamped with the lease.
        let store = Store::open();
        store.ensure_dirs().unwrap();
        shared_row(&store, "wsp-1");
        let held = take(&mut ann, "shared:team", "wsp-1").unwrap();
        let mut claim = json!({ "workspace_id": "cpd-a", "claimed_at": util::now_iso() });
        stamp(&mut claim, &held);
        store.set_claim("wsp-1", claim);
        store.set_binding("cpd-a", json!({ "task_id": "wsp-1", "pane_id": "cpd-a" }));
        let stamped = store.claims()["wsp-1"].clone();
        assert_eq!(stamped[TOKEN], held.token.0);
        assert_eq!(wsp_store::store::record_expires(&stamped), Some(held.expires), "the hub's expiry is the claim's");

        // B's claim is refused, naming A's user.
        let why = take(&mut bo, "shared:team", "wsp-1").unwrap_err();
        assert!(why.contains("wsp-1") && why.contains("ann"), "{why}");

        // Not due yet: nothing is sent. Past half the TTL, a dead seat is not renewed...
        let now = util::epoch_secs();
        assert_eq!(renew_due(&store, &mut ann, now, &mut |_| true), Renewed::default(), "half the TTL has not passed");
        let later = now + SHORT_LEASE as i64;
        assert_eq!(renew_due(&store, &mut ann, later, &mut |_| false), Renewed::default(), "a dead seat is not renewed");
        // ...a live one is, and the claim's expiry moves to the hub's (two seconds on, in real time).
        let before = wsp_store::store::record_expires(&store.claims()["wsp-1"]).unwrap();
        std::thread::sleep(Duration::from_secs(2));
        assert_eq!(renew_due(&store, &mut ann, later, &mut |seat| seat == "cpd-a").renewed, 1);
        let after = wsp_store::store::record_expires(&store.claims()["wsp-1"]).unwrap();
        assert!(after >= before + 1, "{after} after {before}");
        assert_eq!(store.claims()["wsp-1"][TOKEN], held.token.0, "the token is the same one");

        // A's seat is killed and not renewed; the TTL passes and the hub sweeps it.
        running.pass_ttl();
        let heard = loop {
            match feed.next_within(WAIT) {
                Some(Item::Event(e)) if e.kind == EventKind::LeaseExpired => break e,
                Some(_) => continue,
                None => panic!("no lease.expired on the stream"),
            }
        };
        assert_eq!(heard.payload["row"], "wsp-1");
        assert_eq!(heard.payload["token"], held.token.0);

        // A's spoke follows: the claim is gone, the row todo, one line says why.
        assert!(follow_expired(&store, &heard.payload));
        assert!(!store.claims().contains_key("wsp-1"), "the claim is released");
        assert!(store.bindings().get("cpd-a").is_none());
        let row = store.task("wsp-1").unwrap();
        assert_eq!(row.status(), wsp_engine::model::Status::Todo);
        let log = row.section("Log").unwrap_or_default();
        assert_eq!(log.lines().filter(|l| l.contains("lease expired on the hub")).count(), 1, "{log}");
        assert!(!follow_expired(&store, &heard.payload), "once");

        // B takes it with a higher token.
        let theirs = take(&mut bo, "shared:team", "wsp-1").unwrap();
        assert!(theirs.token > held.token, "{theirs:?} after {held:?}");

        drop(running);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// wsp-468: A's ready line and landing line reach the hub as objects under
    /// her token, once each; her lease runs out and B takes the row; A's next
    /// landing is refused `fenced`, her spoke lets go of the claim, and the
    /// hub's head for the row is what B left.
    #[test]
    fn hub_fence_a_verdict_and_a_landing_go_under_the_token_and_a_stale_one_is_refused_fenced() {
        install(lose, |_, _, _| false);
        let _env = wsp_engine::util::isolated("hub-fence");
        std::env::remove_var("WSP_CLAIM_TTL");
        let dir = std::env::temp_dir().join(format!("wsp-468-fence-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let Some((running, tokens)) = serve(Setup::default().lease_secs(SHORT_LEASE)) else { return };
        let (mut ann, mut bo) = (dial(&running, &tokens, "ann"), dial(&running, &tokens, "bo"));

        let store = Store::open();
        store.ensure_dirs().unwrap();
        shared_row(&store, "wsp-1");
        let held = take(&mut ann, "shared:team", "wsp-1").unwrap();
        let mut claim = json!({ "workspace_id": "cpd-a", "claimed_at": util::now_iso() });
        stamp(&mut claim, &held);
        store.set_claim("wsp-1", claim);
        let say = |line: String| {
            let mut t = store.task("wsp-1").unwrap();
            t.log(&line);
            store.save_task(&t).unwrap();
        };
        let subject = super::fence_subject(&RowId("wsp-1".into()));
        let head = |c: &mut Client| c.sync_heads(&HubWorkspace("shared:team".into())).unwrap().get(&subject).copied();
        let body_of = |c: &mut Client, id| {
            let blob = c.sync_get(vec![id]).unwrap().remove(0);
            wsp_objects::decode(&blob.bytes).unwrap()
        };

        // Nothing to say yet; then a ready line is a verdict, sent once.
        assert_eq!(push_all(&store, &mut ann), Pushed::default());
        say(format!("{} abc1234 on m-1", wsp_engine::repair::READY));
        assert_eq!(push_all(&store, &mut ann).sent, 1);
        assert_eq!(push_all(&store, &mut ann).sent, 0, "once");
        let reviewed = head(&mut ann).expect("the verdict moved the row's head");
        match body_of(&mut ann, reviewed) {
            wsp_objects::Object::Event { kind, payload, .. } => {
                assert_eq!(kind, EventKind::RowReviewed);
                assert_eq!((payload["token"].as_u64(), payload["commit"].as_str()), (Some(held.token.0), Some("abc1234")));
            }
            other => panic!("not a verdict: {other:?}"),
        }

        // The lease runs out and B takes the row, and pushes what B has.
        running.pass_ttl();
        let theirs = take(&mut bo, "shared:team", "wsp-1").unwrap();
        push_verdict(&mut bo, "shared:team", "wsp-1", EventKind::RowReviewed, &json!({ "commit": "bbbbbbb" }), theirs.token).unwrap();
        let left = head(&mut bo).unwrap();
        assert_ne!(left, reviewed);

        // A lands under her old token: refused fenced, released, and the head is B's.
        say(format!("{} ccccccc on m-1", wsp_store::log::LANDED));
        let pushed = push_all(&store, &mut ann);
        assert_eq!((pushed.sent, pushed.fenced.as_slice()), (0, ["wsp-1".to_string()].as_slice()), "{pushed:?}");
        assert!(!store.claims().contains_key("wsp-1"), "A's claim is let go");
        assert_eq!(head(&mut bo), Some(left), "the row stays as its new holder left it");
        let again = push_verdict(&mut ann, "shared:team", "wsp-1", EventKind::RowLanded, &json!({ "commit": "ccccccc" }), held.token);
        assert!(matches!(again, Err(PushError::Fenced(_))), "{again:?}");

        drop(running);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// wsp-486: a verifier's hold on a member this spoke holds is pushed once as
    /// a `row.reviewed` with `verdict: holds` and the commit; the pull puts it in
    /// the book of the driver's spoke, where `verdict_held` is true for a member
    /// held elsewhere at that commit, and false for one that is not held elsewhere.
    #[test]
    fn hub_verdict_a_held_verdict_is_pushed_once_and_pulled_into_the_book() {
        install(lose, |_, _, _| false);
        let _env = wsp_engine::util::isolated("hub-verdict-held");
        std::env::remove_var("WSP_CLAIM_TTL");
        let dir = std::env::temp_dir().join(format!("wsp-486-verdict-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let Some((running, tokens)) = serve(Setup::default()) else { return };
        let mut ann = dial(&running, &tokens, "ann");

        let store = Store::open();
        store.ensure_dirs().unwrap();
        shared_row(&store, "wsp-1");
        let held = take(&mut ann, "shared:team", "wsp-1").unwrap();
        let mut claim = json!({ "workspace_id": "cpd-a", "claimed_at": util::now_iso() });
        stamp(&mut claim, &held);
        store.set_claim("wsp-1", claim);
        let mut t = store.task("wsp-1").unwrap();
        t.log(&format!("{} abc1234 on m-1", wsp_engine::repair::READY));
        store.save_task(&t).unwrap();
        wsp_engine::verification::record(&store, "wsp-1", wsp_engine::verification::State::Holds, "checked", None, None).unwrap();

        // Sent once as a verdict object, then the ready line's own event; a second pass sends nothing.
        let first = push_all(&store, &mut ann).sent;
        assert!(first >= 1, "the hold went to the hub");
        assert_eq!(push_all(&store, &mut ann).sent, 0, "once");
        let subject = super::fence_subject(&RowId("wsp-1".into()));
        let head = ann.sync_heads(&HubWorkspace("shared:team".into())).unwrap().get(&subject).copied().expect("a head");
        let mut at = Some(head);
        let mut verdicts = Vec::new();
        while let Some(id) = at {
            let blob = ann.sync_get(vec![id]).unwrap().remove(0);
            let obj = wsp_objects::decode(&blob.bytes).unwrap();
            at = obj.parent().copied();
            if let wsp_objects::Object::Event { kind, payload, .. } = obj {
                if payload["verdict"] == "holds" {
                    assert_eq!(kind, EventKind::RowReviewed);
                    assert_eq!(payload["commit"], "abc1234");
                    verdicts.push(payload);
                }
            }
        }
        assert_eq!(verdicts.len(), 1, "one holding verdict on the hub: {verdicts:?}");

        // The pull's import (hub_sync): the book takes it; the barrier reads it for a member held elsewhere only.
        let (dh, dst) = (dir.join("driver-home"), dir.join("driver-state"));
        let ds = Store::at(dh, dst);
        ds.ensure_dirs().unwrap();
        take_verdict(&ds, "wsp-1", &verdicts[0]);
        assert!(!verdict_held(&ds, "wsp-1"), "not held elsewhere: no");
        Book::edit(&ds, |b| b.elsewhere.insert("wsp-1".into(), "ann".into()));
        assert!(verdict_held(&ds, "wsp-1"), "held elsewhere, verdict pulled");
        assert!(!verdict_held(&ds, "wsp-2"), "no verdict for another row");
        let mut m = wsp_store::model::Task::new("wsp-1", "wsp-1");
        m.log(&format!("{} deadbee on m-1", wsp_engine::repair::READY));
        ds.save_task(&m).unwrap();
        assert!(!verdict_held(&ds, "wsp-1"), "the member moved on to another commit");
        // A later blocks for the same row overrides the earlier hold.
        let mut m = wsp_store::model::Task::new("wsp-1", "wsp-1");
        m.log(&format!("{} abc1234 on m-1", wsp_engine::repair::READY));
        ds.save_task(&m).unwrap();
        assert!(verdict_held(&ds, "wsp-1"), "back at the held commit");
        take_verdict(&ds, "wsp-1", &json!({ "commit": "abc1234", "verdict": "blocks" }));
        assert!(!verdict_held(&ds, "wsp-1"), "a later blocks overrides the hold");

        drop(running);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- wsp-468 push: a shared row's landing is a push to the shared remote ----

    /// git in `dir` with an identity, its output trimmed; a failure fails the test.
    fn git_t(dir: &Path, args: &[&str]) -> String {
        let out = wsp_base::calls::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    /// A bare repository with one commit on `master`: the shared remote.
    fn bare_remote(at: &Path) {
        let seed = at.with_extension("seed");
        crate::testkit::repo_at(&seed, &[("base.txt", "base\n")], "base");
        git_t(at.parent().unwrap(), &["init", "--quiet", "--bare", "-b", "master", &at.display().to_string()]);
        git_t(&seed, &["push", "--quiet", &at.display().to_string(), "master"]);
    }

    /// A clone of `remote` at `at`, its remote named `origin`, with a git
    /// identity of its own in its config: the landing's rebase and commits are
    /// the engine's git calls, which take no identity from the test.
    fn clone_of(remote: &Path, at: &Path) {
        git_t(remote.parent().unwrap(), &["clone", "--quiet", &remote.display().to_string(), &at.display().to_string()]);
        git_t(at, &["config", "user.name", "t"]);
        git_t(at, &["config", "user.email", "t@t"]);
    }

    /// The landing commits in the clones through the engine's own git calls,
    /// which get no identity from [`git_t`]: a clone carries its own, so the
    /// tests do not depend on the developer's global `user.email`.
    #[test]
    fn hub_push_fixture_clones_carry_their_own_git_identity() {
        let env = wsp_engine::util::isolated("hub-push-identity");
        let (remote, mine) = (env.path("remote.git"), env.path("mine"));
        bare_remote(&remote);
        clone_of(&remote, &mine);
        assert_eq!(git_t(&mine, &["config", "--local", "user.email"]), "t@t");
        assert_eq!(git_t(&mine, &["config", "--local", "user.name"]), "t");
    }

    /// Puts `file` in a commit on the clone `at` and pushes it: the remote moves.
    fn move_remote(at: &Path, file: &str) -> String {
        std::fs::write(at.join(file), "theirs\n").unwrap();
        git_t(at, &["add", file]);
        git_t(at, &["commit", "--quiet", "-m", "theirs"]);
        git_t(at, &["push", "--quiet", "origin", "master"]);
        git_t(at, &["rev-parse", "HEAD"])
    }

    /// In `store`: project `p` rooted at the clone `repo` with `checks`, and row
    /// `row` in `workspace` (empty: private) ready at review in a tree of its own
    /// holding a commit of `file`. The ready commit.
    fn ready_row(store: &Store, repo: &Path, row: &str, workspace: &str, file: &str, checks: &str) -> String {
        let tree = repo.join(".worktrees").join(row);
        git_t(repo, &["worktree", "add", "-q", "-b", row, &tree.display().to_string()]);
        std::fs::write(tree.join(file), "mine\n").unwrap();
        git_t(&tree, &["add", file]);
        git_t(&tree, &["commit", "--quiet", "-m", "the work"]);
        let ready = git_t(&tree, &["rev-parse", "HEAD"]);
        let mut p = store.project("p").unwrap_or_else(|| wsp_engine::model::Project::new("p"));
        p.roots = vec![repo.display().to_string()];
        p.body = format!("## Checks\n{checks}\n");
        store.save_project(&p).unwrap();
        let mut t = wsp_store::model::Task::new(row, row);
        t.project = Some("p".into());
        t.status_raw = "review".into();
        if !workspace.is_empty() {
            t.workspace = workspace.parse().unwrap();
        }
        t.log(&format!("{} {ready} on {row}", wsp_engine::repair::READY));
        store.save_task(&t).unwrap();
        ready
    }

    fn engine() {
        wsp_engine::cmd_checkout::install_engine(wsp_engine::cmd_checkout::Engine {
            passed: |_| Default::default(),
            poke_task: |_, _, _| {},
            runs: |_, _| false,
            after_land: wsp_engine::check::after_land,
            rerun: wsp_engine::check::rerun,
            held: |_| None,
            refused: |store, row| crate::hub_journal::refusal(store, row, "land"),
        });
    }

    fn landed(v: wsp_engine::cmd_checkout::Verdict) -> (wsp_engine::cmd_checkout::Landed, Vec<String>) {
        match v {
            wsp_engine::cmd_checkout::Verdict::Landed { landed, reran } => (landed, reran),
            wsp_engine::cmd_checkout::Verdict::Blocked(why) => panic!("blocked: {why}"),
            wsp_engine::cmd_checkout::Verdict::Dirty(why) => panic!("dirty: {why}"),
            wsp_engine::cmd_checkout::Verdict::Nothing => panic!("nothing landed"),
            wsp_engine::cmd_checkout::Verdict::ByMerge(why) => panic!("by merge: {why}"),
        }
    }

    /// Done-when 3: the remote moved since the row was checked, so its first
    /// push is refused; the spoke fetches, rebases, reruns the checks the rebase
    /// can affect and lands on the second try, on the remote and on its trunk.
    #[test]
    fn hub_push_a_landing_that_loses_the_race_reverifies_and_lands_on_the_second_try() {
        engine();
        let env = wsp_engine::util::isolated("hub-push-race");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let (remote, mine, theirs, ran) = (env.path("remote.git"), env.path("mine"), env.path("theirs"), env.path("ran.log"));
        bare_remote(&remote);
        clone_of(&remote, &mine);
        clone_of(&remote, &theirs);
        let ready = ready_row(&store, &mine, "wsp-1", "shared:team", "m.txt", &format!("- always: echo always >> {}", ran.display()));
        let their = move_remote(&theirs, "t.txt");

        let (l, reran) = landed(wsp_engine::cmd_checkout::land_verdict(&store, "wsp-1"));
        assert_eq!(reran, vec!["always".to_string()], "the checks were rerun after the rebase");
        assert_eq!(std::fs::read_to_string(&ran).unwrap(), "always\n");
        assert_ne!(l.sha, ready, "the rebase made a new commit");
        assert_eq!(git_t(&remote, &["rev-parse", "master"]), l.sha, "the remote's trunk is the landed commit");
        assert_eq!(git_t(&mine, &["rev-parse", "master"]), l.sha, "and so is the local trunk");
        assert_eq!(git_t(&remote, &["rev-parse", &format!("{}~1", l.sha)]), their, "on top of the commit that won the race");
        let t = store.task("wsp-1").unwrap();
        let log = t.section("Log").unwrap();
        assert!(log.contains(&format!("{} {} on wsp-1 · since {their} · pushed to origin", wsp_store::log::LANDED, l.sha)), "{log}");
        assert_eq!(pushed_to(&t).as_deref(), Some("origin"));
        let payload = payload_of(&t, EventKind::RowLanded, &l.sha);
        assert_eq!(payload, json!({ "commit": l.sha, "project": "p", "remote": "origin", "remote_commit": l.sha }));
    }

    /// The race lost every time (a check that moves the remote as it runs): three
    /// more tries, then the row is blocked naming the race and nothing landed.
    #[test]
    fn hub_push_a_landing_that_keeps_losing_the_race_blocks_naming_it() {
        engine();
        let env = wsp_engine::util::isolated("hub-push-blocks");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let (remote, mine, theirs) = (env.path("remote.git"), env.path("mine"), env.path("theirs"));
        bare_remote(&remote);
        clone_of(&remote, &mine);
        clone_of(&remote, &theirs);
        let push = format!(
            "- always: echo again >> {d}/again.txt && git -C {d} add again.txt && git -C {d} -c user.name=t -c user.email=t@t commit -q -m again && git -C {d} push -q origin master",
            d = theirs.display()
        );
        ready_row(&store, &mine, "wsp-1", "shared:team", "m.txt", &push);
        move_remote(&theirs, "t.txt");

        let why = match wsp_engine::cmd_checkout::land_verdict(&store, "wsp-1") {
            wsp_engine::cmd_checkout::Verdict::Blocked(why) => why,
            wsp_engine::cmd_checkout::Verdict::Landed { reran, .. } => panic!("not blocked: landed after rerunning {reran:?}"),
            _ => panic!("not blocked"),
        };
        assert!(why.contains("lost the race to the shared remote 4 times") && why.contains("origin/master"), "{why}");
        assert_eq!(git_t(&theirs, &["rev-list", "--count", "master"]), "5", "the base, the first move and three reruns");
        assert_eq!(git_t(&remote, &["rev-parse", "master"]), git_t(&theirs, &["rev-parse", "master"]), "nothing of the row reached the remote");
        assert!(wsp_engine::repair::landed_line(&store.task("wsp-1").unwrap()).is_none(), "no landed line");
    }

    /// A private row lands exactly as today: the local trunk only, no push, no
    /// `pushed to` in its landed line.
    #[test]
    fn hub_push_a_private_row_lands_on_the_local_trunk_only() {
        engine();
        let env = wsp_engine::util::isolated("hub-push-private");
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let (remote, mine) = (env.path("remote.git"), env.path("mine"));
        bare_remote(&remote);
        clone_of(&remote, &mine);
        let before = git_t(&remote, &["rev-parse", "master"]);
        ready_row(&store, &mine, "t-1", "", "m.txt", "");
        let (l, _) = landed(wsp_engine::cmd_checkout::land_verdict(&store, "t-1"));
        assert_eq!(git_t(&mine, &["rev-parse", "master"]), l.sha);
        assert_eq!(git_t(&remote, &["rev-parse", "master"]), before, "nothing was pushed");
        let t = store.task("t-1").unwrap();
        assert!(!t.section("Log").unwrap().contains("pushed to"));
        assert_eq!(pushed_to(&t), None);
    }

    /// Done-when 1: two spokes over a served hub and one bare remote. Each lands
    /// a row of its own (the second losing the race to the first and landing on
    /// its second try); both commits are on the remote's trunk; each spoke's sync
    /// carries its landing to the hub, the other imports it and fetches, and both
    /// caches show both rows landed.
    #[test]
    fn hub_push_two_spokes_land_through_one_bare_remote_and_both_see_both_landed() {
        engine();
        let env = wsp_engine::util::isolated("hub-push-two");
        std::env::remove_var("WSP_CLAIM_TTL");
        let dir = env.path("hub");
        std::fs::create_dir_all(&dir).unwrap();
        let Some((running, tokens)) = serve(Setup::default()) else { return };
        let remote = env.path("remote.git");
        bare_remote(&remote);

        // Two spokes: a store, a login, a clone, a shared row leased and ready.
        let spoke = |who: &str, row: &str, file: &str| {
            let store = Store::at(env.path(&format!("{who}/wsp")), env.path(&format!("{who}/state")));
            store.ensure_dirs().unwrap();
            let repo = env.path(&format!("{who}/repo"));
            clone_of(&remote, &repo);
            ready_row(&store, &repo, row, "shared:team", file, "");
            let held = take(&mut dial(&running, &tokens, who), "shared:team", row).unwrap();
            let mut claim = json!({ "workspace_id": format!("cpd-{who}"), "claimed_at": util::now_iso() });
            stamp(&mut claim, &held);
            store.set_claim(row, claim);
            (store, repo, login(&dir, &running, &tokens, who))
        };
        let (ann, ann_repo, ann_home) = spoke("ann", "wsp-1", "a.txt");
        let (bo, bo_repo, bo_home) = spoke("bo", "wsp-2", "b.txt");

        let (a, _) = landed(wsp_engine::cmd_checkout::land_verdict(&ann, "wsp-1"));
        let (b, _) = landed(wsp_engine::cmd_checkout::land_verdict(&bo, "wsp-2"));
        assert_eq!(git_t(&remote, &["rev-parse", "master"]), b.sha, "bo's landing is the remote's tip");
        assert_eq!(git_t(&remote, &["rev-parse", "master~1"]), a.sha, "on top of ann's");
        assert_eq!(git_t(&ann_repo, &["rev-parse", "origin/master"]), a.sha, "ann has not fetched since");

        // The syncs: each spoke sends its row and its landing, and takes the other's.
        let (mut ann_sync, mut bo_sync) = (crate::hub_sync::Worker::new(ann_home), crate::hub_sync::Worker::new(bo_home));
        for (who, w, s) in [("ann", &mut ann_sync, &ann), ("bo", &mut bo_sync, &bo)] {
            w.turn(s).unwrap_or_else(|e| panic!("{who}: {e}"));
        }
        ann_sync.turn(&ann).unwrap();
        bo_sync.turn(&bo).unwrap();

        // The hub holds both landings, each naming its commit on the remote.
        let mut c = dial(&running, &tokens, "ann");
        let heads = c.sync_heads(&HubWorkspace("shared:team".into())).unwrap();
        for (row, l) in [("wsp-1", &a), ("wsp-2", &b)] {
            let id = *heads.get(&super::fence_subject(&RowId(row.into()))).unwrap_or_else(|| panic!("{row}: no landing on the hub"));
            let blob = c.sync_get(vec![id]).unwrap().remove(0);
            match wsp_objects::decode(&blob.bytes).unwrap() {
                wsp_objects::Object::Event { kind, payload, .. } => {
                    assert_eq!(kind, EventKind::RowLanded, "{row}");
                    assert_eq!((payload["remote"].as_str(), payload["remote_commit"].as_str()), (Some("origin"), Some(l.sha.as_str())), "{row}");
                }
                other => panic!("{row}: not a landing: {other:?}"),
            }
        }

        // Ann imported bo's landing and fetched: her clone has his commit.
        assert_eq!(git_t(&ann_repo, &["rev-parse", "origin/master"]), b.sha, "the import fetched the remote");
        assert_eq!(git_t(&bo_repo, &["rev-parse", "origin/master"]), b.sha);

        // Both caches show both rows landed: the own row's file, the other's head.
        for (who, s, own, other, (o, t)) in [("ann", &ann, "wsp-1", "wsp-2", (&a, &b)), ("bo", &bo, "wsp-2", "wsp-1", (&b, &a))] {
            assert_eq!(wsp_engine::repair::landed_line(&s.task(own).unwrap()).as_deref(), Some(o.sha.as_str()), "{who}: {own}");
            let cache = crate::hub_cache::Cache::read(s);
            let head = cache.get(other).unwrap_or_else(|| panic!("{who}: {other} is not in the cache"));
            assert_eq!(head.landed.as_deref(), Some(t.sha.as_str()), "{who}: {other}");
        }

        drop(running);
    }

    /// Done-when 2 (wsp-466): a row allocated to user A is refused to B's spoke
    /// `forbidden`, and A's spoke takes it.
    #[test]
    fn hub_lease_a_row_allocated_to_another_user_is_forbidden() {
        use wsp_objects::{encode, Object};
        use wsp_hub_protocol::{Blob, RefMove};
        use wsp_store_contract::{Principal as P, Subject};
        let _env = wsp_engine::util::isolated("hub-lease-for");
        let dir = std::env::temp_dir().join(format!("wsp-466-for-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let Some((running, tokens)) = serve(Setup::default()) else { return };
        let (mut ann, mut bo) = (dial(&running, &tokens, "ann"), dial(&running, &tokens, "bo"));

        // The row as publish carries it: its `for:` in the record's fields.
        let ws = HubWorkspace("shared:team".into());
        let subject = Subject::Row(RowId("wsp-2".into()));
        let entity = Entity::Row(Row {
            id: RowId("wsp-2".into()),
            project: ProjectId("wsp".into()),
            workspace: Default::default(),
            title: "wsp-2".into(),
            state: RowState::Todo,
            before_block: None,
            seat: None,
            group: None,
            commit: None,
            fields: BTreeMap::from([("for".to_string(), "ann".to_string())]),
        });
        let object = Object::Record { workspace: ws.clone(), subject: subject.clone(), entity, parent: None, by: P::Person("ann".into()), at: At(1) };
        let (id, bytes) = encode(&object);
        ann.sync_put(vec![Blob { id, bytes }]).unwrap();
        ann.refs_move(RefMove { workspace: ws, subject, from: None, to: id }).unwrap();

        let why = take(&mut bo, "shared:team", "wsp-2").unwrap_err();
        assert!(why.contains("forbidden") && why.contains("wsp-2"), "{why}");
        assert!(take(&mut ann, "shared:team", "wsp-2").is_ok(), "the user it is for takes it");

        drop(running);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A spoke's home logged in to the served hub as `who`.
    pub fn login(dir: &Path, hub: &TestHub, tokens: &BTreeMap<String, String>, who: &str) -> std::path::PathBuf {
        let home = dir.join(format!("home-{who}"));
        hub.login(&home, who, &tokens[who]);
        home
    }

    /// Publishing a claimed row takes the hub's lease before anything is written
    /// (wsp-466 lifting wsp-185's refusal): a hub that refuses leaves the row
    /// private and claimed, nothing renumbered; one that grants moves the claim to
    /// the row's new id with the token and the hub's expiry in it.
    #[test]
    fn hub_lease_publishing_a_claimed_row_takes_the_lease_first_and_a_refusal_publishes_nothing() {
        let _env = wsp_engine::util::isolated("hub-lease-publish");
        std::env::remove_var("WSP_CLAIM_TTL");
        let dir = std::env::temp_dir().join(format!("wsp-466-pub-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let Some((running, tokens)) = serve(Setup::default()) else { return };
        let home = login(&dir, &running, &tokens, "ann");
        let ws: wsp_engine::model::WorkspaceId = "shared:team".parse().unwrap();

        let store = Store::open();
        store.ensure_dirs().unwrap();
        let mut t = wsp_store::model::Task::new("claimed and private", "t-1");
        t.status_raw = "doing".into();
        store.save_task(&t).unwrap();
        store.set_claim("t-1", json!({ "workspace_id": "cpd-a", "claimed_at": util::now_iso() }));
        store.set_binding("cpd-a", json!({ "task_id": "t-1", "pane_id": "cpd-a" }));

        // bo holds every id the hub could hand out next: the take is refused, naming bo.
        let mut bo = dial(&running, &tokens, "bo");
        let mut bos = Vec::new();
        for n in 1..=8 {
            bos.push(take(&mut bo, "shared:team", &format!("{}-{n:03}", wsp_engine::store::INBOX_CODE)).unwrap());
        }
        let why = crate::cmd_publish::publish(&store, "t-1", &ws, &home).unwrap_err();
        assert!(why.contains("nothing was published") && why.contains("bo"), "{why}");
        assert!(store.task("t-1").unwrap().workspace.is_private(), "the row is still private");
        assert!(store.claims().contains_key("t-1") && store.claims()["t-1"].get(TOKEN).is_none(), "the claim is as it was");
        assert!(store.renamed_ids().is_empty(), "nothing was renumbered");

        // bo lets go; the publish takes the lease and the claim follows the row.
        for (n, held) in (1..=8).zip(&bos) {
            bo.lease_release(&RowId(format!("{}-{n:03}", wsp_engine::store::INBOX_CODE)), held.token).unwrap();
        }
        let made = crate::cmd_publish::publish(&store, "t-1", &ws, &home).unwrap();
        let new = made[0].1.clone();
        assert!(!store.task(&new).unwrap().workspace.is_private());
        assert!(!store.claims().contains_key("t-1"), "no claim on the old id");
        let claim = store.claims()[&new].clone();
        assert!(claim[TOKEN].as_u64().is_some(), "{claim}");
        assert!(wsp_store::store::record_expires(&claim).is_some_and(|at| at > util::epoch_secs() + 6 * 86_400), "{claim}");
        assert_eq!(store.panes_for_task(&new), vec!["cpd-a".to_string()], "the seat's binding moved with it");
        assert!(take(&mut bo, "shared:team", &new).unwrap_err().contains("ann"), "ann holds the new id");

        drop(running);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hub_lease_stamp_writes_the_token_and_the_hubs_expiry_over_the_local_one() {
        let at = util::epoch_secs() + 7 * 86_400;
        let mut claim = json!({ "workspace_id": "cpd-1", "expires": "2020-01-01T00:00:00Z", "ttl_s": 5 });
        stamp(&mut claim, &Held { token: Token(4), expires: at });
        assert_eq!(claim[TOKEN], 4);
        assert_eq!(wsp_store::store::record_expires(&claim), Some(at));
        assert!(claim["ttl_s"].as_i64().unwrap() > 6 * 86_400);
    }
    /// `$WSP_HUB_HOME` and the host name set for one spoke's dials: a spoke is
    /// a machine by its hub home (its machine id) and is named by its host.
    fn as_machine<T>(home: &std::path::Path, host: &str, f: impl FnOnce() -> T) -> T {
        let was = (std::env::var_os(hub_client::HUB_HOME_ENV), std::env::var_os("HOSTNAME"));
        std::env::set_var(hub_client::HUB_HOME_ENV, home);
        std::env::set_var("HOSTNAME", host);
        let out = f();
        for (k, v) in [(hub_client::HUB_HOME_ENV, was.0), ("HOSTNAME", was.1)] {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
        out
    }

    /// The hub keys a lease by user and machine (its `b5e529a`): two spokes
    /// logged in as one user, on two machines. The first takes the row; the
    /// second's take and renew are refused, the words saying it is held by
    /// this user on their other machine and naming it. The first renews and
    /// takes again from any connection, after logging out and in again too,
    /// and its machine id is the one it had before.
    #[test]
    fn hub_machine_one_user_on_two_machines_the_second_is_refused_naming_the_first_and_the_first_renews_after_relogin() {
        let env = wsp_engine::util::isolated("hub-machine-two");
        let Some((running, tokens)) = serve_users(Setup::default(), &["ann"]) else { return };
        let (a, b) = (env.path("machine-a"), env.path("machine-b"));
        for h in [&a, &b] {
            running.login(h, "ann", &tokens["ann"]);
        }
        let dial_on = |home: &std::path::Path, host: &str| as_machine(home, host, || crate::cmd_publish::dial(home).unwrap());

        let mut on_a = dial_on(&a, "box-a");
        assert_eq!(on_a.machine().map(|m| m.name.as_str()), Some("box-a"));
        assert_eq!(on_a.held_on(), on_a.machine(), "the hub's welcome names the machine back");
        let id_a = hub_client::machine::kept_id(&a).expect("the dial made a's id");
        let held = take(&mut on_a, "shared:team", "wsp-1").unwrap();

        let mut on_b = dial_on(&b, "box-b");
        assert_ne!(hub_client::machine::kept_id(&b).as_deref(), Some(id_a.as_str()), "another hub home, another machine");
        let why = take(&mut on_b, "shared:team", "wsp-1").unwrap_err();
        assert_eq!(why, "wsp-1 is leased by you on your other machine box-a, not this one: release it there, or let its lease run out");
        match on_b.lease_renew(&RowId("wsp-1".into()), held.token) {
            Err(HubError::Refused(Refusal::Held { holder, .. }, text)) => {
                assert_eq!(holder, Principal::Person("ann".into()));
                assert_eq!(other_machine(&text).as_deref(), Some("box-a"), "{text}");
            }
            other => panic!("b's renew of a's lease: {other:?}"),
        }
        assert_eq!(running.lease_of("wsp-1").map(|l| l.1), Some(held.token), "b's refusals changed nothing");

        // A logs out and in again: the same machine, so its lease is its own still.
        assert!(crate::cmd_login::log_out(&a).unwrap().is_some());
        assert_eq!(hub_client::machine::kept_id(&a).as_deref(), Some(id_a.as_str()), "a logout leaves the machine's id");
        as_machine(&a, "box-a", || crate::cmd_login::log_in(&a, &running.url(), &tokens["ann"], running.ca()).unwrap());
        assert_eq!(hub_client::machine::kept_id(&a).as_deref(), Some(id_a.as_str()), "a login keeps it too");
        let mut again = dial_on(&a, "box-a");
        let (token, _) = again.lease_renew(&RowId("wsp-1".into()), held.token).expect("a renews its lease after logging in again");
        assert_eq!(token, held.token);
        assert_eq!(take(&mut again, "shared:team", "wsp-1").unwrap().token, held.token, "a take by its holder renews it");
        // Released on its machine, the other one takes it.
        again.lease_release(&RowId("wsp-1".into()), held.token).unwrap();
        assert!(take(&mut on_b, "shared:team", "wsp-1").unwrap().token.0 > held.token.0);
    }

    #[test]
    fn hub_machine_the_machine_is_read_from_the_hubs_words_and_only_from_them() {
        let text = "row:wsp-1 is held by person:ann on the machine box-a, not this one: release it there, or let it expire (2026-11-27T10:00:00Z)";
        assert_eq!(other_machine(text).as_deref(), Some("box-a"));
        assert_eq!(other_machine("row:wsp-1 is held by person:ann").as_deref(), None);
        assert_eq!(other_machine("x on the machine , not this one: y"), None);
        let me = Principal::Person("ann".into());
        let held = |text: &str| HubError::Refused(Refusal::Held { lease: LeaseName("row:wsp-1".into()), holder: me.clone() }, text.into());
        assert_eq!(say("wsp-1", held("held, no machine said"), &me), "wsp-1 is leased by you on another machine of yours, not this one: held, no machine said");
        let bo = Principal::Person("bo".into());
        assert_eq!(say("wsp-1", held(text), &bo), "wsp-1 is leased by ann: their spoke holds it until it lets go or the lease runs out");
    }

    /// A hub that counts what dials it, and a home logged in to it as `who`.
    fn counted_login(env: &wsp_engine::util::Isolated, who: &str) -> (std::net::TcpListener, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let home = env.path("login-home");
        let file = home.join(".config/wsp/hubs.json");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let url = format!("tcp://{}", listener.local_addr().unwrap());
        std::fs::write(&file, json!({ "url": url, "token": "t", "user": who, "at": "2026-11-23T00:00:00Z" }).to_string()).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        (listener, home)
    }

    /// Done-when (wsp-416): logged in as jesper, `wsp start` on a shared row
    /// allocated to ed is refused in the words, with no request to the hub; on an
    /// unallocated shared row it goes on to the hub lease; a private row is
    /// today's path.
    #[test]
    fn hub_lease_start_on_a_row_allocated_to_another_user_is_refused_before_the_hub() {
        let env = wsp_engine::util::isolated("hub-lease-416");
        // Core asks the shared-work add-on: the spoke, on this thread.
        let _addon = wsp_engine::addon::scoped(std::sync::Arc::new(crate::spoke::Spoke::new()));
        let (listener, home) = counted_login(&env, "jesper");
        let was = std::env::var_os(crate::hub_client::HUB_HOME_ENV);
        std::env::set_var(crate::hub_client::HUB_HOME_ENV, &home);
        let store = Store::open();
        store.ensure_dirs().unwrap();
        for (id, ws, owner) in [("wsp-1", "shared:team", Some("ed")), ("wsp-2", "shared:team", None), ("t-1", "", Some("ed")), ("wsp-3", "shared:team", Some("jesper"))] {
            let mut t = wsp_store::model::Task::new(id, id);
            if !ws.is_empty() {
                t.workspace = ws.parse().unwrap();
            }
            t.for_user = owner.map(str::to_string);
            store.save_task(&t).unwrap();
        }
        // Takes every connection waiting: one dial can leave two, and the second must not count
        // against a later row.
        let dialled = || std::iter::from_fn(|| listener.accept().ok()).count() > 0;

        let why = before_claim(&store, "wsp-1").unwrap_err();
        assert_eq!(why, "wsp-1 is allocated to ed; it runs on their wsp");
        let args = wsp_engine::base::args::Args::parse(vec!["start".to_string(), "wsp-1".to_string()]);
        assert_eq!(wsp_engine::cmd_task::set_status(&store, &args, wsp_store::model::Status::Doing), 1);
        assert_eq!(store.task("wsp-1").unwrap().status(), wsp_store::model::Status::Todo, "nothing moved");
        assert!(!dialled(), "no request reached the hub");

        // Unallocated, and allocated to the user logged in: on to the hub lease.
        for row in ["wsp-2", "wsp-3"] {
            let why = before_claim(&store, row).unwrap_err();
            assert!(!why.contains("allocated to"), "{why}");
            assert!(dialled(), "{row}: the hub was asked");
        }
        // A private row never asks.
        assert_eq!(before_claim(&store, "t-1").unwrap(), None);
        assert!(!dialled());

        match was {
            Some(h) => std::env::set_var(crate::hub_client::HUB_HOME_ENV, h),
            None => std::env::remove_var(crate::hub_client::HUB_HOME_ENV),
        }
    }
}
