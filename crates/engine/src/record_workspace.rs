//! Where a new record lands (wsp-172): `wsp add`, `wsp worklist new` and
//! `wsp project add` file into the caller's workspace.
//!
//! Private unless `--workspace shared:<name>` or `$WSP_WORKSPACE` says
//! otherwise (the flag first). A shared workspace needs the shared-work add-on
//! to say yes ([`crate::addon::SharedWork::may_file_shared`]: the spoke asks
//! for a login, `~/.config/wsp/hubs.json` or `$WSP_HUB_TOKEN`); with none, a
//! shared workspace is refused and everything is private. A child (sub-task,
//! leg, piece) takes its parent's workspace, whatever the flag says. Moving a
//! record that exists is `wsp publish` (wsp-185), never a flag on an edit: no
//! edit verb reads `--workspace`.
//!
//! This is core, not the add-on's: the workspace is a field of every record,
//! and it is core that stamps it on a new one and keeps a child where its
//! parent is, so a private record stays private whether or not an add-on is
//! there. The add-on is asked two things: whether shared may be filed at all
//! (a login), then whether into this workspace (a member of it, as far as the
//! add-on knows: [`crate::addon::SharedWork::may_file_into`]).

use crate::model::WorkspaceId;
use crate::store::Store;

/// The environment variable that names the default workspace.
pub const ENV: &str = "WSP_WORKSPACE";

/// The workspace a new record is filed in. `parent` is the workspace of the
/// record's parent, when it has one: it decides, and the flag is not read.
pub fn of_new(store: &Store, flag: Option<&str>, parent: Option<&WorkspaceId>) -> Result<WorkspaceId, String> {
    if let Some(p) = parent {
        return Ok(p.clone());
    }
    let env = std::env::var(ENV).ok();
    let addon = crate::addon::get();
    resolve(flag, env.as_deref(), || addon.may_file_shared(store), |ws| addon.may_file_into(store, ws))
}

/// [`of_new`] without a parent, with the flag, the environment's value, the
/// login check and the membership check given, so a test sets no
/// process-wide state.
pub fn resolve(
    flag: Option<&str>,
    env: Option<&str>,
    logged_in: impl FnOnce() -> Result<(), String>,
    member: impl FnOnce(&WorkspaceId) -> Result<(), String>,
) -> Result<WorkspaceId, String> {
    let (said, from) = match flag {
        Some(f) => (f.trim(), "--workspace"),
        None => match env.map(str::trim).filter(|e| !e.is_empty()) {
            Some(e) => (e, ENV),
            None => return Ok(WorkspaceId::Private),
        },
    };
    let ws: WorkspaceId = said
        .parse()
        .map_err(|_| format!("{from} `{said}` is not a workspace: it is `private` or `shared:<name>`"))?;
    if ws.is_private() {
        return Ok(ws);
    }
    if let Err(why) = logged_in() {
        return Err(format!(
            "cannot file into {ws}: {why}\n     `wsp login` first, or leave {from} off to file it private"
        ));
    }
    if let Err(why) = member(&ws) {
        return Err(format!("cannot file into {ws}: {why}\n     or leave {from} off to file it private"));
    }
    Ok(ws)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out() -> Result<(), String> {
        Err("not logged in".into())
    }

    fn member(_: &WorkspaceId) -> Result<(), String> {
        Ok(())
    }

    #[test]
    fn a_workspace_the_user_is_not_a_member_of_is_refused_after_the_login() {
        let not = |ws: &WorkspaceId| Err(format!("ann is not a member of {ws}"));
        let why = resolve(Some("shared:core"), None, || Ok(()), not).unwrap_err();
        assert!(why.starts_with("cannot file into shared:core: ann is not a member of shared:core"), "{why}");
        assert!(why.contains("leave --workspace off") && !why.contains("wsp login"), "{why}");
        // The login is asked first, and private asks neither.
        let why = resolve(Some("shared:core"), None, out, not).unwrap_err();
        assert!(why.contains("wsp login"), "{why}");
        assert_eq!(resolve(Some("private"), None, out, not), Ok(WorkspaceId::Private));
    }

    #[test]
    fn nothing_said_is_private_and_asks_no_login() {
        assert_eq!(resolve(None, None, out, member), Ok(WorkspaceId::Private));
        assert_eq!(resolve(None, Some("  "), out, member), Ok(WorkspaceId::Private));
        assert_eq!(resolve(Some("private"), None, out, member), Ok(WorkspaceId::Private));
    }

    #[test]
    fn shared_while_logged_out_is_refused_naming_login() {
        let why = resolve(Some("shared:core"), None, out, member).unwrap_err();
        assert!(why.contains("wsp login") && why.contains("shared:core"), "{why}");
        let why = resolve(None, Some("shared:core"), out, member).unwrap_err();
        assert!(why.contains("wsp login") && why.contains(ENV), "{why}");
    }

    #[test]
    fn shared_while_logged_in_is_shared_and_the_flag_beats_the_environment() {
        let ws = resolve(Some("shared:core"), Some("shared:other"), || Ok(()), member).unwrap();
        assert_eq!(ws.to_string(), "shared:core");
        let ws = resolve(None, Some("shared:other"), || Ok(()), member).unwrap();
        assert_eq!(ws.to_string(), "shared:other");
        assert_eq!(resolve(Some("private"), Some("shared:other"), out, member), Ok(WorkspaceId::Private));
    }

    #[test]
    fn a_word_that_is_no_workspace_is_refused() {
        assert!(resolve(Some("core"), None, || Ok(()), member).is_err());
        assert!(resolve(Some("true"), None, || Ok(()), member).is_err());
    }

    #[test]
    fn a_child_takes_its_parents_workspace_whatever_the_flag_says() {
        let (_env, store) = crate::testkit::isolated_store("record-workspace-child");
        let shared: WorkspaceId = "shared:core".parse().unwrap();
        assert_eq!(of_new(&store, Some("private"), Some(&shared)), Ok(shared.clone()));
        assert_eq!(of_new(&store, Some("shared:core"), Some(&WorkspaceId::Private)), Ok(WorkspaceId::Private));
    }

    /// With no add-on on the thread a shared workspace is refused, naming the
    /// login, and nothing said is private still.
    #[test]
    fn with_no_addon_shared_is_refused_and_private_is_private() {
        let (_env, store) = crate::testkit::isolated_store("record-workspace-standalone");
        let _standalone = crate::addon::scoped(std::sync::Arc::new(crate::addon::Standalone));
        let why = of_new(&store, Some("shared:core"), None).unwrap_err();
        assert!(why.contains("wsp login") && why.contains("shared:core"), "{why}");
        assert_eq!(of_new(&store, Some("private"), None), Ok(WorkspaceId::Private));
    }
}
