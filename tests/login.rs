//! `wsp login`, `wsp logout` and `wsp hub status` as processes (wsp-462), against
//! a real hub over TLS: HOME and WSP_STATE in a temp dir, the hub in this process
//! with a generated CA, the token on stdin. What is checked is what a person or a
//! log reader could see: the file's mode, what the commands print, the command
//! lines of the login and anything it started, and every file the call left in
//! the state directory. The token is in none of them but `hubs.json`.

use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use wsp_hub::{Config, Hub, TlsFiles};
use wsp_server::testing::TestPki;
use wsp_store_contract::{Command as Cmd, IdempotencyKey, Principal, TokenId, UserName};

const WSP: &str = env!("CARGO_BIN_EXE_wsp");

/// A directory of its own, a hub in it over TLS, and the home and state a `wsp` runs in.
struct Rig {
    dir: PathBuf,
    running: Option<wsp_hub::serve::Running>,
    pki: TestPki,
    url: String,
    tokens: BTreeMap<String, String>,
}

impl Rig {
    fn new(name: &str, users: &[&str]) -> Rig {
        // Short on purpose: a sandbox's fake listens on a unix socket under here, and macOS
        // refuses a path past 104 bytes, which `$TMPDIR` (/var/folders/..) alone nearly fills.
        let dir = PathBuf::from("/tmp").join(format!("wsp-li-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("home")).unwrap();
        std::fs::create_dir_all(dir.join("state")).unwrap();
        let data = dir.join("hub-data");
        let mut tokens = BTreeMap::new();
        {
            let hub = Hub::open(&data).unwrap();
            let apply = |cmd: Cmd, by: &Principal| {
                hub.apply(cmd.clone(), IdempotencyKey(format!("{cmd:?}")), by.clone()).expect("the identity machine takes it");
            };
            let root = Principal::Person("root".into());
            apply(Cmd::HubUserAdd { name: UserName("root".into()), admin: true }, &Principal::Wsp("wsp-hub".into()));
            for user in users {
                let (secret, hash) = wsp_machines::token::mint();
                apply(Cmd::HubUserAdd { name: UserName(user.to_string()), admin: false }, &root);
                apply(Cmd::HubTokenIssue { id: TokenId(format!("t-{user}")), user: UserName(user.to_string()), hash }, &root);
                tokens.insert(user.to_string(), secret);
            }
        }
        let pki = TestPki::new(&dir.join("pki"), "hub");
        let cfg = Config {
            data_dir: data,
            bind: vec![],
            port: 0,
            peers: vec!["127.0.0.0/8".into()],
            tls: Some(TlsFiles { cert: pki.cert.clone(), key: pki.key.clone() }),
            name: "wsp-hub".into(),
            lease_days: wsp_hub::DEFAULT_LEASE_DAYS,
        };
        let running = wsp_hub::serve::start(&cfg).expect("the hub starts");
        let url = format!("tls://localhost:{}", running.addrs()[0].port());
        Rig { dir, running: Some(running), pki, url, tokens }
    }

    fn home(&self) -> PathBuf {
        self.dir.join("home")
    }

    fn hubs(&self) -> PathBuf {
        self.home().join(".config/wsp/hubs.json")
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(WSP);
        c.args(args)
            .env("HOME", self.home())
            .env("WSP_STATE", self.dir.join("state"))
            .env("WSP_HOME", self.dir.join("store"))
            .env_remove("WSP_HUB_TOKEN")
            .env_remove("WSP_HUB_URL")
            .env_remove("WSP_HUB_CA")
            .env_remove("XDG_CONFIG_HOME");
        c
    }

    /// Run `wsp args` with `stdin` as its standard input; the command line of the
    /// process and of everything under it, as sampled while it ran, comes back too.
    fn run(&self, args: &[&str], stdin: &str) -> (Output, Vec<String>) {
        let mut child = self.command(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        let pid = child.id();
        child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        let mut seen = Vec::new();
        loop {
            seen.extend(command_lines(pid));
            if child.try_wait().unwrap().is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        (child.wait_with_output().unwrap(), seen)
    }

    fn login(&self, user: &str) -> Output {
        let token = format!("{}\n", self.tokens[user]);
        let ca = self.pki.ca.display().to_string();
        let (out, _) = self.run(&["login", &self.url, "--ca", &ca], &token);
        out
    }

    /// Every file under the state directory and the home but the login itself, with its text.
    fn left_behind(&self) -> Vec<(PathBuf, String)> {
        fn walk(dir: &Path, skip: &Path, out: &mut Vec<(PathBuf, String)>) {
            let Ok(rd) = std::fs::read_dir(dir) else { return };
            for e in rd.flatten() {
                let p = e.path();
                if p == skip {
                    continue;
                }
                if p.is_dir() {
                    walk(&p, skip, out);
                } else if let Ok(text) = std::fs::read(&p) {
                    out.push((p, String::from_utf8_lossy(&text).into_owned()));
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.dir.join("state"), &self.hubs(), &mut out);
        walk(&self.home(), &self.hubs(), &mut out);
        out
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

/// The command line of process `pid` and of every process under it now, as `/proc` has them.
fn command_lines(pid: u32) -> Vec<String> {
    if !Path::new("/proc/self").exists() {
        // No /proc (macOS): `ps` has the same table.
        let Ok(out) = Command::new("ps").args(["-A", "-o", "pid=,ppid=,command="]).output() else { return Vec::new() };
        return String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| {
                let mut f = l.split_whitespace();
                let p = f.next()?.parse::<u32>().ok()?;
                let pp = f.next()?.parse::<u32>().ok()?;
                (p == pid || pp == pid).then(|| f.collect::<Vec<_>>().join(" "))
            })
            .collect();
    }
    let mut pids = vec![pid];
    if let Ok(rd) = std::fs::read_dir("/proc") {
        for e in rd.flatten() {
            let Some(other) = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{other}/stat")) else { continue };
            // The fourth field, after the parenthesised name, is the parent.
            let parent = stat.rsplit_once(')').and_then(|(_, rest)| rest.split_whitespace().nth(1)).and_then(|p| p.parse::<u32>().ok());
            if parent == Some(pid) {
                pids.push(other);
            }
        }
    }
    pids.into_iter()
        .filter_map(|p| std::fs::read(format!("/proc/{p}/cmdline")).ok())
        .map(|b| String::from_utf8_lossy(&b).replace('\0', " "))
        .collect()
}

fn said(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

#[test]
fn login_writes_hubs_json_at_0600_with_the_url_and_user_a_wrong_token_writes_nothing_and_logout_removes_it() {
    let rig = Rig::new("flow", &["ann", "bo"]);

    // A wrong token: refused, nothing written, nothing said of it.
    let ca = rig.pki.ca.display().to_string();
    let (out, _) = rig.run(&["login", &rig.url, "--ca", &ca], "not-a-token-the-hub-issued\n");
    assert_eq!(out.status.code(), Some(1), "{}", said(&out));
    assert!(!said(&out).contains("not-a-token-the-hub-issued"), "{}", said(&out));
    assert!(!rig.hubs().exists(), "a refused login wrote the file");

    // The right one.
    let out = rig.login("ann");
    assert!(out.status.success(), "{}", said(&out));
    assert!(said(&out).contains(&format!("logged in to {} as ann", rig.url)), "{}", said(&out));
    assert_eq!(std::fs::metadata(rig.hubs()).unwrap().permissions().mode() & 0o777, 0o600);
    let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(rig.hubs()).unwrap()).unwrap();
    assert_eq!((doc["url"].as_str(), doc["user"].as_str()), (Some(rig.url.as_str()), Some("ann")));
    assert_eq!(doc["token"].as_str(), Some(rig.tokens["ann"].as_str()));

    // `hub status` says where and as whom.
    let out = rig.command(&["hub", "status"]).output().unwrap();
    assert!(out.status.success(), "{}", said(&out));
    assert!(said(&out).contains(&rig.url) && said(&out).contains("ann"), "{}", said(&out));
    let out = rig.command(&["hub", "status", "--json"]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!((v["logged_in"].as_bool(), v["user"].as_str(), v["url"].as_str()), (Some(true), Some("ann"), Some(rig.url.as_str())));

    // A second login replaces the first and says so.
    let out = rig.login("bo");
    assert!(out.status.success(), "{}", said(&out));
    assert!(said(&out).contains("replacing the login to") && said(&out).contains("ann"), "{}", said(&out));
    assert!(said(&out).contains("as bo"), "{}", said(&out));
    let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(rig.hubs()).unwrap()).unwrap();
    assert_eq!(doc["user"].as_str(), Some("bo"));

    // A wrong token after that leaves bo's login alone.
    let (out, _) = rig.run(&["login", &rig.url, "--ca", &ca], "nope\n");
    assert_eq!(out.status.code(), Some(1));
    let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(rig.hubs()).unwrap()).unwrap();
    assert_eq!(doc["user"].as_str(), Some("bo"));

    // Logout removes it, says so, and status then says there is none.
    let out = rig.command(&["logout"]).output().unwrap();
    assert!(out.status.success() && said(&out).contains("logged out"), "{}", said(&out));
    assert!(!rig.hubs().exists());
    let out = rig.command(&["hub", "status"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(said(&out).contains("not logged in"), "{}", said(&out));
    let out = rig.command(&["logout"]).output().unwrap();
    assert!(out.status.success() && said(&out).contains("not logged in"), "{}", said(&out));
}

#[test]
fn login_token_is_in_no_command_line_no_output_no_log_and_no_status() {
    let rig = Rig::new("secret", &["ann"]);
    let token = rig.tokens["ann"].clone();
    let ca = rig.pki.ca.display().to_string();

    // The token goes in on stdin: no command line of the login, or of what it started, has it.
    let (out, lines) = rig.run(&["login", &rig.url, "--ca", &ca], &format!("{token}\n"));
    assert!(out.status.success(), "{}", said(&out));
    assert!(!lines.is_empty(), "the login's own command line was not sampled");
    assert!(lines.iter().all(|l| !l.contains(&token)), "the token is on a command line: {lines:?}");
    assert!(!said(&out).contains(&token), "{}", said(&out));

    // Not in what the other two print, nor `--json`.
    for args in [&["hub", "status"][..], &["hub", "status", "--json"], &["logout"]] {
        let out = rig.command(args).output().unwrap();
        assert!(!said(&out).contains(&token), "`wsp {}` printed it: {}", args.join(" "), said(&out));
    }

    // Not in a refusal, whichever way it is refused: a wrong hub, no CA, a token on the command line.
    rig.login("ann");
    let (out, _) = rig.run(&["login", &rig.url], &format!("{token}\n"));
    assert!(!out.status.success() && !said(&out).contains(&token), "{}", said(&out));
    let (out, _) = rig.run(&["login", "tcp://127.0.0.1:1", "--ca", &ca], &format!("{token}\n"));
    assert!(!out.status.success() && !said(&out).contains(&token), "{}", said(&out));
    for args in [vec!["login", rig.url.as_str(), token.as_str()], vec!["login", rig.url.as_str(), "--token", token.as_str()]] {
        let (out, _) = rig.run(&args, "");
        assert_eq!(out.status.code(), Some(2), "{}", said(&out));
        assert!(!said(&out).contains(&token), "{}", said(&out));
    }

    // And in no file the calls left in the state directory or the home, the login itself excepted.
    for (path, text) in rig.left_behind() {
        assert!(!text.contains(&token), "{} holds the token", path.display());
    }
}

#[test]
fn login_over_a_file_wider_than_0600_is_refused_naming_it_and_leaves_it() {
    let rig = Rig::new("wide", &["ann"]);
    std::fs::create_dir_all(rig.hubs().parent().unwrap()).unwrap();
    std::fs::write(rig.hubs(), r#"{"url":"tls://old.invalid:1","token":"old"}"#).unwrap();
    std::fs::set_permissions(rig.hubs(), std::fs::Permissions::from_mode(0o644)).unwrap();
    let out = rig.login("ann");
    assert_eq!(out.status.code(), Some(1), "{}", said(&out));
    assert!(said(&out).contains(&rig.hubs().display().to_string()) && said(&out).contains("0600"), "{}", said(&out));
    assert_eq!(std::fs::read_to_string(rig.hubs()).unwrap(), r#"{"url":"tls://old.invalid:1","token":"old"}"#);
    // Status refuses it too, naming the file.
    let out = rig.command(&["hub", "status"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(said(&out).contains("0600"), "{}", said(&out));
}

/// What `wsp sandbox --fake --name <name>` exported, as the pairs a shell would set.
fn sandbox_exports(rig: &Rig, name: &str) -> Vec<(String, String)> {
    let _ = rig.command(&["init"]).output().unwrap();
    let out = rig.command(&["sandbox", "--fake", "--name", name]).env("WSP_SANDBOX_TETHER", std::process::id().to_string()).output().unwrap();
    assert!(out.status.success(), "{}", said(&out));
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix("export "))
        .filter(|l| !l.contains("$PATH"))
        .filter_map(|l| l.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
        .collect()
}

/// `wsp args` as the sandbox with `exports` runs it, with the same `HOME` as every other.
fn in_sandbox(rig: &Rig, exports: &[(String, String)], args: &[&str], stdin: &str) -> Output {
    let mut c = rig.command(args);
    for (k, v) in exports {
        c.env(k, v);
    }
    let mut child = c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

/// wsp-490: the hub login is kept where a sandbox isolates it. Two sandboxes, one
/// `HOME`, two users logged in against one hub: each says its own user, the
/// second login replaces nothing of the first's, and the `HOME` they share, which
/// stands for the developer's own, never holds a `hubs.json`.
#[test]
fn two_sandboxes_hold_two_logins_and_the_developers_is_untouched() {
    let rig = Rig::new("sandboxes", &["alice", "bob"]);
    let ca = rig.pki.ca.display().to_string();
    let a = sandbox_exports(&rig, "alice-sb");
    let b = sandbox_exports(&rig, "bob-sb");
    for exports in [&a, &b] {
        assert!(exports.iter().any(|(k, _)| k == "WSP_HUB_HOME"), "the exports do not name the hub login's home: {exports:?}");
    }
    let hub_home = |e: &[(String, String)]| e.iter().find(|(k, _)| k == "WSP_HUB_HOME").map(|(_, v)| v.clone()).unwrap();
    assert_ne!(hub_home(&a), hub_home(&b), "two sandboxes share a login directory");

    for (exports, user) in [(&a, "alice"), (&b, "bob")] {
        let out = in_sandbox(&rig, exports, &["login", &rig.url, "--ca", &ca], &format!("{}\n", rig.tokens[user]));
        assert!(out.status.success(), "{}", said(&out));
    }

    for (exports, user, other) in [(&a, "alice", "bob"), (&b, "bob", "alice")] {
        let out = in_sandbox(&rig, exports, &["hub", "status"], "");
        assert!(out.status.success(), "{}", said(&out));
        assert!(said(&out).contains(user) && !said(&out).contains(other), "{user}'s sandbox says: {}", said(&out));
        let out = in_sandbox(&rig, exports, &["whoami"], "");
        assert!(said(&out).contains(user) && !said(&out).contains(other), "{user}'s sandbox says: {}", said(&out));
    }

    assert!(!rig.hubs().exists(), "a sandbox wrote the developer's {}", rig.hubs().display());
    for (exports, user) in [(&a, "alice"), (&b, "bob")] {
        let file = PathBuf::from(hub_home(exports)).join(".config/wsp/hubs.json");
        let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(doc["user"].as_str(), Some(user));
    }
    for name in ["alice-sb", "bob-sb"] {
        let _ = rig.command(&["sandbox", "rm", name]).output();
    }
}
