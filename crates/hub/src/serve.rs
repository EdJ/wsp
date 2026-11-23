//! Serving the hub: [`wsp_server::Server`] over the hub's store, with the hub
//! answering the sync verbs. Hello, requests and subscribe are wsp-server's
//! (its carrier and connection loop, not a copy of them); the hub hands it the
//! store whose event log announces `ref.moved`, what answers the sync verbs,
//! and the peer ranges the config names.
//!
//! Who a hello is, is [`crate::auth`]'s: a user's token names that user, and
//! the bootstrap token, `api.token` in the data directory, names the hub
//! itself only to make the first admin (wsp-161).
//!
//! **TLS** (wsp-461). With `tls` in the config every connection is inside a TLS
//! session, and an address that is not loopback is refused without it.
//!
//! **Leases** (wsp-466). The hub's leases run the config's `lease_days`, and a
//! thread sweeps for expired ones every [`crate::leases::SWEEP`], releasing
//! each and announcing `lease.expired`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;

use wsp_api::{Outcome, Principal, SyncCall, TOKEN_FILE};
use wsp_server::{SyncVerbs, Server, SharedStore};

use crate::auth::Auth;
use crate::{Config, Hub, HubStore};

impl SyncVerbs for Hub {
    fn answer(&self, by: &Principal, call: SyncCall) -> Outcome {
        Hub::answer(self, by, call)
    }
}

/// A hub that is listening.
pub struct Running {
    server: Server,
    hub: Arc<Hub>,
    token_path: PathBuf,
    tls: bool,
    /// Dropped to stop the sweeper, which is then joined.
    sweeper: Option<(Sender<()>, JoinHandle<()>)>,
}

/// Sweeps `hub` for expired leases every `every` until its sender is dropped.
fn sweeper(hub: Arc<Hub>, every: std::time::Duration) -> (Sender<()>, JoinHandle<()>) {
    let (stop, stopped) = mpsc::channel::<()>();
    let thread = std::thread::spawn(move || loop {
        match stopped.recv_timeout(every) {
            Err(RecvTimeoutError::Timeout) => {
                // A failed sweep is tried again at the next; a take of an expired lease expires it anyway.
                let _ = hub.expire_leases();
            }
            _ => return,
        }
    });
    (stop, thread)
}

/// Opens the hub's data directory and starts listening as `cfg` says.
pub fn start(cfg: &Config) -> Result<Running, String> {
    let peers = cfg.ranges()?;
    cfg.tls_required()?;
    let tls = match &cfg.tls {
        Some(files) => Some(wsp_server::tls::server_config(&files.cert, &files.key)?),
        None => None,
    };
    let hub = Hub::open(&cfg.data_dir).map_err(|e| format!("opening {}: {e:?}", cfg.data_dir.display()))?;
    let hub = Arc::new(hub.with_lease_ttl(cfg.lease_ttl()));
    let store: SharedStore = Arc::new(HubStore::new(hub.clone()));
    let mut server_cfg = wsp_server::Config::new(&cfg.data_dir);
    server_cfg.addrs = cfg.bind.clone();
    server_cfg.port = cfg.port;
    server_cfg.instance = cfg.name.clone();
    server_cfg.sync = Some(hub.clone());
    server_cfg.gate = Some(Arc::new(Auth::new(hub.clone(), cfg.name.clone())));
    server_cfg.peers = Some(peers);
    let secure = tls.is_some();
    server_cfg.tls = tls;
    let server = Server::start(store, server_cfg).map_err(|e| e.to_string())?;
    let sweeper = Some(sweeper(hub.clone(), crate::leases::SWEEP));
    Ok(Running { server, hub, token_path: cfg.data_dir.join(TOKEN_FILE), tls: secure, sweeper })
}

impl Running {
    /// The addresses it listens on, loopback first.
    pub fn addrs(&self) -> &[SocketAddr] {
        self.server.addrs()
    }

    /// Whether every connection is inside TLS.
    pub fn tls(&self) -> bool {
        self.tls
    }

    /// Where the bootstrap token is.
    pub fn token_path(&self) -> &std::path::Path {
        &self.token_path
    }

    pub fn hub(&self) -> &Arc<Hub> {
        &self.hub
    }

    /// Stops accepting and closes every client; the data stays for the next start.
    pub fn stop(mut self) {
        if let Some((stop, thread)) = self.sweeper.take() {
            drop(stop);
            let _ = thread.join();
        }
        self.server.stop();
    }
}
