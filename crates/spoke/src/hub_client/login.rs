//! The login a spoke dials the hub with: [`Credential`], the user's token, and
//! the hub's URL and CA. A spoke's own daemon takes the instance's `api.token`;
//! a hub takes the user's token and never the instance's, so there is no
//! instance credential here at all.
//!
//! The user's token is `$WSP_HUB_TOKEN` (tests; `$WSP_TOKEN` too), else the
//! `token` of `~/.config/wsp/hubs.json`, a file only its owner may read (0600:
//! a wider one is refused, naming it). The hub's URL is `$WSP_HUB_URL`, else
//! the file's `url`. No host is a literal here: both come from the environment
//! or the file. The file is one login, `{"url", "token", "user", "at"}`, and a
//! `ca` (a PEM file) when the hub's certificate is a private CA's
//! (`$WSP_HUB_CA`, `--ca <file>`). `wsp login` writes it: `user` is the name
//! the hub gave the token, `at` when. The dial never reads `user`: the hello's
//! hint is `$WSP_HUB_USER`, else `$USER`, and every request is made as the
//! principal the `welcome` names. [`hub_login_in`] reads the rest of the file
//! without the token, for what prints a login.

use std::path::{Path, PathBuf};

use wsp_hub_protocol::Principal;

use super::Silent;

/// The environment variable that holds the user's hub token (tests).
pub const HUB_TOKEN_ENV: &str = "WSP_HUB_TOKEN";
/// The shorter name of the same token, `$WSP_TOKEN` (wsp-165), read when `$WSP_HUB_TOKEN` is unset.
pub const TOKEN_ENV: &str = "WSP_TOKEN";
/// The environment variable that holds the hub's URL.
pub const HUB_URL_ENV: &str = "WSP_HUB_URL";
/// The environment variable that holds the path of the CA file a private hub's
/// certificate chains to (`ca` in `hubs.json`, `--ca <file>`).
pub const HUB_CA_ENV: &str = "WSP_HUB_CA";
/// The environment variable that holds the user's name, the hello's hint.
pub const HUB_USER_ENV: &str = "WSP_HUB_USER";
/// The environment variable that names the directory the login file is under,
/// in place of `$HOME`. `wsp sandbox` sets it, one directory per sandbox.
pub const HUB_HOME_ENV: &str = "WSP_HUB_HOME";
/// The file of the login, under the home directory.
pub const HUBS_FILE: &str = ".config/wsp/hubs.json";

/// The token a client says `hello` to a hub with: a user's.
#[derive(Clone, PartialEq, Eq)]
pub enum Credential {
    /// A user's token, for a hub.
    User(String),
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Credential::User(_) => f.write_str("Credential::User(..)"),
        }
    }
}

impl Credential {
    /// The credential for a hub: `$WSP_HUB_TOKEN`, else `~/.config/wsp/hubs.json`.
    /// [`Silent::NotLoggedIn`] when neither names one; [`Silent::BadHubsFile`]
    /// for a file wider than 0600 (named), unreadable or malformed.
    pub fn for_hub() -> Result<Credential, Silent> {
        Credential::for_hub_in(std::env::var(HUB_TOKEN_ENV).or_else(|_| std::env::var(TOKEN_ENV)).ok(), &home())
    }

    /// [`Credential::for_hub`] with the environment's token and the home
    /// directory given, so a test need not set process-wide variables.
    pub fn for_hub_in(env_token: Option<String>, home: &Path) -> Result<Credential, Silent> {
        if let Some(t) = env_token.map(|t| t.trim().to_string()).filter(|t| !t.is_empty()) {
            return Ok(Credential::User(t));
        }
        match read_login(home)? {
            Some(Login { token: Some(t), .. }) if !t.is_empty() => Ok(Credential::User(t)),
            Some(_) => Err(Silent::NotLoggedIn(format!("{} holds no token", home.join(HUBS_FILE).display()))),
            None => Err(Silent::NotLoggedIn(format!("{HUB_TOKEN_ENV} is unset and there is no {}", home.join(HUBS_FILE).display()))),
        }
    }
}

/// The hub's URL: `$WSP_HUB_URL`, else `hubs.json`'s `url`.
pub fn hub_url() -> Result<String, Silent> {
    hub_url_in(std::env::var(HUB_URL_ENV).ok(), &home())
}

/// [`hub_url`] with the environment's URL and the home directory given.
pub fn hub_url_in(env_url: Option<String>, home: &Path) -> Result<String, Silent> {
    if let Some(u) = env_url.map(|u| u.trim().to_string()).filter(|u| !u.is_empty()) {
        return Ok(u);
    }
    match read_login(home)? {
        Some(Login { url: Some(u), .. }) if !u.is_empty() => Ok(u),
        _ => Err(Silent::NotLoggedIn(format!("{HUB_URL_ENV} is unset and {} names no url", home.join(HUBS_FILE).display()))),
    }
}

/// The CA file a private hub's certificate chains to: `$WSP_HUB_CA`, else
/// `hubs.json`'s `ca`; none when neither names one (the platform's roots then).
pub fn hub_ca() -> Result<Option<PathBuf>, Silent> {
    hub_ca_in(std::env::var(HUB_CA_ENV).ok(), &home())
}

/// [`hub_ca`] with the environment's value and the home directory given.
pub fn hub_ca_in(env_ca: Option<String>, home: &Path) -> Result<Option<PathBuf>, Silent> {
    if let Some(c) = env_ca.map(|c| c.trim().to_string()).filter(|c| !c.is_empty()) {
        return Ok(Some(PathBuf::from(c)));
    }
    Ok(read_login(home)?.and_then(|l| l.ca).filter(|c| !c.is_empty()).map(PathBuf::from))
}

struct Login {
    url: Option<String>,
    token: Option<String>,
    ca: Option<String>,
    user: Option<String>,
    at: Option<String>,
    method: Option<String>,
}

/// What `hubs.json` says of a login, the token left out: for `wsp hub status`
/// and for `wsp login` saying what it replaces. There is nowhere in it a token
/// could be printed from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubLogin {
    pub url: String,
    /// The name the hub gave the token at login; empty in a file written by hand.
    pub user: String,
    /// When the login was made, as the file says.
    pub at: String,
    /// The pinned CA file, if the login names one.
    pub ca: Option<String>,
    /// How the token was had, as `wsp login` wrote it: `token` or `github`.
    /// None in a file written before it was kept, or by hand.
    pub method: Option<String>,
}

/// The login under `home`, without its token; None when there is no file.
/// [`Silent::BadHubsFile`] for a file wider than 0600, unreadable or not a
/// login, and for one that names no url.
pub fn hub_login_in(home: &Path) -> Result<Option<HubLogin>, Silent> {
    let Some(l) = read_login(home)? else { return Ok(None) };
    let Some(url) = l.url.filter(|u| !u.is_empty()) else {
        return Err(Silent::BadHubsFile(format!("{} names no url", home.join(HUBS_FILE).display())));
    };
    Ok(Some(HubLogin { url, user: l.user.unwrap_or_default(), at: l.at.unwrap_or_default(), ca: l.ca.filter(|c| !c.is_empty()), method: l.method.filter(|m| !m.is_empty()) }))
}

fn home() -> PathBuf {
    hub_home().unwrap_or_default()
}

/// The directory `hubs.json` is under (`HUBS_FILE` is relative to it):
/// `$WSP_HUB_HOME` when it is set and not empty, which is how a sandbox keeps
/// its login apart from the developer's and from every other sandbox's,
/// else `$HOME`. None when neither is set.
pub fn hub_home() -> Option<PathBuf> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    env(HUB_HOME_ENV).or_else(|| std::env::var_os("HOME").map(PathBuf::from))
}

/// The login `hubs.json` holds; None when there is no such file.
fn read_login(home: &Path) -> Result<Option<Login>, Silent> {
    let path = home.join(HUBS_FILE);
    let meta = match std::fs::metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Silent::BadHubsFile(format!("{}: {e}", path.display()))),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(Silent::BadHubsFile(format!(
                "{} is mode {mode:03o}, wider than 0600: chmod 600 it",
                path.display()
            )));
        }
    }
    let _ = meta;
    let text = std::fs::read_to_string(&path).map_err(|e| Silent::BadHubsFile(format!("{}: {e}", path.display())))?;
    let v: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| Silent::BadHubsFile(format!("{} is not a login: {e}", path.display())))?;
    let field = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
    if !v.is_object() {
        return Err(Silent::BadHubsFile(format!("{} is not a login: not an object", path.display())));
    }
    Ok(Some(Login { url: field("url"), token: field("token"), ca: field("ca"), user: field("user"), at: field("at"), method: field("method") }))
}

/// The `host:port` a hub's URL names, and whether it is dialled over TLS:
/// `tls://host:port` is; `tcp://host:port` and a bare `host:port` are
/// plain TCP. Any other scheme is refused rather than dialled in the clear.
pub(crate) fn hub_addr(url: &str) -> Result<(String, bool), Silent> {
    let url = url.trim();
    let (addr, secure) = match url.split_once("://") {
        Some(("tls", rest)) => (rest, true),
        Some(("tcp", rest)) => (rest, false),
        Some((scheme, _)) => {
            return Err(Silent::BadUrl(format!("hub url {url}: scheme {scheme} is not supported (tls:// or tcp://)")))
        }
        None => (url, false),
    };
    let addr = addr.trim_end_matches('/');
    if addr.is_empty() || !addr.contains(':') {
        return Err(Silent::BadUrl(format!("hub url {url} is not host:port")));
    }
    Ok((addr.to_string(), secure))
}

/// The file of what the hub's last welcome said of the login's user, under the
/// home directory, beside `hubs.json` and not in it (no token is rewritten to
/// keep it): `{"url", "user", "admin", "at"}`, `admin` `null` where the hub
/// did not say (one older than the welcome's `admin`). It is the login's:
/// `wsp login` drops it and keeps its own welcome's, the worker's dial keeps
/// each welcome's, `wsp logout` removes it, and it is read only for a login
/// of the same url and user ([`kept_admin`]).
pub const WELCOME_FILE: &str = ".config/wsp/welcome.json";

/// Keep what `client`'s welcome said under `home`, for the login to `url`.
/// Best effort: a home that cannot be written keeps nothing, and status then
/// says the hub did not say.
pub fn keep_welcome(home: &Path, url: &str, client: &super::Client) {
    let Principal::Person(user) = client.by() else { return };
    let path = home.join(WELCOME_FILE);
    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let body = serde_json::json!({
        "url": url.trim(),
        "user": user,
        "admin": client.admin(),
        "at": wsp_base::util::iso_at(wsp_base::util::epoch_secs()),
    });
    let tmp = dir.join(format!(".welcome.json.{}.tmp", std::process::id()));
    if std::fs::write(&tmp, body.to_string()).and_then(|_| std::fs::rename(&tmp, &path)).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Whether the hub's last welcome to `login` said its user is a hub admin;
/// None where nothing is kept for this login (its url and user) or the hub
/// did not say.
pub fn kept_admin(home: &Path, login: &HubLogin) -> Option<bool> {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(home.join(WELCOME_FILE)).ok()?).ok()?;
    let same = v["url"].as_str() == Some(login.url.trim()) && v["user"].as_str() == Some(login.user.as_str());
    same.then(|| v["admin"].as_bool()).flatten()
}

/// Drop what a welcome said (a new login, a logout).
pub fn forget_welcome(home: &Path) {
    let _ = std::fs::remove_file(home.join(WELCOME_FILE));
}

/// Who the hello says it is for, as a hint the hub ignores.
pub(crate) fn hint() -> Principal {
    let name = std::env::var(HUB_USER_ENV).or_else(|_| std::env::var("USER")).unwrap_or_default();
    Principal::Person(if name.is_empty() { "unknown".into() } else { name })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(name: &str, mode: u32, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let h = std::env::temp_dir().join(format!("wsp-spoke-cred-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&h);
        std::fs::create_dir_all(h.join(".config/wsp")).unwrap();
        let f = h.join(HUBS_FILE);
        std::fs::write(&f, body).unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(mode)).unwrap();
        h
    }

    const LOGIN: &str = r#"{"url":"tcp://hub.invalid:7000","user":"ed","token":"from-file"}"#;

    #[test]
    fn the_environment_comes_before_the_file() {
        let h = home("order", 0o600, LOGIN);
        assert_eq!(Credential::for_hub_in(Some("from-env".into()), &h), Ok(Credential::User("from-env".into())));
        assert_eq!(Credential::for_hub_in(None, &h), Ok(Credential::User("from-file".into())));
        assert_eq!(Credential::for_hub_in(Some("  ".into()), &h), Ok(Credential::User("from-file".into())));
        // An environment token needs no file, and is not checked against one.
        let wide = home("order-wide", 0o644, LOGIN);
        assert_eq!(Credential::for_hub_in(Some("from-env".into()), &wide), Ok(Credential::User("from-env".into())));
    }

    #[test]
    fn a_hubs_file_wider_than_0600_is_refused_naming_it() {
        for mode in [0o644, 0o640, 0o604] {
            let h = home("wide", mode, LOGIN);
            match Credential::for_hub_in(None, &h) {
                Err(Silent::BadHubsFile(why)) => {
                    assert!(why.contains(&h.join(HUBS_FILE).display().to_string()), "does not name the file: {why}");
                    assert!(why.contains("0600"), "{why}");
                    assert!(!why.contains("from-file"), "{why}");
                }
                other => panic!("mode {mode:o}: {other:?}"),
            }
            assert!(matches!(hub_url_in(None, &h), Err(Silent::BadHubsFile(_))));
        }
        let h = home("narrow", 0o400, LOGIN);
        assert_eq!(Credential::for_hub_in(None, &h), Ok(Credential::User("from-file".into())));
    }

    #[test]
    fn no_token_anywhere_is_not_logged_in() {
        let none = std::env::temp_dir().join(format!("wsp-spoke-cred-none-{}", std::process::id()));
        let e = Credential::for_hub_in(None, &none).unwrap_err();
        assert!(matches!(e, Silent::NotLoggedIn(_)));
        assert!(e.to_string().starts_with("not logged in"), "{e}");
        let h = home("no-token", 0o600, r#"{"url":"tcp://hub.invalid:7000"}"#);
        assert!(matches!(Credential::for_hub_in(None, &h), Err(Silent::NotLoggedIn(_))));
        let h = home("junk", 0o600, "not json");
        assert!(matches!(Credential::for_hub_in(None, &h), Err(Silent::BadHubsFile(_))));
    }

    #[test]
    fn the_url_is_the_environments_then_the_files() {
        let h = home("url", 0o600, LOGIN);
        assert_eq!(hub_url_in(Some("tcp://other.invalid:1".into()), &h).unwrap(), "tcp://other.invalid:1");
        assert_eq!(hub_url_in(None, &h).unwrap(), "tcp://hub.invalid:7000");
    }

    #[test]
    fn a_url_is_host_port_over_tls_or_tcp_and_nothing_else() {
        assert_eq!(hub_addr("tls://hub.invalid:7000/").unwrap(), ("hub.invalid:7000".to_string(), true));
        assert_eq!(hub_addr("tcp://hub.invalid:7000/").unwrap(), ("hub.invalid:7000".to_string(), false));
        assert_eq!(hub_addr("hub.invalid:7000").unwrap(), ("hub.invalid:7000".to_string(), false));
        assert!(hub_addr("https://hub.invalid:7000").is_err());
        assert!(hub_addr("tls://hub.invalid").is_err());
        assert!(hub_addr("hub.invalid").is_err());
    }

    #[test]
    fn a_login_is_read_without_its_token() {
        let h = home("login", 0o600, r#"{"url":"tls://hub.invalid:7000","token":"sekrit-token","user":"ed","at":"2026-11-23T10:00:00Z"}"#);
        let l = hub_login_in(&h).unwrap().unwrap();
        assert_eq!((l.url.as_str(), l.user.as_str(), l.at.as_str(), l.ca.as_deref()), ("tls://hub.invalid:7000", "ed", "2026-11-23T10:00:00Z", None));
        assert!(!format!("{l:?}").contains("sekrit-token"));
        assert_eq!(hub_login_in(&std::env::temp_dir().join("wsp-no-such-home")).unwrap(), None);
        let wide = home("login-wide", 0o644, LOGIN);
        assert!(matches!(hub_login_in(&wide), Err(Silent::BadHubsFile(_))));
        let no_url = home("login-nourl", 0o600, r#"{"token":"t"}"#);
        assert!(matches!(hub_login_in(&no_url), Err(Silent::BadHubsFile(_))));
    }

    #[test]
    fn the_ca_is_the_environments_then_the_files_and_none_without_either() {
        let h = home("ca", 0o600, r#"{"url":"tls://hub.invalid:7000","token":"t","ca":"/etc/hub-ca.pem"}"#);
        assert_eq!(hub_ca_in(Some("/env/ca.pem".into()), &h).unwrap(), Some(PathBuf::from("/env/ca.pem")));
        assert_eq!(hub_ca_in(None, &h).unwrap(), Some(PathBuf::from("/etc/hub-ca.pem")));
        let none = home("noca", 0o600, LOGIN);
        assert_eq!(hub_ca_in(None, &none).unwrap(), None);
        assert_eq!(hub_ca_in(None, &std::env::temp_dir().join("wsp-no-such-home")).unwrap(), None);
    }
}
