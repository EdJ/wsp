//! The identity machine (multiplayer v1; wsp-159): the hub's users and token
//! records, as a pure [`Domain`]. [`Identity::owns`] says which commands are
//! its, so the hub routes them here.
//!
//! | verb | by | does | refused otherwise with |
//! |---|---|---|---|
//! | `hub.user.add` | an active admin user; or, for `admin: true` only, the hub's own instance (`wsp:<instance>`) while no active admin exists | `hub.user.added` | `forbidden`, `invalid` (empty name, or a name the hub has had) |
//! | `hub.user.revoke` | an active admin user | `hub.user.revoked`, then `hub.token.revoked` for each of the user's active tokens, in one decision; a revoked user is an empty decision | `forbidden`, `not_found` (user) |
//! | `hub.token.issue` | an active admin user | `hub.token.issued` | `forbidden`, `not_found` (user), `illegal` (user, `revoked`), `invalid` (id taken, or a hash that is not 64 hex characters) |
//! | `hub.token.revoke` | an active admin user | `hub.token.revoked`; a revoked token is an empty decision | `forbidden`, `not_found` (token) |
//!
//! Who may is checked first, so a principal that may not learns nothing of
//! the state. An admin user is a [`Principal::Person`] whose name is an active
//! user with `admin`. A name is never reused, a revoked user's record is kept.
//!
//! **The events.** `hub.user.added` carries `{"admin"}`, `hub.user.revoked`
//! `null`, `hub.token.issued` `{"user", "hash"}` and `hub.token.revoked`
//! `null`; subjects `User(name)` and `Credential(id)`. A token's `issued` and
//! `revoked` moments are the event's own `at`. Membership of shared workspaces
//! (`hub.workspace.*`) comes with wsp-168's contract.

use serde_json::{json, Value};
use std::result::Result;
use wsp_store_contract::*;

/// The identity machine. Stateless: everything it knows is in the [`State`] it is given.
#[derive(Clone, Copy, Debug, Default)]
pub struct Identity;

impl Identity {
    /// Whether `cmd` is one of this machine's: the four `hub.user.*` and `hub.token.*` verbs.
    pub fn owns(cmd: &Command) -> bool {
        matches!(
            cmd,
            Command::HubUserAdd { .. }
                | Command::HubUserRevoke { .. }
                | Command::HubTokenIssue { .. }
                | Command::HubTokenRevoke { .. }
        )
    }
}

/// Whether `by` is an active admin user.
fn is_admin(state: &State, by: &Principal) -> bool {
    match by {
        Principal::Person(name) => {
            state.users.get(&UserName(name.clone())).is_some_and(|u| u.admin && u.state == IdentityState::Active)
        }
        _ => false,
    }
}

fn no_admin(state: &State) -> bool {
    !state.users.values().any(|u| u.admin && u.state == IdentityState::Active)
}

fn forbidden(by: &Principal) -> Refusal {
    Refusal::Forbidden { by: by.clone() }
}

fn draft(kind: EventKind, subject: Subject, payload: Value) -> Draft {
    Draft { kind, subject, workspace: Default::default(), payload }
}

fn is_hash(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

impl Domain for Identity {
    fn decide(&self, state: &State, cmd: &Command, by: &Principal) -> Result<Vec<Draft>, Refusal> {
        match cmd {
            Command::HubUserAdd { name, admin } => {
                let bootstrap = matches!(by, Principal::Wsp(_)) && *admin && no_admin(state);
                if !is_admin(state, by) && !bootstrap {
                    return Err(forbidden(by));
                }
                if name.0.trim().is_empty() {
                    return Err(Refusal::Invalid { reason: "a user needs a name".into() });
                }
                if state.users.contains_key(name) {
                    return Err(Refusal::Invalid {
                        reason: format!("{} is a name the hub has had; a name is never reused", name.0),
                    });
                }
                Ok(vec![draft(EventKind::HubUserAdded, Subject::User(name.clone()), json!({ "admin": admin }))])
            }
            Command::HubUserRevoke { name } => {
                if !is_admin(state, by) {
                    return Err(forbidden(by));
                }
                let user = state.users.get(name).ok_or_else(|| Refusal::NotFound { subject: Subject::User(name.clone()) })?;
                if user.state == IdentityState::Revoked {
                    return Ok(vec![]);
                }
                let mut out = vec![draft(EventKind::HubUserRevoked, Subject::User(name.clone()), Value::Null)];
                out.extend(
                    state
                        .credentials
                        .values()
                        .filter(|c| c.user == *name && c.state == IdentityState::Active)
                        .map(|c| draft(EventKind::HubTokenRevoked, Subject::Credential(c.id.clone()), Value::Null)),
                );
                Ok(out)
            }
            Command::HubTokenIssue { id, user, hash } => {
                if !is_admin(state, by) {
                    return Err(forbidden(by));
                }
                let u = state.users.get(user).ok_or_else(|| Refusal::NotFound { subject: Subject::User(user.clone()) })?;
                if u.state == IdentityState::Revoked {
                    return Err(Refusal::Illegal {
                        subject: Subject::User(user.clone()),
                        state: "revoked".into(),
                        verb: "hub.token.issue".into(),
                    });
                }
                if state.credentials.contains_key(id) {
                    return Err(Refusal::Invalid { reason: format!("token record {} exists", id.0) });
                }
                if !is_hash(hash) {
                    return Err(Refusal::Invalid { reason: "a token's hash is 64 lowercase hex characters".into() });
                }
                Ok(vec![draft(
                    EventKind::HubTokenIssued,
                    Subject::Credential(id.clone()),
                    json!({ "user": user, "hash": hash }),
                )])
            }
            Command::HubTokenRevoke { id } => {
                if !is_admin(state, by) {
                    return Err(forbidden(by));
                }
                let c = state
                    .credentials
                    .get(id)
                    .ok_or_else(|| Refusal::NotFound { subject: Subject::Credential(id.clone()) })?;
                if c.state == IdentityState::Revoked {
                    return Ok(vec![]);
                }
                Ok(vec![draft(EventKind::HubTokenRevoked, Subject::Credential(id.clone()), Value::Null)])
            }
            _ => Err(Refusal::Invalid { reason: "not an identity command".into() }),
        }
    }

    fn evolve(&self, mut state: State, event: &Event) -> State {
        match (&event.kind, &event.subject) {
            (EventKind::HubUserAdded, Subject::User(name)) => {
                let admin = event.payload.get("admin").and_then(Value::as_bool).unwrap_or(false);
                state
                    .users
                    .insert(name.clone(), User { name: name.clone(), state: IdentityState::Active, admin });
            }
            (EventKind::HubUserRevoked, Subject::User(name)) => {
                if let Some(u) = state.users.get_mut(name) {
                    u.state = IdentityState::Revoked;
                }
            }
            (EventKind::HubTokenIssued, Subject::Credential(id)) => {
                let user = event.payload.get("user").and_then(Value::as_str);
                let hash = event.payload.get("hash").and_then(Value::as_str);
                if let (Some(user), Some(hash)) = (user, hash) {
                    state.credentials.insert(
                        id.clone(),
                        Credential {
                            id: id.clone(),
                            user: UserName(user.to_string()),
                            hash: hash.to_string(),
                            state: IdentityState::Active,
                            issued: event.at,
                            revoked: None,
                        },
                    );
                }
            }
            (EventKind::HubTokenRevoked, Subject::Credential(id)) => {
                if let Some(c) = state.credentials.get_mut(id) {
                    c.state = IdentityState::Revoked;
                    c.revoked = Some(event.at);
                }
            }
            _ => {}
        }
        state
    }
}
