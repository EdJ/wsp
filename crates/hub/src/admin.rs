//! `wsp-hub admin init --config <file> --admin <name>` (wsp-163): the hub's
//! first admin, made once on the hub's host.
//!
//! The hub's bootstrap principal (`wsp:<instance>`) may add the first admin
//! and nothing else, and only an admin may issue a token, so the host makes
//! the admin as the bootstrap principal and then issues its token as that
//! admin: both through the identity machine, as it decides them when they
//! arrive over the API. The token is minted here ([`wsp_machines::token::mint`]),
//! the hub keeps its hash only, and [`init`] hands the token back for the
//! caller to print once. It is written nowhere: not the store, not a log.
//!
//! Refused once the hub has an active admin, whatever the name asked for.

use wsp_machines::token;
use wsp_store_contract::{Command, IdempotencyKey, IdentityState, Principal, TokenId, UserName};

use crate::{Config, Hub};

/// What [`init`] made: the admin, and its token (shown once, never kept).
pub struct First {
    pub admin: String,
    pub token: String,
}

impl std::fmt::Debug for First {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("First").field("admin", &self.admin).field("token", &"..").finish()
    }
}

/// The hub's URL as a spoke dials it, from its config: `tls://` with a
/// certificate, `tcp://` without; the first address bound that is not
/// loopback, else loopback.
pub fn url(cfg: &Config) -> String {
    let (scheme, host) = (
        if cfg.tls.is_some() { "tls" } else { "tcp" },
        cfg.bind.iter().find(|ip| !ip.is_loopback()).map_or_else(|| "127.0.0.1".to_string(), ToString::to_string),
    );
    format!("{scheme}://{host}:{}", cfg.port)
}

/// Makes the hub's first admin `admin` and its token, in the hub whose data
/// directory `cfg` names. Refused, naming why, when the name is empty or the
/// hub already has an active admin.
pub fn init(cfg: &Config, admin: &str) -> Result<First, String> {
    if admin.trim().is_empty() {
        return Err("--admin needs a name".into());
    }
    let hub = Hub::open(&cfg.data_dir).map_err(|e| format!("opening {}: {e:?}", cfg.data_dir.display()))?;
    let state = hub.state().map_err(|e| format!("reading the hub's state: {e:?}"))?;
    if let Some(a) = state.users.values().find(|u| u.admin && u.state == IdentityState::Active) {
        return Err(format!("the hub already has an admin ({}): `admin init` makes the first one only", a.name.0));
    }
    let name = UserName(admin.to_string());
    let bootstrap = Principal::Wsp(cfg.name.clone());
    hub.apply(Command::HubUserAdd { name: name.clone(), admin: true }, IdempotencyKey(format!("admin-init:user:{admin}")), bootstrap)
        .map_err(|e| format!("adding {admin}: {e:?}"))?;
    let (secret, hash) = token::mint();
    let id = TokenId(format!("t-{admin}-init"));
    hub.apply(Command::HubTokenIssue { id, user: name, hash }, IdempotencyKey(format!("admin-init:token:{admin}")), Principal::Person(admin.to_string()))
        .map_err(|e| format!("issuing {admin}'s token: {e:?}"))?;
    Ok(First { admin: admin.to_string(), token: secret })
}

/// What `admin init` prints: the token, once, and how to log in.
pub fn report(cfg: &Config, first: &First) -> String {
    format!(
        "admin {} made. Its token, shown once and kept nowhere:\n\n  {}\n\nlog in from a spoke with:\n\n  wsp login {}\n",
        first.admin,
        first.token,
        url(cfg),
    )
}
