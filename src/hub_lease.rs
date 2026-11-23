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
//! A spoke that is not logged in to a hub has no lease to take, and a private
//! row has none either: both are the standalone case and change nothing.

use std::path::Path;

use serde_json::{json, Value};
use wsp_client::{Client, Error as HubError};
use wsp_objects::WorkspaceId as HubWorkspace;
use wsp_store_contract::{Principal, Refusal, RowId, Token};

use crate::store::Store;
use crate::util;

/// The claim record's field holding the hub's fencing token.
pub(crate) const TOKEN: &str = "hub_token";

/// What the daemon, which sits above the seats, lends this module: ending a
/// claim the way `wsp despawn --lost` does, and whether a seat is alive.
pub(crate) type Release = fn(&Store, &str, &str) -> Result<(), String>;
pub(crate) type Alive = fn(&Store, &str, &mut Option<Option<Vec<String>>>) -> bool;

static HOOKS: std::sync::OnceLock<(Release, Alive)> = std::sync::OnceLock::new();

/// Installs [`Release`] and [`Alive`]; the first install stands. Nothing is
/// renewed or followed in a process that has not (a command at a shell).
pub(crate) fn install(release: Release, alive: Alive) {
    let _ = HOOKS.set((release, alive));
}

/// A lease the hub granted: its token and when it runs out, in epoch seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Held {
    pub(crate) token: Token,
    pub(crate) expires: i64,
}

fn who(p: &Principal) -> String {
    match p {
        Principal::Person(u) => u.to_string(),
        other => format!("{other:?}"),
    }
}

/// What a refused or failed lease verb says.
fn say(row: &str, e: HubError) -> String {
    match e {
        HubError::Refused(Refusal::Held { holder, .. }) => {
            format!("{row} is leased by {}: their spoke holds it until it lets go or the lease runs out", who(&holder))
        }
        HubError::Refused(Refusal::Forbidden { .. }) => format!("{row} is forbidden to you: it is allocated to another user"),
        other => format!("the hub would not lease {row}: {other}"),
    }
}

/// `lease.take` for `row` in `workspace`, the refusals said in words.
pub(crate) fn take(client: &mut Client, workspace: &str, row: &str) -> Result<Held, String> {
    match client.lease_take(&HubWorkspace(workspace.to_string()), &RowId(row.to_string())) {
        Ok((token, at)) => Ok(Held { token, expires: (at.0 / 1000) as i64 }),
        Err(e) => Err(say(row, e)),
    }
}

/// Whether this machine is logged in to a hub (the login file or the env).
fn logged_in(home: &Path) -> bool {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    wsp_client::hub_url_in(env(wsp_client::HUB_URL_ENV), home).is_ok()
}

fn home() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(std::path::PathBuf::from)
}

/// The lease a claim on `row` needs, taken before anything is written: `None`
/// for a private row or a spoke with no login, `Err` for a refusal (the claim
/// is then not made) or a hub that does not answer.
pub(crate) fn before_claim(store: &Store, row: &str) -> Result<Option<Held>, String> {
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
pub(crate) fn stamp(claim: &mut Value, held: &Held) {
    let ttl = (held.expires - util::epoch_secs()).max(0);
    if let Some(o) = claim.as_object_mut() {
        o.insert(TOKEN.into(), json!(held.token.0));
        o.insert("expires".into(), json!(util::iso_at(held.expires)));
        o.insert("ttl_s".into(), json!(ttl));
    }
}

fn token_of(rec: &Value) -> Option<Token> {
    rec.get(TOKEN).and_then(|t| t.as_u64()).map(Token)
}

fn seat_of(rec: &Value) -> Option<&str> {
    ["workspace_id", "workspace", "workspace_label"]
        .iter()
        .find_map(|k| rec.get(k).and_then(|x| x.as_str()).filter(|s| !s.is_empty()))
}

/// The claim on `row` lets go: its lease goes back to the hub, best effort (a
/// lease the hub cannot be told of runs out on its own). Called as the claim
/// ends, so a row released by `despawn`, `done` or a hand-off is free to the
/// next user and not held for a week.
pub(crate) fn let_go(row: &str, claim: &Value) {
    let Some(token) = token_of(claim) else { return };
    let Some(home) = home().filter(|h| logged_in(h)) else { return };
    let Ok(mut client) = crate::cmd_publish::dial(&home) else { return };
    let _ = client.lease_release(&RowId(row.to_string()), token);
}

/// [`renew_due`] with the daemon's reading of which seats are alive.
pub(crate) fn renew_all(store: &Store, client: &mut Client, now: i64) -> Renewed {
    let Some((_, alive)) = HOOKS.get() else { return Renewed::default() };
    let mut seen = None;
    renew_due(store, client, now, &mut |seat| alive(store, seat, &mut seen))
}

/// What one renewal pass did.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Renewed {
    /// Claims whose lease the hub renewed.
    pub(crate) renewed: usize,
    /// Rows whose lease the hub said was gone, and which were released.
    pub(crate) lost: Vec<String>,
}

/// The renewal pass: every claim with a hub lease, whose holder seat `alive`
/// says is alive and whose lease was last set over an hour ago (a short TTL:
/// half of it), is renewed on the hub, and its `expires` moves to the hub's.
/// A lease the hub says is gone or another's releases the claim.
pub(crate) fn renew_due(store: &Store, client: &mut Client, now: i64, alive: &mut dyn FnMut(&str) -> bool) -> Renewed {
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
            Err(HubError::Refused(r)) => {
                let why = format!("hub lease gone: renewing it was refused ({r:?})");
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
            crate::verification::tell_governor(store, row, &format!("{row}: {why}. It is released and back to todo; the last checkpoint is in the seat's worktree."));
            true
        }
        Err(e) => {
            wsp_store::store::log_line(store, &format!("hub lease: {row}: {e}"));
            false
        }
    }
}

/// A `lease.expired` the hub announced, `payload` its `{workspace,row,holder,token}`:
/// where the claim here holds that token it is released. Whether it was.
pub(crate) fn follow_expired(store: &Store, payload: &Value) -> bool {
    let (Some(row), Some(token)) = (payload["row"].as_str(), payload["token"].as_u64()) else { return false };
    drop_claim(store, row, Token(token), &format!("lease expired on the hub: {row}, token {token}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::time::Duration;
    use wsp_api::ApiToken;
    use wsp_client::{Client, Item};
    use wsp_hub::{Config, Hub};
    use wsp_store_contract::{
        At, Command as Cmd, Entity, EventKind, IdempotencyKey, ProjectId, Row, RowState, SharedName, TokenId, UserName,
    };

    const DAY: Duration = Duration::from_secs(86_400);
    const WAIT: Duration = Duration::from_secs(10);

    fn person(n: &str) -> Principal {
        Principal::Person(n.into())
    }

    /// A served hub (no TLS, loopback) with `ann`, `bo` members of `shared:team`.
    fn serve(dir: &Path) -> (wsp_hub::serve::Running, BTreeMap<String, String>) {
        let data = dir.join("hub-data");
        let mut tokens = BTreeMap::new();
        {
            let hub = Hub::open(&data).unwrap();
            let apply = |cmd: Cmd, by: &Principal| {
                hub.apply(cmd.clone(), IdempotencyKey(format!("{cmd:?}")), by.clone()).expect("the identity machine takes it");
            };
            let root = person("root");
            apply(Cmd::HubUserAdd { name: UserName("root".into()), admin: true }, &Principal::Wsp("wsp-hub".into()));
            apply(Cmd::HubWorkspaceAdd { name: SharedName("team".into()) }, &root);
            for user in ["ann", "bo"] {
                apply(Cmd::HubUserAdd { name: UserName(user.into()), admin: false }, &root);
                apply(Cmd::HubMemberSet { workspace: SharedName("team".into()), user: UserName(user.into()), member: true }, &root);
                let (secret, hash) = wsp_machines::token::mint();
                apply(Cmd::HubTokenIssue { id: TokenId(format!("t-{user}")), user: UserName(user.into()), hash }, &root);
                tokens.insert(user.to_string(), secret);
            }
        }
        let cfg = Config {
            data_dir: data,
            bind: vec![],
            port: 0,
            peers: vec!["127.0.0.0/8".into()],
            tls: None,
            name: "wsp-hub".into(),
            lease_days: 7,
        };
        (wsp_hub::serve::start(&cfg).expect("the hub starts"), tokens)
    }

    fn dial(running: &wsp_hub::serve::Running, tokens: &BTreeMap<String, String>, who: &str) -> Client {
        Client::dial_at(vec![running.addrs()[0].to_string()], ApiToken(tokens[who].clone()), person(who), WAIT).expect("the hub answers")
    }

    fn shared_row(store: &Store, id: &str) {
        let mut t = wsp_store::model::Task::new(id, id);
        t.status_raw = "doing".into();
        t.workspace = "shared:team".parse().unwrap();
        store.save_task(&t).unwrap();
    }

    /// Done-when 1 (wsp-466): A leases a row; B's claim on it is refused naming
    /// A's user; A's live seat renews, a dead one does not; with the hub's clock
    /// past the TTL the lease is released and announced, A's spoke releases as
    /// `despawn --lost` does, and B takes it with a higher token.
    #[test]
    fn hub_lease_two_spokes_a_held_row_is_refused_renewed_while_alive_and_follows_the_hub_on_expiry() {
        install(|s, r, w| crate::cmd_spawn::release_lost(s, r, w).map(|_| ()), |_, _, _| false);
        let _env = crate::util::isolated("hub-lease-two");
        std::env::remove_var("WSP_CLAIM_TTL");
        let dir = std::env::temp_dir().join(format!("wsp-466-two-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (running, tokens) = serve(&dir);
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

        // Not due yet: nothing is sent. An hour on, a dead seat is not renewed...
        let now = util::epoch_secs();
        assert_eq!(renew_due(&store, &mut ann, now, &mut |_| true), Renewed::default(), "an hour has not passed");
        let later = now + 2 * 3600;
        assert_eq!(renew_due(&store, &mut ann, later, &mut |_| false), Renewed::default(), "a dead seat is not renewed");
        // ...a live one is, and the claim's expiry moves to the hub's.
        let before = wsp_store::store::record_expires(&store.claims()["wsp-1"]).unwrap();
        running.hub().clock().advance(Duration::from_secs(2 * 3600));
        assert_eq!(renew_due(&store, &mut ann, later, &mut |seat| seat == "cpd-a").renewed, 1);
        let after = wsp_store::store::record_expires(&store.claims()["wsp-1"]).unwrap();
        assert!(after >= before + 2 * 3600 - 2, "{after} after {before}");
        assert_eq!(store.claims()["wsp-1"][TOKEN], held.token.0, "the token is the same one");

        // A's seat is killed and not renewed; the hub's clock passes the TTL.
        running.hub().clock().advance(8 * DAY);
        assert_eq!(running.hub().expire_leases().unwrap().len(), 1);
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
        assert_eq!(row.status(), crate::model::Status::Todo);
        let log = row.section("Log").unwrap_or_default();
        assert_eq!(log.lines().filter(|l| l.contains("lease expired on the hub")).count(), 1, "{log}");
        assert!(!follow_expired(&store, &heard.payload), "once");

        // B takes it with a higher token.
        let theirs = take(&mut bo, "shared:team", "wsp-1").unwrap();
        assert!(theirs.token > held.token, "{theirs:?} after {held:?}");

        running.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Done-when 2 (wsp-466): a row allocated to user A is refused to B's spoke
    /// `forbidden`, and A's spoke takes it.
    #[test]
    fn hub_lease_a_row_allocated_to_another_user_is_forbidden() {
        use wsp_objects::{encode, Object};
        use wsp_api::{Blob, RefMove};
        use wsp_store_contract::{Principal as P, Subject};
        let _env = crate::util::isolated("hub-lease-for");
        let dir = std::env::temp_dir().join(format!("wsp-466-for-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (running, tokens) = serve(&dir);
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

        running.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A spoke's home logged in to the served hub as `who`.
    fn login(dir: &Path, running: &wsp_hub::serve::Running, tokens: &BTreeMap<String, String>, who: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let home = dir.join(format!("home-{who}"));
        let file = home.join(".config/wsp/hubs.json");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let url = format!("tcp://{}", running.addrs()[0]);
        std::fs::write(&file, json!({ "url": url, "token": tokens[who], "user": who, "at": "2026-11-23T00:00:00Z" }).to_string()).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        home
    }

    /// Publishing a claimed row takes the hub's lease before anything is written
    /// (wsp-466 lifting wsp-185's refusal): a hub that refuses leaves the row
    /// private and claimed, nothing renumbered; one that grants moves the claim to
    /// the row's new id with the token and the hub's expiry in it.
    #[test]
    fn hub_lease_publishing_a_claimed_row_takes_the_lease_first_and_a_refusal_publishes_nothing() {
        let _env = crate::util::isolated("hub-lease-publish");
        std::env::remove_var("WSP_CLAIM_TTL");
        let dir = std::env::temp_dir().join(format!("wsp-466-pub-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (running, tokens) = serve(&dir);
        let home = login(&dir, &running, &tokens, "ann");
        let ws: crate::model::WorkspaceId = "shared:team".parse().unwrap();

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
            bos.push(take(&mut bo, "shared:team", &format!("{}-{n:03}", crate::store::INBOX_CODE)).unwrap());
        }
        let why = crate::cmd_publish::publish(&store, "t-1", &ws, &home).unwrap_err();
        assert!(why.contains("nothing was published") && why.contains("bo"), "{why}");
        assert!(store.task("t-1").unwrap().workspace.is_private(), "the row is still private");
        assert!(store.claims().contains_key("t-1") && store.claims()["t-1"].get(TOKEN).is_none(), "the claim is as it was");
        assert!(store.renamed_ids().is_empty(), "nothing was renumbered");

        // bo lets go; the publish takes the lease and the claim follows the row.
        for (n, held) in (1..=8).zip(&bos) {
            bo.lease_release(&RowId(format!("{}-{n:03}", crate::store::INBOX_CODE)), held.token).unwrap();
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

        running.stop();
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
    /// A hub that counts what dials it, and a home logged in to it as `who`.
    fn counted_login(env: &crate::util::Isolated, who: &str) -> (std::net::TcpListener, std::path::PathBuf) {
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
        let env = crate::util::isolated("hub-lease-416");
        let (listener, home) = counted_login(&env, "jesper");
        let was = std::env::var_os("HOME");
        std::env::set_var("HOME", &home);
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
        let dialled = || listener.accept().is_ok();

        let why = before_claim(&store, "wsp-1").unwrap_err();
        assert_eq!(why, "wsp-1 is allocated to ed; it runs on their wsp");
        let args = crate::base::args::Args::parse(vec!["start".to_string(), "wsp-1".to_string()]);
        assert_eq!(crate::cmd_task::set_status(&store, &args, wsp_store::model::Status::Doing), 1);
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
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }
}
