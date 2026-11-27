//! What a hub admin reads of the hub, as fields (`wsp hub state`, `user ls`,
//! `workspace ls`, `lease ls`, `events`): the views `cmd_hub` prints, as JSON
//! and as text.
//!
//! Everything here is what the hub sent, put in fields; nothing is guessed.
//! The hub's `state` verb is an admin's (`wsp-hub crates/ledger/src/conn.rs`,
//! `admits`) and answers the identity whole (users, token records with no
//! hash, shared workspaces, members) and the held leases of the workspaces
//! the caller is a member of only (`identity.rs`, `state_for`). Its `events`
//! verb answers, after an id, at most [`MAX_EVENTS`] events or half a line of
//! them, of those the caller may read: identity events always, a shared
//! workspace's only where the caller is a member, and `through` says where
//! the page stopped (`conn.rs`, `events`, `may_read`). From before what the
//! hub keeps it is refused `too_old` with the oldest id it keeps, which is
//! data here ([`Page::too_old`]), not a failure.
//!
//! A token record's login method is its id's prefix: `gh-` a GitHub login
//! issued it (`wsp-hub crates/ledger/src/github.rs`, `TOKEN_PREFIX`); any
//! other id is a token an admin issued with `hub.token.issue` (`t-<user>-…`
//! from `wsp hub user add` / `wsp hub token`, `t-<admin>-init` from the
//! host's `admin init`), which the hub keeps out of the GitHub rules.
//!
//! A lease in `state` carries its holder, token and expiry, not the machine it
//! is held on (the contract's `Lease`); the hub's reply carries the machine
//! beside it, by lease name (`lease_machines`, the hub's b5c19f9), for those
//! held on one. `machine` is that, or `null` where the reply names none (a
//! holder that named no machine, or a hub older than it), never guessed.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use wsp_hub_protocol::{principal, Answer, Call, Outcome, Verb, MAX_EVENTS};
use wsp_store_contract::{At, Credential, Event, EventId, IdentityState, LeaseName, Principal, Refusal, State, Subject, WorkspaceId};

/// The machine each lease is held on, by its name in `state.leases`, as the hub's `state` reply names it.
pub type Machines = BTreeMap<LeaseName, wsp_hub_protocol::Machine>;

use crate::hub_client::{self, words, Client, Error};
use wsp_engine::util;

/// How the id of a token record a GitHub login issued starts.
pub const GITHUB_PREFIX: &str = "gh-";

/// A token record's login method, from its id: `github` or `issued`.
pub fn method_of(id: &str) -> &'static str {
    if id.starts_with(GITHUB_PREFIX) {
        "github"
    } else {
        "issued"
    }
}

/// A hub moment (milliseconds) as `2026-11-27T09:41:00Z`.
pub fn iso(at: At) -> String {
    util::iso_at((at.0 / 1000) as i64)
}

fn state_word(s: &IdentityState) -> &'static str {
    match s {
        IdentityState::Active => "active",
        IdentityState::Revoked => "revoked",
    }
}

/// A token record as fields; never the secret (the hub keeps only a hash, and
/// `state` sends that empty).
pub fn token_json(c: &Credential) -> Value {
    json!({
        "id": c.id.0,
        "user": c.user.0,
        "method": method_of(&c.id.0),
        "state": state_word(&c.state),
        "issued": iso(c.issued),
        "revoked": c.revoked.map(iso),
    })
}

/// The workspaces each user is a member of.
fn memberships(state: &State) -> BTreeMap<String, Vec<String>> {
    let mut of: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (ws, users) in &state.members {
        for u in users {
            of.entry(u.0.clone()).or_default().push(ws.0.clone());
        }
    }
    of
}

fn tokens_of<'a>(state: &'a State, user: &'a str) -> impl Iterator<Item = &'a Credential> + 'a {
    state.credentials.values().filter(move |c| c.user.0 == user)
}

fn live_methods(state: &State, user: &str) -> Vec<&'static str> {
    let set: BTreeSet<&'static str> = tokens_of(state, user).filter(|c| c.state == IdentityState::Active).map(|c| method_of(&c.id.0)).collect();
    set.into_iter().collect()
}

/// The hub's people: name, role, state, the login methods of their live
/// tokens, each token record, and the workspaces each is a member of.
pub fn people(state: &State) -> Vec<Value> {
    let of = memberships(state);
    state
        .users
        .values()
        .map(|u| {
            let tokens: Vec<Value> = tokens_of(state, &u.name.0).map(token_json).collect();
            let live = tokens_of(state, &u.name.0).filter(|c| c.state == IdentityState::Active).count();
            json!({
                "name": u.name.0,
                "role": if u.admin { "admin" } else { "user" },
                "state": state_word(&u.state),
                "logs_in_with": live_methods(state, &u.name.0),
                "live_tokens": live,
                "tokens": tokens,
                "workspaces": of.get(&u.name.0).cloned().unwrap_or_default(),
            })
        })
        .collect()
}

/// [`people`] as text: a line per person, a line per token record under it.
pub fn people_lines(state: &State) -> Vec<String> {
    let of = memberships(state);
    let mut lines = Vec::new();
    for u in state.users.values() {
        let live = tokens_of(state, &u.name.0).filter(|c| c.state == IdentityState::Active).count();
        let role = if u.admin { "admin" } else { "user" };
        let ws = of.get(&u.name.0).map(|w| w.join(", ")).unwrap_or_default();
        let ws = if ws.is_empty() { "in no workspace".to_string() } else { format!("in {ws}") };
        lines.push(format!("{:<20} {role:<6} {:<8} {live} token(s)  {ws}", u.name.0, state_word(&u.state)));
        for c in tokens_of(state, &u.name.0) {
            let state = match c.revoked {
                Some(at) if c.state == IdentityState::Revoked => format!("revoked {}", iso(at)),
                _ => state_word(&c.state).to_string(),
            };
            lines.push(format!("    {}  {:<6}  issued {}  {state}", c.id.0, method_of(&c.id.0), iso(c.issued)));
        }
    }
    if lines.is_empty() {
        lines.push("no users".into());
    }
    lines
}

/// The user a principal is, if a person.
pub fn person(by: &Principal) -> Option<&str> {
    match by {
        Principal::Person(name) => Some(name),
        _ => None,
    }
}

/// The workspace and row a lease name of `state` names
/// (`hubrow:["shared:team","wsp-412"]`, the hub's `leases::state_name`).
pub fn lease_where(name: &str) -> Option<(String, String)> {
    let (ws, row): (String, String) = serde_json::from_str(name.strip_prefix("hubrow:")?).ok()?;
    Some((ws.strip_prefix("shared:").unwrap_or(&ws).to_string(), row))
}

fn is_member(state: &State, ws: &str, me: Option<&str>) -> bool {
    me.is_some_and(|me| state.members.iter().any(|(w, users)| w.0 == ws && users.iter().any(|u| u.0 == me)))
}

/// The shared workspaces, each with its members; `member` says whether the
/// caller `me` is one, and `leases` how many leases `state` shows in it,
/// `null` where the caller is not a member (the hub shows none: not zero, unknown).
pub fn workspaces(state: &State, me: Option<&str>) -> Vec<Value> {
    state
        .workspaces
        .keys()
        .map(|w| {
            let members: Vec<&str> = state.members.get(w).map(|m| m.iter().map(|u| u.0.as_str()).collect()).unwrap_or_default();
            let member = is_member(state, &w.0, me);
            let leases = member.then(|| state.leases.keys().filter(|n| lease_where(&n.0).is_some_and(|(ws, _)| ws == w.0)).count());
            json!({ "name": w.0, "members": members, "member": member, "leases": leases })
        })
        .collect()
}

pub fn workspaces_lines(state: &State, me: Option<&str>) -> Vec<String> {
    let mut lines = Vec::new();
    for v in workspaces(state, me) {
        let name = v["name"].as_str().unwrap_or_default();
        let members: Vec<&str> = v["members"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
        let who = if members.is_empty() { "no members".to_string() } else { format!("{} member(s): {}", members.len(), members.join(", ")) };
        let you = match v["leases"].as_u64() {
            Some(n) => format!("you are a member; {n} lease(s)"),
            None => "you are not a member: its leases and events are not shown to you".to_string(),
        };
        lines.push(format!("{name:<20} {who}  ({you})"));
    }
    if lines.is_empty() {
        lines.push("no shared workspaces".into());
    }
    lines
}

/// The leases `state` shows (only those of workspaces the caller is a member
/// of): workspace, row, holder, fencing token, expiry, and the machine
/// `{id, name}` it is held on where the reply names one (`null` where not).
pub fn leases(state: &State, machines: &Machines) -> Vec<Value> {
    state
        .leases
        .values()
        .map(|l| {
            let (ws, row) = lease_where(&l.name.0).map_or((Value::Null, Value::Null), |(w, r)| (json!(w), json!(r)));
            json!({
                "name": l.name.0,
                "workspace": ws,
                "row": row,
                "holder": principal::text(&l.holder),
                "token": l.token.0,
                "expires": iso(l.expires),
                "machine": machines.get(&l.name).map(|m| json!({ "id": m.id, "name": m.name })),
            })
        })
        .collect()
}

/// The shared workspaces the caller is not a member of: their leases and
/// events the hub does not show it.
pub fn not_shown(state: &State, me: Option<&str>) -> Vec<String> {
    state.workspaces.keys().filter(|w| !is_member(state, &w.0, me)).map(|w| w.0.clone()).collect()
}

pub fn leases_lines(state: &State, machines: &Machines, me: Option<&str>) -> Vec<String> {
    let mut lines: Vec<String> = leases(state, machines)
        .iter()
        .map(|l| {
            let on = l["machine"]["name"].as_str().map(|m| format!(" on {m}")).unwrap_or_default();
            format!(
                "{:<14} {:<12} held by {}{on}  token {}  until {}",
                l["row"].as_str().unwrap_or("?"),
                l["workspace"].as_str().unwrap_or("?"),
                l["holder"].as_str().unwrap_or_default(),
                l["token"],
                l["expires"].as_str().unwrap_or_default()
            )
        })
        .collect();
    if lines.is_empty() {
        lines.push("no leases you can see".into());
    }
    let hidden = not_shown(state, me);
    if !hidden.is_empty() {
        lines.push(format!("not shown to you (you are not a member): {}", hidden.join(", ")));
    }
    lines
}

/// One page of `events`, or the hub's `too_old` with the oldest id it keeps.
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    pub from: EventId,
    pub events: Vec<Event>,
    /// The hub's head when it read them; none on `too_old`, which carries none.
    pub head: Option<EventId>,
    /// Where the page stopped: the next page reads after it.
    pub through: EventId,
    /// `too_old`: `from` is past what the hub keeps; this is the oldest id it keeps.
    pub too_old: Option<EventId>,
}

/// `events {from, limit}`, once.
pub fn events_page(client: &mut Client, from: EventId, limit: Option<u32>) -> Result<Page, Error> {
    let reply = client.request(Call::Verb(Verb::Events { from, limit }))?;
    match reply.outcome {
        Outcome::Ok(Answer::Events { events, head, through }) => Ok(Page { from, events, head: Some(head), through, too_old: None }),
        Outcome::Ok(other) => Err(Error::Unexpected(format!("events was answered {}", serde_json::to_string(&other).unwrap_or_default()))),
        Outcome::Err(f) => match f.refusal {
            Some(Refusal::TooOld { oldest }) => Ok(Page { from, events: Vec::new(), head: None, through: from, too_old: Some(oldest) }),
            _ => Err(hub_client::refused(f)),
        },
    }
}

/// Where the hub's trail starts for this caller: the first event it returns
/// reading from the start of what it keeps, and whether anything was pruned
/// (`from` 0 refused `too_old`, naming the oldest id kept).
#[derive(Debug, Clone, PartialEq)]
pub struct Horizon {
    pub pruned: Option<EventId>,
    pub first: Option<(EventId, At)>,
}

pub fn horizon(client: &mut Client) -> Result<Horizon, Error> {
    let mut page = events_page(client, EventId(0), Some(MAX_EVENTS))?;
    let pruned = page.too_old;
    if let Some(oldest) = pruned {
        page = events_page(client, EventId(oldest.0.saturating_sub(1)), Some(MAX_EVENTS))?;
    }
    Ok(Horizon { pruned, first: page.events.first().map(|e| (e.id, e.at)) })
}

pub fn horizon_json(h: &Horizon) -> Value {
    json!({
        "oldest_event": h.first.map(|(id, at)| json!({ "id": id.0, "at": iso(at) })),
        "pruned": h.pruned.is_some(),
        "oldest_kept_id": h.pruned.map(|e| e.0),
    })
}

/// An event's kind by its wire name (`hub.member.set`).
pub fn kind_name(e: &Event) -> String {
    serde_json::to_value(e.kind).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_else(|| "unknown".into())
}

fn workspace_name(w: &WorkspaceId) -> Option<&str> {
    match w {
        WorkspaceId::Private => None,
        WorkspaceId::Shared(n) => Some(&n.0),
    }
}

/// An event as fields: id, time, principal (text form), kind, subject (the
/// hub's form), workspace (`null`: an identity event, in none) and payload,
/// as the hub sent them.
pub fn event_json(e: &Event) -> Value {
    json!({
        "id": e.id.0,
        "at": iso(e.at),
        "principal": principal::text(&e.principal),
        "kind": kind_name(e),
        "subject": serde_json::to_value(&e.subject).unwrap_or(Value::Null),
        "workspace": workspace_name(&e.workspace),
        "payload": e.payload,
    })
}

pub fn event_line(e: &Event) -> String {
    let mut line = format!("#{:<6} {}  {:<14} {:<18} {}", e.id.0, iso(e.at), principal::text(&e.principal), kind_name(e), words::subject(&e.subject));
    if let Some(w) = workspace_name(&e.workspace) {
        line.push_str(&format!(" in {w}"));
    }
    if !e.payload.is_null() {
        line.push_str(&format!("  {}", e.payload));
    }
    line
}

/// Which events `wsp hub events` keeps of a page; each filter given must hold.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    /// A kind (`lease.taken`) or a kind's prefix up to a dot (`lease`, `hub.token`); any of them.
    pub kinds: Vec<String>,
    /// A shared workspace: the event is in it, or is about it or a membership of it.
    pub workspace: Option<String>,
    /// A user: the event is by them, about them or their membership or token
    /// record, a token issued to them, or a lease they hold.
    pub person: Option<String>,
    /// A row: the event is about it, its claim, or its lease.
    pub row: Option<String>,
    /// Events at or after this moment (hub milliseconds).
    pub since: Option<u64>,
}

impl Filter {
    pub fn is_empty(&self) -> bool {
        *self == Filter::default()
    }

    /// Whether `e` passes; `owners` maps a token record's id to its user, for `person`.
    pub fn keeps(&self, e: &Event, owners: &BTreeMap<String, String>) -> bool {
        let kind = kind_name(e);
        if !self.kinds.is_empty() && !self.kinds.iter().any(|k| kind == *k || kind.starts_with(&format!("{k}."))) {
            return false;
        }
        if let Some(w) = &self.workspace {
            let about = match &e.subject {
                Subject::Workspace(n) | Subject::Member { workspace: n, .. } => n.0 == *w,
                _ => false,
            };
            if workspace_name(&e.workspace) != Some(w.as_str()) && !about {
                return false;
            }
        }
        if let Some(p) = &self.person {
            let holder = e.payload.get("holder").and_then(|h| serde_json::from_value::<Principal>(h.clone()).ok());
            let about = match &e.subject {
                Subject::User(u) | Subject::Member { user: u, .. } => u.0 == *p,
                Subject::Credential(t) => owners.get(&t.0) == Some(p),
                _ => false,
            };
            let by = person(&e.principal) == Some(p.as_str());
            let holds = holder.as_ref().and_then(person) == Some(p.as_str());
            let issued_to = e.payload.get("user").and_then(Value::as_str) == Some(p.as_str());
            if !(by || about || holds || issued_to) {
                return false;
            }
        }
        if let Some(r) = &self.row {
            let about = match &e.subject {
                Subject::Row(id) | Subject::Claim(id) => id.0 == *r,
                Subject::Lease(l) => l.0 == format!("row:{r}"),
                _ => false,
            };
            if !about && e.payload.get("row").and_then(Value::as_str) != Some(r.as_str()) {
                return false;
            }
        }
        if let Some(since) = self.since {
            if e.at.0 < since {
                return false;
            }
        }
        true
    }
}

/// `--since`: a time (`2026-11-27`, `2026-11-27T09:00:00Z`) or a span back
/// from `now_secs` (`90s`, `15m`, `1h`, `2d`), as hub milliseconds.
pub fn parse_since(s: &str, now_secs: i64) -> Option<u64> {
    let s = s.trim();
    let looks_dated = s.len() >= 10 && s.as_bytes()[4] == b'-' && s.as_bytes()[7] == b'-' && s[..4].chars().all(|c| c.is_ascii_digit());
    if looks_dated {
        let secs = util::epoch_of(s);
        return (secs > 0).then(|| secs as u64 * 1000);
    }
    let (n, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit())?);
    let n: i64 = n.parse().ok()?;
    let secs = match unit {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86_400,
        _ => return None,
    };
    Some((now_secs - secs).max(0) as u64 * 1000)
}

/// Token record id → user, for [`Filter::keeps`].
pub fn owners(state: &State) -> BTreeMap<String, String> {
    state.credentials.values().map(|c| (c.id.0.clone(), c.user.0.clone())).collect()
}

/// The keys of [`info_json`], each `null` where the hub does not say.
const INFO_KEYS: &[&str] = &["version", "protocol", "server", "quotas", "event_days", "lease_secs", "sweep_secs", "max_connections", "max_connections_per_peer", "login", "usage"];

/// What `hub.info` said, as the hub spells it (a usage's workspace by its
/// name, `team`); every key `null` where the hub did not say (`None`: a hub
/// older than `hub.info`, which answers `unsupported`). `login` is `null` too
/// on a hub with no login (an admin issues every token).
pub fn info_json(info: Option<&wsp_hub_protocol::HubInfo>) -> Value {
    let mut out = serde_json::Map::new();
    let said = info.and_then(|i| serde_json::to_value(i).ok());
    for k in INFO_KEYS {
        out.insert((*k).into(), said.as_ref().and_then(|v| v.get(*k)).cloned().unwrap_or(Value::Null));
    }
    if let Some(Value::Array(usage)) = out.get_mut("usage") {
        for u in usage {
            if let Some(w) = u.get("workspace").and_then(Value::as_str).map(|w| w.strip_prefix("shared:").unwrap_or(w).to_string()) {
                u["workspace"] = json!(w);
            }
        }
    }
    Value::Object(out)
}

pub fn info_lines(info: Option<&wsp_hub_protocol::HubInfo>) -> Vec<String> {
    let Some(i) = info else { return vec!["facts       the hub does not say (a hub older than hub.info)".into()] };
    let q = &i.quotas;
    let mut lines = vec![
        format!("build       {} (protocol {})", i.version, i.protocol),
        format!("kept        events {} day(s); a lease runs {}s; swept every {}s", i.event_days, i.lease_secs, i.sweep_secs),
        format!("connections {} at most, {} per peer", i.max_connections, i.max_connections_per_peer),
        format!(
            "quotas      object {} B, {} objects a put, workspace {} B, {} refs a workspace, message {} B, queue {}, {} login tokens a person, ids {} ahead",
            q.max_object_bytes, q.max_objects_per_put, q.max_workspace_bytes, q.max_refs_per_workspace, q.max_message_bytes, q.max_queue, q.max_login_tokens, q.max_id_advance
        ),
        match &i.login {
            Some(l) => format!("login       {} (its tokens last {} day(s)); a token an admin issues does not expire", l.method, l.token_days),
            None => "login       none: an admin issues every token".into(),
        },
    ];
    for u in &i.usage {
        lines.push(format!("usage       {}: {} B of {}, {} ref(s) of {}", u.workspace.0.strip_prefix("shared:").unwrap_or(&u.workspace.0), u.bytes, q.max_workspace_bytes, u.refs, q.max_refs_per_workspace));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use wsp_store_contract::{EventKind, LeaseName, SharedName, TokenId, UserName};

    fn event(id: u64, kind: EventKind, subject: Subject, ws: Option<&str>, by: &str, payload: Value) -> Event {
        Event {
            id: EventId(id),
            at: At(1_764_236_460_000 + id * 1000),
            principal: Principal::Person(by.into()),
            kind,
            subject,
            workspace: ws.map_or(WorkspaceId::Private, |w| WorkspaceId::Shared(SharedName(w.into()))),
            payload,
        }
    }

    #[test]
    fn the_login_method_is_the_token_ids_prefix() {
        assert_eq!(method_of("gh-bo-0a1b2c3d4e5f"), "github");
        assert_eq!(method_of("t-bo-18f2a"), "issued");
        assert_eq!(method_of("t-ann-init"), "issued");
    }

    #[test]
    fn a_lease_name_gives_its_workspace_and_row() {
        assert_eq!(lease_where(r#"hubrow:["shared:team","wsp-412"]"#), Some(("team".into(), "wsp-412".into())));
        assert_eq!(lease_where("row:wsp-1"), None);
    }

    #[test]
    fn the_filters_keep_what_they_name() {
        let taken = event(
            3,
            EventKind::LeaseTaken,
            Subject::Lease(LeaseName("row:wsp-1".into())),
            Some("team"),
            "bo",
            json!({ "holder": { "person": "bo" }, "row": "wsp-1", "token": 2 }),
        );
        let member = event(4, EventKind::HubMemberSet, Subject::Member { workspace: SharedName("team".into()), user: UserName("cy".into()) }, None, "ann", json!({ "member": true }));
        let revoked = event(5, EventKind::HubTokenRevoked, Subject::Credential(TokenId("t-cy-1".into())), None, "ann", Value::Null);
        let owners: BTreeMap<String, String> = [("t-cy-1".to_string(), "cy".to_string())].into();
        let f = |f: Filter| [&taken, &member, &revoked].iter().filter(|e| f.keeps(e, &owners)).map(|e| e.id.0).collect::<Vec<_>>();
        assert_eq!(f(Filter { kinds: vec!["lease".into()], ..Filter::default() }), vec![3]);
        assert_eq!(f(Filter { kinds: vec!["hub.member.set".into(), "hub.token".into()], ..Filter::default() }), vec![4, 5]);
        assert_eq!(f(Filter { workspace: Some("team".into()), ..Filter::default() }), vec![3, 4]);
        assert_eq!(f(Filter { person: Some("cy".into()), ..Filter::default() }), vec![4, 5]);
        assert_eq!(f(Filter { person: Some("bo".into()), ..Filter::default() }), vec![3]);
        assert_eq!(f(Filter { row: Some("wsp-1".into()), ..Filter::default() }), vec![3]);
        assert_eq!(f(Filter { since: Some(taken.at.0 + 1000), ..Filter::default() }), vec![4, 5]);
    }

    #[test]
    fn since_takes_a_time_or_a_span() {
        assert_eq!(parse_since("2026-11-27", 0), Some(util::epoch_of("2026-11-27") as u64 * 1000));
        assert_eq!(parse_since("2026-11-27T09:00:00Z", 0), Some(util::epoch_of("2026-11-27T09:00:00Z") as u64 * 1000));
        assert_eq!(parse_since("2d", 200_000), Some((200_000 - 172_800) * 1000));
        assert_eq!(parse_since("15m", 1000), Some(100 * 1000));
        assert_eq!(parse_since("soon", 1000), None);
        assert_eq!(parse_since("5w", 1000), None);
    }
}
