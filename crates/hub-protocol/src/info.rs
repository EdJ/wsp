//! `hub.info`: what a hub tells an admin of itself, read-only. Nothing
//! secret is in it: no token or hash, no file path, no address or port, no
//! peer range, no GitHub client id, no list of who may log in or who is made
//! an admin. A field added later is `#[serde(default)]`; one a newer hub
//! adds is passed over.

use serde::{Deserialize, Serialize};
use wsp_objects::WorkspaceId;

/// The answer to `hub.info`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubInfo {
    /// The hub's build (its package version).
    pub version: String,
    /// The protocol it speaks.
    pub protocol: u32,
    /// The instance's name, as the welcome says it.
    pub server: String,
    /// Every quota of the config's `quotas`, at its effective value.
    pub quotas: HubQuotas,
    /// How many days an event is kept.
    pub event_days: u32,
    /// A lease's TTL, in seconds: the config's `lease_secs`, else its `lease_days`.
    pub lease_secs: u64,
    /// How often expired leases and old events are swept, in seconds.
    pub sweep_secs: u64,
    /// How many connections the hub serves at once.
    pub max_connections: u64,
    /// How many of them one peer address may hold.
    pub max_connections_per_peer: u64,
    /// The hub's login, where it has one (none: an admin issues every token).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<HubLogin>,
    /// What each shared workspace the asking admin is a member of holds,
    /// against `max_workspace_bytes` and `max_refs_per_workspace`, by name.
    /// A workspace it is no member of is not here.
    #[serde(default)]
    pub usage: Vec<WorkspaceUsage>,
}

/// The quotas, as the config's `quotas` names them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubQuotas {
    pub max_object_bytes: u64,
    pub max_objects_per_put: u64,
    pub max_workspace_bytes: u64,
    pub max_message_bytes: u64,
    pub max_queue: u64,
    /// The most live tokens a login keeps for one person.
    pub max_login_tokens: u64,
    pub max_refs_per_workspace: u64,
    pub max_id_advance: u64,
}

/// The login a hub runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubLogin {
    /// `github`.
    pub method: String,
    /// How many days a token a login issued is good for. A token an admin
    /// issues by hand does not expire.
    pub token_days: u32,
}

/// What one shared workspace holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceUsage {
    pub workspace: WorkspaceId,
    /// The bytes of its objects, as `max_workspace_bytes` counts them.
    pub bytes: u64,
    /// Its refs, retired subjects included, as `max_refs_per_workspace` counts them.
    pub refs: u64,
}
