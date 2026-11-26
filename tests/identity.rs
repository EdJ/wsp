//! MP1 gate (wsp-167): posing as another engineer is refused, end to end. A real
//! hub runs as an external program over TLS (`wsp_spoke::testhub`: `WSP_HUB_BIN`
//! or `WSP_HUB_IMAGE`; a generated CA), its first admin `ed`
//! made as `wsp-hub admin init` makes it, and every person is the real `wsp`
//! binary with a home and state of their own, logged in with the token the
//! hub's admin handed out (`wsp hub user add`). What posing is tried by is the
//! hub's own wire, spoken in raw frames over TLS, since the real client never
//! says another `by`.
//!
//! The hub names a connection from its token, whatever `by` the hello sent
//! (wsp-161): a hello that says `person:ed` on jesper's token is welcomed as
//! jesper, and a request that says `person:ed` after that is `forbidden`. Each
//! case prints one line, `identity: <case>: <code>`.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use wsp_spoke::testhub::protocol::{Call, Code, NoArgs, Outcome, SyncCall, Verb, WorkspaceId};
use wsp_spoke::hub_client::{Client, Item, Silent};
use wsp_spoke::testhub::{Setup, TestHub, Wire};
use wsp_store_contract::{EventId, EventKind, Principal, Subject, UserName};

const WSP: &str = env!("CARGO_BIN_EXE_wsp");
const WAIT: Duration = Duration::from_secs(10);

fn person(name: &str) -> Principal {
    Principal::Person(name.into())
}

// ──────────────────────────────── the rig ────────────────────────────────

/// A real hub over TLS (`wsp_spoke::testhub`), and a home for each person who logs in to it.
struct Rig {
    dir: PathBuf,
    hub: TestHub,
    url: String,
    /// `ed`'s token when the first admin was made; none for a hub that has none.
    admin_token: Option<String>,
}

impl Rig {
    /// `None` when no hub is named: the test is skipped.
    fn new(name: &str, first_admin: bool) -> Option<Rig> {
        let dir = std::env::temp_dir().join(format!("wsp-identity-it-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // `wsp-hub admin init --admin ed`, as the binary calls it, before it serves.
        let mut hub = TestHub::start(Setup::default().tls().no_admin())?;
        let admin_token = first_admin.then(|| {
            let (ok, out, err) = hub.admin_init("ed");
            assert!(ok, "admin init: {out}{err}");
            out.split(|c: char| !c.is_ascii_hexdigit()).find(|w| w.len() == 64).expect("admin init printed a token").to_string()
        });
        if let Some(t) = &admin_token {
            hub.adopt_admin(t);
        }
        let url = hub.url();
        Some(Rig { dir, hub, url, admin_token })
    }

    fn ca(&self) -> String {
        self.hub.ca().unwrap().display().to_string()
    }

    /// `wsp args` as `who`, in `who`'s own home and state.
    fn wsp(&self, who: &str, args: &[&str], stdin: &str) -> Output {
        let home = self.dir.join(format!("home-{who}"));
        std::fs::create_dir_all(&home).unwrap();
        let mut child = Command::new(WSP)
            .args(args)
            .env("HOME", &home)
            .env("WSP_STATE", self.dir.join(format!("state-{who}")))
            .env("WSP_HOME", self.dir.join(format!("store-{who}")))
            .env_remove("WSP_HUB_TOKEN")
            .env_remove("WSP_TOKEN")
            .env_remove("WSP_HUB_URL")
            .env_remove("WSP_HUB_CA")
            .env_remove("WSP_HUB_USER")
            .env_remove("XDG_CONFIG_HOME")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    }

    fn login(&self, who: &str, token: &str) {
        let out = self.wsp(who, &["login", &self.url, "--ca", &self.ca()], &format!("{token}\n"));
        assert!(out.status.success(), "{who} logs in: {}", said(&out));
    }

    /// `wsp hub user add <name> --json` as `who`: the token it printed.
    fn add_user(&self, who: &str, name: &str) -> String {
        let out = self.wsp(who, &["hub", "user", "add", name, "--json"], "");
        assert!(out.status.success(), "{who} adds {name}: {}", said(&out));
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("--json prints one object");
        v["token"]["token"].as_str().expect("the token, once").to_string()
    }

    fn raw(&self, token: &str, hello_by: Principal) -> Result<Wire, (Code, String)> {
        self.hub.raw(token, hello_by)
    }

    fn client(&self, token: &str) -> Result<Client, Silent> {
        self.hub.try_client(token, WAIT)
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn said(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

/// The hub as the table of the gate sets it up: `ed` the admin; `jesper` a member of `shared:mp`;
/// `cara` a user who is no member. Tokens as `wsp hub user add` printed them.
struct Gate {
    rig: Rig,
    ed: String,
    jesper: String,
    cara: String,
}

fn gate(name: &str) -> Option<Gate> {
    let rig = Rig::new(name, true)?;
    let ed = rig.admin_token.clone().unwrap();
    rig.login("ed", &ed);
    let jesper = rig.add_user("ed", "jesper");
    let cara = rig.add_user("ed", "cara");
    for args in [&["hub", "workspace", "add", "mp"][..], &["hub", "member", "add", "mp", "jesper"]] {
        let out = rig.wsp("ed", args, "");
        assert!(out.status.success(), "ed: wsp {}: {}", args.join(" "), said(&out));
    }
    rig.login("jesper", &jesper);
    rig.login("cara", &cara);
    // `wsp whoami` reads a store, so jesper's shell has one.
    let out = rig.wsp("jesper", &["init"], "");
    assert!(out.status.success(), "jesper's `wsp init`: {}", said(&out));
    Some(Gate { rig, ed, jesper, cara })
}

// ─────────────────────────────── the raw wire ─────────────────────────────

/// `call` as `by` on a raw connection (`testhub::Wire`): its outcome, or none when the connection ended first.
fn ask_as(w: &mut Wire, by: &Principal, call: Call) -> Option<Outcome> {
    w.ask_as(by, call).map(|r| r.outcome)
}

fn code_of(outcome: &Option<Outcome>) -> Option<Code> {
    match outcome {
        Some(Outcome::Err(f)) => Some(f.code),
        _ => None,
    }
}

fn state() -> Call {
    Call::Verb(Verb::State(NoArgs {}))
}

fn heads() -> Call {
    Call::Sync(SyncCall::Heads { workspace: WorkspaceId("shared:mp".into()) })
}

/// One line per case: what was tried and the code the hub refused it with.
fn said_line(case: &str, code: Code) {
    println!("identity: {case}: {}", format!("{code:?}").to_lowercase());
}

// ───────────────────────────────── the cases ──────────────────────────────

#[test]
fn posing_as_another_engineer_is_refused_on_every_path() {
    let Some(g) = gate("posing") else { return };
    let rig = &g.rig;

    // jesper's token, a hello that says person:ed: welcomed as jesper, never as ed; and the request
    // that says person:ed after it is forbidden, while the one that says jesper is answered.
    let mut jesper = rig.raw(&g.jesper, person("ed")).expect("jesper's token is welcomed");
    assert_eq!(jesper.by, person("jesper"), "hello by person:ed on jesper's token is welcomed as jesper, not ed");
    let posed = ask_as(&mut jesper, &person("ed"), state());
    assert_eq!(code_of(&posed), Some(Code::Forbidden), "jesper's token, hello by person:ed, request by person:ed: {posed:?}");
    said_line("jesper's token, hello by person:ed", Code::Forbidden);

    // jesper's connection sending a request by person:ed, hello honest.
    let mut jesper = rig.raw(&g.jesper, person("jesper")).unwrap();
    for (what, call) in [("a read", state()), ("a sync verb", heads())] {
        let posed = ask_as(&mut jesper, &person("ed"), call);
        assert_eq!(code_of(&posed), Some(Code::Forbidden), "jesper's connection, request by person:ed ({what}): {posed:?}");
    }
    said_line("jesper's connection, request by person:ed", Code::Forbidden);

    // and by the hub itself, which is no seat of jesper's either: a seat token has no place on a hub, so the
    // nearest is a connection claiming to be the hub or a seat.
    for other in [Principal::Wsp("wsp-hub".into()), Principal::Seat(wsp_store_contract::SeatId("cpd-1".into()))] {
        let posed = ask_as(&mut jesper, &other, state());
        assert_eq!(code_of(&posed), Some(Code::Forbidden), "jesper's connection, request by {other:?}: {posed:?}");
    }
    said_line("jesper's connection, request by wsp:wsp-hub or a seat", Code::Forbidden);
    assert!(matches!(ask_as(&mut jesper, &person("jesper"), heads()), Some(Outcome::Ok(_))), "as himself he is answered");

    // a made-up token, and one that is the hash of a real one.
    for (what, bad) in [("a made-up token", "tk-this-was-never-issued".to_string()), ("the hash of a real token", wsp_spoke::hub_client::token::hash(&g.jesper))] {
        let refused = rig.raw(&bad, person("jesper")).err();
        assert_eq!(refused.as_ref().map(|r| r.0), Some(Code::Unauthorised), "{what}: {refused:?}");
    }
    said_line("a made-up token", Code::Unauthorised);

    // cara's token running `wsp hub user add`: forbidden, and nobody was added.
    let out = rig.wsp("cara", &["hub", "user", "add", "eve"], "");
    assert_eq!(out.status.code(), Some(1), "{}", said(&out));
    assert!(said(&out).contains("forbidden"), "cara's `wsp hub user add` says forbidden: {}", said(&out));
    let users = rig.wsp("ed", &["hub", "user", "ls"], "");
    assert!(!said(&users).contains("eve"), "eve was not added: {}", said(&users));
    let out = rig.wsp("cara", &["hub", "user", "add", "eve", "--admin"], "");
    assert!(said(&out).contains("forbidden"), "{}", said(&out));
    said_line("cara's token running `wsp hub user add`", Code::Forbidden);

    // cara is no member of shared:mp: her reads of it are refused; jesper's are not.
    let mut cara = rig.raw(&g.cara, person("cara")).unwrap();
    let read = ask_as(&mut cara, &person("cara"), heads());
    assert_eq!(code_of(&read), Some(Code::Forbidden), "cara, no member, reading shared:mp: {read:?}");
    said_line("cara, no member of shared:mp, reads it", Code::Forbidden);
    let mut jesper = rig.raw(&g.jesper, person("jesper")).unwrap();
    assert!(matches!(ask_as(&mut jesper, &person("jesper"), heads()), Some(Outcome::Ok(_))));

    // ed's own token says who it is: the real binary agrees.
    let out = rig.wsp("jesper", &["whoami", "--json"], "");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|_| panic!("{}", said(&out)));
    assert_eq!(v["by"], "person:jesper", "`wsp whoami` in jesper's shell: {}", said(&out));
    let ed = rig.raw(&g.ed, person("jesper")).unwrap();
    assert_eq!(ed.by, person("ed"), "ed's token is ed, whatever hello said");
}

#[test]
fn the_bootstrap_token_speaks_for_no_person() {
    let Some(rig) = Rig::new("bootstrap", false) else { return };
    let bootstrap = rig.hub.bootstrap();
    let mut me = rig.raw(&bootstrap, person("ed")).expect("the bootstrap token, while the hub has no admin");
    assert_eq!(me.by, Principal::Wsp("wsp-hub".into()), "hello by person:ed on the bootstrap token is welcomed as the hub itself");
    let posed = ask_as(&mut me, &person("ed"), state());
    assert_eq!(code_of(&posed), Some(Code::Forbidden), "the bootstrap token, request by person:ed: {posed:?}");
    let mut me = rig.raw(&bootstrap, person("ed")).unwrap();
    let mine = me.by.clone();
    assert_eq!(code_of(&ask_as(&mut me, &mine, state())), Some(Code::Forbidden), "and as itself it reads nothing");
    said_line("the bootstrap token, request by person:ed", Code::Forbidden);

    // Once the hub has an admin, the bootstrap token names nobody.
    let Some(gated) = gate("bootstrap-after") else { return };
    let bootstrap = gated.rig.hub.bootstrap();
    let refused = gated.rig.raw(&bootstrap, person("ed")).err();
    assert_eq!(refused.as_ref().map(|r| r.0), Some(Code::Unauthorised), "the bootstrap token after the first admin: {refused:?}");
    said_line("the bootstrap token once an admin exists", Code::Unauthorised);
}

#[test]
fn a_revoked_engineers_subscription_is_dropped_and_their_next_hello_is_refused() {
    let Some(g) = gate("revoke") else { return };
    let rig = &g.rig;

    // jesper subscribed, and holding a connection open.
    let jesper_client = rig.client(&g.jesper).expect("jesper dials");
    let mut feed = jesper_client.subscribe(None);
    assert!(matches!(feed.next_within(WAIT), Some(Item::State { .. })), "a subscription opens with the snapshot");
    let mut open = rig.raw(&g.jesper, person("jesper")).unwrap();
    assert!(matches!(ask_as(&mut open, &person("jesper"), heads()), Some(Outcome::Ok(_))));

    // ed revokes him, with the real binary.
    let out = rig.wsp("ed", &["hub", "user", "revoke", "jesper"], "");
    assert!(out.status.success(), "{}", said(&out));

    // The subscription is dropped within a beat, with the reason; the open connection is cut.
    let start = Instant::now();
    while feed.refused().is_none() && start.elapsed() < WAIT {
        let _ = feed.next_within(Duration::from_millis(200));
    }
    assert!(feed.refused().is_some(), "jesper's subscription was not dropped within {WAIT:?}");
    let cut = ask_as(&mut open, &person("jesper"), heads());
    assert!(!matches!(cut, Some(Outcome::Ok(_))), "jesper's open connection still answers after the revoke: {cut:?}");
    said_line("revoked: jesper's subscription dropped", Code::Unauthorised);

    // His next hello is refused, over the wire and from the CLI.
    let refused = rig.raw(&g.jesper, person("jesper")).err();
    assert_eq!(refused.as_ref().map(|r| r.0), Some(Code::Unauthorised), "jesper's hello after the revoke: {refused:?}");
    let out = rig.wsp("jesper", &["hub", "user", "ls"], "");
    assert_eq!(out.status.code(), Some(1), "{}", said(&out));
    said_line("revoked: jesper's next hello", Code::Unauthorised);

    // Nobody else was dropped.
    assert!(rig.raw(&g.cara, person("cara")).is_ok(), "cara is still welcomed");
}

#[test]
fn what_the_hub_records_names_the_person_who_did_it() {
    let Some(g) = gate("attribution") else { return };
    let rig = &g.rig;

    // `wsp whoami` in jesper's shell is jesper, and says so under --json.
    let out = rig.wsp("jesper", &["whoami"], "");
    assert!(said(&out).contains("person:jesper"), "{}", said(&out));

    // The hub's own event log (its subscribers are not told of users) names the admin who did each
    // thing, not the user it was done to; and the refused `wsp hub user add eve` left none.
    let out = rig.wsp("cara", &["hub", "user", "add", "eve"], "");
    assert_eq!(out.status.code(), Some(1), "{}", said(&out));
    // Read as the admin reads it, over the wire: `events` from the start.
    let events = rig.hub.events_after(EventId(0));
    let of = |kind: EventKind, user: &str| events.iter().filter(|e| e.kind == kind && e.subject == Subject::User(UserName(user.into()))).collect::<Vec<_>>();
    let added = of(EventKind::HubUserAdded, "jesper");
    assert_eq!(added.len(), 1, "one event added jesper: {added:?}");
    assert_eq!(added[0].principal, person("ed"), "the event that added jesper names ed, who did it");
    assert_eq!(of(EventKind::HubUserAdded, "ed")[0].principal, Principal::Wsp("wsp-hub".into()), "ed was made by the hub's own bootstrap");
    assert!(of(EventKind::HubUserAdded, "eve").is_empty(), "cara's refused add left no event: {events:?}");
    assert!(events.iter().all(|e| e.principal != person("cara") && e.principal != person("jesper")), "the two non-admins did nothing the log records");
    println!("identity: attribution: jesper's add is ed's event, and cara's refused add left none");
}
