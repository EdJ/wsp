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
//! `logout` disables the connection first ([`crate::hub_switch::disable`]: the
//! shared rows in flight parked, the journal flushed, the leases given back),
//! then removes the file and says so; a sync worker (wsp-464) looks for the
//! file before each turn and takes none while it is gone. `hub status` prints
//! the URL and user, never the token, and the switch's state. The token is in no word that reaches a log, a message or a
//! child's command line: [`scrub`] takes it out of anything a dial said, and the
//! only child this module starts is `stty`, with fixed arguments.
//!
//! `login --github` reads no token: it asks the hub to log the person in with
//! GitHub (the hub runs GitHub's device flow, [`hub_client::login_github`]),
//! prints the address to open and the code to enter, waits while the person
//! does, and writes the user and token the hub hands back to the same file, as
//! a token login writes them. The token is never printed. A refused login, a
//! hub that has no GitHub login, or a code that runs out writes nothing.
//!
//! `login` needs no store; `logout` and `hub status` read this spoke's switch
//! in it.

use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;
use crate::hub_client::{self, hub_login_in, Client, Credential, HubLogin, Silent, HUBS_FILE, HUB_CA_ENV};
use wsp_hub_protocol::{Code, LoginDone, LoginPrompt};
use wsp_store_contract::Principal;

use wsp_engine::base::args::Args;
use wsp_engine::store::Store;
use wsp_engine::util;

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
    match hub_client::hub_home().filter(|h| !h.as_os_str().is_empty()) {
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

/// Log in to the hub at `url` with GitHub, under `home`: the hub runs GitHub's
/// device flow, `prompt` is handed the address to open and the code to enter,
/// and on `login_done` the user and token the hub hands back are written to
/// the file as [`log_in`] writes a token's. Nothing is written on any other
/// answer. `ca` pins a private CA for a `tls://` hub and is kept in the file.
pub fn log_in_github(home: &Path, url: &str, ca: Option<&Path>, prompt: impl FnMut(&LoginPrompt)) -> Result<LoggedIn, String> {
    log_in_github_with(home, url, ca, |ca| hub_client::login_github(url, ca, WAIT, prompt))
}

/// [`log_in_github`] with the login itself handed in (a test's scripted hub):
/// `login` is given the pinned CA, made absolute, and answers the hub's
/// `login_done` or why not.
fn log_in_github_with(
    home: &Path,
    url: &str,
    ca: Option<&Path>,
    login: impl FnOnce(Option<&Path>) -> Result<LoginDone, Silent>,
) -> Result<LoggedIn, String> {
    let path = home.join(HUBS_FILE);
    // A file that cannot be written is found before the hub, and GitHub, are bothered.
    if !owner_only(&path)? {
        return Err(format!("{} is wider than 0600: chmod 600 it, or remove it, and log in again", path.display()));
    }
    let replaced = hub_login_in(home).ok().flatten();
    let ca = ca.map(absolute);
    let done = login(ca.as_deref()).map_err(|s| match s {
        Silent::Refused(r) if r.code == Code::Unsupported => format!("this hub does not log in with GitHub: {}", r.text),
        Silent::Refused(r) => format!("the hub refused the login: {:?}: {}", r.code, r.text),
        other => other.to_string(),
    })?;
    let token = done.token.0;
    let user = done.user.trim().to_string();
    if user.is_empty() || user.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!("the hub's login names no user it could be ({:?}); nothing was written", scrub(&user, &token)));
    }
    if token.is_empty() || token.len() > MAX_TOKEN || token.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("the hub's login handed back something that is not a token; nothing was written".into());
    }
    write_login(home, url.trim(), &token, &user, ca.as_deref())?;
    Ok(LoggedIn { url: url.trim().to_string(), user, replaced })
}

/// What a person is told to do while a GitHub login waits: the address, the
/// code, and how long it lasts. It goes to stderr, beside where a token
/// prompt would.
pub fn github_prompt_text(url: &str, p: &LoginPrompt) -> String {
    let lasts = if p.expires_secs >= 120 { format!("{} minutes", p.expires_secs / 60) } else { format!("{} seconds", p.expires_secs) };
    format!("to log in to {url} with GitHub, open {} and enter the code {} (it lasts {lasts}); waiting for it…", p.url, p.code)
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
        [] => Err("usage: wsp login <hub-url> [--github] [--ca <file>]   (the token is read from the prompt, or from stdin; --github logs in with GitHub instead)".into()),
        _ => Err("`wsp login` takes the hub's URL and nothing else: the token is read from the prompt or stdin, never from the command line".into()),
    }
}

pub fn login(store: &Store, args: &Args) -> i32 {
    // 2 for a command line that is not one, 1 for what the login met.
    let go = || -> Result<LoggedIn, (i32, String)> {
        let url = url_of(args).map_err(|w| (2, w))?;
        let met = |w: String| (1, w);
        let home = home().map_err(met)?;
        let ca = args.get("ca").or_else(|| std::env::var(HUB_CA_ENV).ok()).filter(|c| !c.trim().is_empty()).map(PathBuf::from);
        if args.has("github") {
            if !url.starts_with("tls://") && !is_loopback(&url) {
                eprintln!("wsp: {url} is not tls://, so the token the hub hands back crosses the network in the clear; a hub off this machine takes tls://");
            }
            return log_in_github(&home, &url, ca.as_deref(), |p| eprintln!("{}", github_prompt_text(&url, p))).map_err(met);
        }
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
                // A logout disables first, and the switch stays off over it.
                if crate::hub_switch::off(store) {
                    println!("the hub connection is off here (it was disabled, or a logout disabled it): `wsp hub enable` turns it on and resumes what the disconnect parked");
                }
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

pub fn logout(store: &Store, args: &Args) -> i32 {
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
    // A clean disconnect first, while the login is there to dial with: shared
    // rows in flight parked, the journal flushed, the leases given back.
    if hub_login_in(&home).ok().flatten().is_some() {
        let done = crate::hub_switch::disable(store, &home);
        println!("{}", crate::cmd_hub::disabled_text(&done, false));
    }
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
            println!("a running sync worker stops at its next look");
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
        // This spoke's own switch.
        Some(word @ ("enable" | "disable")) if args.rest.len() == 1 => return crate::cmd_hub::switch(store, args, word),
        // The admin verbs (wsp-163): user, token, workspace, member.
        Some("user" | "token" | "workspace" | "member") => return crate::cmd_hub::run(store, args),
        _ => {
            eprintln!("usage: wsp hub status [--json] | enable | disable | user … | token … | workspace … | member … (wsp help)");
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
                    None => json!({ "logged_in": false, "connection": crate::hub_switch::status_json(store) }),
                    Some(l) => json!({ "logged_in": true, "url": l.url, "user": l.user, "at": l.at, "ca": l.ca, "connection": crate::hub_switch::status_json(store) }),
                };
                println!("{doc}");
            } else {
                println!("{}", status_text(&login));
                // Logged in: on or off, what the disconnect parked, what is
                // unsent, the leases held and what the hub refused. Not logged
                // in: only what a logout left parked, and the way back.
                let lines = match login.is_some() {
                    true => crate::hub_switch::status_lines(store),
                    false => crate::hub_switch::logged_out_lines(store),
                };
                for line in lines {
                    println!("{line}");
                }
            }
            if login.is_some() { 0 } else { 1 }
        }
    }
}

/// The verbs this module answers to.
pub const VERBS: &[wsp_engine::verbs::Verb] = &[
    wsp_engine::verbs::Verb {
        names: &["login"],
        run: login,
        help: &[wsp_engine::verbs::Help {
            section: wsp_engine::verbs::Section::Machines,
            order: 100,
            text: r##"  wsp login <hub-url> [--ca FILE]   store your token for a hub, tls://host:port:
                                    read from the prompt or stdin, never the
                                    command line. One hub per wsp: a second login
                                    replaces the first. --ca pins a private CA
  wsp login <hub-url> --github      log in with GitHub instead: open the address
                                    shown, enter the code; the hub's token for
                                    you is stored, never shown"##,
        }],
    },
    wsp_engine::verbs::Verb {
        names: &["logout"],
        run: logout,
        help: &[wsp_engine::verbs::Help {
            section: wsp_engine::verbs::Section::Machines,
            order: 110,
            text: r##"  wsp logout                        disable the hub connection (wsp hub disable),
                                    then forget the hub's token"##,
        }],
    },
    wsp_engine::verbs::Verb {
        names: &["hub"],
        run: hub,
        help: &[
            wsp_engine::verbs::Help {
                section: wsp_engine::verbs::Section::Machines,
                order: 120,
                text: r##"  wsp hub status [--json]           the hub you are logged in to, and as whom; on or
                                    off, what the disconnect parked, what is unsent"##,
            },
            wsp_engine::verbs::Help { section: wsp_engine::verbs::Section::Machines, order: 121, text: crate::cmd_hub::HELP },
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
                    let (secret, hash) = hub_client::token::mint();
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
        assert_eq!(Credential::for_hub_in(None, &home), Ok(Credential::User(hub.tokens["ann"].clone())));
        assert_eq!(hub_client::hub_url_in(None, &home).unwrap(), hub.url);
        assert_eq!(hub_client::hub_ca_in(None, &home).unwrap(), Some(hub.pki.ca.clone()));

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
        assert!(matches!(Credential::for_hub_in(None, &home), Err(Silent::NotLoggedIn(_))));
    }

    /// A hub's side of a GitHub login, scripted: its answers in order, and
    /// every frame the client sent.
    struct Scripted {
        answers: std::collections::VecDeque<wsp_hub_protocol::Frame>,
        sent: Vec<wsp_hub_protocol::Frame>,
    }

    impl wsp_hub_protocol::Connection for Scripted {
        fn send(&mut self, frame: &wsp_hub_protocol::Frame) -> Result<(), wsp_hub_protocol::ConnError> {
            self.sent.push(frame.clone());
            Ok(())
        }
        fn recv(&mut self, _wait: Duration) -> Result<Option<wsp_hub_protocol::Frame>, wsp_hub_protocol::ConnError> {
            self.answers.pop_front().map(Some).ok_or(wsp_hub_protocol::ConnError::Closed)
        }
    }

    fn github_hub(answers: Vec<wsp_hub_protocol::Frame>) -> Scripted {
        Scripted { answers: answers.into(), sent: Vec::new() }
    }

    fn device() -> LoginPrompt {
        LoginPrompt { url: "https://github.com/login/device".into(), code: "ABCD-1234".into(), expires_secs: 900 }
    }

    /// `wsp login --github`: the prompt is shown, and the user and token the
    /// hub hands back are written as a token login writes them, at 0600; the
    /// token is in nothing the login says.
    #[test]
    fn login_github_shows_the_code_and_writes_the_hubs_user_and_token_as_a_token_login_does() {
        use std::os::unix::fs::PermissionsExt;
        use wsp_hub_protocol::{ApiToken, Frame};
        let h = home_dir("github");
        let token = "gh-token-0123456789abcdef";
        let mut hub = github_hub(vec![
            Frame::LoginPrompt(device()),
            Frame::LoginDone(LoginDone { user: "ed".into(), token: ApiToken(token.into()) }),
        ]);
        let mut shown = Vec::new();
        let done = log_in_github_with(&h, " tls://hub.invalid:7489 ", Some(Path::new("/etc/hub-ca.pem")), |ca| {
            assert_eq!(ca, Some(Path::new("/etc/hub-ca.pem")));
            hub_client::login_github_on(&mut hub, "hub.invalid:7489", Duration::from_secs(1), |p| shown.push(github_prompt_text("tls://hub.invalid:7489", p)))
        })
        .unwrap();
        assert_eq!(done, LoggedIn { url: "tls://hub.invalid:7489".into(), user: "ed".into(), replaced: None });
        assert_eq!(hub.sent.len(), 1, "the login frame and nothing else: no token is sent");
        assert!(matches!(&hub.sent[0], Frame::Login(l) if l.method == "github"));
        assert_eq!(shown.len(), 1);
        assert!(shown[0].contains("https://github.com/login/device") && shown[0].contains("ABCD-1234") && shown[0].contains("15 minutes"), "{}", shown[0]);
        assert!(!format!("{done:?}{shown:?}").contains(token));

        let path = h.join(HUBS_FILE);
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!((doc["url"].as_str(), doc["user"].as_str(), doc["token"].as_str()), (Some("tls://hub.invalid:7489"), Some("ed"), Some(token)));
        assert_eq!(doc["ca"], "/etc/hub-ca.pem");
        assert!(doc["at"].as_str().is_some_and(|a| a.ends_with('Z')), "{doc}");
        // What the dial reads is the login just written.
        assert_eq!(Credential::for_hub_in(None, &h), Ok(Credential::User(token.into())));
        assert_eq!(hub_login_in(&h).unwrap().unwrap().user, "ed");

        // A second GitHub login replaces the first and says so.
        let mut again = github_hub(vec![Frame::LoginDone(LoginDone { user: "ann".into(), token: ApiToken("other-token".into()) })]);
        let done = log_in_github_with(&h, "tls://hub.invalid:7489", None, |_| hub_client::login_github_on(&mut again, "hub", Duration::from_secs(1), |_| {})).unwrap();
        assert_eq!(done.replaced.map(|r| r.user), Some("ed".into()));
        assert_eq!(hub_login_in(&h).unwrap().unwrap().user, "ann");
    }

    /// A refusal, a hub with no GitHub login, a code that runs out (the hub's
    /// `unauthorised`), a connection that closes or a token that is none:
    /// nothing is written, and the login before is left as it was.
    #[test]
    fn login_github_refused_or_cut_short_writes_nothing() {
        use wsp_hub_protocol::{ApiToken, Frame, Refused};
        let h = home_dir("github-refused");
        write_login(&h, "tls://old.invalid:1", "old-tok", "bo", None).unwrap();
        let before = std::fs::read_to_string(h.join(HUBS_FILE)).unwrap();
        let refused = |code, text: &str| Frame::Refused(Refused { code, text: text.into() });
        let cases: Vec<(Vec<Frame>, &str)> = vec![
            (vec![Frame::LoginPrompt(device()), refused(Code::Forbidden, "mallory is not on this hub's list of GitHub users")], "refused the login: Forbidden"),
            (vec![refused(Code::Unsupported, "no github")], "does not log in with GitHub"),
            (vec![Frame::LoginPrompt(device()), refused(Code::Unauthorised, "the code ran out before it was entered")], "Unauthorised"),
            (vec![Frame::LoginPrompt(device())], "closed"),
            (vec![Frame::LoginDone(LoginDone { user: "ed".into(), token: ApiToken("two words".into()) })], "not a token"),
            (vec![Frame::LoginDone(LoginDone { user: " ".into(), token: ApiToken("tok".into()) })], "names no user"),
        ];
        for (answers, says) in cases {
            let mut hub = github_hub(answers);
            let why = log_in_github_with(&h, "tls://hub.invalid:7489", None, |_| hub_client::login_github_on(&mut hub, "hub", Duration::from_secs(1), |_| {})).unwrap_err();
            assert!(why.contains(says), "{why}");
            assert!(!why.contains("two words") && !why.contains("old-tok"), "{why}");
            assert_eq!(std::fs::read_to_string(h.join(HUBS_FILE)).unwrap(), before, "{says}: the login before is untouched");
        }
    }

    #[test]
    fn login_github_over_a_file_wider_than_0600_is_refused_before_the_hub_is_asked() {
        use std::os::unix::fs::PermissionsExt;
        let h = home_dir("github-wide");
        let path = h.join(HUBS_FILE);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let why = log_in_github_with(&h, "tls://hub.invalid:1", None, |_| panic!("the hub is not asked")).unwrap_err();
        assert!(why.contains("0600"), "{why}");
    }

    #[test]
    fn login_github_is_a_flag_the_url_follows() {
        let args = wsp_engine::base::args::Args::parse(["login", "--github", "tls://hub.invalid:7489"].map(String::from).to_vec());
        assert!(args.has("github"));
        assert_eq!(url_of(&args).unwrap(), "tls://hub.invalid:7489");
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
