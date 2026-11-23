//! Serving the hub: [`wsp_server::Server`] over the hub's store, with the hub
//! answering the sync verbs. Hello, requests and subscribe are wsp-server's
//! (its carrier and connection loop, not a copy of them); the hub hands it the
//! store whose event log announces `ref.moved`, what answers the sync verbs,
//! and the peer ranges the config names.
//!
//! Who a hello is, is [`crate::auth`]'s: a user's token names that user, and
//! the bootstrap token, `api.token` in the data directory, names the hub
//! itself only to make the first admin (wsp-161).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use wsp_api::{Outcome, Principal, SyncCall, TOKEN_FILE};
use wsp_server::{SyncVerbs, Server, SharedStore};

use crate::auth::Auth;
use crate::{Config, Hub, HubStore};

impl SyncVerbs for Hub {
    fn answer(&self, _by: &Principal, call: SyncCall) -> Outcome {
        Hub::answer(self, call)
    }
}

/// A hub that is listening.
pub struct Running {
    server: Server,
    hub: Arc<Hub>,
    token_path: PathBuf,
}

/// Opens the hub's data directory and starts listening as `cfg` says.
pub fn start(cfg: &Config) -> Result<Running, String> {
    let peers = cfg.ranges()?;
    let hub = Arc::new(Hub::open(&cfg.data_dir).map_err(|e| format!("opening {}: {e:?}", cfg.data_dir.display()))?);
    let store: SharedStore = Arc::new(HubStore::new(hub.clone()));
    let mut server_cfg = wsp_server::Config::new(&cfg.data_dir);
    server_cfg.addrs = cfg.bind.clone();
    server_cfg.port = cfg.port;
    server_cfg.instance = cfg.name.clone();
    server_cfg.sync = Some(hub.clone());
    server_cfg.gate = Some(Arc::new(Auth::new(hub.clone(), cfg.name.clone())));
    server_cfg.peers = Some(peers);
    let server = Server::start(store, server_cfg).map_err(|e| e.to_string())?;
    Ok(Running { server, hub, token_path: cfg.data_dir.join(TOKEN_FILE) })
}

impl Running {
    /// The addresses it listens on, loopback first.
    pub fn addrs(&self) -> &[SocketAddr] {
        self.server.addrs()
    }

    /// Where the bootstrap token is.
    pub fn token_path(&self) -> &std::path::Path {
        &self.token_path
    }

    pub fn hub(&self) -> &Arc<Hub> {
        &self.hub
    }

    /// Stops accepting and closes every client; the data stays for the next start.
    pub fn stop(self) {
        self.server.stop();
    }
}
