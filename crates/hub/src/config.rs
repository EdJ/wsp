//! The config file `wsp-hub serve --config <file>` reads: one JSON object.
//!
//! ```json
//! {
//!   "data_dir": "/var/lib/wsp-hub",
//!   "bind": ["100.64.0.7"],
//!   "port": 7489,
//!   "peers": ["127.0.0.0/8", "::1/128", "100.64.0.0/10"],
//!   "name": "wsp-hub"
//! }
//! ```
//!
//! - `data_dir` (required): where the objects, `hub.db`, the bootstrap token
//!   (`api.token`, made on first start at mode 0600), `api.json` and the log
//!   (`api.log`) live. A relative path is read from the config file's own
//!   directory.
//! - `bind`: addresses to listen on beside loopback, which is always bound.
//!   Each must be in `peers`. None by default: loopback alone, until
//!   wsp-461's TLS makes anything wider safe.
//! - `port`: one port for every address; `0` takes an ephemeral one, which
//!   `api.json` and the start line then name. Default [`DEFAULT_PORT`].
//! - `peers`: the CIDR blocks a peer's address must be in. Default loopback
//!   (`127.0.0.0/8`, `::1/128`).
//! - `name`: the instance `welcome` names. Default `wsp-hub`.
//!
//! A key this file does not know is an error, so a misspelt `peers` does not
//! quietly leave the default.

use std::net::IpAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use wsp_server::Ranges;

/// The port the hub listens on unless told otherwise: the one after the state
/// core server's (7488).
pub const DEFAULT_PORT: u16 = 7489;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub data_dir: PathBuf,
    #[serde(default)]
    pub bind: Vec<IpAddr>,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "loopback")]
    pub peers: Vec<String>,
    #[serde(default = "default_name")]
    pub name: String,
}

fn default_port() -> u16 {
    DEFAULT_PORT
}

fn loopback() -> Vec<String> {
    vec!["127.0.0.0/8".into(), "::1/128".into()]
}

fn default_name() -> String {
    "wsp-hub".into()
}

impl Config {
    /// The config at `path`.
    pub fn load(path: &Path) -> Result<Config, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut cfg: Config = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        if cfg.data_dir.is_relative() {
            let base = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
            cfg.data_dir = base.join(&cfg.data_dir);
        }
        cfg.ranges()?;
        Ok(cfg)
    }

    /// The `peers` as ranges, or the first that does not read.
    pub fn ranges(&self) -> Result<Ranges, String> {
        let ranges = Ranges::parse(&self.peers).map_err(|e| format!("peers: {e}"))?;
        if ranges.is_empty() {
            return Err("peers: no range, so no peer could connect".into());
        }
        Ok(ranges)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(body: &str) -> Result<Config, String> {
        let dir = std::env::temp_dir().join(format!("wsp-hub-config-{}-{}", std::process::id(), body.len()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hub.json");
        std::fs::write(&path, body).unwrap();
        let cfg = Config::load(&path);
        let _ = std::fs::remove_dir_all(&dir);
        cfg
    }

    #[test]
    fn a_config_of_a_data_dir_alone_is_loopback_on_the_default_port() {
        let cfg = load(r#"{"data_dir": "d"}"#).unwrap();
        assert_eq!((cfg.port, cfg.name.as_str()), (DEFAULT_PORT, "wsp-hub"));
        assert!(cfg.bind.is_empty());
        assert!(cfg.ranges().unwrap().contains(&"127.0.0.1".parse().unwrap()));
        assert!(!cfg.ranges().unwrap().contains(&"100.64.0.1".parse().unwrap()));
        assert!(cfg.data_dir.ends_with("d") && cfg.data_dir.is_absolute(), "from the config's directory: {:?}", cfg.data_dir);
    }

    #[test]
    fn a_config_that_does_not_read_is_refused_and_says_why() {
        for bad in [
            r#"{}"#,
            r#"{"data_dir": "d", "peer": []}"#,
            r#"{"data_dir": "d", "peers": ["10.0.0.0/40"]}"#,
            r#"{"data_dir": "d", "peers": []}"#,
            r#"{"data_dir": "d", "bind": ["not an ip"]}"#,
            r#"not json"#,
        ] {
            assert!(load(bad).is_err(), "{bad}");
        }
    }
}
