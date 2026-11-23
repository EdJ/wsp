//! `wsp hub` — the hub's admin verbs, from a logged-in admin's spoke (wsp-163).
//!
//! ```text
//! wsp hub user add <name> [--admin]     add a user, issue its token, print it once
//! wsp hub user ls                       the hub's users and how many live tokens each has
//! wsp hub user revoke <name>            revoke the user and every token of its
//! wsp hub token <name>                  issue a new token, revoking the old ones
//! wsp hub workspace add <name>          a shared workspace
//! wsp hub member add|rm <ws> <user>     a user in a shared workspace, or out of it
//! ```
//!
//! Each is one or two identity commands sent to the hub over the API as the
//! user the login names ([`wsp_client::Client::dial_hub_ca`]); the hub's
//! identity machine decides them, so a user who is no admin is `forbidden`.
//! The token is minted here ([`wsp_machines::token::mint`] through the
//! machines crate's own `token`) and only its hash is sent: it is printed once
//! to stdout and is in no log, no command, no event and no file. `wsp hub`
//! needs no local store.

use std::time::Duration;

use serde_json::json;
use wsp_api::{Answer, Code, Outcome};
use wsp_client::{Client, Credential, Error};
use wsp_store_contract::{
    Command, IdentityState, Refusal, SharedName, State, TokenId, UserName,
};

use crate::base::args::Args;
use crate::store::Store;

const WAIT: Duration = Duration::from_secs(10);

/// What a verb made: the lines to print, and the token it minted, if any.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Done {
    pub lines: Vec<String>,
    /// `(user, token)`: shown once.
    pub token: Option<(String, String)>,
}

fn dial() -> Result<Client, String> {
    let url = wsp_client::hub_url().map_err(|e| e.to_string())?;
    let credential = Credential::for_hub().map_err(|e| e.to_string())?;
    let ca = wsp_client::hub_ca().map_err(|e| e.to_string())?;
    Client::dial_hub_ca(&url, credential, ca.as_deref(), WAIT).map_err(|e| e.to_string())
}

/// A failed call in words: a refusal for want of being an admin says
/// `forbidden`.
fn say(e: Error) -> String {
    match e {
        Error::Refused(Refusal::Forbidden { by }) => {
            format!("forbidden: {} is not an admin of this hub", wsp_api::principal::text(&by))
        }
        Error::Refused(r) => format!("the hub refused it: {r:?}"),
        Error::Failed(f) if f.code == Code::Forbidden => format!("forbidden: {}", f.text),
        other => other.to_string(),
    }
}

fn send(client: &mut Client, cmd: Command) -> Result<(), String> {
    client.apply(&cmd).map(|_| ()).map_err(say)
}

/// The hub's whole state: `wip` leaves out the users and token records.
fn state(client: &mut Client) -> Result<State, String> {
    match client.request("state", json!({})).map_err(say)?.outcome {
        Outcome::Ok(Answer::State { state, .. }) => Ok(state),
        Outcome::Ok(other) => Err(format!("the hub answered state with {other:?}")),
        Outcome::Err(f) => Err(format!("the hub refused state: {}", f.text)),
    }
}

fn fresh_id(user: &str) -> TokenId {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default();
    TokenId(format!("t-{user}-{nanos:x}"))
}

/// Issues `user` a token: minted here, its hash sent, the token returned.
fn issue(client: &mut Client, user: &str) -> Result<String, String> {
    let (secret, hash) = wsp_machines::token::mint();
    send(client, Command::HubTokenIssue { id: fresh_id(user), user: UserName(user.to_string()), hash })?;
    Ok(secret)
}

fn user_add(client: &mut Client, name: &str, admin: bool) -> Result<Done, String> {
    send(client, Command::HubUserAdd { name: UserName(name.to_string()), admin })?;
    let token = issue(client, name).map_err(|why| format!("{name} was added, but its token was not issued ({why}); `wsp hub token {name}` issues one"))?;
    let kind = if admin { "admin " } else { "" };
    Ok(Done { lines: vec![format!("{kind}user {name} added")], token: Some((name.to_string(), token)) })
}

fn user_ls(client: &mut Client) -> Result<Done, String> {
    let state = state(client)?;
    let mut lines = Vec::new();
    for u in state.users.values() {
        let live = state.credentials.values().filter(|c| c.user == u.name && c.state == IdentityState::Active).count();
        let state = if u.state == IdentityState::Active { "active" } else { "revoked" };
        let role = if u.admin { "admin" } else { "user" };
        lines.push(format!("{:<20} {role:<6} {state:<8} {live} token(s)", u.name.0));
    }
    if lines.is_empty() {
        lines.push("no users".into());
    }
    Ok(Done { lines, token: None })
}

fn user_revoke(client: &mut Client, name: &str) -> Result<Done, String> {
    send(client, Command::HubUserRevoke { name: UserName(name.to_string()) })?;
    Ok(Done { lines: vec![format!("user {name} revoked, and every token of its")], token: None })
}

/// A new token for `name`, the old ones revoked after it: issued first, so a
/// failure never leaves a user with none.
fn token_reissue(client: &mut Client, name: &str) -> Result<Done, String> {
    let old: Vec<TokenId> = state(client)?
        .credentials
        .values()
        .filter(|c| c.user.0 == name && c.state == IdentityState::Active)
        .map(|c| c.id.clone())
        .collect();
    let token = issue(client, name)?;
    for id in old {
        send(client, Command::HubTokenRevoke { id })?;
    }
    Ok(Done { lines: vec![format!("a new token for {name}; the old ones are revoked")], token: Some((name.to_string(), token)) })
}

fn workspace_add(client: &mut Client, name: &str) -> Result<Done, String> {
    send(client, Command::HubWorkspaceAdd { name: SharedName(name.to_string()) })?;
    Ok(Done { lines: vec![format!("shared workspace {name} added")], token: None })
}

fn member_set(client: &mut Client, workspace: &str, user: &str, member: bool) -> Result<Done, String> {
    send(client, Command::HubMemberSet { workspace: SharedName(workspace.to_string()), user: UserName(user.to_string()), member })?;
    let what = if member { format!("{user} is a member of {workspace}") } else { format!("{user} is no member of {workspace}") };
    Ok(Done { lines: vec![what], token: None })
}

const USAGE: &str = "usage: wsp hub user add <name> [--admin] | user ls | user revoke <name> | token <name> | workspace add <name> | member add|rm <workspace> <user>";

/// Runs the words after `hub` against `client`.
pub fn run_words(client: &mut Client, words: &[&str], admin: bool) -> Result<Done, String> {
    match words {
        ["user", "add", name] => user_add(client, name, admin),
        ["user", "ls"] => user_ls(client),
        ["user", "revoke", name] => user_revoke(client, name),
        ["token", name] => token_reissue(client, name),
        ["workspace", "add", name] => workspace_add(client, name),
        ["member", "add", ws, user] => member_set(client, ws, user, true),
        ["member", "rm", ws, user] => member_set(client, ws, user, false),
        _ => Err(USAGE.into()),
    }
}

/// Whether the words name a verb of this module, checked before dialling.
fn known(words: &[&str]) -> bool {
    matches!(
        words,
        ["user", "add", _] | ["user", "ls"] | ["user", "revoke", _] | ["token", _] | ["workspace", "add", _] | ["member", "add" | "rm", _, _]
    )
}

/// What the verb prints, the token once and last.
pub fn render(done: &Done, json: bool, url: Option<&str>) -> String {
    if json {
        let token = done.token.as_ref().map(|(user, token)| json!({"user": user, "token": token}));
        return json!({"lines": done.lines, "token": token}).to_string();
    }
    let mut out = done.lines.join("\n");
    if let Some((user, token)) = &done.token {
        out.push_str(&format!("\n\ntoken for {user}, shown once and kept nowhere:\n\n  {token}\n\n"));
        match url {
            Some(url) => out.push_str(&format!("{user} logs in with: wsp login {url}")),
            None => out.push_str(&format!("{user} logs in with: wsp login <hub-url>")),
        }
    }
    out
}

pub fn run(_store: &Store, args: &Args) -> i32 {
    let words: Vec<&str> = args.rest.iter().map(String::as_str).collect();
    if !known(&words) {
        eprintln!("wsp: {USAGE}");
        return 2;
    }
    let mut client = match dial() {
        Ok(c) => c,
        Err(why) => {
            eprintln!("wsp: hub: {why}");
            return 1;
        }
    };
    match run_words(&mut client, &words, args.has("admin")) {
        Ok(done) => {
            println!("{}", render(&done, args.has("json"), wsp_client::hub_url().ok().as_deref()));
            0
        }
        Err(why) => {
            eprintln!("wsp: hub: {why}");
            1
        }
    }
}

/// The help of the admin verbs, carried by `wsp hub`'s entry in `cmd_login`
/// (the one verb `hub`, whose `status` is the login's).
pub const HELP: &str = r##"  wsp hub user add <name> [--admin]    a hub user and its token, printed once (admin, logged in)
  wsp hub user ls                      the hub's users and their live tokens
  wsp hub user revoke <name>           revoke a user and every token of its
  wsp hub token <name>                 a new token for a user, the old ones revoked
  wsp hub workspace add <name>         a shared workspace
  wsp hub member add|rm <ws> <user>    a user in a shared workspace, or out of it"##;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use wsp_hub::{admin, serve, Config};

    /// A hub served in process from a directory of its own, with its first admin `root`.
    struct Hub {
        dir: PathBuf,
        running: Option<serve::Running>,
        root: String,
    }

    impl Hub {
        fn start(tag: &str) -> Hub {
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let dir = std::env::temp_dir().join(format!("wsp-cmd-hub-{tag}-{}-{nanos}", std::process::id()));
            let cfg = Config {
                data_dir: dir.join("data"),
                bind: vec![],
                port: 0,
                peers: vec!["127.0.0.0/8".into()],
                tls: None,
                name: "hub-t".into(),
            };
            let root = admin::init(&cfg, "root").expect("the first admin").token;
            let running = serve::start(&cfg).expect("the hub starts");
            Hub { dir, running: Some(running), root }
        }

        fn url(&self) -> String {
            format!("tcp://{}", self.running.as_ref().unwrap().addrs()[0])
        }

        fn dial(&self, token: &str) -> Result<Client, String> {
            Client::dial_hub(&self.url(), Credential::User(token.into()), Duration::from_secs(5)).map_err(|e| e.to_string())
        }
    }

    impl Drop for Hub {
        fn drop(&mut self) {
            if let Some(r) = self.running.take() {
                r.stop();
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn token_of(done: &Done) -> String {
        done.token.as_ref().expect("a token was minted").1.clone()
    }

    #[test]
    fn the_admin_adds_a_user_a_workspace_and_a_member_and_the_users_token_logs_in() {
        let hub = Hub::start("flow");
        let mut root = hub.dial(&hub.root).expect("the first admin's token logs in");

        let added = run_words(&mut root, &["user", "add", "ed"], false).unwrap();
        let ed = token_of(&added);
        assert_eq!(ed.len(), 64);
        run_words(&mut root, &["workspace", "add", "core"], false).unwrap();
        run_words(&mut root, &["member", "add", "core", "ed"], false).unwrap();
        run_words(&mut root, &["member", "rm", "core", "ed"], false).unwrap();
        run_words(&mut root, &["member", "add", "core", "ed"], false).unwrap();

        let ls = run_words(&mut root, &["user", "ls"], false).unwrap().lines.join("\n");
        assert!(ls.contains("root") && ls.contains("admin"), "{ls}");
        assert!(ls.lines().any(|l| l.starts_with("ed") && l.contains("user") && l.contains("1 token")), "{ls}");
        assert!(!ls.contains(&ed) && !ls.contains(&wsp_machines::token::hash(&ed)), "no token or hash is listed: {ls}");

        // The new user's token logs in; it is no admin, so adding a user is forbidden.
        let mut as_ed = hub.dial(&ed).expect("the new user's token logs in");
        let why = run_words(&mut as_ed, &["user", "add", "mallory"], false).unwrap_err();
        assert!(why.starts_with("forbidden"), "{why}");
        let why = run_words(&mut as_ed, &["workspace", "add", "x"], false).unwrap_err();
        assert!(why.starts_with("forbidden"), "{why}");
    }

    #[test]
    fn an_admin_flag_makes_an_admin_who_may_add_users() {
        let hub = Hub::start("admin-flag");
        let mut root = hub.dial(&hub.root).unwrap();
        let ann = token_of(&run_words(&mut root, &["user", "add", "ann"], true).unwrap());
        let mut as_ann = hub.dial(&ann).unwrap();
        run_words(&mut as_ann, &["user", "add", "bo"], false).expect("an admin adds a user");
    }

    #[test]
    fn a_reissued_token_replaces_the_old_one_and_a_revoked_user_logs_in_no_more() {
        let hub = Hub::start("reissue");
        let mut root = hub.dial(&hub.root).unwrap();
        let old = token_of(&run_words(&mut root, &["user", "add", "ed"], false).unwrap());
        assert!(hub.dial(&old).is_ok());

        let new = token_of(&run_words(&mut root, &["token", "ed"], false).unwrap());
        assert_ne!(old, new);
        assert!(hub.dial(&old).is_err(), "the old token is revoked");
        assert!(hub.dial(&new).is_ok(), "the new one logs in");

        run_words(&mut root, &["user", "revoke", "ed"], false).unwrap();
        assert!(hub.dial(&new).is_err(), "a revoked user's tokens are revoked");
        let ls = run_words(&mut root, &["user", "ls"], false).unwrap().lines.join("\n");
        assert!(ls.lines().any(|l| l.starts_with("ed") && l.contains("revoked") && l.contains("0 token")), "{ls}");
    }

    #[test]
    fn a_token_is_printed_once_and_a_listing_prints_none() {
        let done = Done { lines: vec!["user ed added".into()], token: Some(("ed".into(), "ab".repeat(32))) };
        let text = render(&done, false, Some("tls://hub.example:7489"));
        assert_eq!(text.matches(&"ab".repeat(32)).count(), 1, "{text}");
        assert!(text.contains("wsp login tls://hub.example:7489"), "{text}");
        let json = render(&done, true, None);
        assert_eq!(json.matches(&"ab".repeat(32)).count(), 1, "{json}");
        let none = render(&Done { lines: vec!["no users".into()], token: None }, false, None);
        assert_eq!(none, "no users");
    }

    #[test]
    fn the_words_are_checked_before_anything_is_dialled() {
        assert!(known(&["user", "add", "ed"]) && known(&["member", "rm", "core", "ed"]) && known(&["token", "ed"]));
        assert!(!known(&["user"]) && !known(&["member", "set", "a", "b"]) && !known(&[]) && !known(&["user", "add"]));
    }
}
