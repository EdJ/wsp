//! Where a new record lands (wsp-172): `wsp add`, `wsp worklist new` and
//! `wsp project add` file into the caller's workspace.
//!
//! Private unless `--workspace shared:<name>` or `$WSP_WORKSPACE` says
//! otherwise (the flag first). A shared workspace needs a login
//! (`~/.config/wsp/hubs.json` or `$WSP_HUB_TOKEN`); membership is read from the
//! cache's member list once wsp-465 lands, and until then a login is what is
//! asked. A child (sub-task, leg, piece) takes its parent's workspace, whatever
//! the flag says. Moving a record that exists is `wsp publish` (wsp-185), never
//! a flag on an edit: no edit verb reads `--workspace`.

use crate::model::WorkspaceId;

/// The environment variable that names the default workspace.
pub const ENV: &str = "WSP_WORKSPACE";

/// The workspace a new record is filed in. `parent` is the workspace of the
/// record's parent, when it has one: it decides, and the flag is not read.
pub fn of_new(flag: Option<&str>, parent: Option<&WorkspaceId>) -> Result<WorkspaceId, String> {
    if let Some(p) = parent {
        return Ok(p.clone());
    }
    let env = std::env::var(ENV).ok();
    resolve(flag, env.as_deref(), || wsp_client::Credential::for_hub().map(|_| ()).map_err(|e| e.to_string()))
}

/// [`of_new`] without a parent, with the flag, the environment's value and the
/// login check given, so a test sets no process-wide state.
pub fn resolve(flag: Option<&str>, env: Option<&str>, logged_in: impl FnOnce() -> Result<(), String>) -> Result<WorkspaceId, String> {
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
    Ok(ws)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out() -> Result<(), String> {
        Err("not logged in: WSP_HUB_TOKEN is unset and there is no hubs.json".into())
    }

    #[test]
    fn nothing_said_is_private_and_asks_no_login() {
        assert_eq!(resolve(None, None, out), Ok(WorkspaceId::Private));
        assert_eq!(resolve(None, Some("  "), out), Ok(WorkspaceId::Private));
        assert_eq!(resolve(Some("private"), None, out), Ok(WorkspaceId::Private));
    }

    #[test]
    fn shared_while_logged_out_is_refused_naming_login() {
        let why = resolve(Some("shared:core"), None, out).unwrap_err();
        assert!(why.contains("wsp login") && why.contains("shared:core"), "{why}");
        let why = resolve(None, Some("shared:core"), out).unwrap_err();
        assert!(why.contains("wsp login") && why.contains(ENV), "{why}");
    }

    #[test]
    fn shared_while_logged_in_is_shared_and_the_flag_beats_the_environment() {
        let ws = resolve(Some("shared:core"), Some("shared:other"), || Ok(())).unwrap();
        assert_eq!(ws.to_string(), "shared:core");
        let ws = resolve(None, Some("shared:other"), || Ok(())).unwrap();
        assert_eq!(ws.to_string(), "shared:other");
        assert_eq!(resolve(Some("private"), Some("shared:other"), out), Ok(WorkspaceId::Private));
    }

    #[test]
    fn a_word_that_is_no_workspace_is_refused() {
        assert!(resolve(Some("core"), None, || Ok(())).is_err());
        assert!(resolve(Some("true"), None, || Ok(())).is_err());
    }

    #[test]
    fn a_child_takes_its_parents_workspace_whatever_the_flag_says() {
        let shared: WorkspaceId = "shared:core".parse().unwrap();
        assert_eq!(of_new(Some("private"), Some(&shared)), Ok(shared.clone()));
        assert_eq!(of_new(Some("shared:core"), Some(&WorkspaceId::Private)), Ok(WorkspaceId::Private));
    }
}
