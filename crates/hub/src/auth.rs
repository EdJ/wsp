//! Who is speaking to the hub (wsp-161): the hello's token decides it, never
//! its `by`. The hub is public, so there is no mode that trusts the hello.
//!
//! - A token whose hash ([`wsp_machines::token::hash`]) is an active token
//!   record's, of an active user, names `person:<user>`.
//! - The hub's bootstrap token (`api.token` in its data directory, made on
//!   first start) names the hub itself, `wsp:<instance>`, while the hub has no
//!   active admin, and may then do one thing: `hub.user.add { admin: true }`.
//!   Once an admin exists it names nobody.
//! - Anything else is refused `unauthorised`, saying no more than that: an
//!   unknown token and a revoked one read alike.
//!
//! [`Auth`] is the [`Gate`] the hub hands wsp-server: the principal it
//! resolves replaces the hello's `by`, `welcome` names it, and a request whose
//! `by` is another is `forbidden` (wsp-server's own check, against it). The
//! identity commands are its to route ([`Identity::owns`]), so the identity
//! machine decides them under the connection's principal.
//!
//! A hello is resolved against the state as it is then; a connection keeps the
//! principal it was welcomed as until it closes.

use std::sync::Arc;

use wsp_api::{ApiToken, Call, Code, Failure, Hello};
use wsp_machines::{token, Identity};
use wsp_server::{Gate, Welcomed};
use wsp_store_contract::{Command, IdentityState, Principal, Refusal, State};

use crate::Hub;

/// The hub's gate: its store, and the instance its bootstrap principal names.
pub struct Auth {
    hub: Arc<Hub>,
    instance: String,
}

impl Auth {
    pub fn new(hub: Arc<Hub>, instance: impl Into<String>) -> Auth {
        Auth { hub, instance: instance.into() }
    }
}

/// Why a hello is refused, whichever way its token is wrong.
pub const UNKNOWN: &str = "the token is none this hub has issued, or it is revoked";

/// Why the bootstrap token's hello is refused once the hub has an admin.
pub const BOOTSTRAPPED: &str = "the hub's bootstrap token makes only the first admin, and the hub has an active admin";

fn no_admin(state: &State) -> bool {
    !state.users.values().any(|u| u.admin && u.state == IdentityState::Active)
}

/// Who `token` names on a hub in `state` whose instance is `instance` and
/// whose bootstrap token is `bootstrap`, or why it names nobody.
pub fn resolve(state: &State, instance: &str, bootstrap: &ApiToken, token: &str) -> Result<Principal, String> {
    let hash = token::hash(token);
    // Every record is compared, whichever matches, and only hashes are.
    let mut found = None;
    for c in state.credentials.values() {
        if token::same(&c.hash, &hash) && c.state == IdentityState::Active {
            found = Some(c.user.clone());
        }
    }
    if let Some(user) = found {
        return match state.users.get(&user) {
            Some(u) if u.state == IdentityState::Active => Ok(Principal::Person(user.0)),
            _ => Err(UNKNOWN.into()),
        };
    }
    if token::same(token, &bootstrap.0) {
        return if no_admin(state) { Ok(Principal::Wsp(instance.to_string())) } else { Err(BOOTSTRAPPED.into()) };
    }
    Err(UNKNOWN.into())
}

/// The id of the active token record `token` is, from the same state a hello
/// was resolved against; none for the bootstrap token.
pub fn credential(state: &State, token: &str) -> Option<String> {
    let hash = token::hash(token);
    state
        .credentials
        .values()
        .find(|c| token::same(&c.hash, &hash) && c.state == IdentityState::Active)
        .map(|c| c.id.0.clone())
}

/// Whether `by` may make `call` on the hub `instance`: the hub itself (the
/// bootstrap token) only `hub.user.add { admin: true }`; a person anything,
/// the identity machine deciding its identity commands.
pub fn admits(instance: &str, by: &Principal, call: &Call) -> Result<(), Failure> {
    match by {
        Principal::Person(_) => Ok(()),
        Principal::Wsp(i) if i == instance => match call {
            Call::Command(Command::HubUserAdd { admin: true, .. }) => Ok(()),
            _ => Err(forbidden(by, "the hub's bootstrap token makes the first admin (hub.user.add, admin) and nothing else")),
        },
        _ => Err(forbidden(by, "the hub speaks with its users and its bootstrap token only")),
    }
}

fn forbidden(by: &Principal, why: &str) -> Failure {
    Failure {
        code: Code::Forbidden,
        text: format!("{}: {why}", wsp_api::principal::text(by)),
        refusal: Some(Refusal::Forbidden { by: by.clone() }),
    }
}

impl Gate for Auth {
    fn hello(&self, hello: &Hello, bootstrap: &ApiToken) -> Result<Welcomed, String> {
        let state = self.hub.state().map_err(|e| format!("the hub's store did not answer: {e:?}"))?;
        let by = resolve(&state, &self.instance, bootstrap, &hello.token.0)?;
        Ok(Welcomed { by, credential: credential(&state, &hello.token.0) })
    }

    fn admits(&self, by: &Principal, call: &Call) -> Result<(), Failure> {
        admits(&self.instance, by, call)
    }

    fn owns(&self, cmd: &Command) -> bool {
        Identity::owns(cmd)
    }
}
