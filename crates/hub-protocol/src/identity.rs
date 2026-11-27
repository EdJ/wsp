//! The identity commands: the hub's users, token records, shared workspaces
//! and members. On the wire each is a command of the state core's envelope
//! (`verb`, `args`, and the request's `key`), spelled exactly as the
//! contract's `Command` spells the same seven verbs; the hub's identity machine
//! decides them, and they are the only commands a hub runs. `hub.user.admin`
//! came after the first six: a hub older than it decodes it as an unknown
//! verb and answers it `unsupported`.

use serde::{Deserialize, Serialize};
use wsp_store_contract::{SharedName, TokenId, UserName};

/// The identity commands, as [`IdentityCommand`] names them on the wire.
pub const COMMAND_VERBS: &[&str] = &[
    "hub.user.add",
    "hub.user.revoke",
    "hub.token.issue",
    "hub.token.revoke",
    "hub.workspace.add",
    "hub.member.set",
    "hub.user.admin",
];

/// One identity command, tagged by `verb`, with its `args`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verb", content = "args")]
pub enum IdentityCommand {
    /// Add a hub user; a second user of one name is `invalid`.
    #[serde(rename = "hub.user.add")]
    HubUserAdd {
        name: UserName,
        #[serde(default)]
        admin: bool,
    },
    /// Revoke a user and every active token of theirs.
    #[serde(rename = "hub.user.revoke")]
    HubUserRevoke { name: UserName },
    /// Record a token for a user, by the hex SHA-256 of the token (never the token).
    #[serde(rename = "hub.token.issue")]
    HubTokenIssue { id: TokenId, user: UserName, hash: String },
    /// Revoke a token record.
    #[serde(rename = "hub.token.revoke")]
    HubTokenRevoke { id: TokenId },
    /// Add a shared workspace; a second of one name is `invalid`.
    #[serde(rename = "hub.workspace.add")]
    HubWorkspaceAdd { name: SharedName },
    /// Make a user a member of a shared workspace (`member: true`) or not.
    #[serde(rename = "hub.member.set")]
    HubMemberSet {
        workspace: SharedName,
        user: UserName,
        member: bool,
    },
    /// Make an active user a hub admin (`admin: true`) or not. Setting what
    /// is, is an empty decision; clearing the last active admin (oneself
    /// included) is `illegal`.
    #[serde(rename = "hub.user.admin")]
    HubUserAdmin { name: UserName, admin: bool },
}

impl IdentityCommand {
    /// The verb's name on the wire, one of [`COMMAND_VERBS`].
    pub fn verb(&self) -> &'static str {
        match self {
            IdentityCommand::HubUserAdd { .. } => "hub.user.add",
            IdentityCommand::HubUserRevoke { .. } => "hub.user.revoke",
            IdentityCommand::HubTokenIssue { .. } => "hub.token.issue",
            IdentityCommand::HubTokenRevoke { .. } => "hub.token.revoke",
            IdentityCommand::HubWorkspaceAdd { .. } => "hub.workspace.add",
            IdentityCommand::HubMemberSet { .. } => "hub.member.set",
            IdentityCommand::HubUserAdmin { .. } => "hub.user.admin",
        }
    }
}
