//! `wsp login <hub-url>`, `wsp logout`, `wsp hub status` (wsp-462, multiplayer
//! v1, decision d6): the one place a person's hub token is stored.
//!
//! `login` reads the token from the terminal prompt (echo off) or from stdin,
//! never from the command line, dials the hub (over TLS for a `tls://` URL),
//! says hello with it and, on `welcome`, writes `~/.config/wsp/hubs.json`:
//!
//! ```json
//! { "url": "tls://hub.example:7489", "token": "...", "user": "ed", "at": "2026-11-23T10:00:00Z" }
//! ```
//!
//! plus `ca` when the hub's certificate is a private CA's (`--ca <file>`,
//! `$WSP_HUB_CA`). `user` is the name the hub's `welcome` gave the token, the
//! one every later request is made as (wsp-461). The file is made at 0600 and
//! never wider; one that already exists wider is refused, naming it, and left as
//! it is. One hub per wsp in v1: a second login replaces the first and says so.
//! A wrong token, an unreachable hub or a certificate that is refused writes
//! nothing.
//!
//! `logout` removes the file and says so; a sync worker (wsp-464) reads the file
//! at each turn and stops when it is gone. `hub status` prints the URL and user,
//! never the token. The token is in no word that reaches a log, a message or a
//! child's command line: [`scrub`] takes it out of anything a dial said, and the
//! only child this module starts is `stty`, with fixed arguments.
//!
//! None of the three needs a store.

use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;
use wsp_client::{hub_login_in, Client, Credential, HubLogin, Silent, HUBS_FILE, HUB_CA_ENV};
use wsp_store_contract::Principal;

use crate::base::args::Args;
use crate::store::Store;
use crate::util;

/// How long a login waits for the hub, dial and welcome together.
const WAIT: Duration = Duration::from_secs(10);

/// A token is one word this long at most.
const MAX_TOKEN: usize = 1024;

/// What a successful login did.
#[derive(Debug, PartialEq, Eq)]
pub struct LoggedIn {
    pub url: String,
    pub user: String,
    /// The login this one replaced.
    pub replaced: Option<HubLogin>,
}

/// The home directory the login file is under: `$HOME`, which is a path or nothing.
fn home() -> Result<PathBuf, String> {
    match wsp_client::hub_home().filter(|h| !h.as_os_str().is_empty()) {
        Some(h) => Ok(h),
        None => Err("HOME is not set, so there is no ~/.config/wsp/hubs.json to write".into()),
    }
}

/// `text` with every place `token` occurs taken out. What a dial or a refusal
/// says is the hub's and the library's words, which should never hold the
/// token, and this is what makes that not matter.
pub fn scrub(text: &str, token: &str) -> String {
    if token.is_empty() {
        return text.to_string();
    }
    text.replace(token, "…")
}

/// Whether `url` is a place the token does not cross the network to: loopback.
fn is_loopback(url: &str) -> bool {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.rsplit_once(':').map_or(rest, |(h, _)| h).trim_start_matches('[').trim_end_matches(']');
    host == "localhost" || host == "::1" || host.starts_with("127.")
}

/// Whether the file is only its owner's. A file that is not there is.
fn owner_only(path: &Path) -> Result<bool, String> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        return Ok(meta.permissions().mode() & 0o077 == 0);
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        Ok(true)
    }
}

/// Write `hubs.json` under `home`: a new file at 0600 renamed over the old one,
/// so there is no moment a reader finds it wider or half written. A file that
/// is there and wider than 0600 is refused and left as it is.
fn write_login(home: &Path, url: &str, token: &str, user: &str, ca: Option<&Path>) -> Result<(), String> {
    let path = home.join(HUBS_FILE);
    if !owner_only(&path)? {
        return Err(format!("{} is wider than 0600: chmod 600 it, or remove it, and log in again", path.display()));
    }
    let dir = path.parent().expect("a file under a directory");
    make_dir(dir)?;
    let mut doc = json!({ "url": url, "token": token, "user": user, "at": util::iso_at(util::epoch_secs()) });
    if let Some(ca) = ca {
        doc["ca"] = json!(ca.display().to_string());
    }
    let mut text = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
    text.push('\n');
    let tmp = dir.join(format!(".hubs.json.{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let written = create_private(&tmp).and_then(|mut f| f.write_all(text.as_bytes()).and_then(|_| f.sync_all()));
    let done = written.and_then(|_| std::fs::rename(&tmp, &path));
    if let Err(e) = done {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("{}: {e}", path.display()));
    }
    Ok(())
}

#[cfg(unix)]
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)
}

#[cfg(not(unix))]
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new().write(true).create_new(true).open(path)
}

/// `dir` and what is missing above it, a new one at 0700.
fn make_dir(dir: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).map_err(|e| format!("{}: {e}", dir.display()))
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))
    }
}

/// Log in to the hub at `url` with `token`, under `home`: dial, hello, and on
/// `welcome` write the file. Nothing is written on any other answer. `ca` pins
/// a private CA for a `tls://` hub and is kept in the file.
pub fn log_in(home: &Path, url: &str, token: &str, ca: Option<&Path>) -> Result<LoggedIn, String> {
    let path = home.join(HUBS_FILE);
    // A file that cannot be written is found before the hub is bothered, and before a person types a token twice.
    if !owner_only(&path)? {
        return Err(format!("{} is wider than 0600: chmod 600 it, or remove it, and log in again", path.display()));
    }
    // A file that is there and unreadable as a login is replaced, and said not to be a login.
    let replaced = hub_login_in(home).ok().flatten();
    let ca = ca.map(absolute);
    let client = Client::dial_hub_ca(url, Credential::User(token.to_string()), ca.as_deref(), WAIT)
        .map_err(|s: Silent| scrub(&s.to_string(), token))?;
    let user = match client.by() {
        Principal::Person(name) if !name.is_empty() => name.clone(),
        other => {
            return Err(format!(
                "the hub accepted the token but names it {}, which is not a person: a login is a user's token",
                scrub(&format!("{other:?}"), token)
            ))
        }
    };
    write_login(home, url.trim(), token, &user, ca.as_deref())?;
    Ok(LoggedIn { url: url.trim().to_string(), user, replaced })
}

/// `path` made absolute against the working directory, which a pinned CA must
/// be: the file is read by whatever dials next, from wherever it runs.
fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir().map(|d| d.join(path)).unwrap_or_else(|_| path.to_path_buf())
}

/// The token from `input`: its first line, trimmed, one word.
fn token_from(input: &mut dyn BufRead) -> Result<String, String> {
    let mut line = String::new();
    input.take(MAX_TOKEN as u64 + 2).read_line(&mut line).map_err(|e| format!("reading the token: {e}"))?;
    let token = line.trim().to_string();
    if token.is_empty() {
        return Err("no token was given: it is read from the prompt or the first line of stdin".into());
    }
    if token.len() > MAX_TOKEN || token.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("that is not a token: it is one word, on one line".into());
    }
    Ok(token)
}

/// The token typed at a terminal, with echo off for the line.
fn token_from_terminal(url: &str) -> Result<String, String> {
    let stty = |arg: &str| {
        std::process::Command::new("stty").arg(arg).stdin(std::process::Stdio::inherit()).status().map(|s| s.success()).unwrap_or(false)
    };
    if !stty("-echo") {
        return Err("cannot turn the terminal's echo off to read the token: pipe it on stdin instead".into());
    }
    eprint!("token for {url}: ");
    let _ = std::io::stderr().flush();
    let read = token_from(&mut std::io::stdin().lock());
    let _ = stty("echo");
    eprintln!();
    read
}

/// The words after `login`: the URL and nothing else. A second word would be a
/// token on the command line, and it is refused without being repeated.
fn url_of(args: &Args) -> Result<String, String> {
    for name in ["token", "password", "secret"] {
        if args.given(name) {
            return Err(format!("`--{name}` is not taken: the token is read from the prompt or stdin, never from the command line, where `ps` shows it"));
        }
    }
    match args.rest.as_slice() {
        [url] if !url.trim().is_empty() => Ok(url.trim().to_string()),
        [] => Err("usage: wsp login <hub-url> [--ca <file>]   (the token is read from the prompt, or from stdin)".into()),
        _ => Err("`wsp login` takes the hub's URL and nothing else: the token is read from the prompt or stdin, never from the command line".into()),
    }
}

pub fn login(_store: &Store, args: &Args) -> i32 {
    // 2 for a command line that is not one, 1 for what the login met.
    let go = || -> Result<LoggedIn, (i32, String)> {
        let url = url_of(args).map_err(|w| (2, w))?;
        let met = |w: String| (1, w);
        let home = home().map_err(met)?;
        let ca = args.get("ca").or_else(|| std::env::var(HUB_CA_ENV).ok()).filter(|c| !c.trim().is_empty()).map(PathBuf::from);
        let token = if util::stdin_is_tty() { token_from_terminal(&url) } else { token_from(&mut std::io::stdin().lock()) };
        let token = token.map_err(met)?;
        if !url.starts_with("tls://") && !is_loopback(&url) {
            eprintln!("wsp: {url} is not tls://, so the token crosses the network in the clear; a hub off this machine takes tls://");
        }
        log_in(&home, &url, &token, ca.as_deref()).map_err(met)
    };
    match go() {
        Ok(done) => {
            if args.json() {
                println!("{}", json!({ "url": done.url, "user": done.user, "replaced": done.replaced.as_ref().map(|r| json!({ "url": r.url, "user": r.user })) }));
            } else {
                if let Some(old) = &done.replaced {
                    let who = if old.user.is_empty() { String::new() } else { format!(" as {}", old.user) };
                    println!("replacing the login to {}{who}: one hub per wsp", old.url);
                }
                println!("logged in to {} as {}", done.url, done.user);
            }
            0
        }
        Err((code, why)) => {
            eprintln!("wsp: login: {why}");
            code
        }
    }
}

/// Remove the login under `home`: what it was, None when there was none.
pub fn log_out(home: &Path) -> Result<Option<Option<HubLogin>>, String> {
    let path = home.join(HUBS_FILE);
    if !path.exists() {
        return Ok(None);
    }
    // What it was is only for the saying; a file that does not read is removed all the same.
    let was = hub_login_in(home).ok().flatten();
    std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Some(was))
}

pub fn logout(_store: &Store, args: &Args) -> i32 {
    if !args.rest.is_empty() {
        eprintln!("usage: wsp logout");
        return 2;
    }
    let home = match home() {
        Ok(h) => h,
        Err(why) => {
            eprintln!("wsp: logout: {why}");
            return 1;
        }
    };
    match log_out(&home) {
        Ok(None) => {
            println!("not logged in to any hub; nothing to remove");
            0
        }
        Ok(Some(was)) => {
            match was {
                Some(l) if !l.user.is_empty() => println!("logged out of {} (was {})", l.url, l.user),
                Some(l) => println!("logged out of {}", l.url),
                None => println!("logged out: removed {}", home.join(HUBS_FILE).display()),
            }
            println!("a running sync worker stops at its next turn");
            0
        }
        Err(why) => {
            eprintln!("wsp: logout: {why}");
            1
        }
    }
}

/// What `wsp hub status` prints for `login` (None: not logged in), the token never in it.
pub fn status_text(login: &Option<HubLogin>) -> String {
    match login {
        None => "not logged in to a hub: `wsp login <hub-url>`".to_string(),
        Some(l) => {
            let mut out = format!("hub   {}\nuser  {}", l.url, if l.user.is_empty() { "(not named)" } else { &l.user });
            if !l.at.is_empty() {
                out.push_str(&format!("\nsince {}", l.at));
            }
            if let Some(ca) = &l.ca {
                out.push_str(&format!("\nca    {ca}"));
            }
            out
        }
    }
}

pub fn hub(store: &Store, args: &Args) -> i32 {
    match args.rest.first().map(String::as_str) {
        Some("status") if args.rest.len() == 1 => {}
        // The admin verbs (wsp-163): user, token, workspace, member.
        Some("user" | "token" | "workspace" | "member") => return crate::cmd_hub::run(store, args),
        _ => {
            eprintln!("usage: wsp hub status [--json] | user … | token … | workspace … | member … (wsp help)");
            return 2;
        }
    }
    let read = home().and_then(|h| hub_login_in(&h).map_err(|e| e.to_string()));
    match read {
        Err(why) => {
            eprintln!("wsp: hub status: {why}");
            1
        }
        Ok(login) => {
            if args.json() {
                let doc = match &login {
                    None => json!({ "logged_in": false }),
                    Some(l) => json!({ "logged_in": true, "url": l.url, "user": l.user, "at": l.at, "ca": l.ca }),
                };
                println!("{doc}");
            } else {
                println!("{}", status_text(&login));
            }
            if login.is_some() { 0 } else { 1 }
        }
    }
}

/// The verbs this module answers to.
pub const VERBS: &[crate::verbs::Verb] = &[
    crate::verbs::Verb {
        names: &["login"],
        run: login,
        help: &[crate::verbs::Help {
            section: crate::verbs::Section::Machines,
            order: 100,
            text: r##"  wsp login <hub-url> [--ca FILE]   store your token for a hub, tls://host:port:
                                    read from the prompt or stdin, never the
                                    command line. One hub per wsp: a second login
                                    replaces the first. --ca pins a private CA"##,
        }],
    },
    crate::verbs::Verb {
        names: &["logout"],
        run: logout,
        help: &[crate::verbs::Help {
            section: crate::verbs::Section::Machines,
            order: 110,
            text: r##"  wsp logout                        forget the hub's token; a sync worker stops"##,
        }],
    },
    crate::verbs::Verb {
        names: &["hub"],
        run: hub,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Machines,
                order: 120,
                text: r##"  wsp hub status [--json]           the hub you are logged in to, and as whom"##,
            },
            crate::verbs::Help { section: crate::verbs::Section::Machines, order: 121, text: crate::cmd_hub::HELP },
        ],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    fn home_dir(name: &str) -> PathBuf {
        let h = std::env::temp_dir().join(format!("wsp-login-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&h);
        std::fs::create_dir_all(&h).unwrap();
        h
    }

    #[test]
    fn login_url_is_the_one_word_after_it_and_a_token_on_the_command_line_is_refused_unrepeated() {
        let ok = Args::synth("login", &["tls://hub.invalid:7489"], &[]);
        assert_eq!(url_of(&ok).unwrap(), "tls://hub.invalid:7489");
        let extra = Args::synth("login", &["tls://hub.invalid:7489", "s3cr3t-token"], &[]);
        let why = url_of(&extra).unwrap_err();
        assert!(why.contains("never from the command line") && !why.contains("s3cr3t"), "{why}");
        let flag = Args::synth("login", &["tls://hub.invalid:7489"], &[("token", "s3cr3t-token")]);
        let why = url_of(&flag).unwrap_err();
        assert!(why.contains("--token") && !why.contains("s3cr3t"), "{why}");
        assert!(url_of(&Args::synth("login", &[], &[])).unwrap_err().starts_with("usage:"));
    }

    #[test]
    fn login_token_is_the_first_line_one_word() {
        assert_eq!(token_from(&mut "abc123\nsecond\n".as_bytes()).unwrap(), "abc123");
        assert_eq!(token_from(&mut "  abc123  \r\n".as_bytes()).unwrap(), "abc123");
        assert!(token_from(&mut "".as_bytes()).is_err());
        assert!(token_from(&mut "\n".as_bytes()).is_err());
        assert!(token_from(&mut "two words\n".as_bytes()).is_err());
        let long = "x".repeat(MAX_TOKEN + 10);
        assert!(token_from(&mut long.as_bytes()).is_err());
    }

    #[test]
    fn login_scrub_takes_the_token_out_of_what_a_dial_said() {
        assert_eq!(scrub("refused tok-1 and tok-1", "tok-1"), "refused … and …");
        assert_eq!(scrub("nothing here", ""), "nothing here");
    }

    #[test]
    fn login_file_is_made_at_0600_and_replaced_at_0600() {
        use std::os::unix::fs::PermissionsExt;
        let h = home_dir("write");
        let path = h.join(HUBS_FILE);
        write_login(&h, "tls://hub.invalid:1", "tok-a", "ann", None).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(path.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
        let l = hub_login_in(&h).unwrap().unwrap();
        assert_eq!((l.url.as_str(), l.user.as_str()), ("tls://hub.invalid:1", "ann"));
        assert!(l.at.ends_with('Z'), "{}", l.at);
        write_login(&h, "tls://hub.invalid:2", "tok-b", "bo", Some(Path::new("/etc/ca.pem"))).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        let l = hub_login_in(&h).unwrap().unwrap();
        assert_eq!((l.url.as_str(), l.user.as_str(), l.ca.as_deref()), ("tls://hub.invalid:2", "bo", Some("/etc/ca.pem")));
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(leftovers.len(), 1, "{leftovers:?}");
    }

    #[test]
    fn login_over_a_file_wider_than_0600_is_refused_and_leaves_it() {
        use std::os::unix::fs::PermissionsExt;
        let h = home_dir("wide");
        let path = h.join(HUBS_FILE);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"url":"tls://old.invalid:1","token":"old-tok"}"#).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        // Refused before any dial: the hub named here does not exist.
        let why = log_in(&h, "tls://hub.invalid:1", "new-tok", None).unwrap_err();
        assert!(why.contains(&path.display().to_string()) && why.contains("0600"), "{why}");
        assert!(!why.contains("new-tok") && !why.contains("old-tok"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), r#"{"url":"tls://old.invalid:1","token":"old-tok"}"#);
        assert!(write_login(&h, "tls://hub.invalid:1", "new-tok", "ann", None).is_err());
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o644);
    }

    #[test]
    fn login_logout_removes_the_file_and_says_what_it_was() {
        let h = home_dir("logout");
        assert_eq!(log_out(&h).unwrap(), None);
        write_login(&h, "tls://hub.invalid:1", "tok-a", "ann", None).unwrap();
        let was = log_out(&h).unwrap().unwrap().unwrap();
        assert_eq!((was.url.as_str(), was.user.as_str()), ("tls://hub.invalid:1", "ann"));
        assert!(!h.join(HUBS_FILE).exists());
        assert_eq!(log_out(&h).unwrap(), None);
        // A file that does not read is removed all the same.
        std::fs::create_dir_all(h.join(HUBS_FILE).parent().unwrap()).unwrap();
        std::fs::write(h.join(HUBS_FILE), "junk").unwrap();
        assert_eq!(log_out(&h).unwrap(), Some(None));
        assert!(!h.join(HUBS_FILE).exists());
    }

    #[test]
    fn login_status_names_the_hub_and_user_and_has_no_token_to_name() {
        assert!(status_text(&None).starts_with("not logged in"));
        let h = home_dir("status");
        write_login(&h, "tls://hub.invalid:1", "tok-sekrit", "ann", None).unwrap();
        let l = hub_login_in(&h).unwrap();
        let said = status_text(&l);
        assert!(said.contains("tls://hub.invalid:1") && said.contains("ann"), "{said}");
        assert!(!said.contains("tok-sekrit"));
    }

    /// A hub over TLS on an ephemeral loopback port, with `users` and their tokens, all under `dir`.
    struct TlsHub {
        running: Option<wsp_hub::serve::Running>,
        pki: wsp_server::testing::TestPki,
        url: String,
        tokens: std::collections::BTreeMap<String, String>,
    }

    impl TlsHub {
        fn start(dir: &Path, users: &[&str]) -> TlsHub {
            use wsp_store_contract::{Command as Cmd, IdempotencyKey, TokenId, UserName};
            let data = dir.join("hub-data");
            let mut tokens = std::collections::BTreeMap::new();
            {
                let hub = wsp_hub::Hub::open(&data).unwrap();
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
            let pki = wsp_server::testing::TestPki::new(&dir.join("pki"), "hub");
            let cfg = wsp_hub::Config {
                data_dir: data,
                bind: vec![],
                port: 0,
                peers: vec!["127.0.0.0/8".into()],
                tls: Some(wsp_hub::TlsFiles { cert: pki.cert.clone(), key: pki.key.clone() }),
                name: "wsp-hub".into(),
                lease_days: wsp_hub::DEFAULT_LEASE_DAYS,
            };
            let running = wsp_hub::serve::start(&cfg).expect("the hub starts");
            let port = running.addrs()[0].port();
            TlsHub { running: Some(running), pki, url: format!("tls://localhost:{port}"), tokens }
        }
    }

    impl Drop for TlsHub {
        fn drop(&mut self) {
            if let Some(r) = self.running.take() {
                r.stop();
            }
        }
    }

    /// Done-when 1 (wsp-462): login writes hubs.json at 0600 with the URL and the
    /// user as the hub names them; a wrong token is refused and writes nothing; a
    /// second login replaces the first and says so; logout removes it.
    #[test]
    fn login_against_a_tls_hub_writes_the_file_a_wrong_token_writes_nothing_and_logout_removes_it() {
        use std::os::unix::fs::PermissionsExt;
        let dir = home_dir("tls");
        let hub = TlsHub::start(&dir, &["ann", "bo"]);
        let home = dir.join("home");
        let path = home.join(HUBS_FILE);

        // A wrong token: refused by the hub, nothing written, the token said nowhere.
        let why = log_in(&home, &hub.url, "not-a-token-the-hub-issued", Some(&hub.pki.ca)).unwrap_err();
        assert!(why.contains("refused") || why.contains("Unauthorised"), "{why}");
        assert!(!why.contains("not-a-token-the-hub-issued"), "{why}");
        assert!(!path.exists() && !home.exists(), "a refused login wrote something");

        // Without the CA the platform's roots do not vouch for the hub: refused by name, nothing written.
        let why = log_in(&home, &hub.url, &hub.tokens["ann"], None).unwrap_err();
        assert!(why.contains("UnknownIssuer") && !why.contains(&hub.tokens["ann"]), "{why}");
        assert!(!home.exists());

        // The right token: the file, at 0600, with the hub's name for the user.
        let done = log_in(&home, &hub.url, &hub.tokens["ann"], Some(&hub.pki.ca)).unwrap();
        assert_eq!((done.url.as_str(), done.user.as_str(), done.replaced.is_none()), (hub.url.as_str(), "ann", true));
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(doc["url"], hub.url.as_str());
        assert_eq!(doc["user"], "ann");
        assert_eq!(doc["token"], hub.tokens["ann"].as_str());
        assert_eq!(doc["ca"], hub.pki.ca.display().to_string());
        assert!(doc["at"].as_str().is_some_and(|a| a.ends_with('Z')), "{doc}");

        // The client's own reading of the file is what it dials with.
        assert_eq!(wsp_client::Credential::for_hub_in(None, &home), Ok(wsp_client::Credential::User(hub.tokens["ann"].clone())));
        assert_eq!(wsp_client::hub_url_in(None, &home).unwrap(), hub.url);
        assert_eq!(wsp_client::hub_ca_in(None, &home).unwrap(), Some(hub.pki.ca.clone()));

        // A second login replaces the first, and says what it replaced; a wrong one leaves the first.
        assert!(log_in(&home, &hub.url, "nope", Some(&hub.pki.ca)).is_err());
        assert_eq!(hub_login_in(&home).unwrap().unwrap().user, "ann");
        let done = log_in(&home, &hub.url, &hub.tokens["bo"], Some(&hub.pki.ca)).unwrap();
        assert_eq!(done.user, "bo");
        assert_eq!(done.replaced.map(|r| r.user), Some("ann".to_string()));
        assert_eq!(hub_login_in(&home).unwrap().unwrap().user, "bo");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);

        // Logout removes it and the client is no longer logged in.
        assert!(log_out(&home).unwrap().is_some());
        assert!(!path.exists());
        assert!(matches!(wsp_client::Credential::for_hub_in(None, &home), Err(Silent::NotLoggedIn(_))));
    }

    #[test]
    fn login_loopback_is_where_plain_tcp_is_not_a_warning() {
        for url in ["tcp://127.0.0.1:7489", "localhost:7489", "tcp://[::1]:7489"] {
            assert!(is_loopback(url), "{url}");
        }
        for url in ["tcp://hub.example:7489", "10.0.0.5:7489"] {
            assert!(!is_loopback(url), "{url}");
        }
    }
}
