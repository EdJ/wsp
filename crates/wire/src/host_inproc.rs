//! `WSP_HOST=inproc` after M1 (wsp-019's decision, wsp-030): the manager's own
//! code, run inside this process. M0's inference path is gone, not kept as a
//! rollback; `inproc` now means only *where* the manager runs.
//!
//! [`reach`] starts a [`wsp_manager::Manager`] on loopback, on a port of its own,
//! with its `<exec_dir>` at `<state>/inproc/manager` (its token, journal, lock
//! and seat run directory), and hands back what to dial: the same [`Tcp`] and
//! [`Subscription`] the installed manager is reached by, so a seat's `seat.*`,
//! `turn` and `order.*` events are journalled, heard and folded the same way
//! under both choices. One code path, two places it runs.
//!
//! **One per state dir, across processes.** The manager locks its
//! `<exec_dir>`. The first wsp process to take the lock serves, and writes
//! where it listens to [`PUBLISHED`] in that directory, with its pid. Another
//! process (the daemon beside a CLI call, or two CLI calls) finds the lock
//! held and dials the published address instead. When the serving process
//! ends, its lock goes with it; the next [`reach`] starts one again, and the
//! journal, kept in the directory, goes on from its head.
//!
//! It adopts no seat from compound's own run directory: a seat it did not
//! start is one the [`Tcp`]'s in-process half answers for, as before.
//!
//! [`Tcp`]: crate::host_tcp::Tcp
//! [`Subscription`]: crate::host_tcp::Subscription

use std::collections::BTreeMap;
use std::fs;
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::json;
use wsp_host_api::Token;
use wsp_manager::{Config, Manager, StartError};

use crate::host_tcp::{HostError, SILENT};
use crate::manager::{self, Local};
use crate::util;

/// Under the manager's `<exec_dir>`: where the serving process says it listens.
pub const PUBLISHED: &str = "listening.json";

/// The in-process manager's `<exec_dir>` for `state`.
pub fn exec_dir(state: &Path) -> PathBuf {
    state.join("inproc").join("manager")
}

/// The name the in-process manager gives itself: this machine's, marked, so
/// its journal's place (`<state>/manager/<host>.last`) is never the
/// installed manager's.
pub fn host_id() -> String {
    format!("{}.inproc", util::hostname())
}

/// What to dial for `state`'s in-process manager: the one this process runs
/// (started here on the first call), or the one another wsp process runs and
/// published. [`HostError::Inproc`] when neither can be had.
pub fn reach(state: &Path) -> Result<(Local, Token), HostError> {
    static RUNNING: OnceLock<Mutex<BTreeMap<PathBuf, (Local, Token)>>> = OnceLock::new();
    let running = RUNNING.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut map = running.lock().unwrap_or_else(|e| e.into_inner());
    prune_gone(&mut map);
    if let Some(had) = map.get(state) {
        return Ok(had.clone());
    }
    let exec = exec_dir(state);
    let mut cfg = Config::new(&exec, &host_id());
    cfg.addrs = vec![IpAddr::V4(Ipv4Addr::LOCALHOST)];
    cfg.port = 0;
    match Manager::start(cfg) {
        Ok(m) => {
            let port = m.addrs().first().map(|a| a.port()).ok_or_else(|| HostError::Inproc("it bound no address".into()))?;
            let local = Local { host: host_id(), addr: IpAddr::V4(Ipv4Addr::LOCALHOST), port, exec_dir: exec.clone() };
            let token = token_of(&exec)?;
            publish(&exec, &local);
            m.note("started in-process (WSP_HOST=inproc)");
            // Served until this process ends, or until `state` is removed from
            // under it ([`prune_gone`]); its lock ends with either.
            held().lock().unwrap_or_else(|e| e.into_inner()).insert(state.to_path_buf(), m);
            map.insert(state.to_path_buf(), (local.clone(), token.clone()));
            Ok((local, token))
        }
        Err(StartError::Locked { holder, .. }) => {
            drop(map);
            published(&exec, holder.trim())
        }
        Err(e) => Err(HostError::Inproc(e.to_string())),
    }
}

/// The managers this process runs, by the state dir they serve.
fn held() -> &'static Mutex<BTreeMap<PathBuf, Manager>> {
    static HELD: OnceLock<Mutex<BTreeMap<PathBuf, Manager>>> = OnceLock::new();
    HELD.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Stop the managers of states that no longer exist. A test's state is removed
/// when the test ends, and its manager held a lock, a log, a journal and a
/// listener per address open for the rest of the process: ~10 fds a test,
/// which ran a 256-fd seat out of them (wsp-770).
fn prune_gone(map: &mut BTreeMap<PathBuf, (Local, Token)>) {
    let gone: Vec<PathBuf> = map.keys().filter(|s| !s.exists()).cloned().collect();
    for state in gone {
        map.remove(&state);
        let m = held().lock().unwrap_or_else(|e| e.into_inner()).remove(&state);
        if let Some(m) = m {
            m.stop();
        }
    }
}

/// How many in-process managers this process still runs.
pub fn running_count() -> usize {
    held().lock().unwrap_or_else(|e| e.into_inner()).len()
}

fn token_of(exec: &Path) -> Result<Token, HostError> {
    manager::manager_token(exec, Duration::ZERO)
        .map(Token)
        .ok_or_else(|| HostError::Inproc(format!("no token in {}", util::contract(exec))))
}

fn publish(exec: &Path, local: &Local) {
    let body = json!({ "pid": std::process::id(), "host": local.host, "port": local.port });
    let path = exec.join(PUBLISHED);
    let tmp = path.with_extension("tmp");
    if fs::write(&tmp, body.to_string()).is_ok() {
        let _ = fs::rename(&tmp, &path);
    }
}

/// The address the lock's holder published, waited for up to [`SILENT`]: a
/// holder that has taken the lock and not yet bound has not written it.
fn published(exec: &Path, holder: &str) -> Result<(Local, Token), HostError> {
    let deadline = Instant::now() + SILENT;
    loop {
        let read = fs::read_to_string(exec.join(PUBLISHED)).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok());
        if let Some(v) = read {
            let pid = v["pid"].as_u64().map(|p| p.to_string());
            if let (Some(port), Some(host)) = (v["port"].as_u64(), v["host"].as_str()) {
                if holder.is_empty() || pid.as_deref() == Some(holder) {
                    let local = Local { host: host.to_string(), addr: IpAddr::V4(Ipv4Addr::LOCALHOST), port: port as u16, exec_dir: exec.to_path_buf() };
                    return Ok((local, token_of(exec)?));
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(HostError::Inproc(format!(
                "pid {holder} holds {} and has not said where it listens",
                util::contract(exec)
            )));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}
