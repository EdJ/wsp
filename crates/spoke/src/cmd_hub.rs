//! `wsp hub` — the hub's admin verbs, from a logged-in admin's spoke (wsp-163).
//!
//! ```text
//! wsp hub state                         the admin's snapshot: the hub, people, workspaces, leases
//! wsp hub user add <name> [--admin]     add a user, issue its token, print it once
//! wsp hub user ls                       the hub's people, their token records and workspaces
//! wsp hub user revoke <name>            revoke the user and every token of its
//! wsp hub token <name>                  issue a new token, revoking the old admin-issued ones
//! wsp hub token revoke <id>             revoke one token record
//! wsp hub workspace add <name>          a shared workspace
//! wsp hub workspace ls                  the shared workspaces and their members
//! wsp hub member add|rm <ws> <user>     a user in a shared workspace, or out of it
//! wsp hub lease ls                      the leases the hub shows the caller
//! wsp hub events [--from N] [filters]   the hub's trail, a page at a time
//! wsp hub disable                       stop taking part in shared work, the login kept
//! wsp hub enable                        take part again, taking back what the disable parked
//! ```
//!
//! `disable` and `enable` are this spoke's own switch ([`crate::hub_switch`]),
//! not the hub's admin verbs: any logged-in user runs them, on their own wsp.
//!
//! Each is one or two identity commands or reads sent to the hub over the API
//! as the user the login names ([`crate::hub_client::Client::dial_hub_ca`]);
//! the hub decides them, so a user who is no admin is `forbidden`. The reads
//! are put in fields by [`crate::hub_admin`]. A refusal is said in words
//! ([`crate::hub_client::words`]) on stderr, and with `--json` also as
//! `{"error": {"code", "message", <the refusal's fields>}}` on stdout.
//!
//! The token is minted here ([`crate::hub_client::token::mint`]) and only
//! its hash is sent: it is printed once to stdout (with `--json`, as
//! `token.token`) and is in no log, no command, no event and no file. `wsp
//! hub` needs no local store.

use std::time::Duration;

use serde_json::{json, Map, Value};
use crate::hub_admin::{self, Filter};
use crate::hub_client::{self, token, words, Client, Credential, Error, Silent};
use wsp_hub_protocol::{Answer, Call, Code, IdentityCommand as Command, NoArgs, Outcome, Verb, MAX_EVENTS};
use wsp_store_contract::{EventId, IdentityState, Refusal, SharedName, State, TokenId, UserName};

use wsp_engine::base::args::Args;
use wsp_engine::store::Store;
use wsp_engine::util;

const WAIT: Duration = Duration::from_secs(10);

/// A token minted for a user: shown once, never kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Minted {
    pub user: String,
    /// The token record's id on the hub (`t-<user>-<hex>`).
    pub id: String,
    pub token: String,
}

/// What a verb made: the lines to print, the token it minted, if any, and its
/// fields for `--json`.
#[derive(Debug, Default, PartialEq)]
pub struct Done {
    pub lines: Vec<String>,
    /// Shown once.
    pub token: Option<Minted>,
    /// The verb's fields: a read's whole document, or what a write adds beside
    /// `lines` and `token`.
    pub fields: Map<String, Value>,
    /// A read: `--json` prints `fields` alone.
    pub read: bool,
    /// Done in part (a new token issued, an old one not revoked): exit 1, the output printed all the same.
    pub partial: bool,
}

/// A failed verb: a sentence, a stable code, and the refusal's fields.
#[derive(Debug, Clone, PartialEq)]
pub struct Said {
    /// The hub's wire code (`illegal`, `not_found`, `forbidden`, `too_old`,
    /// `unauthorised`, …), or wsp's own where the hub said nothing:
    /// `not_logged_in`, `unreachable`, `timeout`, `lost`, `unexpected`.
    pub code: String,
    pub message: String,
    pub fields: Map<String, Value>,
}

impl std::fmt::Display for Said {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl Said {
    fn new(code: &str, message: impl Into<String>) -> Said {
        Said { code: code.into(), message: message.into(), fields: Map::new() }
    }

    /// `{"error": {"code", "message", …fields}}`.
    pub fn json(&self) -> Value {
        let mut e = Map::new();
        e.insert("code".into(), json!(self.code));
        e.insert("message".into(), json!(self.message));
        for (k, v) in &self.fields {
            e.entry(k.clone()).or_insert_with(|| v.clone());
        }
        json!({ "error": e })
    }

    /// The same, with what was done before it failed said first.
    fn after(mut self, done: &str) -> Said {
        self.message = format!("{done}; {}", self.message);
        self
    }
}

fn silent(s: Silent) -> Said {
    let code = match &s {
        Silent::NotLoggedIn(_) | Silent::BadHubsFile(_) => "not_logged_in".to_string(),
        Silent::Refused(r) => words::code(r.code),
        _ => "unreachable".to_string(),
    };
    Said::new(&code, s.to_string())
}

fn dial() -> Result<Client, Said> {
    let url = hub_client::hub_url().map_err(silent)?;
    let credential = Credential::for_hub().map_err(silent)?;
    let ca = hub_client::hub_ca().map_err(silent)?;
    Client::dial_hub_ca(&url, credential, ca.as_deref(), WAIT).map_err(silent)
}

/// A failed call in words: a refusal for want of being an admin says
/// `forbidden`; any other refusal is the hub's, said as a sentence, its
/// fields kept for `--json`.
fn say(e: Error) -> Said {
    match e {
        Error::Refused(r @ Refusal::Forbidden { .. }, _) => {
            let Refusal::Forbidden { by } = &r else { unreachable!() };
            let message = format!("forbidden: {} is not an admin of this hub", wsp_hub_protocol::principal::text(by));
            let mut fields = words::refusal_fields(&r);
            fields.insert("by".into(), json!(wsp_hub_protocol::principal::text(by)));
            Said { code: "forbidden".into(), message, fields }
        }
        Error::Refused(r, text) => Said { code: words::refusal_code(&r), message: words::refusal(&r, &text), fields: words::refusal_fields(&r) },
        Error::Failed(f) => {
            let mut said = Said::new(&words::code(f.code), words::failure(&f));
            said.fields.insert("text".into(), json!(f.text));
            if f.code == Code::Forbidden {
                said.message = format!("forbidden: {}", f.text);
            }
            said
        }
        Error::Silent(s) => silent(s),
        Error::Timeout => Said::new("timeout", e.to_string()),
        Error::Lost(_) => Said::new("lost", e.to_string()),
        Error::Unexpected(_) => Said::new("unexpected", e.to_string()),
    }
}

/// Sends `cmd`; the ids of the events it wrote (none: it changed nothing).
fn send(client: &mut Client, cmd: Command) -> Result<Vec<EventId>, Said> {
    client.apply(&cmd).map_err(say)
}

/// The hub's state as the admin reads it, and the event id it is consistent at.
fn state(client: &mut Client) -> Result<(State, EventId), Said> {
    match client.request(Call::Verb(Verb::State(NoArgs {}))).map_err(say)?.outcome {
        Outcome::Ok(Answer::State { state, at }) => Ok((state, at)),
        Outcome::Ok(_) => Err(Said::new("unexpected", "the hub answered state with something other than its state")),
        Outcome::Err(f) => Err(say(hub_client::refused(f))),
    }
}

fn fresh_id(user: &str) -> TokenId {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default();
    TokenId(format!("t-{user}-{nanos:x}"))
}

/// Issues `user` a token: minted here, its hash sent, the token returned.
fn issue(client: &mut Client, user: &str) -> Result<Minted, Said> {
    let (secret, hash) = token::mint();
    let id = fresh_id(user);
    send(client, Command::HubTokenIssue { id: id.clone(), user: UserName(user.to_string()), hash })?;
    Ok(Minted { user: user.to_string(), id: id.0, token: secret })
}

fn user_add(client: &mut Client, name: &str, admin: bool) -> Result<Done, Said> {
    send(client, Command::HubUserAdd { name: UserName(name.to_string()), admin })?;
    let token = issue(client, name).map_err(|why| why.after(&format!("{name} was added, but its token was not issued (`wsp hub token {name}` issues one)")))?;
    let kind = if admin { "admin " } else { "" };
    Ok(Done { lines: vec![format!("{kind}user {name} added")], token: Some(token), ..Done::default() })
}

fn user_ls(client: &mut Client) -> Result<Done, Said> {
    let (state, at) = state(client)?;
    Ok(read(hub_admin::people_lines(&state), json!({ "users": hub_admin::people(&state), "at": at.0 })))
}

fn user_revoke(client: &mut Client, name: &str) -> Result<Done, Said> {
    let wrote = send(client, Command::HubUserRevoke { name: UserName(name.to_string()) })?;
    let line = if wrote.is_empty() { format!("user {name} was revoked already") } else { format!("user {name} revoked, and every token of its") };
    let mut done = Done { lines: vec![line], ..Done::default() };
    done.fields.insert("already".into(), json!(wrote.is_empty()));
    Ok(done)
}

/// A new token for `name`, then its live admin-issued tokens revoked after it
/// (issued first, so a failure never leaves a user with none). Its GitHub
/// logins' tokens (`gh-`) are kept: they are the GitHub login's, which
/// limits and expires them itself.
fn token_reissue(client: &mut Client, name: &str) -> Result<Done, Said> {
    let (st, _) = state(client)?;
    let live: Vec<&wsp_store_contract::Credential> = st.credentials.values().filter(|c| c.user.0 == name && c.state == IdentityState::Active).collect();
    let (github, issued): (Vec<_>, Vec<_>) = live.into_iter().partition(|c| hub_admin::method_of(&c.id.0) == "github");
    let token = issue(client, name)?;
    let (mut revoked, mut not_revoked) = (Vec::new(), Vec::new());
    for c in issued {
        match send(client, Command::HubTokenRevoke { id: c.id.clone() }) {
            Ok(_) => revoked.push(c.id.0.clone()),
            Err(why) => not_revoked.push((c.id.0.clone(), why)),
        }
    }
    let kept: Vec<String> = github.iter().map(|c| c.id.0.clone()).collect();
    let list = |ids: &[String]| if ids.is_empty() { "none".to_string() } else { ids.join(", ") };
    let mut lines = vec![format!("a new token for {name} ({})", token.id), format!("revoked (issued by an admin): {}", list(&revoked)), format!("kept (GitHub logins): {}", list(&kept))];
    for (id, why) in &not_revoked {
        lines.push(format!("not revoked: {id}: {why}"));
    }
    let mut done = Done { lines, token: Some(token), partial: !not_revoked.is_empty(), ..Done::default() };
    done.fields.insert("revoked".into(), json!(revoked));
    done.fields.insert("kept".into(), json!(kept));
    done.fields.insert("not_revoked".into(), json!(not_revoked.iter().map(|(id, why)| json!({ "id": id, "error": why.json()["error"] })).collect::<Vec<_>>()));
    Ok(done)
}

fn token_revoke(client: &mut Client, id: &str) -> Result<Done, Said> {
    let wrote = send(client, Command::HubTokenRevoke { id: TokenId(id.to_string()) })?;
    let line = if wrote.is_empty() { format!("token {id} was revoked already") } else { format!("token {id} revoked") };
    let mut done = Done { lines: vec![line], ..Done::default() };
    done.fields.insert("revoked".into(), json!(id));
    done.fields.insert("already".into(), json!(wrote.is_empty()));
    Ok(done)
}

fn workspace_add(client: &mut Client, name: &str) -> Result<Done, Said> {
    send(client, Command::HubWorkspaceAdd { name: SharedName(name.to_string()) })?;
    Ok(Done { lines: vec![format!("shared workspace {name} added")], ..Done::default() })
}

fn workspace_ls(client: &mut Client) -> Result<Done, Said> {
    let (state, at) = state(client)?;
    let me = hub_admin::person(client.by()).map(str::to_string);
    Ok(read(hub_admin::workspaces_lines(&state, me.as_deref()), json!({ "workspaces": hub_admin::workspaces(&state, me.as_deref()), "at": at.0 })))
}

fn member_set(client: &mut Client, workspace: &str, user: &str, member: bool) -> Result<Done, Said> {
    send(client, Command::HubMemberSet { workspace: SharedName(workspace.to_string()), user: UserName(user.to_string()), member })?;
    let what = if member { format!("{user} is a member of {workspace}") } else { format!("{user} is no member of {workspace}") };
    Ok(Done { lines: vec![what], ..Done::default() })
}

fn lease_ls(client: &mut Client) -> Result<Done, Said> {
    let (state, at) = state(client)?;
    let me = hub_admin::person(client.by()).map(str::to_string);
    let doc = json!({ "leases": hub_admin::leases(&state), "not_shown": hub_admin::not_shown(&state, me.as_deref()), "at": at.0 });
    Ok(read(hub_admin::leases_lines(&state, me.as_deref()), doc))
}

/// The admin's snapshot: the hub (its instance name, who is asking, where its
/// trail starts), its people, workspaces and the leases it shows.
fn hub_state(client: &mut Client, url: &str) -> Result<Done, Said> {
    let (state, at) = state(client)?;
    let horizon = hub_admin::horizon(client).map_err(say)?;
    let me = hub_admin::person(client.by()).map(str::to_string);
    let mut hub = json!({
        "url": url,
        "server": client.server(),
        "by": wsp_hub_protocol::principal::text(client.by()),
        "admin": client.admin(),
        "at": at.0,
    });
    if let (Some(h), Value::Object(more)) = (hub.as_object_mut(), hub_admin::horizon_json(&horizon)) {
        h.extend(more);
    }
    let users = hub_admin::people(&state);
    let workspaces = hub_admin::workspaces(&state, me.as_deref());
    let leases = hub_admin::leases(&state);
    let not_shown = hub_admin::not_shown(&state, me.as_deref());
    let admins = state.users.values().filter(|u| u.admin && u.state == IdentityState::Active).count();
    let revoked = state.users.values().filter(|u| u.state == IdentityState::Revoked).count();
    let live: Vec<&str> = state.credentials.values().filter(|c| c.state == IdentityState::Active).map(|c| hub_admin::method_of(&c.id.0)).collect();
    let github = live.iter().filter(|m| **m == "github").count();
    let mine = workspaces.iter().filter(|w| w["member"] == json!(true)).count();
    let mut lines = vec![
        format!("hub         {} at {url}, as {}{}", client.server(), me.as_deref().unwrap_or("?"), if client.admin() == Some(true) { " (admin)" } else { "" }),
        format!("people      {} ({admins} active admin(s), {revoked} revoked)", users.len()),
        format!("tokens      {} live ({github} GitHub, {} issued)", live.len(), live.len() - github),
        format!("workspaces  {} (you are in {mine})", workspaces.len()),
        format!("leases      {} you can see", leases.len()),
    ];
    match (&horizon.first, horizon.pruned) {
        (Some((id, t)), pruned) => lines.push(format!(
            "trail       from event {} at {}{}",
            id.0,
            hub_admin::iso(*t),
            if pruned.is_some() { " (older events are pruned)" } else { "" }
        )),
        (None, _) => lines.push("trail       no event you can read".into()),
    }
    if !not_shown.is_empty() {
        lines.push(format!("not shown to you (you are not a member): {}", not_shown.join(", ")));
    }
    Ok(read(lines, json!({ "hub": hub, "users": users, "workspaces": workspaces, "leases": leases, "not_shown": not_shown })))
}

/// What `wsp hub events` asks: after which id, how many the hub reads at
/// most, and which of them to keep.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventsAsk {
    pub from: u64,
    pub limit: Option<u32>,
    pub filter: Filter,
}

fn events(client: &mut Client, ask: &EventsAsk) -> Result<Done, Said> {
    let owners = match &ask.filter.person {
        Some(_) => hub_admin::owners(&state(client)?.0),
        None => Default::default(),
    };
    let page = hub_admin::events_page(client, EventId(ask.from), ask.limit).map_err(say)?;
    let kept: Vec<_> = page.events.iter().filter(|e| ask.filter.keeps(e, &owners)).collect();
    let more = page.head.is_some_and(|h| page.through < h);
    let next_from = match page.too_old {
        Some(oldest) => Some(oldest.0.saturating_sub(1)),
        None => more.then_some(page.through.0),
    };
    let mut lines: Vec<String> = kept.iter().map(|e| hub_admin::event_line(e)).collect();
    match (page.too_old, page.head) {
        (Some(oldest), _) => lines.push(format!(
            "the events after {} are past what the hub keeps: the oldest it keeps is event {}; read on with --from {}",
            ask.from,
            oldest.0,
            oldest.0.saturating_sub(1)
        )),
        (None, Some(head)) if more => {
            if kept.is_empty() {
                lines.push("no events in this page".into());
            }
            lines.push(format!("more after event {} (the hub's head is {}): --from {}", page.through.0, head.0, page.through.0))
        }
        (None, head) => {
            if kept.is_empty() {
                lines.push("no events".into());
            }
            lines.push(format!("through event {}, the hub's head", head.map_or(page.through.0, |h| h.0)));
        }
    }
    let doc = json!({
        "from": ask.from,
        "through": page.through.0,
        "head": page.head.map(|h| h.0),
        "more": more,
        "next_from": next_from,
        "too_old": page.too_old.map(|o| json!({ "oldest": o.0 })),
        "events": kept.iter().map(|e| hub_admin::event_json(e)).collect::<Vec<_>>(),
    });
    Ok(read(lines, doc))
}

fn read(lines: Vec<String>, doc: Value) -> Done {
    let fields = match doc {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    Done { lines, fields, read: true, ..Done::default() }
}

const USAGE: &str = "usage: wsp hub state | user add <name> [--admin] | user ls | user revoke <name> | token <name> | token revoke <id> | workspace add <name> | workspace ls | member add|rm <workspace> <user> | lease ls | events [--from N] [--limit N] [--kind K] [--workspace W] [--person U] [--row R] [--since T]";

/// The flags a verb reads beside its words.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Opts {
    pub admin: bool,
    pub events: EventsAsk,
    /// The hub's URL, as the login names it, for `state`.
    pub url: String,
}

/// Runs the words after `hub` against `client`.
pub fn run_words(client: &mut Client, words: &[&str], opts: &Opts) -> Result<Done, Said> {
    match words {
        ["state"] => hub_state(client, &opts.url),
        ["user", "add", name] => user_add(client, name, opts.admin),
        ["user", "ls"] => user_ls(client),
        ["user", "revoke", name] => user_revoke(client, name),
        ["token", "revoke", id] => token_revoke(client, id),
        ["token", name] => token_reissue(client, name),
        ["workspace", "add", name] => workspace_add(client, name),
        ["workspace", "ls"] => workspace_ls(client),
        ["member", "add", ws, user] => member_set(client, ws, user, true),
        ["member", "rm", ws, user] => member_set(client, ws, user, false),
        ["lease", "ls"] => lease_ls(client),
        ["events"] => events(client, &opts.events),
        _ => Err(Said::new("usage", USAGE)),
    }
}

/// Whether the words name a verb of this module, checked before dialling.
fn known(words: &[&str]) -> bool {
    match words {
        // `wsp hub token revoke` with no id is a slip, not a new token for a user named `revoke`.
        ["token", "revoke"] => false,
        ["state"] | ["user", "add", _] | ["user", "ls"] | ["user", "revoke", _] | ["token", _] | ["token", "revoke", _] => true,
        ["workspace", "add", _] | ["workspace", "ls"] | ["member", "add" | "rm", _, _] | ["lease", "ls"] | ["events"] => true,
        _ => false,
    }
}

/// `wsp hub events`' flags, read and checked before anything is dialled.
fn events_ask(args: &Args) -> Result<EventsAsk, String> {
    let mut ask = EventsAsk::default();
    if let Some(from) = args.get("from") {
        ask.from = from.parse().map_err(|_| format!("--from takes an event id, a whole number, not `{from}`"))?;
    }
    if let Some(limit) = args.get("limit") {
        let n: u32 = limit.parse().ok().filter(|n| (1..=MAX_EVENTS).contains(n)).ok_or_else(|| format!("--limit takes 1 to {MAX_EVENTS}, not `{limit}`"))?;
        ask.limit = Some(n);
    }
    ask.filter.kinds = args.each("kind");
    ask.filter.workspace = args.get("workspace");
    ask.filter.person = args.get("person");
    ask.filter.row = args.get("row");
    if let Some(since) = args.get("since") {
        let at = hub_admin::parse_since(&since, util::epoch_secs())
            .ok_or_else(|| format!("--since takes a time (2026-11-27, 2026-11-27T09:00:00Z) or a span (90s, 15m, 1h, 2d), not `{since}`"))?;
        ask.filter.since = Some(at);
    }
    Ok(ask)
}

/// What the verb prints. A write: its lines, then the token once and last;
/// `--json` `{"lines", "token": null | {"user", "id", "token"}, …its fields}`.
/// A read: its lines; `--json` its document.
pub fn render(done: &Done, json: bool, url: Option<&str>) -> String {
    if json {
        if done.read {
            return Value::Object(done.fields.clone()).to_string();
        }
        let mut doc = Map::new();
        doc.insert("lines".into(), json!(done.lines));
        doc.insert("token".into(), done.token.as_ref().map_or(Value::Null, |t| json!({ "user": t.user, "id": t.id, "token": t.token })));
        for (k, v) in &done.fields {
            doc.entry(k.clone()).or_insert_with(|| v.clone());
        }
        return Value::Object(doc).to_string();
    }
    let mut out = done.lines.join("\n");
    if let Some(t) = &done.token {
        out.push_str(&format!("\n\ntoken for {}, shown once and kept nowhere:\n\n  {}\n\n", t.user, t.token));
        match url {
            Some(url) => out.push_str(&format!("{} logs in with: wsp login {url}", t.user)),
            None => out.push_str(&format!("{} logs in with: wsp login <hub-url>", t.user)),
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
    let json = args.json();
    let mut opts = Opts { admin: words.starts_with(&["user", "add"]) && args.has("admin"), ..Opts::default() };
    if words == ["events"] {
        match events_ask(args) {
            Ok(ask) => opts.events = ask,
            Err(why) => {
                eprintln!("wsp: hub events: {why}");
                return 2;
            }
        }
    }
    let fail = |said: Said| {
        eprintln!("wsp: hub: {said}");
        if json {
            println!("{}", said.json());
        }
        1
    };
    let mut client = match dial() {
        Ok(c) => c,
        Err(said) => return fail(said),
    };
    let url = hub_client::hub_url().ok();
    opts.url = url.clone().unwrap_or_default();
    match run_words(&mut client, &words, &opts) {
        Ok(done) => {
            println!("{}", render(&done, json, url.as_deref()));
            if done.partial { 1 } else { 0 }
        }
        Err(said) => fail(said),
    }
}

/// `wsp hub disable` / `wsp hub enable`: the switch, under the login there is.
pub fn switch(store: &Store, args: &Args, word: &str) -> i32 {
    let Some(home) = hub_client::hub_home().filter(|h| hub_client::hub_login_in(h).ok().flatten().is_some()) else {
        eprintln!("wsp: hub {word}: not logged in to a hub, so there is nothing to switch (`wsp login <hub-url>`)");
        return 1;
    };
    match word {
        "disable" if args.has("dry-run") => {
            println!("{}", preview_text(&crate::hub_switch::preview(store), true, args.json()));
            0
        }
        "disable" => {
            let done = crate::hub_switch::disable(store, &home);
            println!("{}", disabled_text(&done, args.json()));
            0
        }
        _ => {
            let done = crate::hub_switch::enable(store, &home);
            println!("{}", enabled_text(&done, args.json()));
            if done.still_parked.is_empty() { 0 } else { 1 }
        }
    }
}

/// What `wsp hub disable --dry-run` and `wsp logout --dry-run` print: what the
/// disable would do now (`logged_in` false: a logout with no login disables
/// nothing). JSON: `{"dry_run":true,"logged_in","off","park":[{"row","status"}],
/// "ending":[{"row","seat"}],"leases":[…],"unsent"}`.
pub fn preview_text(p: &crate::hub_switch::Preview, logged_in: bool, json: bool) -> String {
    if json {
        return json!({
            "dry_run": true, "logged_in": logged_in, "off": p.off,
            "park": p.park.iter().map(|(r, s)| json!({ "row": r, "status": s })).collect::<Vec<_>>(),
            "ending": p.ending.iter().map(|(r, s)| json!({ "row": r, "seat": s })).collect::<Vec<_>>(),
            "leases": p.leases, "unsent": p.unsent,
        })
        .to_string();
    }
    if !logged_in {
        return "dry run: not logged in, so nothing would be disabled".into();
    }
    let mut out = vec!["dry run: nothing is done".to_string()];
    if p.off {
        out.push("the hub connection is off already; what is left would be done again".into());
    }
    match p.park.is_empty() {
        true => out.push("would park: nothing".into()),
        false => out.push(format!("would park: {}", p.park.iter().map(|(r, s)| format!("{r} ({s})")).collect::<Vec<_>>().join(" "))),
    }
    for (row, seat) in &p.ending {
        out.push(format!("{row}: its agent on {seat} would end when its turn is over; its lease would go back then"));
    }
    if p.unsent > 0 {
        out.push(format!("would send first: {} journal entries", p.unsent));
    }
    match p.leases.is_empty() {
        true => out.push("would give back: no lease".into()),
        false => out.push(format!("would give back (if the hub answers): {}", p.leases.join(" "))),
    }
    out.join("\n")
}

/// What `wsp hub disable` (and `wsp logout`, which disables first) prints.
pub fn disabled_text(d: &crate::hub_switch::Disabled, json: bool) -> String {
    if json {
        return json!({
            "enabled": false, "was_off": d.was_off, "parked": d.parked, "unreachable": d.unreachable,
            "unsent": d.unsent, "released": d.released, "not_released": d.not_released,
            "ending": d.ending.iter().map(|(r, s)| json!({ "row": r, "seat": s })).collect::<Vec<_>>(),
            "not_ended": d.not_ended.iter().map(|(r, s, why)| json!({ "row": r, "seat": s, "why": why })).collect::<Vec<_>>(),
        })
        .to_string();
    }
    let mut out = vec![if d.was_off { "the hub connection was off already; what was left is done again".to_string() } else { "hub connection disabled: no shared work is taken here".to_string() }];
    if !d.parked.is_empty() {
        out.push(format!("parked: {}", d.parked.join(" ")));
    }
    for (row, seat) in &d.ending {
        out.push(format!("{row}: its agent on {seat} ends when its turn is over (its tree is kept); the lease goes back then"));
    }
    for (row, seat, why) in &d.not_ended {
        out.push(format!("{row}: its agent on {seat} could not be set to end ({why}); end it with `wsp despawn --pane {seat} --keep-tree`"));
    }
    match &d.unreachable {
        None => out.push(format!("journal flushed; {} lease(s) given back", d.released.len())),
        Some(why) => out.push(format!("the hub did not answer ({why}): the journal is kept and the leases run out on the hub")),
    }
    if d.unsent > 0 {
        out.push(format!("unsent: {} journal entries", d.unsent));
    }
    if !d.not_released.is_empty() {
        out.push(format!("not given back: {}", d.not_released.join(" ")));
    }
    out.push("the login is kept; `wsp hub enable` takes part again".into());
    out.join("\n")
}

fn enabled_text(e: &crate::hub_switch::Enabled, json: bool) -> String {
    if json {
        let still: Vec<_> = e.still_parked.iter().map(|(r, why)| json!({ "row": r, "why": why })).collect();
        let replay = e.replay.as_ref().map(|r| match r {
            Ok(()) => json!({ "ok": true }),
            Err(why) => json!({ "ok": false, "why": why }),
        });
        return json!({ "enabled": true, "was_on": e.was_on, "resumed": e.resumed, "still_parked": still, "replay": replay }).to_string();
    }
    let mut out = vec![if e.was_on { "the hub connection was on already".to_string() } else { "hub connection enabled: shared work is taken and synced again".to_string() }];
    if !e.resumed.is_empty() {
        out.push(format!(
            "resumed, their leases taken again: {} — no agent is started for them: each is ready to be started again (`wsp spawn <row>`), and a member of a list this wsp drives is reseated by the run once it has been without a seat for a while",
            e.resumed.join(" ")
        ));
    }
    for (row, why) in &e.still_parked {
        out.push(format!("still parked: {row}: {why}"));
    }
    match &e.replay {
        Some(Err(why)) => out.push(format!("the journal was not replayed: {why}; the sync worker tries again")),
        Some(Ok(())) => out.push("journal replayed".into()),
        None => out.push("the daemon's sync worker replays the journal".into()),
    }
    out.join("\n")
}

/// The help of the admin verbs, carried by `wsp hub`'s entry in `cmd_login`
/// (the one verb `hub`, whose `status` is the login's).
pub const HELP: &str = r##"  wsp hub state [--json]               the admin's snapshot: the hub's name, who asks,
                                       where its trail starts; people, workspaces, leases
  wsp hub user add <name> [--admin]    a hub user and its token, printed once (admin, logged in)
  wsp hub user ls [--json]             the hub's people: role, state, token records
                                       (id, method, issued, revoked), workspaces
  wsp hub user revoke <name>           revoke a user and every token of its
  wsp hub token <name>                 a new token for a user; its admin-issued tokens
                                       revoked, its GitHub logins' kept
  wsp hub token revoke <id>            revoke one token record, by its id
  wsp hub workspace add <name>         a shared workspace
  wsp hub workspace ls [--json]        the shared workspaces and their members
  wsp hub member add|rm <ws> <user>    a user in a shared workspace, or out of it
  wsp hub lease ls [--json]            the leases the hub shows you (your workspaces')
  wsp hub events [--from N] [--json]   the hub's trail, one page (at most 1000) after
                                       event N; --limit N, --kind K, --workspace W,
                                       --person U, --row R, --since T filter the page
  wsp hub disable                      stop taking shared work here, cleanly: shared
                                       rows in flight parked, the journal flushed,
                                       leases given back; the login is kept
  wsp hub disable --dry-run [--json]   what disable would park, end and give back,
                                       and the journal it would send; does nothing
  wsp hub enable                       take part again: parked rows resumed where
                                       their lease can be taken"##;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testhub::{Backend, Setup, TestHub};
    use wsp_objects::WorkspaceId;
    use wsp_store_contract::RowId;

    /// A real hub with its first admin `root` (`testhub`); `None` when no hub is named.
    struct Hub {
        hub: TestHub,
        root: String,
    }

    impl Hub {
        fn start(_tag: &str) -> Option<Hub> {
            let hub = TestHub::start(Setup::default())?;
            let root = hub.admin_token().to_string();
            Some(Hub { hub, root })
        }

        fn dial(&self, token: &str) -> Result<Client, String> {
            self.hub.try_client(token, Duration::from_secs(5)).map_err(|e| e.to_string())
        }
    }

    fn token_of(done: &Done) -> String {
        done.token.as_ref().expect("a token was minted").token.clone()
    }

    /// The words, no flags.
    fn w(client: &mut Client, words: &[&str]) -> Result<Done, Said> {
        run_words(client, words, &Opts::default())
    }

    fn admin_add(client: &mut Client, name: &str) -> Done {
        run_words(client, &["user", "add", name], &Opts { admin: true, ..Opts::default() }).unwrap()
    }

    fn doc(done: &Done) -> Value {
        serde_json::from_str(&render(done, true, None)).unwrap()
    }

    #[test]
    fn the_admin_adds_a_user_a_workspace_and_a_member_and_the_users_token_logs_in() {
        let Some(hub) = Hub::start("flow") else { return };
        let mut root = hub.dial(&hub.root).expect("the first admin's token logs in");

        let added = w(&mut root, &["user", "add", "ed"]).unwrap();
        let ed = token_of(&added);
        assert_eq!(ed.len(), 64);
        w(&mut root, &["workspace", "add", "core"]).unwrap();
        w(&mut root, &["member", "add", "core", "ed"]).unwrap();
        w(&mut root, &["member", "rm", "core", "ed"]).unwrap();
        w(&mut root, &["member", "add", "core", "ed"]).unwrap();

        let ls = w(&mut root, &["user", "ls"]).unwrap().lines.join("\n");
        assert!(ls.contains("root") && ls.contains("admin"), "{ls}");
        assert!(ls.lines().any(|l| l.starts_with("ed") && l.contains("user") && l.contains("1 token") && l.contains("in core")), "{ls}");
        assert!(!ls.contains(&ed) && !ls.contains(&token::hash(&ed)), "no token or hash is listed: {ls}");

        // The new user's token logs in; it is no admin, so adding a user, and every read, is forbidden.
        let mut as_ed = hub.dial(&ed).expect("the new user's token logs in");
        for words in [&["user", "add", "mallory"][..], &["workspace", "add", "x"], &["user", "ls"], &["state"], &["events"], &["lease", "ls"], &["token", "revoke", "t-root-init"]] {
            let why = w(&mut as_ed, words).unwrap_err();
            assert_eq!(why.code, "forbidden", "{words:?}: {why:?}");
            assert_eq!(why.message, "forbidden: person:ed is not an admin of this hub", "{words:?}");
            assert_eq!(why.json()["error"]["by"], json!("person:ed"));
        }
    }

    #[test]
    fn an_admin_flag_makes_an_admin_who_may_add_users() {
        let Some(hub) = Hub::start("admin-flag") else { return };
        let mut root = hub.dial(&hub.root).unwrap();
        let ann = token_of(&admin_add(&mut root, "ann"));
        let mut as_ann = hub.dial(&ann).unwrap();
        w(&mut as_ann, &["user", "add", "bo"]).expect("an admin adds a user");
    }

    /// `wsp hub token <name>` revokes the admin-issued tokens only: a GitHub
    /// login's (`gh-`) is kept, and the output names both.
    #[test]
    fn a_reissued_token_replaces_the_admin_issued_ones_and_keeps_the_github_logins() {
        let Some(hub) = Hub::start("reissue") else { return };
        let mut root = hub.dial(&hub.root).unwrap();
        let first = w(&mut root, &["user", "add", "ed"]).unwrap();
        let (old, old_id) = (token_of(&first), first.token.as_ref().unwrap().id.clone());
        assert!(old_id.starts_with("t-ed-"), "{old_id}");
        assert!(hub.dial(&old).is_ok());
        // A GitHub login's token record, as the hub's login writes it (`gh-<user>-<12 hex>`).
        let (_, hash) = token::mint();
        let gh = "gh-ed-0123456789ab".to_string();
        send(&mut root, Command::HubTokenIssue { id: TokenId(gh.clone()), user: UserName("ed".into()), hash }).unwrap();

        let done = w(&mut root, &["token", "ed"]).unwrap();
        let new = token_of(&done);
        assert_ne!(old, new);
        assert_eq!(done.fields["revoked"], json!([old_id]));
        assert_eq!(done.fields["kept"], json!([gh]));
        assert_eq!(done.lines[1], format!("revoked (issued by an admin): {old_id}"));
        assert_eq!(done.lines[2], format!("kept (GitHub logins): {gh}"));
        assert!(hub.dial(&old).is_err(), "the old token is revoked");
        assert!(hub.dial(&new).is_ok(), "the new one logs in");
        let (st, _) = state(&mut root).unwrap();
        assert_eq!(st.credentials[&TokenId(gh.clone())].state, IdentityState::Active, "the GitHub login's token is kept");

        w(&mut root, &["user", "revoke", "ed"]).unwrap();
        assert!(hub.dial(&new).is_err(), "a revoked user's tokens are revoked");
        let ls = w(&mut root, &["user", "ls"]).unwrap().lines.join("\n");
        assert!(ls.lines().any(|l| l.starts_with("ed") && l.contains("revoked") && l.contains("0 token")), "{ls}");
        let again = w(&mut root, &["user", "revoke", "ed"]).unwrap();
        assert_eq!(again.lines, vec!["user ed was revoked already".to_string()]);
    }

    #[test]
    fn one_token_is_revoked_by_its_record_id() {
        let Some(hub) = Hub::start("token-revoke") else { return };
        let mut root = hub.dial(&hub.root).unwrap();
        let added = w(&mut root, &["user", "add", "bo"]).unwrap();
        let (bo, id) = (token_of(&added), added.token.as_ref().unwrap().id.clone());
        let second = token_of(&w(&mut root, &["token", "bo"]).unwrap());
        let second_id = state(&mut root).unwrap().0.credentials.values().find(|c| c.user.0 == "bo" && c.state == IdentityState::Active).unwrap().id.0.clone();
        assert!(hub.dial(&bo).is_err());

        let done = w(&mut root, &["token", "revoke", &second_id]).unwrap();
        assert_eq!(done.lines, vec![format!("token {second_id} revoked")]);
        assert_eq!((doc(&done)["revoked"].clone(), doc(&done)["already"].clone(), doc(&done)["token"].clone()), (json!(second_id), json!(false), Value::Null));
        assert!(hub.dial(&second).is_err(), "the revoked token logs in no more");
        let again = w(&mut root, &["token", "revoke", &id]).unwrap();
        assert_eq!(again.lines, vec![format!("token {id} was revoked already")]);

        let why = w(&mut root, &["token", "revoke", "t-nobody-1"]).unwrap_err();
        assert_eq!((why.code.as_str(), why.message.as_str()), ("not_found", "the hub has no token record t-nobody-1"));
        assert_eq!(why.json(), json!({ "error": { "code": "not_found", "message": "the hub has no token record t-nobody-1", "subject": { "credential": "t-nobody-1" } } }));
    }

    #[test]
    fn the_hubs_refusals_are_sentences_with_their_fields() {
        let Some(hub) = Hub::start("refusals") else { return };
        let mut root = hub.dial(&hub.root).unwrap();
        let why = w(&mut root, &["user", "revoke", "root"]).unwrap_err();
        assert_eq!(why.message, "user root is the last active admin, so the hub refuses hub.user.revoke");
        assert_eq!(why.json()["error"], json!({ "code": "illegal", "message": why.message, "subject": { "user": "root" }, "state": "the last active admin", "verb": "hub.user.revoke" }));
        let why = w(&mut root, &["user", "add", "root"]).unwrap_err();
        assert_eq!((why.code.as_str(), why.message.as_str()), ("invalid", "root is a name the hub has had; a name is never reused"));
        w(&mut root, &["workspace", "add", "team"]).unwrap();
        let why = w(&mut root, &["workspace", "add", "team"]).unwrap_err();
        assert_eq!(why.message, "shared workspace team exists");
        let why = w(&mut root, &["member", "add", "design", "root"]).unwrap_err();
        assert_eq!((why.code.as_str(), why.message.as_str()), ("not_found", "the hub has no shared workspace design"));
        w(&mut root, &["user", "add", "cy"]).unwrap();
        w(&mut root, &["user", "revoke", "cy"]).unwrap();
        let why = w(&mut root, &["member", "add", "team", "cy"]).unwrap_err();
        assert_eq!(why.message, "user cy is revoked, so the hub refuses hub.member.set");
        let why = w(&mut root, &["token", "cy"]).unwrap_err();
        assert_eq!(why.message, "user cy is revoked, so the hub refuses hub.token.issue");
    }

    /// The reads put the hub's state in fields: people with their token
    /// records and workspaces, the workspaces with their members, and the
    /// leases of the workspaces the admin is a member of only.
    #[test]
    fn the_reads_are_fields_and_follow_the_hubs_visibility() {
        let Some(hub) = Hub::start("reads") else { return };
        let mut root = hub.dial(&hub.root).unwrap();
        let ann = token_of(&admin_add(&mut root, "ann"));
        let mut a = hub.dial(&ann).unwrap();
        let bo = token_of(&w(&mut a, &["user", "add", "bo"]).unwrap());
        let cy = token_of(&w(&mut a, &["user", "add", "cy"]).unwrap());
        for (ws, user) in [("team", "ann"), ("team", "bo"), ("design", "cy")] {
            let _ = w(&mut a, &["workspace", "add", ws]);
            w(&mut a, &["member", "add", ws, user]).unwrap();
        }
        let team = WorkspaceId("shared:team".into());
        let design = WorkspaceId("shared:design".into());
        let (tok, _) = hub.hub.client(&bo).lease_take(&team, &RowId("wsp-1".into())).unwrap();
        hub.hub.client(&cy).lease_take(&design, &RowId("wsp-2".into())).unwrap();

        let users = doc(&w(&mut a, &["user", "ls"]).unwrap());
        let bo_row = users["users"].as_array().unwrap().iter().find(|u| u["name"] == "bo").unwrap().clone();
        assert_eq!((bo_row["role"].clone(), bo_row["state"].clone(), bo_row["workspaces"].clone()), (json!("user"), json!("active"), json!(["team"])));
        assert_eq!((bo_row["logs_in_with"].clone(), bo_row["live_tokens"].clone()), (json!(["issued"]), json!(1)));
        let t = &bo_row["tokens"][0];
        assert!(t["id"].as_str().unwrap().starts_with("t-bo-") && t["method"] == "issued" && t["state"] == "active" && t["revoked"].is_null(), "{t}");
        assert!(t["issued"].as_str().unwrap().ends_with('Z'), "{t}");
        assert!(!users.to_string().contains(&bo), "no token in a listing");

        let ws = doc(&w(&mut a, &["workspace", "ls"]).unwrap());
        assert_eq!(
            ws["workspaces"],
            json!([
                { "name": "design", "members": ["cy"], "member": false, "leases": null },
                { "name": "team", "members": ["ann", "bo"], "member": true, "leases": 1 },
            ])
        );

        let leases = doc(&w(&mut a, &["lease", "ls"]).unwrap());
        let l = &leases["leases"][0];
        assert_eq!(leases["leases"].as_array().unwrap().len(), 1, "design's lease is not shown to ann: {leases}");
        assert_eq!((l["workspace"].clone(), l["row"].clone(), l["holder"].clone(), l["token"].clone()), (json!("team"), json!("wsp-1"), json!("person:bo"), json!(tok.0)));
        assert!(l.get("machine").is_none(), "state carries no machine: {l}");
        assert_eq!(leases["not_shown"], json!(["design"]));
        let text = w(&mut a, &["lease", "ls"]).unwrap().lines.join("\n");
        assert!(text.contains("wsp-1") && text.contains("held by person:bo") && text.contains("not shown to you (you are not a member): design"), "{text}");

        let st = doc(&run_words(&mut a, &["state"], &Opts { url: "tcp://hub.example.com:7489".into(), ..Opts::default() }).unwrap());
        assert_eq!((st["hub"]["server"].clone(), st["hub"]["by"].clone(), st["hub"]["admin"].clone()), (json!("wsp-hub"), json!("person:ann"), json!(true)));
        assert_eq!((st["hub"]["pruned"].clone(), st["hub"]["oldest_event"]["id"].clone()), (json!(false), json!(1)));
        assert_eq!(st["users"].as_array().unwrap().len(), 4);
        assert_eq!(st["not_shown"], json!(["design"]));
    }

    /// `wsp hub events` pages as the hub does, filters a page, and reads a
    /// start past what the hub keeps as data: `too_old` and where to read on.
    #[test]
    fn the_trail_pages_filters_and_says_the_horizon() {
        let Some(mut hub) = Hub::start("events") else { return };
        let mut root = hub.dial(&hub.root).unwrap();
        w(&mut root, &["workspace", "add", "team"]).unwrap();
        w(&mut root, &["member", "add", "team", "root"]).unwrap();
        let bo = token_of(&w(&mut root, &["user", "add", "bo"]).unwrap());
        w(&mut root, &["member", "add", "team", "bo"]).unwrap();
        hub.hub.client(&bo).lease_take(&WorkspaceId("shared:team".into()), &RowId("wsp-1".into())).unwrap();

        let all = |client: &mut Client, ask: EventsAsk| run_words(client, &["events"], &Opts { events: ask, ..Opts::default() }).unwrap();
        let page = doc(&all(&mut root, EventsAsk::default()));
        let kinds: Vec<&str> = page["events"].as_array().unwrap().iter().map(|e| e["kind"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["hub.user.added", "hub.token.issued", "hub.workspace.added", "hub.member.set", "hub.user.added", "hub.token.issued", "hub.member.set", "lease.taken"]);
        assert_eq!((page["more"].clone(), page["next_from"].clone(), page["too_old"].clone()), (json!(false), Value::Null, Value::Null));
        let taken = page["events"][7].clone();
        assert_eq!((taken["principal"].clone(), taken["workspace"].clone(), taken["payload"]["row"].clone()), (json!("person:bo"), json!("team"), json!("wsp-1")));

        // Two at a time: the hub's own paging, `through` and `next_from` saying where to read on.
        let first = doc(&all(&mut root, EventsAsk { limit: Some(2), ..EventsAsk::default() }));
        assert_eq!((first["events"].as_array().unwrap().len(), first["through"].clone(), first["more"].clone(), first["next_from"].clone()), (2, json!(2), json!(true), json!(2)));
        let next = doc(&all(&mut root, EventsAsk { from: 2, limit: Some(2), ..EventsAsk::default() }));
        assert_eq!(next["events"][0]["id"], json!(3));

        let mut only = |f: Filter| doc(&all(&mut root, EventsAsk { filter: f, ..EventsAsk::default() }))["events"].as_array().unwrap().iter().map(|e| e["id"].as_u64().unwrap()).collect::<Vec<_>>();
        assert_eq!(only(Filter { kinds: vec!["lease".into()], ..Filter::default() }), vec![8]);
        assert_eq!(only(Filter { person: Some("bo".into()), ..Filter::default() }), vec![5, 6, 7, 8]);
        assert_eq!(only(Filter { row: Some("wsp-1".into()), ..Filter::default() }), vec![8]);
        assert_eq!(only(Filter { workspace: Some("team".into()), ..Filter::default() }), vec![3, 4, 7, 8]);

        // Pruned as the hub's sweep prunes (its events and its prune mark), on a stopped hub's file.
        if !matches!(hub.hub.backend, Backend::Bin(_)) || std::process::Command::new("sqlite3").arg("-version").output().is_err() {
            return;
        }
        drop(root);
        hub.hub.stop();
        let db = hub.hub.dir().join("data").join("ledger.db");
        let out = std::process::Command::new("sqlite3")
            .arg(&db)
            .arg("DELETE FROM events WHERE id <= 4; INSERT INTO events_pruned (id, through) VALUES (1, 4) ON CONFLICT(id) DO UPDATE SET through = MAX(through, excluded.through);")
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        hub.hub.restart();
        let mut root = hub.dial(&hub.root).unwrap();
        let old = all(&mut root, EventsAsk::default());
        let v = doc(&old);
        assert_eq!((v["too_old"].clone(), v["next_from"].clone(), v["events"].clone(), v["head"].clone()), (json!({ "oldest": 5 }), json!(4), json!([]), Value::Null));
        assert_eq!(old.lines, vec!["the events after 0 are past what the hub keeps: the oldest it keeps is event 5; read on with --from 4".to_string()]);
        assert_eq!(doc(&all(&mut root, EventsAsk { from: 4, ..EventsAsk::default() }))["events"][0]["id"], json!(5));
        let st = doc(&w(&mut root, &["state"]).unwrap());
        assert_eq!((st["hub"]["pruned"].clone(), st["hub"]["oldest_kept_id"].clone(), st["hub"]["oldest_event"]["id"].clone()), (json!(true), json!(5), json!(5)));
    }

    #[test]
    fn a_token_is_printed_once_and_a_listing_prints_none() {
        let minted = Minted { user: "ed".into(), id: "t-ed-1".into(), token: "ab".repeat(32) };
        let done = Done { lines: vec!["user ed added".into()], token: Some(minted), ..Done::default() };
        let text = render(&done, false, Some("tls://hub.example:7489"));
        assert_eq!(text.matches(&"ab".repeat(32)).count(), 1, "{text}");
        assert!(text.contains("wsp login tls://hub.example:7489"), "{text}");
        let json = render(&done, true, None);
        assert_eq!(json.matches(&"ab".repeat(32)).count(), 1, "{json}");
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["token"], json!({ "user": "ed", "id": "t-ed-1", "token": "ab".repeat(32) }), "the token is a field of its own");
        assert!(!v["lines"].to_string().contains(&"ab".repeat(32)));
        let none = render(&Done { lines: vec!["no users".into()], ..Done::default() }, false, None);
        assert_eq!(none, "no users");
    }

    /// Every refusal an admin verb can get reads as words, never Rust's debug form.
    #[test]
    fn a_refusal_is_a_sentence_not_rusts_debug_form() {
        use wsp_store_contract::{EventId, Subject};
        let cases = [
            (
                Refusal::Illegal { subject: Subject::User(UserName("bo".into())), state: "the last active admin".into(), verb: "hub.user.revoke".into() },
                "user bo is the last active admin, so the hub refuses hub.user.revoke",
            ),
            (Refusal::Invalid { reason: "cy is a name the hub has had; a name is never reused".into() }, "cy is a name the hub has had; a name is never reused"),
            (Refusal::NotFound { subject: Subject::Workspace(SharedName("design".into())) }, "the hub has no shared workspace design"),
            (Refusal::NotFound { subject: Subject::Credential(TokenId("t-bo-1".into())) }, "the hub has no token record t-bo-1"),
            (Refusal::TooOld { oldest: EventId(41) }, "the events asked for are past what the hub keeps: the oldest it keeps is event 41"),
        ];
        for (r, want) in cases {
            let text = format!("{r:?}");
            assert_eq!(say(Error::Refused(r, text)).to_string(), want);
        }
    }

    #[test]
    fn the_words_are_checked_before_anything_is_dialled() {
        assert!(known(&["user", "add", "ed"]) && known(&["member", "rm", "core", "ed"]) && known(&["token", "ed"]));
        assert!(known(&["state"]) && known(&["events"]) && known(&["lease", "ls"]) && known(&["workspace", "ls"]) && known(&["token", "revoke", "t-ed-1"]));
        assert!(!known(&["user"]) && !known(&["member", "set", "a", "b"]) && !known(&[]) && !known(&["user", "add"]));
        assert!(!known(&["token", "revoke"]) && !known(&["workspace", "ls", "x"]) && !known(&["events", "x"]));
    }
}
