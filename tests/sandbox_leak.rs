//! A `wsp sandbox --fake` server dies with what it belongs to (wsp-778).
//!
//! On 2026-11-25 the machine sat at load ~34 under 167 `wsp sandbox serve`
//! processes at ppid 1, left by test runs hours old. A fake is spawned to
//! outlive `wsp sandbox --fake`, so nothing but a teardown ever ended it — and a
//! teardown that raced the removal of its directory, a test killed by a
//! timeout, or a panic before the guard existed, left it serving a socket
//! nobody could reach. Each test here is one of those ways, and asserts that no
//! `sandbox serve` of its own is left.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::Value;

const WSP: &str = env!("CARGO_BIN_EXE_wsp");

fn scratch(tag: &str) -> PathBuf {
    let dir = PathBuf::from("/tmp").join(format!("sbl{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir).unwrap()
}

fn bare(dir: &Path) -> Command {
    let mut c = Command::new(WSP);
    for (k, _) in std::env::vars().filter(|(k, _)| k.starts_with("WSP_") || k.starts_with("COMPOUND_") || k.starts_with("CLAUDE")) {
        c.env_remove(k);
    }
    c.env("HOME", dir.join("home"))
        .env("WSP_HOME", dir.join("store"))
        .env("WSP_STATE", dir.join("state"))
        .env_remove("XDG_CONFIG_HOME")
        .env("TZ", "UTC");
    c
}

/// The pids of every `sandbox serve` bound at `socket`.
fn servers(socket: &str) -> Vec<u32> {
    let out = Command::new("ps").args(["-Ao", "pid=,args="]).output().unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains("sandbox serve") && l.contains(socket))
        .filter_map(|l| l.split_whitespace().next()?.parse().ok())
        .collect()
}

fn gone_within(socket: &str, within: Duration) -> bool {
    let end = Instant::now() + within;
    loop {
        if servers(socket).is_empty() {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A fake sandbox under `dir`, as the test harnesses start one; its socket.
fn up(dir: &Path, name: &str, tether: Option<u32>) -> String {
    std::fs::create_dir_all(dir.join("home")).unwrap();
    assert!(bare(dir).arg("init").output().unwrap().status.success());
    let mut c = bare(dir);
    c.args(["sandbox", "--fake", "--json", "--name", name]);
    if let Some(pid) = tether {
        c.env("WSP_SANDBOX_TETHER", pid.to_string());
    }
    let out = c.output().unwrap();
    let v: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("sandbox json: {e}: {}", String::from_utf8_lossy(&out.stderr)));
    let socket = v["socket"].as_str().unwrap().to_string();
    assert_eq!(servers(&socket).len(), 1, "no `sandbox serve` on {socket}");
    socket
}

/// What a harness holds: `sandbox rm` and the directory, on drop.
struct Guard {
    dir: PathBuf,
    name: String,
}

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = bare(&self.dir).args(["sandbox", "rm", &self.name]).output();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Last resort, so a failing test here does not leak what it is about.
fn reap(socket: &str) {
    for pid in servers(socket) {
        let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
    }
}

#[test]
fn a_test_that_panics_holding_its_guard_leaves_no_sandbox_serve() {
    let dir = scratch("p");
    let name = format!("sblp{}", std::process::id());
    let socket = up(&dir, &name, Some(std::process::id()));
    let caught = std::panic::catch_unwind(|| {
        let _guard = Guard { dir: dir.clone(), name: name.clone() };
        panic!("the test failed mid-way");
    });
    assert!(caught.is_err());
    let ok = gone_within(&socket, Duration::from_secs(5));
    reap(&socket);
    assert!(ok, "a panicking test left `sandbox serve` on {socket}");
}

/// The hub_cache rig's leak: the directory removed out from under a fake that
/// no `sandbox rm` reached. The fake notices its socket is gone and ends.
#[test]
fn a_fake_whose_directory_is_removed_ends() {
    let dir = scratch("d");
    let socket = up(&dir, &format!("sbld{}", std::process::id()), None);
    std::fs::remove_dir_all(&dir).unwrap();
    let ok = gone_within(&socket, Duration::from_secs(5));
    reap(&socket);
    assert!(ok, "a fake outlived its sandbox directory: {socket}");
}

/// A run killed outright runs no `Drop` and removes no directory: the tether is
/// what is left, and the fake ends when it does.
#[test]
fn a_fake_ends_when_the_process_it_is_tethered_to_dies() {
    let dir = scratch("t");
    let mut run = Command::new("sleep").arg("60").spawn().unwrap();
    let socket = up(&dir, &format!("sblt{}", std::process::id()), Some(run.id()));
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(servers(&socket).len(), 1, "the fake ended while its tether was alive");
    run.kill().unwrap();
    run.wait().unwrap();
    let ok = gone_within(&socket, Duration::from_secs(5));
    reap(&socket);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "a fake outlived the process it was tethered to: {socket}");
}
