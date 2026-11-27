//! `wsp login`, `wsp logout` and `wsp hub status` as processes (wsp-462), against
//! a real hub over TLS: HOME and WSP_STATE in a temp dir, the hub an external
//! program (`wsp_spoke::testhub`: `WSP_HUB_BIN` or `WSP_HUB_IMAGE`) with a
//! generated CA, the token on stdin. What is checked is what a person or a
//! log reader could see: the file's mode, what the commands print, the command
//! lines of the login and anything it started, and every file the call left in
//! the state directory. The token is in none of them but `hubs.json`.

use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use wsp_spoke::testhub::{Setup, TestHub};

const WSP: &str = env!("CARGO_BIN_EXE_wsp");

/// A directory of its own, a real hub over TLS (`testhub`), and the home and state a `wsp` runs in.
struct Rig {
    dir: PathBuf,
    _hub: TestHub,
    ca: PathBuf,
    url: String,
    tokens: BTreeMap<String, String>,
}

impl Rig {
    /// `None` when no hub is named: the test is skipped (`wsp_spoke::testhub`).
    fn new(name: &str, users: &[&str]) -> Option<Rig> {
        let hub = TestHub::start(Setup::default().tls())?;
        // A sandbox's fake listens on a unix socket under here, and macOS refuses a path past
        // 104 bytes: `.cargo/test-run` keeps `$TMPDIR` short.
        let dir = std::env::temp_dir().join(format!("wsp-li-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("home")).unwrap();
        std::fs::create_dir_all(dir.join("state")).unwrap();
        let all: Vec<(&str, &[&str])> = users.iter().map(|u| (*u, &[][..])).collect();
        let tokens = hub.seed(&[], &all);
        let (ca, url) = (hub.ca().unwrap().to_path_buf(), hub.url());
        Some(Rig { dir, _hub: hub, ca, url, tokens })
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
        let ca = self.ca.display().to_string();
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
    let Some(rig) = Rig::new("flow", &["ann", "bo"]) else { return };

    // A wrong token: refused, nothing written, nothing said of it.
    let ca = rig.ca.display().to_string();
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
    let Some(rig) = Rig::new("secret", &["ann"]) else { return };
    let token = rig.tokens["ann"].clone();
    let ca = rig.ca.display().to_string();

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
    let Some(rig) = Rig::new("wide", &["ann"]) else { return };
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
    let Some(rig) = Rig::new("sandboxes", &["alice", "bob"]) else { return };
    let ca = rig.ca.display().to_string();
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

/// `wsp hub disable --dry-run` and `wsp logout --dry-run` (and `-n`) reach the
/// verbs through the binary's dry-run gate (`dry_run` in `src/main.rs`), print
/// the preview `docs/multiplayer.md` documents ("Park preview"), and change
/// nothing: the login, the switch, the state directory and the home are as
/// they were. Not logged in, `logout -n` says so in the same shape, and
/// `hub disable -n` is the verb's own refusal, not the gate's.
#[test]
fn hub_disable_and_logout_dry_run_print_the_preview_through_the_cli_and_change_nothing() {
    let Some(rig) = Rig::new("dryrun", &["ann"]) else { return };
    let run = |args: &[&str]| rig.command(args).output().unwrap();
    let json = |o: &Output| -> serde_json::Value {
        serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("not JSON ({e}): {}", said(o)))
    };
    let keys = |v: &serde_json::Value| v.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
    let documented = ["dry_run", "ending", "leases", "logged_in", "off", "park", "unsent"];

    // Not logged in.
    for flag in ["--dry-run", "-n"] {
        let out = run(&["logout", flag, "--json"]);
        assert_eq!(out.status.code(), Some(0), "{}", said(&out));
        let v = json(&out);
        assert_eq!(keys(&v), documented, "{v}");
        assert_eq!(v, serde_json::json!({"dry_run": true, "logged_in": false, "off": false, "park": [], "ending": [], "leases": [], "unsent": 0}));
        let out = run(&["logout", flag]);
        assert_eq!((out.status.code(), String::from_utf8_lossy(&out.stdout).trim()), (Some(0), "dry run: not logged in, so nothing would be disabled"), "{}", said(&out));
        let out = run(&["hub", "disable", flag, "--json"]);
        assert!(!said(&out).contains("does not look first"), "the gate refused it: {}", said(&out));
        assert_eq!(out.status.code(), Some(1), "{}", said(&out));
        assert!(said(&out).contains("not logged in to a hub"), "{}", said(&out));
    }
    assert!(!rig.hubs().exists());

    // Logged in.
    assert!(rig.login("ann").status.success());
    let login = std::fs::read(rig.hubs()).unwrap();
    // Every call appends its own line to `calls.jsonl` (the call tally, `wsp calls`); nothing else may change.
    let left = || rig.left_behind().into_iter().filter(|(p, _)| !p.ends_with("calls.jsonl")).collect::<Vec<_>>();
    let before = left();
    for args in [&["hub", "disable", "--dry-run", "--json"][..], &["hub", "disable", "-n", "--json"], &["logout", "--dry-run", "--json"], &["logout", "-n", "--json"]] {
        let out = run(args);
        assert_eq!(out.status.code(), Some(0), "wsp {}: {}", args.join(" "), said(&out));
        let v = json(&out);
        assert_eq!(keys(&v), documented, "{v}");
        assert_eq!(v, serde_json::json!({"dry_run": true, "logged_in": true, "off": false, "park": [], "ending": [], "leases": [], "unsent": 0}), "wsp {}", args.join(" "));
    }
    for args in [&["hub", "disable", "--dry-run"][..], &["logout", "-n"]] {
        let out = run(args);
        assert_eq!(out.status.code(), Some(0), "{}", said(&out));
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.starts_with("dry run: nothing is done\nwould park: nothing"), "wsp {}: {text}", args.join(" "));
    }
    assert_eq!(std::fs::read(rig.hubs()).unwrap(), login, "the login is as it was");
    assert_eq!(left(), before, "a dry run left something behind");
    let v = json(&run(&["hub", "status", "--json"]));
    assert_eq!((v["logged_in"].as_bool(), v["user"].as_str()), (Some(true), Some("ann")), "{v}");
    assert_ne!(v["off"].as_bool(), Some(true), "the dry run switched the hub off: {v}");
}

/// The admin verbs as a person (or the app) runs them, against a real hub:
/// what each prints with `--json`, its exit code, and its refusals as
/// sentences with a stable `{"error": {"code", "message", …}}`. Run with
/// `--nocapture` to see every command and its output (the examples in
/// docs/multiplayer.md are these). A token a verb mints is printed once, as
/// `token.token`, and is in no file the calls leave behind, `calls.jsonl`
/// (which records each call's verb and flag names only) included.
#[test]
fn hub_admin_verbs_print_fields_refuse_in_words_and_keep_no_token() {
    use wsp_spoke::testhub::protocol::IdentityCommand;
    use wsp_spoke::testhub::user;
    let Some(rig) = Rig::new("admin", &[]) else { return };
    let run = |args: &[&str]| {
        let out = rig.command(args).output().unwrap();
        eprintln!("$ wsp {}\n[exit {}]\n{}{}", args.join(" "), out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        out
    };
    let json = |o: &Output| -> serde_json::Value { serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("not JSON ({e}): {}", said(o))) };

    // Not logged in: refused before anything is dialled, in words and as a code.
    let out = run(&["hub", "user", "ls", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(json(&out)["error"]["code"], "not_logged_in", "{}", said(&out));
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("wsp: hub: not logged in"), "{}", said(&out));

    // ann, an admin, logs in.
    rig._hub.apply(IdentityCommand::HubUserAdd { name: user("ann"), admin: true });
    let ann = rig._hub.issue("ann");
    let ca = rig.ca.display().to_string();
    let (out, _) = rig.run(&["login", &rig.url, "--ca", &ca], &format!("{ann}\n"));
    assert!(out.status.success(), "{}", said(&out));
    let status = json(&run(&["hub", "status", "--json"]));
    assert_eq!((status["admin"].clone(), status["server"].clone()), (serde_json::json!(true), serde_json::json!("wsp-hub")));

    for args in [&["hub", "workspace", "add", "team"][..], &["hub", "member", "add", "team", "ann"], &["hub", "workspace", "add", "design"]] {
        assert!(run(args).status.success());
    }
    let added = json(&run(&["hub", "user", "add", "bo", "--json"]));
    let bo = added["token"]["token"].as_str().unwrap().to_string();
    assert_eq!(added["lines"], serde_json::json!(["user bo added"]));
    assert!(added["token"]["id"].as_str().unwrap().starts_with("t-bo-"));
    assert!(run(&["hub", "member", "add", "team", "bo"]).status.success());
    let cy_first = json(&run(&["hub", "user", "add", "cy", "--json"]))["token"]["token"].as_str().unwrap().to_string();
    assert!(run(&["hub", "member", "add", "design", "cy"]).status.success());
    let again = json(&run(&["hub", "token", "cy", "--json"]));
    let cy = again["token"]["token"].as_str().unwrap().to_string();
    assert_eq!(again["kept"], serde_json::json!([]));
    assert_eq!(again["revoked"].as_array().unwrap().len(), 1);
    let cy_id = again["token"]["id"].as_str().unwrap().to_string();

    rig._hub.client(&bo).lease_take(&wsp_objects::WorkspaceId("shared:team".into()), &wsp_store_contract::RowId("wsp-1".into())).unwrap();
    for args in [&["hub", "user", "ls", "--json"][..], &["hub", "user", "ls"], &["hub", "workspace", "ls", "--json"], &["hub", "workspace", "ls"], &["hub", "lease", "ls", "--json"], &["hub", "lease", "ls"], &["hub", "state", "--json"], &["hub", "state"], &["hub", "events", "--limit", "3", "--json"], &["hub", "events", "--kind", "lease", "--json"], &["hub", "events", "--person", "cy"]] {
        let out = run(args);
        assert_eq!(out.status.code(), Some(0), "wsp {}: {}", args.join(" "), said(&out));
    }
    let leases = json(&run(&["hub", "lease", "ls", "--json"]));
    assert_eq!((leases["leases"][0]["row"].clone(), leases["not_shown"].clone()), (serde_json::json!("wsp-1"), serde_json::json!(["design"])));

    let revoked = json(&run(&["hub", "token", "revoke", &cy_id, "--json"]));
    assert_eq!((revoked["revoked"].clone(), revoked["already"].clone()), (serde_json::json!(cy_id), serde_json::json!(false)));

    // Refusals: a sentence on stderr, the code and the refusal's fields on stdout with --json, exit 1.
    assert!(run(&["hub", "user", "revoke", "root"]).status.success());
    let out = run(&["hub", "user", "revoke", "ann", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(String::from_utf8_lossy(&out.stderr).trim(), "wsp: hub: user ann is the last active admin, so the hub refuses hub.user.revoke: make another user an admin first (`wsp hub user admin <name>`)");
    assert_eq!(json(&out)["error"]["code"], "illegal");
    for args in [&["hub", "user", "add", "bo", "--json"][..], &["hub", "member", "add", "nowhere", "bo", "--json"], &["hub", "token", "revoke", "t-nobody-1", "--json"]] {
        let out = run(args);
        assert_eq!(out.status.code(), Some(1), "{}", said(&out));
        assert!(!said(&out).contains(" { ") && !said(&out).contains("UserName("), "debug form: {}", said(&out));
    }
    // As bo, no admin.
    let bo_home = rig.dir.join("bo-home");
    std::fs::create_dir_all(&bo_home).unwrap();
    let out = rig.command(&["login", &rig.url, "--ca", &ca]).env("HOME", &bo_home).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().and_then(|mut c| {
        c.stdin.take().unwrap().write_all(format!("{bo}\n").as_bytes())?;
        c.wait_with_output()
    }).unwrap();
    assert!(out.status.success(), "{}", said(&out));
    let out = rig.command(&["hub", "state", "--json"]).env("HOME", &bo_home).output().unwrap();
    eprintln!("$ wsp hub state --json   (as bo)\n[exit {}]\n{}{}", out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(json(&out)["error"], serde_json::json!({ "code": "forbidden", "message": "forbidden: person:bo is not an admin of this hub", "by": "person:bo" }));
    // Usage: exit 2, nothing dialled.
    assert_eq!(run(&["hub", "events", "--since", "soon"]).status.code(), Some(2));
    assert_eq!(run(&["hub", "token", "revoke"]).status.code(), Some(2));

    // No token minted here is in any file the calls left, calls.jsonl included.
    let left = rig.left_behind();
    assert!(left.iter().any(|(p, text)| p.ends_with("calls.jsonl") && text.contains("\"hub\"")), "the calls were recorded: {:?}", left.iter().map(|(p, _)| p).collect::<Vec<_>>());
    for (path, text) in left {
        for t in [&bo, &cy_first, &cy] {
            assert!(!text.contains(t.as_str()), "{} holds a minted token", path.display());
        }
    }
}

/// The verbs the hub's b5c19f9 made possible, as processes: `user admin`,
/// `lease release`, `hub.info` in `state`, the machine in `lease ls`, and
/// `hub status --json`'s `admin` kept from what the last dial was told.
/// `--nocapture` prints each command and its output.
#[test]
fn hub_admin_flag_release_and_info_through_the_cli() {
    use wsp_spoke::testhub::protocol::IdentityCommand;
    use wsp_spoke::testhub::user;
    let Some(rig) = Rig::new("admin2", &[]) else { return };
    let run = |args: &[&str]| {
        let out = rig.command(args).output().unwrap();
        eprintln!("$ wsp {}\n[exit {}]\n{}{}", args.join(" "), out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        out
    };
    let json = |o: &Output| -> serde_json::Value { serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("not JSON ({e}): {}", said(o))) };
    rig._hub.apply(IdentityCommand::HubUserAdd { name: user("ann"), admin: true });
    let ann = rig._hub.issue("ann");
    let ca = rig.ca.display().to_string();
    assert!(rig.run(&["login", &rig.url, "--ca", &ca], &format!("{ann}\n")).0.status.success());
    for args in [&["hub", "workspace", "add", "team"][..], &["hub", "member", "add", "team", "ann"]] {
        assert!(run(args).status.success());
    }
    let bo = json(&run(&["hub", "user", "add", "bo", "--json"]))["token"]["token"].as_str().unwrap().to_string();
    assert!(run(&["hub", "member", "add", "team", "bo"]).status.success());

    // Make bo an admin, and not; root gives its flag up; ann, the last, cannot.
    let v = json(&run(&["hub", "user", "admin", "bo", "--json"]));
    assert_eq!(v, serde_json::json!({ "lines": ["bo is a hub admin now"], "token": null, "user": "bo", "admin": true, "changed": true }));
    assert!(run(&["hub", "user", "admin", "bo"]).status.success());
    assert!(run(&["hub", "user", "admin", "bo", "--remove", "--json"]).status.success());
    assert!(run(&["hub", "user", "admin", "root", "--remove"]).status.success());
    let out = run(&["hub", "user", "admin", "ann", "--remove", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(json(&out)["error"]["code"], "illegal");

    // A lease bo holds, released by ann.
    rig._hub.client(&bo).lease_take(&wsp_objects::WorkspaceId("shared:team".into()), &wsp_store_contract::RowId("wsp-7".into())).unwrap();
    let leases = json(&run(&["hub", "lease", "ls", "--json"]));
    assert!(leases["leases"][0]["machine"]["id"].is_string(), "{leases}");
    assert!(run(&["hub", "lease", "ls"]).status.success());
    let v = json(&run(&["hub", "lease", "release", "wsp-7", "--json"]));
    assert_eq!((v["released"].clone(), v["holder"].clone()), (serde_json::json!("wsp-7"), serde_json::json!("person:bo")));
    let out = run(&["hub", "lease", "release", "wsp-7", "--json"]);
    assert_eq!((out.status.code(), json(&out)["error"]["code"].clone()), (Some(1), serde_json::json!("not_found")));
    assert!(run(&["hub", "events", "--kind", "lease", "--json"]).status.success());

    let st = json(&run(&["hub", "state", "--json"]));
    assert_eq!(st["hub"]["info"]["server"], "wsp-hub");
    assert!(run(&["hub", "state"]).status.success());
    let status = json(&run(&["hub", "status", "--json"]));
    assert_eq!(status["admin"], serde_json::json!(true));
}
