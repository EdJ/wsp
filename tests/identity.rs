//! MP1 gate (wsp-167): posing as another engineer is refused, end to end. A hub
//! is served in this process over TLS (a generated CA), its first admin `ed`
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

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use wsp_api::{ApiToken, Call, Code, Frame, Hello, NoArgs, Outcome, ReqId, Request, SyncCall, Verb, WorkspaceId, PROTOCOL};
use wsp_client::{Client, Credential, Item};

use wsp_hub::{admin, Config, TlsFiles};
use wsp_server::testing::TestPki;
use wsp_store_contract::{EventId, EventKind, IdempotencyKey, Principal, Subject, UserName};

const WSP: &str = env!("CARGO_BIN_EXE_wsp");
const WAIT: Duration = Duration::from_secs(10);

fn person(name: &str) -> Principal {
    Principal::Person(name.into())
}

// ──────────────────────────────── the rig ────────────────────────────────

/// A hub over TLS in a directory of its own, and a home for each person who logs in to it.
struct Rig {
    dir: PathBuf,
    running: Option<wsp_hub::serve::Running>,
    pki: TestPki,
    url: String,
    /// `ed`'s token when the first admin was made; none for a hub that has none.
    admin_token: Option<String>,
}

impl Rig {
    fn new(name: &str, first_admin: bool) -> Rig {
        let dir = std::env::temp_dir().join(format!("wsp-identity-it-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pki = TestPki::new(&dir.join("pki"), "hub");
        let cfg = Config {
            data_dir: dir.join("hub-data"),
            bind: vec![],
            port: 0,
            peers: vec!["127.0.0.0/8".into()],
            tls: Some(TlsFiles { cert: pki.cert.clone(), key: pki.key.clone() }),
            name: "wsp-hub".into(),
            lease_days: wsp_hub::DEFAULT_LEASE_DAYS,
        };
        // `wsp-hub admin init --admin ed`, as the binary calls it.
        let admin_token = first_admin.then(|| admin::init(&cfg, "ed").expect("the first admin").token);
        let running = wsp_hub::serve::start(&cfg).expect("the hub starts");
        let url = format!("tls://localhost:{}", running.addrs()[0].port());
        Rig { dir, running: Some(running), pki, url, admin_token }
    }

    fn addr(&self) -> std::net::SocketAddr {
        self.running.as_ref().unwrap().addrs()[0]
    }

    fn ca(&self) -> String {
        self.pki.ca.display().to_string()
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
        Wire::open(self.addr(), &self.pki.ca, token, hello_by)
    }

    fn client(&self, token: &str) -> Result<Client, wsp_client::Silent> {
        Client::dial_hub_ca(&self.url, Credential::User(token.into()), Some(&self.pki.ca), WAIT)
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        if let Some(r) = self.running.take() {
            r.stop();
        }
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

fn gate(name: &str) -> Gate {
    let rig = Rig::new(name, true);
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
    Gate { rig, ed, jesper, cara }
}

// ─────────────────────────────── the raw wire ─────────────────────────────

/// A TLS connection spoken to in frames, and who `welcome` said it is.
struct Wire {
    lines: BufReader<StreamOwned<ClientConnection, TcpStream>>,
    by: Principal,
    next: u64,
}

impl Wire {
    fn open(addr: std::net::SocketAddr, ca: &Path, token: &str, hello_by: Principal) -> Result<Wire, (Code, String)> {
        let mut roots = RootCertStore::empty();
        let certs = CertificateDer::pem_file_iter(ca).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        roots.add_parsable_certificates(certs);
        let config = ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let conn = ClientConnection::new(Arc::new(config), ServerName::try_from("localhost").unwrap()).unwrap();
        let tcp = TcpStream::connect_timeout(&addr, WAIT).unwrap();
        tcp.set_read_timeout(Some(WAIT)).unwrap();
        let mut w = Wire { lines: BufReader::new(StreamOwned::new(conn, tcp)), by: hello_by.clone(), next: 1 };
        w.send(&Frame::Hello(Hello { token: ApiToken(token.into()), by: hello_by, client: "identity gate".into(), protocol: PROTOCOL }));
        match w.recv() {
            Some(Frame::Welcome(welcome)) => {
                w.by = welcome.by.expect("welcome says who the connection is");
                Ok(w)
            }
            Some(Frame::Refused(r)) => Err((r.code, r.text)),
            other => panic!("a hello is answered welcome or refused, not {other:?}"),
        }
    }

    fn send(&mut self, frame: &Frame) {
        let mut line = serde_json::to_vec(frame).unwrap();
        line.push(b'\n');
        let stream = self.lines.get_mut();
        stream.write_all(&line).unwrap();
        stream.flush().unwrap();
    }

    /// The next frame; none when the connection is closed.
    fn recv(&mut self) -> Option<Frame> {
        let mut line = String::new();
        match self.lines.read_line(&mut line) {
            Ok(n) if n > 0 => Some(serde_json::from_str(&line).unwrap_or_else(|e| panic!("{line:?}: {e}"))),
            _ => None,
        }
    }

    /// `call` as `by`: its outcome, or none when the connection ended first.
    fn ask_as(&mut self, by: &Principal, call: Call) -> Option<Outcome> {
        let id = ReqId(self.next);
        self.next += 1;
        let key = matches!(call, Call::Command(_)).then(|| IdempotencyKey(format!("identity-{}", id.0)));
        // A write to a connection the hub cut may fail; that is an ended connection too.
        let mut line = serde_json::to_vec(&Frame::Request(Request { v: PROTOCOL, id, key, by: by.clone(), call })).unwrap();
        line.push(b'\n');
        let stream = self.lines.get_mut();
        if stream.write_all(&line).and_then(|()| stream.flush()).is_err() {
            return None;
        }
        loop {
            match self.recv()? {
                Frame::Reply(r) if r.id == id => return Some(r.outcome),
                _ => continue,
            }
        }
    }
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
    let g = gate("posing");
    let rig = &g.rig;

    // jesper's token, a hello that says person:ed: welcomed as jesper, never as ed; and the request
    // that says person:ed after it is forbidden, while the one that says jesper is answered.
    let mut jesper = rig.raw(&g.jesper, person("ed")).expect("jesper's token is welcomed");
    assert_eq!(jesper.by, person("jesper"), "hello by person:ed on jesper's token is welcomed as jesper, not ed");
    let posed = jesper.ask_as(&person("ed"), state());
    assert_eq!(code_of(&posed), Some(Code::Forbidden), "jesper's token, hello by person:ed, request by person:ed: {posed:?}");
    said_line("jesper's token, hello by person:ed", Code::Forbidden);

    // jesper's connection sending a request by person:ed, hello honest.
    let mut jesper = rig.raw(&g.jesper, person("jesper")).unwrap();
    for (what, call) in [("a read", state()), ("a sync verb", heads())] {
        let posed = jesper.ask_as(&person("ed"), call);
        assert_eq!(code_of(&posed), Some(Code::Forbidden), "jesper's connection, request by person:ed ({what}): {posed:?}");
    }
    said_line("jesper's connection, request by person:ed", Code::Forbidden);

    // and by the hub itself, which is no seat of jesper's either: a seat token has no place on a hub, so the
    // nearest is a connection claiming to be the hub or a seat.
    for other in [Principal::Wsp("wsp-hub".into()), Principal::Seat(wsp_store_contract::SeatId("cpd-1".into()))] {
        let posed = jesper.ask_as(&other, state());
        assert_eq!(code_of(&posed), Some(Code::Forbidden), "jesper's connection, request by {other:?}: {posed:?}");
    }
    said_line("jesper's connection, request by wsp:wsp-hub or a seat", Code::Forbidden);
    assert!(matches!(jesper.ask_as(&person("jesper"), heads()), Some(Outcome::Ok(_))), "as himself he is answered");

    // a made-up token, and one that is the hash of a real one.
    for (what, bad) in [("a made-up token", "tk-this-was-never-issued".to_string()), ("the hash of a real token", wsp_machines::token::hash(&g.jesper))] {
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
    let read = cara.ask_as(&person("cara"), heads());
    assert_eq!(code_of(&read), Some(Code::Forbidden), "cara, no member, reading shared:mp: {read:?}");
    said_line("cara, no member of shared:mp, reads it", Code::Forbidden);
    let mut jesper = rig.raw(&g.jesper, person("jesper")).unwrap();
    assert!(matches!(jesper.ask_as(&person("jesper"), heads()), Some(Outcome::Ok(_))));

    // ed's own token says who it is: the real binary agrees.
    let out = rig.wsp("jesper", &["whoami", "--json"], "");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|_| panic!("{}", said(&out)));
    assert_eq!(v["by"], "person:jesper", "`wsp whoami` in jesper's shell: {}", said(&out));
    let ed = rig.raw(&g.ed, person("jesper")).unwrap();
    assert_eq!(ed.by, person("ed"), "ed's token is ed, whatever hello said");
}

#[test]
fn the_bootstrap_token_speaks_for_no_person() {
    let rig = Rig::new("bootstrap", false);
    let bootstrap = std::fs::read_to_string(rig.running.as_ref().unwrap().token_path()).unwrap().trim().to_string();
    let mut me = rig.raw(&bootstrap, person("ed")).expect("the bootstrap token, while the hub has no admin");
    assert_eq!(me.by, Principal::Wsp("wsp-hub".into()), "hello by person:ed on the bootstrap token is welcomed as the hub itself");
    let posed = me.ask_as(&person("ed"), state());
    assert_eq!(code_of(&posed), Some(Code::Forbidden), "the bootstrap token, request by person:ed: {posed:?}");
    let mut me = rig.raw(&bootstrap, person("ed")).unwrap();
    let mine = me.by.clone();
    assert_eq!(code_of(&me.ask_as(&mine, state())), Some(Code::Forbidden), "and as itself it reads nothing");
    said_line("the bootstrap token, request by person:ed", Code::Forbidden);

    // Once the hub has an admin, the bootstrap token names nobody.
    let gated = gate("bootstrap-after");
    let bootstrap = std::fs::read_to_string(gated.rig.running.as_ref().unwrap().token_path()).unwrap().trim().to_string();
    let refused = gated.rig.raw(&bootstrap, person("ed")).err();
    assert_eq!(refused.as_ref().map(|r| r.0), Some(Code::Unauthorised), "the bootstrap token after the first admin: {refused:?}");
    said_line("the bootstrap token once an admin exists", Code::Unauthorised);
}

#[test]
fn a_revoked_engineers_subscription_is_dropped_and_their_next_hello_is_refused() {
    let g = gate("revoke");
    let rig = &g.rig;

    // jesper subscribed, and holding a connection open.
    let jesper_client = rig.client(&g.jesper).expect("jesper dials");
    let mut feed = jesper_client.subscribe(None);
    assert!(matches!(feed.next_within(WAIT), Some(Item::State { .. })), "a subscription opens with the snapshot");
    let mut open = rig.raw(&g.jesper, person("jesper")).unwrap();
    assert!(matches!(open.ask_as(&person("jesper"), heads()), Some(Outcome::Ok(_))));

    // ed revokes him, with the real binary.
    let out = rig.wsp("ed", &["hub", "user", "revoke", "jesper"], "");
    assert!(out.status.success(), "{}", said(&out));

    // The subscription is dropped within a beat, with the reason; the open connection is cut.
    let start = Instant::now();
    while feed.refused().is_none() && start.elapsed() < WAIT {
        let _ = feed.next_within(Duration::from_millis(200));
    }
    assert!(feed.refused().is_some(), "jesper's subscription was not dropped within {WAIT:?}");
    let cut = open.ask_as(&person("jesper"), heads());
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
    let g = gate("attribution");
    let rig = &g.rig;

    // `wsp whoami` in jesper's shell is jesper, and says so under --json.
    let out = rig.wsp("jesper", &["whoami"], "");
    assert!(said(&out).contains("person:jesper"), "{}", said(&out));

    // The hub's own event log (its subscribers are not told of users) names the admin who did each
    // thing, not the user it was done to; and the refused `wsp hub user add eve` left none.
    let out = rig.wsp("cara", &["hub", "user", "add", "eve"], "");
    assert_eq!(out.status.code(), Some(1), "{}", said(&out));
    let db = wsp_store_sqlite::SqliteStore::open_read_only(rig.dir.join("hub-data").join("hub.db")).expect("the hub's store reads");
    let events = wsp_store_contract::Events::events_after(&db, EventId(0), 1000).expect("the events");
    let of = |kind: EventKind, user: &str| events.iter().filter(|e| e.kind == kind && e.subject == Subject::User(UserName(user.into()))).collect::<Vec<_>>();
    let added = of(EventKind::HubUserAdded, "jesper");
    assert_eq!(added.len(), 1, "one event added jesper: {added:?}");
    assert_eq!(added[0].principal, person("ed"), "the event that added jesper names ed, who did it");
    assert_eq!(of(EventKind::HubUserAdded, "ed")[0].principal, Principal::Wsp("wsp-hub".into()), "ed was made by the hub's own bootstrap");
    assert!(of(EventKind::HubUserAdded, "eve").is_empty(), "cara's refused add left no event: {events:?}");
    assert!(events.iter().all(|e| e.principal != person("cara") && e.principal != person("jesper")), "the two non-admins did nothing the log records");
    println!("identity: attribution: jesper's add is ed's event, and cara's refused add left none");
}
