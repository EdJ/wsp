//! The privacy gate (wsp-180): B cannot see A's private rows by any read,
//! subscribe or presence path, against the real binaries.
//!
//! A wsp-hub over its own listener, with three users: `ed` and `jesper`, members
//! of `shared:core`, and `cara`, logged in but a member of nothing. Each is a
//! `--fake` sandbox with a `wsp daemon`, and dials the hub over TLS through a
//! **tap**: a front that ends the spoke's TLS session itself (the test's own CA
//! and a certificate for `localhost`), writes down every frame the spoke sends in
//! the clear, and carries it on to the hub over loopback. What a frame holds
//! includes what is base64 inside it (an object travels as a `Blob`), so the tap
//! reads those too.
//!
//! Ed adds a private row, a private list holding it, a seat on it and an ask on
//! it, then a row in `shared:core` and a published one. Then:
//!
//! 1. no frame any spoke sent, and no byte the hub keeps (its object store, its
//!    refs), carries the private row's id, title, list, ask or the hash of a record
//!    of any of them;
//! 2. jesper sees the two shared rows and none of the private ones, by `ls`,
//!    `show`, `wip --json`, `worklist show` and `ask`;
//! 3. cara is `forbidden` for `shared:core` and her cache holds nothing of it.
//!
//! The tap is shown to bite: a self-check plants a private id in a frame, and a
//! private title in an object the hub keeps, and the same readers find them.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use wsp_api::Blob;
use wsp_client::{Client, Credential};
use wsp_hub::{Config, Hub};
use wsp_objects::{Object, WorkspaceId};
use wsp_server::tcp::Tcp as Line;
use wsp_server::testing::TestPki;
use wsp_store_contract::{
    At, Command as Cmd, Entity, IdempotencyKey, Principal, ProjectId, Row, RowId, RowState, SharedName, Subject, TokenId, UserName,
};

const WSP: &str = env!("CARGO_BIN_EXE_wsp");
const MACHINE: &str = r#"{"cores": 8, "load": [1.5, 1.25, 1.0], "mem_free": 60, "swap": [512, 4096], "thermal": 100}"#;
const TURN_MS: &str = "300";
const SHARED: &str = "shared:core";

// What is private to ed. Distinctive on purpose, so that a hit is a leak.
const P_TITLE: &str = "Moonlit Heist Plan";
const P_LIST: &str = "zq-hideout";
const P_LIST_TITLE: &str = "Hideout Ledger";
const P_ASK: &str = "Should the Moonlit heist begin at dawn";
const ONE: &str = "Core row one";
const TWO: &str = "Core row two";

// ───────────────────────────────── the tap ─────────────────────────────────

/// Every frame a spoke sent, with the user whose spoke it was.
#[derive(Clone, Default)]
struct Tap(Arc<Mutex<Vec<(String, String)>>>);

impl Tap {
    fn push(&self, who: &str, line: String) {
        self.0.lock().unwrap().push((who.to_string(), line));
    }

    fn frames(&self) -> Vec<(String, String)> {
        self.0.lock().unwrap().clone()
    }

    /// The frames (all users', or `who`'s) in which a needle is found: the user,
    /// the needle, and the frame's head.
    fn leaks(&self, needles: &[String], who: Option<&str>) -> Vec<String> {
        let mut found = Vec::new();
        for (user, line) in self.frames() {
            if who.is_some_and(|w| w != user) {
                continue;
            }
            for text in readable(&line) {
                for n in needles {
                    if text.contains(n.as_str()) {
                        found.push(format!("{user} sent `{n}` in: {}", line.chars().take(160).collect::<String>()));
                    }
                }
            }
        }
        found
    }
}

/// A frame as text, and every string inside it that is base64 of text (an object
/// on the wire is its canonical JSON in base64), so a private title cannot hide.
fn readable(line: &str) -> Vec<String> {
    fn walk(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::String(s) => {
                out.push(s.clone());
                if s.len() >= 16 {
                    if let Some(bytes) = unbase64(s) {
                        let text = String::from_utf8_lossy(&bytes).into_owned();
                        match serde_json::from_str::<Value>(&text) {
                            Ok(inner) => {
                                out.push(text);
                                walk(&inner, out);
                            }
                            Err(_) => out.push(text),
                        }
                    }
                }
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            Value::Object(o) => o.iter().for_each(|(k, x)| {
                out.push(k.clone());
                walk(x, out)
            }),
            _ => {}
        }
    }
    let mut out = vec![line.to_string()];
    if let Ok(v) = serde_json::from_str::<Value>(line) {
        walk(&v, &mut out);
    }
    out
}

/// RFC 4648 standard base64, padded; `None` for anything that is not.
fn unbase64(s: &str) -> Option<Vec<u8>> {
    let s = s.as_bytes();
    if s.is_empty() || s.len() % 4 != 0 {
        return None;
    }
    let val = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let mut out = Vec::new();
    for (i, chunk) in s.chunks(4).enumerate() {
        let last = i == s.len() / 4 - 1;
        let pad = chunk.iter().rev().take_while(|c| **c == b'=').count();
        if pad > 2 || (pad > 0 && !last) {
            return None;
        }
        let mut n = 0u32;
        for c in &chunk[..4 - pad] {
            n = (n << 6) | val(*c)? as u32;
        }
        n <<= 6 * pad as u32;
        let bytes = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(&bytes[..3 - pad]);
    }
    Some(out)
}

/// A TLS listener on loopback for one user's spoke: ends the session, records
/// each line the spoke sends, and carries every line both ways to the hub.
fn front(who: &str, hub: SocketAddr, pki: &TestPki, tap: &Tap) -> SocketAddr {
    let config = wsp_server::tls::server_config(&pki.cert, &pki.key).expect("the front's certificate");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (who, tap) = (who.to_string(), tap.clone());
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(stream) = conn else { continue };
            let Ok(spoke) = Line::accept_tls(stream, config.clone()) else { continue };
            let Ok(up) = Line::dial(hub, Duration::from_secs(5)) else { continue };
            let (mut spoke_w, mut hub_w) = (spoke.try_clone().unwrap(), up.try_clone().unwrap());
            let (spoke_end, hub_end) = (spoke.try_clone().unwrap(), up.try_clone().unwrap());
            let (mut spoke_r, mut hub_r) = (spoke, up);
            let (w, t) = (who.clone(), tap.clone());
            let ends = (spoke_end.try_clone().unwrap(), hub_end.try_clone().unwrap());
            std::thread::spawn(move || {
                loop {
                    match spoke_r.recv_line(Duration::from_secs(1)) {
                        Ok(Some(line)) => {
                            let text = String::from_utf8_lossy(&line).into_owned();
                            t.push(&w, text.clone());
                            if hub_w.send_line(&format!("{text}\n")).is_err() {
                                break;
                            }
                        }
                        Ok(None) => {}
                        Err(_) => break,
                    }
                }
                ends.0.shutdown();
                ends.1.shutdown();
            });
            std::thread::spawn(move || {
                loop {
                    match hub_r.recv_line(Duration::from_secs(1)) {
                        Ok(Some(line)) => {
                            let text = String::from_utf8_lossy(&line).into_owned();
                            if spoke_w.send_line(&format!("{text}\n")).is_err() {
                                break;
                            }
                        }
                        Ok(None) => {}
                        Err(_) => break,
                    }
                }
                spoke_end.shutdown();
                hub_end.shutdown();
            });
        }
    });
    addr
}

// ─────────────────────────────── the hub rig ───────────────────────────────

/// A hub with `shared:core`, its users and a front each, behind the tap.
struct HubRig {
    dir: PathBuf,
    pki: TestPki,
    running: Option<wsp_hub::serve::Running>,
    tokens: BTreeMap<String, String>,
    urls: BTreeMap<String, String>,
    tap: Tap,
}

impl HubRig {
    /// `users` all exist; those in `members` are in `shared:core`.
    fn up(tag: &str, users: &[&str], members: &[&str]) -> HubRig {
        let dir = PathBuf::from("/tmp").join(format!("pv-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let data = dir.join("hub");
        let mut tokens = BTreeMap::new();
        {
            let hub = Hub::open(&data).unwrap();
            let apply = |cmd: Cmd, by: &Principal| {
                hub.apply(cmd.clone(), IdempotencyKey(format!("{cmd:?}")), by.clone()).expect("the identity machine takes it");
            };
            let root = Principal::Person("root".into());
            let core = SharedName("core".into());
            apply(Cmd::HubUserAdd { name: UserName("root".into()), admin: true }, &Principal::Wsp("wsp-hub".into()));
            apply(Cmd::HubWorkspaceAdd { name: core.clone() }, &root);
            for who in users {
                let (secret, hash) = wsp_machines::token::mint();
                apply(Cmd::HubUserAdd { name: UserName(who.to_string()), admin: false }, &root);
                if members.contains(who) {
                    apply(Cmd::HubMemberSet { workspace: core.clone(), user: UserName(who.to_string()), member: true }, &root);
                }
                apply(Cmd::HubTokenIssue { id: TokenId(format!("t-{who}")), user: UserName(who.to_string()), hash }, &root);
                tokens.insert(who.to_string(), secret);
            }
        }
        let cfg = Config {
            data_dir: data,
            bind: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            port: 0,
            peers: vec!["127.0.0.0/8".into()],
            tls: None,
            name: "wsp-hub".into(),
            lease_days: wsp_hub::DEFAULT_LEASE_DAYS,
        };
        let running = wsp_hub::serve::start(&cfg).expect("the hub starts");
        let pki = TestPki::new(&dir.join("pki"), "hub");
        let tap = Tap::default();
        let urls = users
            .iter()
            .map(|who| {
                let at = front(who, running.addrs()[0], &pki, &tap);
                (who.to_string(), format!("tls://localhost:{}", at.port()))
            })
            .collect();
        HubRig { dir, pki, running: Some(running), tokens, urls, tap }
    }

    fn data(&self) -> PathBuf {
        self.dir.join("hub")
    }

    /// `who` straight on the wire, through their front (so the tap sees it).
    fn client(&self, who: &str) -> Client {
        Client::dial_hub_ca(&self.urls[who], Credential::User(self.tokens[who].clone()), Some(&self.pki.ca), Duration::from_secs(10))
            .expect("dials the hub through the tap")
    }

    /// What the hub keeps that names one of `needles`: files of its data
    /// directory (objects, refs, the identity database), and any object whose
    /// subject is one of `subjects`.
    fn kept(&self, needles: &[String], subjects: &[Subject]) -> Vec<String> {
        let mut found = Vec::new();
        let mut files = Vec::new();
        walk(&self.data(), &mut files);
        for (path, bytes) in &files {
            let text = String::from_utf8_lossy(bytes);
            for n in needles {
                // An object's file is named by its hash, split `ab/cdef…` after two digits.
                if text.contains(n.as_str()) || path.to_string_lossy().replace('/', "").contains(n.as_str()) {
                    found.push(format!("{} holds `{n}`", path.display()));
                }
            }
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if path.components().any(|c| c.as_os_str() == "objects") {
                if let Ok(Object::Record { subject, .. } | Object::Event { subject, .. }) = wsp_objects::decode(bytes) {
                    if subjects.contains(&subject) {
                        found.push(format!("object {name} is a record of private {subject:?}"));
                    }
                }
            }
        }
        found
    }

    fn heads(&self) -> Vec<Subject> {
        let hub = self.running.as_ref().unwrap().hub();
        hub.heads(&WorkspaceId(SHARED.into())).unwrap().heads.iter().map(|(s, _)| s.clone()).collect()
    }
}

impl Drop for HubRig {
    fn drop(&mut self) {
        if let Some(r) = self.running.take() {
            r.stop();
        }
        let dir = self.dir.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            let _ = std::fs::remove_dir_all(dir);
        });
    }
}

/// Every file under `dir` with its bytes.
fn walk(dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if let Ok(bytes) = std::fs::read(&p) {
            out.push((p, bytes));
        }
    }
}

// ───────────────────────────────── a spoke ─────────────────────────────────

fn bare(env: &BTreeMap<String, String>, home: &Path) -> Command {
    let mut c = Command::new(WSP);
    for (k, _) in std::env::vars().filter(|(k, _)| k.starts_with("WSP_") || k.starts_with("COMPOUND_") || k.starts_with("CLAUDE")) {
        c.env_remove(k);
    }
    c.envs(env)
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env("WSP_TEST_MACHINE", MACHINE)
        .env("TZ", "UTC")
        .env("WSP_HOST", "manager");
    c
}

/// One person's wsp: a fake sandbox logged in to the hub through the tap, with a daemon.
struct Spoke {
    user: String,
    dir: PathBuf,
    name: String,
    outer: BTreeMap<String, String>,
    env: BTreeMap<String, String>,
    home: PathBuf,
    daemon: Option<Child>,
}

impl Spoke {
    fn up(rig: &HubRig, who: &str) -> Spoke {
        let dir = rig.dir.join(who);
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let outer: BTreeMap<String, String> = [
            ("WSP_HOME".to_string(), dir.join("store").display().to_string()),
            ("WSP_STATE".to_string(), dir.join("state").display().to_string()),
        ]
        .into();
        bare(&outer, &home).arg("init").output().unwrap();
        let name = format!("priv-{who}-{}", std::process::id());
        let out = bare(&outer, &home).args(["sandbox", "--fake", "--json", "--name", &name]).env("WSP_SANDBOX_TETHER", std::process::id().to_string()).output().unwrap();
        let v: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("sandbox json: {e}: {}", String::from_utf8_lossy(&out.stderr)));
        let env: BTreeMap<String, String> = v["env"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string())).collect();
        // The login, as `wsp login` writes it, under the sandbox's hub home (wsp-490).
        let hubs = std::path::PathBuf::from(&env["WSP_HUB_HOME"]).join(".config/wsp/hubs.json");
        std::fs::create_dir_all(hubs.parent().unwrap()).unwrap();
        let login = serde_json::json!({
            "url": rig.urls[who], "token": rig.tokens[who], "user": who, "at": "2026-11-23T00:00:00Z",
            "ca": rig.pki.ca.display().to_string(),
        });
        std::fs::write(&hubs, login.to_string()).unwrap();
        std::fs::set_permissions(&hubs, std::fs::Permissions::from_mode(0o600)).unwrap();
        let s = Spoke { user: who.into(), dir, name, outer, env, home, daemon: None };
        s.ok(&["init"]);
        s.ok(&["project", "add", "alpha", "--name", "Alpha"]);
        s
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = bare(&self.env, &self.home);
        c.args(args);
        c
    }

    fn run(&self, args: &[&str]) -> (bool, String) {
        let o = self.cmd(args).output().unwrap();
        (o.status.success(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
    }

    fn ok(&self, args: &[&str]) -> String {
        let (ok, out) = self.run(args);
        assert!(ok, "{}: wsp {}: {out}", self.user, args.join(" "));
        out
    }

    fn store(&self) -> PathBuf {
        PathBuf::from(&self.env["WSP_HOME"])
    }

    fn state(&self) -> PathBuf {
        PathBuf::from(&self.env["WSP_STATE"])
    }

    fn start_daemon(&mut self) {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let out = std::fs::File::create(self.dir.join("daemon.out")).unwrap();
        let err = out.try_clone().unwrap();
        let mut c = self.cmd(&["daemon"]);
        c.env("WSP_API_PORT", port.to_string()).env("WSP_HUB_SYNC_MS", TURN_MS).stdout(out).stderr(err);
        self.daemon = Some(c.spawn().unwrap());
    }

    fn turns(&self) -> u64 {
        std::fs::read_to_string(self.state().join("hub/sync.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v["turns"].as_u64())
            .unwrap_or(0)
    }

    fn wait(&self, what: &str, within: Duration, mut done: impl FnMut(&Spoke) -> bool) {
        let end = Instant::now() + within;
        while !done(self) {
            assert!(
                Instant::now() < end,
                "{}: {what} not within {within:?}; turns {}\ndaemon: {}",
                self.user,
                self.turns(),
                std::fs::read_to_string(self.dir.join("daemon.out")).unwrap_or_default()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn rows(&self) -> Vec<Value> {
        let (ok, out) = self.run(&["ls", "--json"]);
        assert!(ok, "{}: ls --json: {out}", self.user);
        let v: Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("ls --json: {e}: {out}"));
        v.as_array().cloned().or_else(|| v["rows"].as_array().cloned()).unwrap_or_default()
    }

    fn id_of(&self, title: &str) -> String {
        self.rows()
            .iter()
            .find(|r| r["title"].as_str() == Some(title) || r["caption"]["text"].as_str() == Some(title))
            .and_then(|r| r["id"].as_str().map(str::to_string))
            .unwrap_or_else(|| panic!("{}: no row `{title}` in ls", self.user))
    }

    fn ids(&self) -> BTreeSet<String> {
        self.rows().iter().filter_map(|r| r["id"].as_str().map(str::to_string)).collect()
    }

    /// Everything this spoke keeps on disk, store and state, with its text.
    fn kept(&self) -> Vec<(PathBuf, String)> {
        let mut files = Vec::new();
        walk(&self.store(), &mut files);
        walk(&self.state().join("hub"), &mut files);
        files.into_iter().map(|(p, b)| (p, String::from_utf8_lossy(&b).into_owned())).collect()
    }
}

impl Drop for Spoke {
    fn drop(&mut self) {
        if let Some(mut d) = self.daemon.take() {
            let _ = d.kill();
            let _ = d.wait();
        }
        let _ = bare(&self.outer, &self.home).args(["sandbox", "rm", &self.name]).output();
    }
}

// ──────────────────────────────── the scenario ────────────────────────────────

/// What ed made, private and shared.
struct Made {
    private_id: String,
    mid: String,
    one: String,
    two: String,
}

impl Made {
    /// What none of the other users, the hub or the wire may carry: the row's id,
    /// title, list, the ask and its id.
    fn private(&self) -> Vec<String> {
        [&self.private_id, P_TITLE, P_LIST, P_LIST_TITLE, P_ASK, &self.mid].iter().map(|s| s.to_string()).collect()
    }
}

/// Ed's private row (in project `hush`), a private list holding it, a seat on it
/// and an ask on it; then a row in `shared:core` and a published one.
fn ed_makes(ed: &Spoke) -> Made {
    let proj = ed.dir.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    for args in [&["init", "-q", "."][..], &["commit", "-q", "--allow-empty", "-m", "i"][..]] {
        assert!(Command::new("git")
            .args(["-c", "user.name=ed", "-c", "user.email=ed@example.invalid"])
            .args(args)
            .current_dir(&proj)
            .status()
            .unwrap()
            .success());
    }
    ed.ok(&["project", "add", "hush", "--name", "Hush"]);
    ed.ok(&["project", "set", "hush", &format!("roots={}", proj.display())]);

    ed.ok(&["add", P_TITLE, "-p", "hush"]);
    let private_id = ed.id_of(P_TITLE);
    ed.ok(&["worklist", "new", P_LIST, P_LIST_TITLE]);
    ed.ok(&["worklist", "add", P_LIST, &private_id]);
    let spawned = ed.ok(&["spawn", &private_id, "-p", "hush", "--headless"]);
    let seat = spawned
        .lines()
        .find_map(|l| l.trim().strip_prefix("bound to "))
        .unwrap_or_else(|| panic!("spawn bound no seat: {spawned}"))
        .trim()
        .to_string();
    let mut ask = ed.cmd(&["ask", &private_id, P_ASK]);
    ask.env("WSP_SEAT_ID", &seat);
    let o = ask.output().unwrap();
    let said = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    assert!(o.status.success(), "ask: {said}");
    let mid = said
        .split_whitespace()
        .find(|w| w.starts_with("m-"))
        .unwrap_or_else(|| panic!("ask printed no id: {said}"))
        .to_string();

    ed.ok(&["add", ONE, "-p", "alpha", "--workspace", SHARED]);
    let one = ed.id_of(ONE);
    ed.ok(&["add", TWO, "-p", "alpha"]);
    let before = ed.id_of(TWO);
    ed.ok(&["publish", &before, "--to", SHARED]);
    let two = ed.id_of(TWO);
    assert_ne!(before, two, "the published row took a hub id");
    Made { private_id, mid, one, two }
}

#[test]
fn what_ed_keeps_private_reaches_no_frame_no_object_and_no_other_user() {
    let rig = HubRig::up("gate", &["ed", "jesper", "cara"], &["ed", "jesper"]);
    let mut ed = Spoke::up(&rig, "ed");
    let mut jesper = Spoke::up(&rig, "jesper");
    let mut cara = Spoke::up(&rig, "cara");
    for s in [&mut ed, &mut jesper, &mut cara] {
        s.start_daemon();
    }
    for s in [&ed, &jesper, &cara] {
        s.wait("a first turn", Duration::from_secs(60), |s| s.turns() >= 1);
    }

    let made = ed_makes(&ed);
    let shared: BTreeSet<String> = [made.one.clone(), made.two.clone()].into();

    // Jesper holds the two shared rows, and only those.
    jesper.wait("the two shared rows", Duration::from_secs(60), |s| shared.is_subset(&s.ids()));
    // Let every spoke take more turns, so what would leak has had its chance.
    for s in [&ed, &jesper, &cara] {
        let from = s.turns();
        s.wait("three more turns", Duration::from_secs(60), |s| s.turns() >= from + 3);
    }
    assert_eq!(jesper.ids(), shared, "jesper's rows are the two shared ones");

    // 1. Frames and the hub's store.
    let needles = made.private();
    let frames = rig.tap.frames();
    assert!(frames.iter().any(|(u, _)| u == "ed") && frames.iter().any(|(u, _)| u == "jesper"), "the tap saw both spokes");
    assert!(
        !rig.tap.leaks(&[ONE.to_string(), TWO.to_string()], Some("ed")).is_empty(),
        "the tap reads what ed sent, shared titles inside objects included, or it proves nothing"
    );
    let leaked = rig.tap.leaks(&needles, None);
    assert!(leaked.is_empty(), "frames carry what is private:\n{}", leaked.join("\n"));

    let subjects = [Subject::Row(RowId(made.private_id.clone())), Subject::List(wsp_store_contract::ListId(P_LIST.into()))];
    let kept = rig.kept(&needles, &subjects);
    assert!(kept.is_empty(), "the hub keeps what is private:\n{}", kept.join("\n"));
    let heads = rig.heads();
    assert!(heads.iter().all(|h| !subjects.contains(h)), "the hub has a head for a private subject: {heads:?}");
    assert!(
        heads.contains(&Subject::Row(RowId(made.one.clone()))) && heads.contains(&Subject::Row(RowId(made.two.clone()))),
        "the hub holds both shared rows: {heads:?}"
    );

    // Not a hash of ed's own private records either: any object file ed's spoke
    // keeps that names a private thing is a name the hub must not hold.
    let mut private_hashes = Vec::new();
    for (path, text) in ed.kept() {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if name.len() == 64 && name.chars().all(|c| c.is_ascii_hexdigit()) && needles.iter().any(|n| text.contains(n.as_str())) {
            private_hashes.push(name);
        }
    }
    let leaked = rig.tap.leaks(&private_hashes, None);
    assert!(leaked.is_empty(), "frames carry the hash of a private record:\n{}", leaked.join("\n"));
    let kept = rig.kept(&private_hashes, &[]);
    assert!(kept.is_empty(), "the hub keeps the hash of a private record:\n{}", kept.join("\n"));

    // 2. Jesper's reads, table-driven: each is run, and none of what it prints
    // names a private thing (but the argument it was asked about, echoed back).
    struct Read {
        what: &'static str,
        args: Vec<String>,
        /// Text the read must print (the shared rows).
        shows: Vec<String>,
    }
    let list = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let reads = vec![
        Read { what: "ls", args: list(&["ls"]), shows: vec![made.one.clone(), made.two.clone()] },
        Read { what: "ls --json", args: list(&["ls", "--json"]), shows: vec![made.one.clone(), made.two.clone()] },
        Read { what: "ls --all", args: list(&["ls", "--all"]), shows: vec![made.one.clone(), made.two.clone()] },
        Read { what: "show shared one", args: list(&["show", &made.one]), shows: vec![ONE.into()] },
        Read { what: "show shared two", args: list(&["show", &made.two]), shows: vec![TWO.into()] },
        Read { what: "show private row", args: list(&["show", &made.private_id]), shows: vec![] },
        Read { what: "show private row --log", args: list(&["show", &made.private_id, "--log"]), shows: vec![] },
        Read { what: "wip", args: list(&["wip"]), shows: vec![] },
        Read { what: "wip --json", args: list(&["wip", "--json"]), shows: vec![] },
        Read { what: "worklist show", args: list(&["worklist", "show", P_LIST]), shows: vec![] },
        Read { what: "worklist ls", args: list(&["worklist", "ls"]), shows: vec![] },
        Read { what: "ask", args: list(&["ask"]), shows: vec![] },
        Read { what: "ask about the row", args: list(&["ask", &made.private_id]), shows: vec![] },
        Read { what: "ask by its id", args: list(&["ask", &made.mid]), shows: vec![] },
        Read { what: "flag", args: list(&["flag"]), shows: vec![] },
    ];
    for r in &reads {
        let args: Vec<&str> = r.args.iter().map(String::as_str).collect();
        let (ok, out) = jesper.run(&args);
        for want in &r.shows {
            assert!(out.contains(want.as_str()), "jesper's `{}` shows `{want}`: {out}", r.what);
        }
        // A read names its own argument in a refusal (`no such task X`).
        for n in needles.iter().filter(|n| !r.args.contains(n)) {
            assert!(!out.contains(n.as_str()), "jesper's `{}` (ok {ok}) shows `{n}`: {out}", r.what);
        }
        if r.what.starts_with("show private") || r.what == "worklist show" || r.what == "ask by its id" {
            assert!(!ok, "jesper's `{}` finds something private: {out}", r.what);
        }
    }
    // Nor does anything of it sit in jesper's own store or cache.
    for (path, text) in jesper.kept() {
        for n in needles.iter() {
            assert!(!text.contains(n.as_str()), "jesper's {} holds `{n}`", path.display());
        }
    }

    // 3. Cara: logged in, a member of nothing.
    let mut c = rig.client("cara");
    let refused = c.sync_heads(&WorkspaceId(SHARED.into())).expect_err("cara may not read shared:core");
    assert!(format!("{refused:?} {refused}").to_lowercase().contains("forbidden"), "forbidden for shared:core: {refused}");
    cara.ok(&["add", "Cara's own", "-p", "alpha"]);
    let own = cara.id_of("Cara's own");
    let (ok, why) = cara.run(&["publish", &own, "--to", SHARED]);
    assert!(!ok && why.contains("forbidden"), "publish to shared:core is forbidden for cara: {why}");
    assert_eq!(cara.ids(), BTreeSet::from([own]), "cara's ls is her own row alone");
    let from = cara.turns();
    cara.wait("three more turns", Duration::from_secs(60), |s| s.turns() >= from + 3);
    let mut hold: Vec<String> = needles.clone();
    // Titles, not ids: cara's own first row may share a code with a shared one.
    hold.extend([ONE.into(), TWO.into(), SHARED.into()]);
    for (path, text) in cara.kept() {
        if path.file_name().is_some_and(|n| n == "sync.json") {
            continue;
        }
        for n in &hold {
            assert!(!text.contains(n.as_str()), "cara's cache {} holds `{n}`: {text}", path.display());
        }
    }
    // Her `show` of the id a shared row has (her own first row may carry it) and of
    // the private one names neither row.
    for id in [&made.one, &made.private_id] {
        let (_, out) = cara.run(&["show", id]);
        assert!(!out.contains(ONE) && !out.contains(P_TITLE), "cara's `show {id}` names a row of ed's: {out}");
    }
    drop((ed, jesper, cara));
}

/// The tap and the store check are shown to bite: a private id planted in a
/// frame, and a private title inside an object, are found by the same readers.
#[test]
fn the_tap_and_the_store_scan_find_a_private_id_planted_in_a_frame_and_an_object() {
    let rig = HubRig::up("bite", &["ed", "cara"], &["ed"]);
    let mut ed = rig.client("ed");
    let needles = vec!["hush-001".to_string(), P_TITLE.to_string()];
    assert!(rig.tap.leaks(&needles, None).is_empty(), "a clean wire to begin with");
    assert!(rig.kept(&needles, &[]).is_empty(), "a clean store to begin with");

    // A private id in the clear, in a frame's arguments: the hub refuses the
    // verb, which is beside the point; the frame went by.
    let _ = ed.request("show", serde_json::json!({ "id": "hush-001" }));
    let seen = rig.tap.leaks(&needles, Some("ed"));
    assert!(seen.iter().any(|l| l.contains("`hush-001`")), "the tap finds an id planted in a frame: {seen:?}");
    assert!(rig.tap.leaks(&needles, Some("cara")).is_empty(), "the tap is per user: cara sent nothing");

    // A private title inside an object: base64 in the frame, plain in the store.
    let object = Object::Record {
        workspace: WorkspaceId(SHARED.into()),
        subject: Subject::Row(RowId("hush-001".into())),
        entity: Entity::Row(Row {
            workspace: Default::default(),
            id: RowId("hush-001".into()),
            project: ProjectId("hush".into()),
            title: P_TITLE.into(),
            state: RowState::Todo,
            before_block: None,
            seat: None,
            group: None,
            commit: None,
            fields: BTreeMap::new(),
        }),
        parent: None,
        by: Principal::Person("ed".into()),
        at: At(1),
    };
    let (id, bytes) = wsp_objects::encode(&object);
    ed.sync_put(vec![Blob { id, bytes }]).expect("ed may put into shared:core");
    let seen = rig.tap.leaks(&[P_TITLE.to_string()], Some("ed"));
    assert!(seen.iter().any(|l| l.contains(P_TITLE)), "the tap reads the title inside the object's base64: {seen:?}");
    let subjects = [Subject::Row(RowId("hush-001".into()))];
    let kept = rig.kept(&[P_TITLE.to_string()], &subjects);
    assert!(kept.iter().any(|l| l.contains("holds")), "the store scan finds the title: {kept:?}");
    assert!(kept.iter().any(|l| l.contains("is a record of private")), "the store scan finds the subject: {kept:?}");
    let hashed = rig.kept(&[id.hex()], &[]);
    assert!(!hashed.is_empty(), "the store scan finds an object by its hash: {hashed:?}");
    assert!(!rig.tap.leaks(&[id.hex()], Some("ed")).is_empty(), "the tap finds a hash in a frame");
}

#[test]
fn base64_reads_what_a_blob_carries() {
    assert_eq!(unbase64("TW9vbg==").unwrap(), b"Moon");
    assert_eq!(unbase64("TW9vbmw=").unwrap(), b"Moonl");
    assert_eq!(unbase64("TW9vbmxp").unwrap(), b"Moonli");
    assert!(unbase64("not base64!").is_none());
    let blob = serde_json::to_string(&serde_json::json!({ "objects": [{ "bytes": "TW9vbmxpdCBIZWlzdA==" }] })).unwrap();
    assert!(readable(&blob).iter().any(|t| t == "Moonlit Heist"));
}
