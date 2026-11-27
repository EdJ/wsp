//! What this spoke knows of its user's shared workspaces, so a filing into a
//! workspace the user is not a member of is refused when it is filed
//! ([`wsp_engine::addon::SharedWork::may_file_into`]), not only by the hub at
//! the next sync.
//!
//! **Where it comes from.** The state the hub's stream opens with names each
//! shared workspace's members, scoped to the user (the `state` verb is an
//! admin's, refused `forbidden` to anyone else). The sync worker reads it on
//! every turn that reaches the hub and keeps the user's workspaces in
//! `$WSP_STATE/hub/members.json` ([`Members`]), with the user it was read for
//! and when; a filing that finds no yes there asks the hub itself and keeps
//! what it says.
//!
//! **What a filing decides** ([`check`]):
//!
//! - kept, and a member: filed, with no dial. A membership revoked since the
//!   last answer is the hub's to refuse at the sync (a row is refused at once,
//!   since its id is the hub's to give);
//! - otherwise, connected and switched on: the hub is asked now, and its
//!   answer decides (and is kept);
//! - the hub not answering, or switched off (which dials nothing): what is
//!   kept decides, a refusal saying when the hub last said so;
//! - nothing kept for this user (never connected here) and no hub to ask: the
//!   filing goes ahead, provisional, and the hub refuses it at the first sync,
//!   as before. A membership granted while offline is not known here either
//!   until a turn reaches the hub.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use wsp_engine::model::WorkspaceId;
use wsp_engine::store::Store;
use wsp_store_contract::UserName;

use crate::hub_client::{Client, Item};

/// `$WSP_STATE/hub/members.json`: the shared workspaces the hub last said
/// `user` is a member of, and when (epoch seconds).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Members {
    pub user: String,
    pub workspaces: BTreeSet<String>,
    pub at: i64,
}

fn path(store: &Store) -> std::path::PathBuf {
    store.state.join("hub").join("members.json")
}

impl Members {
    pub fn read(store: &Store) -> Option<Members> {
        let v: Value = serde_json::from_str(&std::fs::read_to_string(path(store)).ok()?).ok()?;
        Some(Members {
            user: v["user"].as_str()?.to_string(),
            workspaces: v["workspaces"].as_array()?.iter().filter_map(|w| w.as_str().map(str::to_string)).collect(),
            at: v["at"].as_i64().unwrap_or(0),
        })
    }

    pub fn write(&self, store: &Store) -> std::io::Result<()> {
        let path = path(store);
        let dir = path.parent().expect("under hub/");
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!(".members.json.{}.tmp", std::process::id()));
        std::fs::write(&tmp, json!({ "user": self.user, "workspaces": self.workspaces, "at": self.at }).to_string())?;
        std::fs::rename(&tmp, &path)
    }
}

/// The shared workspaces (`shared:<name>`) the user `client` is welcomed as
/// is a member of: the state the hub's stream opens with, which it scopes to
/// the user (the hub's `state` verb is an admin's); `Err` where it does not
/// say within the dial's wait.
pub fn of_hub(client: &mut Client) -> Result<Members, String> {
    let user = wsp_engine::door::user_of(client.by());
    let mut feed = client.subscribe(None);
    let state = match feed.next_within(WAIT) {
        Some(Item::State { state, .. }) => state,
        _ => return Err(feed.why_silent().map(|w| w.to_string()).unwrap_or_else(|| "the hub's stream did not open with its state".into())),
    };
    let me = UserName(user.clone());
    let workspaces = state
        .members
        .iter()
        .filter(|(w, users)| users.contains(&me) && state.workspaces.contains_key(*w))
        .map(|(w, _)| format!("shared:{}", w.0))
        .collect();
    Ok(Members { user, workspaces, at: wsp_engine::util::epoch_secs() })
}

/// How long the stream's opening state is waited for (the worker's dial wait).
const WAIT: std::time::Duration = std::time::Duration::from_secs(3);

/// The hub's answer now, kept; `None` where it did not give one.
pub fn refresh(store: &Store, client: &mut Client) -> Option<Members> {
    let m = of_hub(client).ok()?;
    let _ = m.write(store);
    Some(m)
}

/// Whether `user` may file into `ws`, from what is kept and, where that does
/// not say yes, from the hub when `ask` reaches it (see the module's doc).
pub fn check(store: &Store, ws: &WorkspaceId, user: &str, ask: impl FnOnce() -> Option<Client>) -> Result<(), String> {
    if ws.is_private() {
        return Ok(());
    }
    let name = ws.to_string();
    let kept = Members::read(store).filter(|m| m.user == user);
    if kept.as_ref().is_some_and(|m| m.workspaces.contains(&name)) {
        return Ok(());
    }
    let (known, fresh) = match ask().and_then(|mut c| refresh(store, &mut c)).filter(|m| m.user == user) {
        Some(m) => (Some(m), true),
        None => (kept, false),
    };
    let Some(m) = known else { return Ok(()) };
    if m.workspaces.contains(&name) {
        return Ok(());
    }
    let bare = name.strip_prefix("shared:").unwrap_or(&name);
    let when = match fresh {
        true => String::new(),
        false => format!(" (as the hub last said, {})", wsp_engine::util::iso_at(m.at)),
    };
    Err(format!("{user} is not a member of {name} on the hub{when}; an admin adds one with `wsp hub member add {bare} {user}`"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept(store: &Store, user: &str, ws: &[&str]) {
        Members { user: user.into(), workspaces: ws.iter().map(|w| w.to_string()).collect(), at: 1_800_000_000 }.write(store).unwrap();
    }

    #[test]
    fn members_what_is_kept_decides_where_the_hub_cannot_be_asked() {
        let (_env, store) = crate::testkit::isolated_store("members-kept");
        let team: WorkspaceId = "shared:team".parse().unwrap();
        let other: WorkspaceId = "shared:other".parse().unwrap();
        // Nothing kept, no hub: unknown, so yes.
        assert_eq!(check(&store, &other, "ann", || None), Ok(()));
        kept(&store, "ann", &["shared:team"]);
        assert_eq!(check(&store, &team, "ann", || panic!("a member is not asked again")), Ok(()));
        let why = check(&store, &other, "ann", || None).unwrap_err();
        assert!(why.starts_with("ann is not a member of shared:other on the hub (as the hub last said, 2027-01-15T08:00:00Z)"), "{why}");
        assert!(why.ends_with("`wsp hub member add other ann`"), "{why}");
        // Kept for another user (a new login): nothing known of this one.
        assert_eq!(check(&store, &other, "bo", || None), Ok(()));
        assert_eq!(check(&store, &WorkspaceId::Private, "ann", || panic!("private asks nothing")), Ok(()));
        assert_eq!(Members::read(&store).map(|m| m.at), Some(1_800_000_000));
    }
}
