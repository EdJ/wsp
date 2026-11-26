//! A real hub for tests, run as an external program: the hub is a program of
//! its own repository (`wsp-hub`), and wsp neither contains nor links it.
//! Behind `test-support` (and in this crate's own tests).
//!
//! - `WSP_HUB_BIN=<path to wsp-hub>`: `wsp-hub serve` runs as a child process
//!   on an ephemeral loopback port, its data in a scratch directory.
//! - `WSP_HUB_IMAGE=<image>`: `docker run` of the image, its data in a volume
//!   of its own, its config and a throwaway CA's certificate mounted read-only
//!   from a scratch directory, published on a free port of 127.0.0.1 and
//!   served over TLS (a hub refuses a non-loopback bind without it). The
//!   container and its volume are removed when the [`TestHub`] is dropped, a
//!   panicking test's too.
//!
//! With neither, [`TestHub::start`] answers `None` and the test returns
//! early: one notice per test binary, written past the test harness's
//! capture, names the two variables. With `WSP_HUB_REQUIRED=1` a missing hub
//! fails the test instead (for CI).
//!
//! The test speaks to the hub over the wire alone: its first admin is made
//! with `wsp-hub admin init`, users, workspaces, members and tokens by that
//! admin's identity commands through the spoke's own [`hub_client`], and what
//! a test inspects is read by verbs ([`TestHub::admin`]), raw frames
//! ([`TestHub::raw`]), or the hub's data files ([`TestHub::data_files`]).
//! Time passes in real time: a hub started with [`Setup::lease_secs`] leases
//! for that many seconds and sweeps every second.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};
use wsp_hub_protocol::{
    Answer, ApiToken, Call, Code, Event, EventId, Frame, Hello, IdentityCommand, NoArgs, Outcome, Principal, Reply, ReqId,
    Request, SharedName, State, TokenId, UserName, Verb, PROTOCOL,
};
use wsp_store_contract::IdempotencyKey;

use crate::hub_client::{self, Client, Credential, Silent};

/// The hub's wire, for a test that speaks it in raw frames: the spoke is the one crate that links it.
pub use wsp_hub_protocol as protocol;

/// How long a dial or a reply is waited for.
pub const WAIT: Duration = Duration::from_secs(10);

/// The variable naming the hub binary.
pub const BIN_ENV: &str = "WSP_HUB_BIN";
/// The variable naming the hub's container image.
pub const IMAGE_ENV: &str = "WSP_HUB_IMAGE";
/// The variable that makes a missing hub a failure.
pub const REQUIRED_ENV: &str = "WSP_HUB_REQUIRED";

/// What runs the hub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    Bin(PathBuf),
    Image(String),
}

impl Backend {
    pub fn name(&self) -> &'static str {
        match self {
            Backend::Bin(_) => "binary",
            Backend::Image(_) => "container",
        }
    }
}

/// The backend the environment names, the image before the binary; none when neither is set.
pub fn backend() -> Option<Backend> {
    let set = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    if let Some(image) = set(IMAGE_ENV) {
        return Some(Backend::Image(image));
    }
    set(BIN_ENV).map(|b| Backend::Bin(PathBuf::from(b)))
}

static NOTICE: Once = Once::new();

/// No hub is named: fail under `WSP_HUB_REQUIRED=1`, else say so once for this
/// test binary, past the harness's capture, so that the skip is never silent.
fn no_hub() {
    let test = std::thread::current().name().unwrap_or("?").to_string();
    if std::env::var(REQUIRED_ENV).is_ok_and(|v| v == "1") {
        panic!("{test} needs a hub and none is named: set {BIN_ENV}=<path to wsp-hub> or {IMAGE_ENV}=<image> ({REQUIRED_ENV}=1 is set)");
    }
    NOTICE.call_once(|| {
        let _ = std::io::stderr().write_all(
            format!(
                "\nNOTICE: the tests that need a hub are SKIPPED in this test binary (first: {test}): set {BIN_ENV}=<path to wsp-hub> \
                 or {IMAGE_ENV}=<image> to run them; {REQUIRED_ENV}=1 makes a missing hub a failure.\n"
            )
            .as_bytes(),
        );
    });
}

/// How a test wants its hub.
#[derive(Debug, Clone)]
pub struct Setup {
    /// `admin init --admin root` before it serves.
    pub admin: bool,
    /// TLS even as a binary (a container is always TLS).
    pub tls: bool,
    /// A lease's TTL in seconds, swept every second.
    pub lease_secs: Option<u64>,
    /// Config keys beside the fixture's own.
    pub extra: Map<String, Value>,
}

impl Default for Setup {
    fn default() -> Setup {
        Setup { admin: true, tls: false, lease_secs: None, extra: Map::new() }
    }
}

impl Setup {
    pub fn tls(mut self) -> Setup {
        self.tls = true;
        self
    }

    pub fn no_admin(mut self) -> Setup {
        self.admin = false;
        self
    }

    pub fn lease_secs(mut self, secs: u64) -> Setup {
        self.lease_secs = Some(secs);
        self
    }

    pub fn key(mut self, key: &str, value: Value) -> Setup {
        self.extra.insert(key.into(), value);
        self
    }
}

/// A generated CA and a certificate for `localhost` signed by it, as PEM files.
#[derive(Debug, Clone)]
pub struct Pki {
    pub ca: PathBuf,
    pub cert: PathBuf,
    pub key: PathBuf,
}

impl Pki {
    pub fn new(dir: &Path, name: &str) -> Pki {
        use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair, KeyUsagePurpose};
        std::fs::create_dir_all(dir).expect("the pki directory");
        let ca_key = KeyPair::generate().expect("a CA key");
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("CA params");
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        ca_params.distinguished_name.push(DnType::CommonName, format!("wsp test CA {name}"));
        let ca_cert = ca_params.self_signed(&ca_key).expect("a CA certificate");
        let leaf_key = KeyPair::generate().expect("a leaf key");
        let leaf_params = CertificateParams::new(vec!["localhost".to_string()]).expect("leaf params");
        let issuer = Issuer::from_params(&ca_params, &ca_key);
        let leaf = leaf_params.signed_by(&leaf_key, &issuer).expect("a leaf certificate");
        let write = |file: String, pem: String| {
            let path = dir.join(file);
            std::fs::write(&path, pem).expect("a pem file");
            path
        };
        Pki {
            ca: write(format!("{name}-ca.pem"), ca_cert.pem()),
            cert: write(format!("{name}.pem"), leaf.pem()),
            key: write(format!("{name}-key.pem"), leaf_key.serialize_pem()),
        }
    }
}

enum Inner {
    Bin { child: Option<Child>, bin: PathBuf },
    Image { image: String, container: String, volume: String },
}

/// A hub served for a test. Dropping it stops the hub and removes what it made.
pub struct TestHub {
    dir: PathBuf,
    pub backend: Backend,
    inner: Inner,
    addr: SocketAddr,
    pki: Option<Pki>,
    admin: Option<String>,
    config: PathBuf,
    lease: Option<Duration>,
}

static NEXT: AtomicU64 = AtomicU64::new(0);

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// The token `admin init` printed: its one 64-hex word.
fn token_in(out: &str) -> Option<String> {
    out.split(|c: char| !c.is_ascii_hexdigit()).find(|w| w.len() == 64).map(str::to_string)
}

fn docker(args: &[&str]) -> std::process::Output {
    Command::new("docker").args(args).output().expect("docker runs")
}

/// A user's name as the identity commands carry it.
pub fn user(name: &str) -> UserName {
    UserName(name.into())
}

impl TestHub {
    /// A hub as `setup` asks, from the backend the environment names; `None`
    /// (the test then returns, see the module's doc) when it names none.
    pub fn start(setup: Setup) -> Option<TestHub> {
        let Some(backend) = backend() else {
            no_hub();
            return None;
        };
        Some(TestHub::start_on(backend, setup))
    }

    fn start_on(backend: Backend, setup: Setup) -> TestHub {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let test = std::thread::current().name().unwrap_or("hub").replace("::", "-");
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        // The system's temp directory: a container mounts from it where it cannot from every disk.
        let dir = std::env::temp_dir().join(format!("wsp-testhub-{}-{n}-{nanos}", std::process::id()));
        let etc = dir.join("etc");
        std::fs::create_dir_all(&etc).unwrap();
        let container = matches!(backend, Backend::Image(_));
        let pki = (setup.tls || container).then(|| Pki::new(&etc, "hub"));
        let mut cfg = Map::new();
        if container {
            cfg.insert("data_dir".into(), json!("/var/lib/wsp-hub"));
            cfg.insert("bind".into(), json!(["0.0.0.0"]));
            cfg.insert("port".into(), json!(7489));
            cfg.insert("peers".into(), json!(["0.0.0.0/0", "::/0"]));
            cfg.insert("tls".into(), json!({ "cert": "/etc/wsp-hub/hub.pem", "key": "/etc/wsp-hub/hub-key.pem" }));
            // Through a published port every client is one address: the per-peer cap is the whole cap.
            cfg.insert("max_connections_per_peer".into(), json!(512));
        } else {
            cfg.insert("data_dir".into(), json!(dir.join("data")));
            cfg.insert("port".into(), json!(0));
            cfg.insert("peers".into(), json!(["127.0.0.0/8"]));
            if let Some(pki) = &pki {
                cfg.insert("tls".into(), json!({ "cert": pki.cert, "key": pki.key }));
            }
        }
        cfg.insert("name".into(), json!("wsp-hub"));
        if let Some(secs) = setup.lease_secs {
            cfg.insert("lease_secs".into(), json!(secs));
            cfg.insert("sweep_secs".into(), json!(1));
        }
        for (k, v) in &setup.extra {
            cfg.insert(k.clone(), v.clone());
        }
        let config = etc.join("hub.json");
        std::fs::write(&config, serde_json::to_string_pretty(&Value::Object(cfg)).unwrap()).unwrap();
        let inner = match &backend {
            Backend::Bin(bin) => Inner::Bin { child: None, bin: bin.clone() },
            Backend::Image(image) => {
                use std::os::unix::fs::PermissionsExt;
                // The hub runs as another uid: what it reads from the mount is world-readable (test keys only).
                std::fs::set_permissions(&etc, std::fs::Permissions::from_mode(0o755)).unwrap();
                for f in std::fs::read_dir(&etc).unwrap().flatten() {
                    std::fs::set_permissions(f.path(), std::fs::Permissions::from_mode(0o644)).unwrap();
                }
                let name: String = format!("wsp-spoke-test-{}-{n}-{}", std::process::id(), test.replace('_', "-")).chars().take(60).collect();
                let name = name.trim_end_matches('-').to_string();
                Inner::Image { image: image.clone(), volume: format!("{name}-data"), container: name }
            }
        };
        let addr = match &backend {
            Backend::Image(_) => format!("127.0.0.1:{}", free_port()).parse().unwrap(),
            Backend::Bin(_) => "127.0.0.1:1".parse().unwrap(),
        };
        let lease = setup.lease_secs.map(Duration::from_secs);
        let mut hub = TestHub { dir, backend, inner, addr, pki, admin: None, config, lease };
        if setup.admin {
            let (ok, out, err) = hub.admin_init("root");
            assert!(ok, "wsp-hub admin init: {out}{err}");
            hub.admin = Some(token_in(&out).expect("admin init printed a token"));
        }
        hub.up();
        hub
    }

    /// `wsp-hub admin init --admin <name>` on this hub's config: success, stdout, stderr.
    pub fn admin_init(&self, name: &str) -> (bool, String, String) {
        let out = match &self.inner {
            Inner::Bin { bin, .. } => Command::new(bin).args(["admin", "init", "--config"]).arg(&self.config).args(["--admin", name]).output(),
            Inner::Image { image, volume, .. } => Command::new("docker")
                .args(["run", "--rm", "-v", &format!("{volume}:/var/lib/wsp-hub"), "-v", &format!("{}:/etc/wsp-hub:ro", self.etc().display()), image])
                .args(["admin", "init", "--config", "/etc/wsp-hub/hub.json", "--admin", name])
                .output(),
        }
        .expect("wsp-hub admin init runs");
        (out.status.success(), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
    }

    fn etc(&self) -> PathBuf {
        self.dir.join("etc")
    }

    /// Starts serving, and waits until it answers.
    fn up(&mut self) {
        let config = self.config.clone();
        let stderr = self.dir.join("hub.stderr");
        match &mut self.inner {
            Inner::Bin { child, bin } => {
                let err = std::fs::OpenOptions::new().create(true).append(true).open(&stderr).unwrap();
                let mut c = Command::new(&*bin)
                    .args(["serve", "--config"])
                    .arg(&config)
                    .stdout(Stdio::piped())
                    .stderr(err)
                    .spawn()
                    .unwrap_or_else(|e| panic!("{} serve: {e}", bin.display()));
                let mut out = BufReader::new(c.stdout.take().unwrap());
                let mut line = String::new();
                out.read_line(&mut line).unwrap();
                assert!(line.starts_with("wsp-hub: listening on 127.0.0.1:"), "the hub's start line: {line:?}; stderr: {}", std::fs::read_to_string(&stderr).unwrap_or_default());
                // What it prints later is read and dropped, so the pipe never fills.
                std::thread::spawn(move || {
                    let mut sink = Vec::new();
                    let _ = out.read_to_end(&mut sink);
                });
                let first = line.trim_start_matches("wsp-hub: listening on ").split(|ch: char| ch == ' ' || ch == ';' || ch == ',').next().unwrap().trim().to_string();
                self.addr = first.parse().unwrap_or_else(|e| panic!("{first:?}: {e}"));
                *child = Some(c);
            }
            Inner::Image { image, container, volume } => {
                let out = docker(&[
                    "run",
                    "-d",
                    "--name",
                    container,
                    "-p",
                    &format!("127.0.0.1:{}:7489", self.addr.port()),
                    "-v",
                    &format!("{volume}:/var/lib/wsp-hub"),
                    "-v",
                    &format!("{}:/etc/wsp-hub:ro", self.dir.join("etc").display()),
                    image,
                ]);
                assert!(out.status.success(), "docker run: {}", String::from_utf8_lossy(&out.stderr));
                self.wait_listening(1);
            }
        }
    }

    /// Waits for the container's `n`th start line, then for its port to take a TLS session.
    fn wait_listening(&self, n: usize) {
        let Inner::Image { container, .. } = &self.inner else { return };
        let until = Instant::now() + Duration::from_secs(60);
        loop {
            let logs = docker(&["logs", container]);
            let text = String::from_utf8_lossy(&logs.stdout).into_owned() + &String::from_utf8_lossy(&logs.stderr);
            if text.matches("listening on").count() >= n {
                break;
            }
            assert!(Instant::now() < until, "the container never listened: {text}");
            std::thread::sleep(Duration::from_millis(100));
        }
        while Line::dial(self.addr, self.ca(), Duration::from_millis(500)).is_err() {
            assert!(Instant::now() < until, "the published port never answered");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Ends it the hard way, as a crash does, and starts it again on the same data.
    pub fn restart(&mut self) {
        match &mut self.inner {
            Inner::Bin { child, .. } => {
                if let Some(mut c) = child.take() {
                    let _ = c.kill();
                    let _ = c.wait();
                }
            }
            Inner::Image { container, .. } => {
                let starts = String::from_utf8_lossy(&docker(&["logs", container]).stdout).matches("listening on").count();
                // A hub already stopped is started all the same.
                let _ = docker(&["kill", container]);
                let out = docker(&["start", container]);
                assert!(out.status.success(), "docker start: {}", String::from_utf8_lossy(&out.stderr));
                self.wait_listening(starts + 1);
                return;
            }
        }
        self.up();
    }

    /// Stops it; a later [`TestHub::restart`] starts it again on the same data.
    pub fn stop(&mut self) {
        match &mut self.inner {
            Inner::Bin { child, .. } => {
                if let Some(mut c) = child.take() {
                    let _ = c.kill();
                    let _ = c.wait();
                }
            }
            Inner::Image { container, .. } => {
                let _ = docker(&["kill", container]);
            }
        }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The URL a spoke dials: `tls://localhost:<port>` or `tcp://127.0.0.1:<port>`.
    pub fn url(&self) -> String {
        match &self.pki {
            Some(_) => format!("tls://localhost:{}", self.addr.port()),
            None => format!("tcp://{}", self.addr),
        }
    }

    /// The CA a client pins, for a hub served over TLS.
    pub fn ca(&self) -> Option<&Path> {
        self.pki.as_ref().map(|p| p.ca.as_path())
    }

    /// The throwaway PKI of a hub served over TLS (a test's own TLS front may serve its certificate).
    pub fn pki(&self) -> Option<&Pki> {
        self.pki.as_ref()
    }

    /// A scratch directory the hub's own files are beside, removed with it.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Takes `token` as the admin's: a test that ran `admin init` itself ([`Setup::no_admin`]).
    pub fn adopt_admin(&mut self, token: &str) {
        self.admin = Some(token.to_string());
    }

    /// The token `admin init` printed for `root`.
    pub fn admin_token(&self) -> &str {
        self.admin.as_deref().expect("a hub started with an admin")
    }

    /// The lease TTL the hub was started with.
    pub fn lease(&self) -> Option<Duration> {
        self.lease
    }

    /// Waits until every lease taken so far has run past its TTL and been swept.
    pub fn pass_ttl(&self) {
        std::thread::sleep(self.lease.expect("a hub started with lease_secs") + Duration::from_millis(2500));
    }

    /// A spoke's client of this hub, dialled with `token`.
    pub fn client(&self, token: &str) -> Client {
        self.try_client(token, WAIT).unwrap_or_else(|e| panic!("the hub at {} answers: {e}", self.url()))
    }

    /// [`TestHub::client`] with the dial's failure handed back.
    pub fn try_client(&self, token: &str, wait: Duration) -> Result<Client, Silent> {
        Client::dial_hub_ca(&self.url(), Credential::User(token.into()), self.ca(), wait)
    }

    /// The admin's client.
    pub fn admin(&self) -> Client {
        self.client(self.admin_token())
    }

    /// Workspaces, and users each with a token (its record's id `t-<user>`) and
    /// the workspaces it is a member of, made by the admin over the wire; each
    /// user's token by name.
    pub fn seed(&self, workspaces: &[&str], users: &[(&str, &[&str])]) -> BTreeMap<String, String> {
        let mut root = self.admin();
        for w in workspaces {
            apply(&mut root, IdentityCommand::HubWorkspaceAdd { name: SharedName(w.to_string()) });
        }
        let mut tokens = BTreeMap::new();
        for (name, member_of) in users {
            apply(&mut root, IdentityCommand::HubUserAdd { name: user(name), admin: false });
            for w in *member_of {
                apply(&mut root, IdentityCommand::HubMemberSet { workspace: SharedName(w.to_string()), user: user(name), member: true });
            }
            tokens.insert(name.to_string(), issue_as(&mut root, name, &format!("t-{name}")));
        }
        tokens
    }

    /// An identity command, applied by the admin; panics when it is refused.
    pub fn apply(&self, cmd: IdentityCommand) {
        apply(&mut self.admin(), cmd);
    }

    /// `users`, each a member of every one of `workspaces` (made first): their tokens.
    pub fn seed_members(&self, workspaces: &[&str], users: &[&str]) -> BTreeMap<String, String> {
        let all: Vec<(&str, &[&str])> = users.iter().map(|u| (*u, workspaces)).collect();
        self.seed(workspaces, &all)
    }

    /// A new token for `name`, issued by the admin.
    pub fn issue(&self, name: &str) -> String {
        issue(&mut self.admin(), name)
    }

    /// The lease on shared row `row`, as the admin's state has it: its holder and token.
    pub fn lease_of(&self, row: &str) -> Option<(Principal, wsp_hub_protocol::Token)> {
        let needle = format!("\"{row}\"");
        self.state().leases.into_values().find(|l| l.name.0.contains(&needle)).map(|l| (l.holder, l.token))
    }

    /// The hub's state, as its admin reads it.
    pub fn state(&self) -> State {
        self.admin().state().expect("the admin reads the state").0
    }

    /// The events after `from`, as the admin's `events` reads them (one page).
    pub fn events_after(&self, from: EventId) -> Vec<Event> {
        let mut root = self.admin();
        match root.request(Call::Verb(Verb::Events { from, limit: None })).expect("events").outcome {
            Outcome::Ok(Answer::Events { events, .. }) => events,
            other => panic!("events: {other:?}"),
        }
    }

    /// A login file under `home` for `user` with `token`, as `wsp login` writes it.
    pub fn login(&self, home: &Path, user: &str, token: &str) {
        use std::os::unix::fs::PermissionsExt;
        let file = home.join(hub_client::HUBS_FILE);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let mut doc = json!({ "url": self.url(), "token": token, "user": user, "at": "2026-11-23T00:00:00Z" });
        if let Some(ca) = self.ca() {
            doc["ca"] = json!(ca.display().to_string());
        }
        std::fs::write(&file, doc.to_string()).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    /// A file of the data directory, as text (empty when there is none).
    pub fn data_file(&self, name: &str) -> String {
        match &self.inner {
            Inner::Image { container, .. } => {
                let out = docker(&["exec", container, "cat", &format!("/var/lib/wsp-hub/{name}")]);
                String::from_utf8_lossy(&out.stdout).into_owned()
            }
            Inner::Bin { .. } => std::fs::read_to_string(self.dir.join("data").join(name)).unwrap_or_default(),
        }
    }

    /// The hub's log, `api.log`.
    pub fn log(&self) -> String {
        self.data_file("api.log")
    }

    /// The bootstrap token, `api.token`.
    pub fn bootstrap(&self) -> String {
        self.data_file("api.token").trim().to_string()
    }

    /// Every file the hub keeps in its data directory, by its path under it, with its bytes.
    pub fn data_files(&self) -> Vec<(PathBuf, Vec<u8>)> {
        let root = match &self.inner {
            Inner::Bin { .. } => self.dir.join("data"),
            Inner::Image { container, .. } => {
                let to = self.dir.join("copied");
                let _ = std::fs::remove_dir_all(&to);
                let out = docker(&["cp", &format!("{container}:/var/lib/wsp-hub"), &to.display().to_string()]);
                assert!(out.status.success(), "docker cp: {}", String::from_utf8_lossy(&out.stderr));
                to
            }
        };
        let mut out = Vec::new();
        every_file(&root, &mut out);
        out.into_iter().filter_map(|p| std::fs::read(&p).ok().map(|b| (p.strip_prefix(&root).unwrap().to_path_buf(), b))).collect()
    }

    /// A connection that has said nothing yet.
    pub fn dial(&self) -> Line {
        Line::dial(self.addr, self.ca(), WAIT).expect("the hub accepts")
    }

    /// A connection welcomed with `token`, whose hello says `hello_by`; the
    /// hub's refusal otherwise.
    pub fn raw(&self, token: &str, hello_by: Principal) -> Result<Wire, (Code, String)> {
        let mut line = self.dial();
        let hello = Hello { token: ApiToken(token.into()), by: hello_by, client: "test".into(), protocol: PROTOCOL };
        line.send(&Frame::Hello(hello)).map_err(|e| (Code::Unknown, e))?;
        match line.recv(WAIT) {
            Ok(Some(Frame::Welcome(w))) => Ok(Wire { line, by: w.by.expect("welcome names who"), head: w.head, next: 1 }),
            Ok(Some(Frame::Refused(r))) => Err((r.code, r.text)),
            other => Err((Code::Unknown, format!("{other:?}"))),
        }
    }
}

fn apply(client: &mut Client, cmd: IdentityCommand) {
    client.apply(&cmd).unwrap_or_else(|e| panic!("{cmd:?}: {e}"));
}

fn issue(root: &mut Client, name: &str) -> String {
    let id = format!("t-{name}-{}", NEXT.fetch_add(1, Ordering::SeqCst));
    issue_as(root, name, &id)
}

fn issue_as(root: &mut Client, name: &str, id: &str) -> String {
    let (secret, hash) = hub_client::token::mint();
    apply(root, IdentityCommand::HubTokenIssue { id: TokenId(id.into()), user: user(name), hash });
    secret
}

fn every_file(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            every_file(&p, out);
        } else {
            out.push(p);
        }
    }
}

impl Drop for TestHub {
    fn drop(&mut self) {
        match &mut self.inner {
            Inner::Bin { child, .. } => {
                if let Some(mut c) = child.take() {
                    let _ = c.kill();
                    let _ = c.wait();
                }
            }
            Inner::Image { container, volume, .. } => {
                let _ = docker(&["rm", "-f", container]);
                let _ = docker(&["volume", "rm", "-f", volume]);
            }
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

// ---------------------------------------------------------------- the raw wire

/// A welcomed connection spoken to in frames, as any client may (a test that
/// says another `by` than the real client ever does).
pub struct Wire {
    pub line: Line,
    /// Who the hub's `welcome` named.
    pub by: Principal,
    pub head: EventId,
    next: u64,
}

impl Wire {
    /// `call` as `by`: its reply, or none when the connection ended first.
    pub fn ask_as(&mut self, by: &Principal, call: Call) -> Option<Reply> {
        let id = ReqId(self.next);
        self.next += 1;
        let key = matches!(call, Call::Command(_)).then(|| IdempotencyKey(format!("raw-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst))));
        // A write to a connection the hub cut may fail: that is an ended connection too.
        self.line.send(&Frame::Request(Request { v: PROTOCOL, id, key, by: by.clone(), call })).ok()?;
        let until = Instant::now() + WAIT;
        while Instant::now() < until {
            match self.line.recv(WAIT) {
                Ok(Some(Frame::Reply(r))) if r.id == id => return Some(r),
                Ok(Some(_)) => {}
                Ok(None) => return None,
                Err(_) => return None,
            }
        }
        None
    }

    /// `call` as the connection's own principal: its outcome.
    pub fn ask(&mut self, call: Call) -> Option<Outcome> {
        let by = self.by.clone();
        self.ask_as(&by, call).map(|r| r.outcome)
    }

    /// The hub's state, as this connection may read it.
    pub fn state(&mut self) -> Option<Outcome> {
        self.ask(Call::Verb(Verb::State(NoArgs {})))
    }
}

enum Io {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
    Served(Box<rustls::StreamOwned<rustls::ServerConnection, TcpStream>>),
}

impl Io {
    fn sock(&self) -> &TcpStream {
        match self {
            Io::Plain(s) => s,
            Io::Tls(s) => &s.sock,
            Io::Served(s) => &s.sock,
        }
    }
}

/// One end of a connection carrying lines (frames as JSON, one a line): plain
/// TCP, a TLS client checked against a CA, or a TLS server end with a test
/// certificate ([`Line::accept_tls`], for a test's own front).
pub struct Line {
    io: Io,
    partial: Vec<u8>,
}

fn client_config(ca: &Path) -> Result<Arc<rustls::ClientConfig>, String> {
    use rustls::pki_types::pem::PemObject;
    let mut roots = rustls::RootCertStore::empty();
    let certs = rustls::pki_types::CertificateDer::pem_file_iter(ca).map_err(|e| e.to_string())?.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
    roots.add_parsable_certificates(certs);
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// A TLS server's config for the certificate and key of `pki`.
pub fn server_config(pki: &Pki) -> Arc<rustls::ServerConfig> {
    use rustls::pki_types::pem::PemObject;
    let certs = rustls::pki_types::CertificateDer::pem_file_iter(&pki.cert).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
    let key = rustls::pki_types::PrivateKeyDer::from_pem_file(&pki.key).unwrap();
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();
    Arc::new(config)
}

impl Line {
    /// Dial `addr` in `wait`, inside TLS checked against `ca` (for `localhost`) when one is given.
    pub fn dial(addr: SocketAddr, ca: Option<&Path>, wait: Duration) -> Result<Line, String> {
        let tcp = TcpStream::connect_timeout(&addr, wait).map_err(|e| e.to_string())?;
        tcp.set_nodelay(true).map_err(|e| e.to_string())?;
        let io = match ca {
            None => Io::Plain(tcp),
            Some(ca) => {
                let conn = rustls::ClientConnection::new(client_config(ca)?, rustls::pki_types::ServerName::try_from("localhost").unwrap()).map_err(|e| e.to_string())?;
                let mut s = rustls::StreamOwned::new(conn, tcp);
                s.sock.set_read_timeout(Some(wait)).map_err(|e| e.to_string())?;
                while s.conn.is_handshaking() {
                    s.conn.complete_io(&mut s.sock).map_err(|e| e.to_string())?;
                }
                Io::Tls(Box::new(s))
            }
        };
        Ok(Line { io, partial: Vec::new() })
    }

    /// Dial `host:port` text (a URL's address part).
    pub fn dial_text(addr: &str, ca: Option<&Path>, wait: Duration) -> Result<Line, String> {
        let mut last = format!("{addr}: no address");
        for a in addr.to_socket_addrs().map_err(|e| e.to_string())? {
            match Line::dial(a, ca, wait) {
                Ok(l) => return Ok(l),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    /// The server end of an accepted `stream`, its TLS session ended here with `config`.
    pub fn accept_tls(stream: TcpStream, config: Arc<rustls::ServerConfig>) -> Result<Line, String> {
        let conn = rustls::ServerConnection::new(config).map_err(|e| e.to_string())?;
        let mut s = rustls::StreamOwned::new(conn, stream);
        s.sock.set_read_timeout(Some(WAIT)).map_err(|e| e.to_string())?;
        while s.conn.is_handshaking() {
            s.conn.complete_io(&mut s.sock).map_err(|e| e.to_string())?;
        }
        Ok(Line { io: Io::Served(Box::new(s)), partial: Vec::new() })
    }

    /// Writes `text` as it is (a line ends with its own `\n`).
    pub fn send_raw(&mut self, text: &[u8]) -> Result<(), String> {
        let r = match &mut self.io {
            Io::Plain(s) => s.write_all(text).and_then(|_| s.flush()),
            Io::Tls(s) => s.write_all(text).and_then(|_| s.flush()),
            Io::Served(s) => s.write_all(text).and_then(|_| s.flush()),
        };
        r.map_err(|e| e.to_string())
    }

    /// Writes one frame as one line.
    pub fn send(&mut self, frame: &Frame) -> Result<(), String> {
        let mut line = serde_json::to_vec(frame).map_err(|e| e.to_string())?;
        line.push(b'\n');
        self.send_raw(&line)
    }

    /// The next whole line, without its `\n`, waiting up to `wait`; `Ok(None)` when none came.
    pub fn recv_line(&mut self, wait: Duration) -> Result<Option<Vec<u8>>, String> {
        let deadline = Instant::now() + wait;
        let mut chunk = [0u8; 16384];
        loop {
            if let Some(at) = self.partial.iter().position(|b| *b == b'\n') {
                let mut line: Vec<u8> = self.partial.drain(..=at).collect();
                line.pop();
                return Ok(Some(line));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            self.io.sock().set_read_timeout(Some(left.max(Duration::from_millis(1)))).map_err(|e| e.to_string())?;
            let read = match &mut self.io {
                Io::Plain(s) => s.read(&mut chunk),
                Io::Tls(s) => s.read(&mut chunk),
                Io::Served(s) => s.read(&mut chunk),
            };
            match read {
                Ok(0) => return Err("closed".into()),
                Ok(n) => self.partial.extend_from_slice(&chunk[..n]),
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted) => {}
                Err(e) => return Err(e.to_string()),
            }
        }
    }

    /// The next frame, waiting up to `wait`.
    pub fn recv(&mut self, wait: Duration) -> Result<Option<Frame>, String> {
        match self.recv_line(wait)? {
            Some(line) => serde_json::from_slice(&line).map(Some).map_err(|e| format!("{e}: {}", String::from_utf8_lossy(&line))),
            None => Ok(None),
        }
    }

    pub fn shutdown(&self) {
        let _ = self.io.sock().shutdown(std::net::Shutdown::Both);
    }
}

// ---------------------------------------------------------------- a tap

/// What a [`Tap`] passed on: `> ` before a line the client sent, `< ` before one the hub sent.
pub type Lines = Arc<std::sync::Mutex<Vec<String>>>;

/// A front a spoke dials in place of the hub: it ends the spoke's session
/// itself (TLS with the hub's own test certificate when the hub is TLS),
/// writes down every line both ways in the clear, and carries each on to the
/// hub. Pointed at no hub, it takes a connection and closes it, as a stopped
/// hub's port would; pointed at a restarted hub, the spoke's URL stays the same.
pub struct Tap {
    addr: SocketAddr,
    ca: Option<PathBuf>,
    upstream: Arc<std::sync::Mutex<Option<(SocketAddr, Option<PathBuf>)>>>,
    lines: Lines,
    open: Arc<std::sync::Mutex<Vec<TcpStream>>>,
}

impl Tap {
    /// A tap in front of `hub`.
    pub fn start(hub: &TestHub) -> Tap {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let tls = hub.pki().map(server_config);
        let upstream = Arc::new(std::sync::Mutex::new(Some((hub.addr(), hub.ca().map(Path::to_path_buf)))));
        let lines: Lines = Arc::default();
        let open: Arc<std::sync::Mutex<Vec<TcpStream>>> = Arc::default();
        let (up, seen, held) = (upstream.clone(), lines.clone(), open.clone());
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(stream) = conn else { continue };
                {
                    // Only the connections still open: a clone held past its end keeps the descriptor.
                    let mut held = held.lock().unwrap();
                    held.retain(|s| s.peer_addr().is_ok());
                    if let Ok(c) = stream.try_clone() {
                        held.push(c);
                    }
                }
                let to = up.lock().unwrap().clone();
                let (tls, seen) = (tls.clone(), seen.clone());
                std::thread::spawn(move || {
                    let Some((hub, ca)) = to else {
                        let _ = stream.shutdown(std::net::Shutdown::Both);
                        return;
                    };
                    let client = match tls {
                        Some(config) => Line::accept_tls(stream, config),
                        None => Ok(Line { io: Io::Plain(stream), partial: Vec::new() }),
                    };
                    let Ok(mut client) = client else { return };
                    let Ok(mut up) = Line::dial(hub, ca.as_deref(), WAIT) else {
                        client.shutdown();
                        return;
                    };
                    pump(&mut client, &mut up, &seen);
                    client.shutdown();
                    up.shutdown();
                });
            }
        });
        Tap { addr, ca: hub.ca().map(Path::to_path_buf), upstream, lines, open }
    }

    /// No hub behind it, and every connection it holds closed.
    pub fn cut(&self) {
        self.point_at(None);
        for c in self.open.lock().unwrap().drain(..) {
            let _ = c.shutdown(std::net::Shutdown::Both);
        }
    }

    /// Carries on to `hub` from now on (a restarted one), or to nothing.
    pub fn point_at(&self, hub: Option<&TestHub>) {
        *self.upstream.lock().unwrap() = hub.map(|h| (h.addr(), h.ca().map(Path::to_path_buf)));
    }

    /// The URL a spoke dials to reach the hub through the tap.
    pub fn url(&self) -> String {
        match &self.ca {
            Some(_) => format!("tls://localhost:{}", self.addr.port()),
            None => format!("tcp://{}", self.addr),
        }
    }

    /// The CA the spoke pins (the hub's own), when it is TLS.
    pub fn ca(&self) -> Option<&Path> {
        self.ca.as_deref()
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Every line that passed, in order, each `> ` (from the client) or `< ` (from the hub).
    pub fn lines(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }

    /// The lines the client sent, without their mark.
    pub fn sent(&self) -> Vec<String> {
        self.lines().into_iter().filter_map(|l| l.strip_prefix("> ").map(str::to_string)).collect()
    }

    /// Everything that passed both ways, as one text.
    pub fn wire(&self) -> String {
        self.lines().join("\n")
    }

    /// The shared record, for a test that wants to read it from another thread.
    pub fn record(&self) -> Lines {
        self.lines.clone()
    }
}

/// Carries lines both ways between `client` and `hub` until either ends, recording each.
fn pump(client: &mut Line, hub: &mut Line, seen: &Lines) {
    let step = Duration::from_millis(5);
    loop {
        match client.recv_line(step) {
            Ok(Some(line)) => {
                seen.lock().unwrap().push(format!("> {}", String::from_utf8_lossy(&line)));
                let mut l = line;
                l.push(b'\n');
                if hub.send_raw(&l).is_err() {
                    return;
                }
            }
            Ok(None) => {}
            Err(_) => return,
        }
        match hub.recv_line(step) {
            Ok(Some(line)) => {
                seen.lock().unwrap().push(format!("< {}", String::from_utf8_lossy(&line)));
                let mut l = line;
                l.push(b'\n');
                if client.send_raw(&l).is_err() {
                    return;
                }
            }
            Ok(None) => {}
            Err(_) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixture itself: a hub starts, is seeded over the wire, answers as
    /// the users it made, keeps them across a restart, and writes its log.
    #[test]
    fn testhub_starts_seeds_answers_and_survives_a_restart() {
        let Some(mut hub) = TestHub::start(Setup::default()) else { return };
        let tokens = hub.seed_members(&["team"], &["ann"]);
        let mut ann = hub.client(&tokens["ann"]);
        assert_eq!(ann.whoami().unwrap().0, Principal::Person("ann".into()));
        let raw = hub.raw(&tokens["ann"], Principal::Person("someone".into())).expect("welcomed");
        assert_eq!(raw.by, Principal::Person("ann".into()));
        hub.restart();
        assert_eq!(hub.client(&tokens["ann"]).whoami().unwrap().0, Principal::Person("ann".into()));
        assert!(hub.state().users.contains_key(&user("ann")));
        assert!(!hub.log().is_empty(), "the hub writes api.log");
        assert!(hub.data_files().iter().any(|(p, _)| p.starts_with("objects") || p.ends_with("api.log")));
    }
}
