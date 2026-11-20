//! `wsp-manager`: run the agent manager in the foreground (launchd keeps it
//! running; wsp-024 installs it).
//!
//! ```text
//! wsp-manager [--exec-dir DIR] [--host ID] [--port N]
//!             [--tailnet ADDR]... [--no-tailnet] [--volume PATH]...
//!             [--compound-sup PATH] [--seats N] [--inproc-run DIR]...
//!             [--build N] [--check N] [--tree-root DIR]
//! ```
//!
//! It binds loopback and this host's tailnet addresses (from `tailscale ip`,
//! or each `--tailnet`), and nothing else. Without a tailnet it serves on
//! loopback alone and logs that it does.
//!
//! At start it adopts the seats the in-process host started (wsp-029): those
//! in each `--inproc-run`, or in compound's own run directory
//! (`$COMPOUND_RUN_DIR`, else `~/.compound/run`) when none is named.
//!
//! `--build` and `--check` are its job capacity per class, a hard cap that
//! `hello` reports (2 and 4 unless told). `--tree-root` is the directory a
//! job's `dir` is relative to (the home directory unless told).

use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::process::ExitCode;

use wsp_manager::{tailnet_addrs, Config, Manager, DEFAULT_PORT};

const USAGE: &str = "usage: wsp-manager [--exec-dir DIR] [--host ID] [--port N] [--tailnet ADDR]... [--no-tailnet] [--volume PATH]... [--compound-sup PATH] [--seats N] [--inproc-run DIR]... [--build N] [--check N] [--tree-root DIR]";

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("wsp-manager: {why}");
            ExitCode::FAILURE
        }
    }
}

/// compound's own run directory, where the in-process host's seats stand:
/// `$COMPOUND_RUN_DIR`, else `~/.compound/run`.
fn compound_run_dir() -> PathBuf {
    match std::env::var_os("COMPOUND_RUN_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => wsp_base::util::home().join(".compound/run"),
    }
}

/// `$WSP_MANAGER_DIR`, else `~/.local/state/wsp-manager`: where the hooks
/// look for the token too.
fn default_exec_dir() -> PathBuf {
    wsp_host::hooks::manager_dir()
}

fn run(argv: Vec<String>) -> Result<(), String> {
    let mut exec_dir = default_exec_dir();
    let mut host = None;
    let mut port = DEFAULT_PORT;
    let mut tailnet: Vec<IpAddr> = Vec::new();
    let mut no_tailnet = false;
    let mut volumes = Vec::new();
    let mut sup = None;
    let mut seats = None;
    let mut inproc_runs = Vec::new();
    let (mut build, mut check, mut tree_root) = (None, None, None);
    let mut it = argv.into_iter();
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value\n{USAGE}"));
        match flag.as_str() {
            "--exec-dir" => exec_dir = wsp_base::util::expand(&value()?),
            "--host" => host = Some(value()?),
            "--port" => port = value()?.parse().map_err(|e| format!("--port: {e}"))?,
            "--tailnet" => tailnet.push(value()?.parse().map_err(|e| format!("--tailnet: {e}"))?),
            "--no-tailnet" => no_tailnet = true,
            "--volume" => volumes.push(wsp_base::util::expand(&value()?)),
            "--compound-sup" => sup = Some(wsp_base::util::expand(&value()?)),
            "--seats" => seats = Some(value()?.parse().map_err(|e| format!("--seats: {e}"))?),
            "--inproc-run" => inproc_runs.push(wsp_base::util::expand(&value()?)),
            "--build" => build = Some(value()?.parse().map_err(|e| format!("--build: {e}"))?),
            "--check" => check = Some(value()?.parse().map_err(|e| format!("--check: {e}"))?),
            "--tree-root" => tree_root = Some(wsp_base::util::expand(&value()?)),
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
    }

    let host = host.unwrap_or_else(wsp_base::util::hostname);
    let mut cfg = Config::new(&exec_dir, &host);
    cfg.port = port;
    cfg.sup = sup;
    if let Some(n) = seats {
        cfg.seats = n;
    }
    if let Some(n) = build {
        cfg.build = n;
    }
    if let Some(n) = check {
        cfg.check = n;
    }
    if let Some(root) = tree_root {
        cfg.tree_root = root;
    }
    if inproc_runs.is_empty() {
        inproc_runs.push(compound_run_dir());
    }
    cfg.inproc_runs = inproc_runs;
    if !volumes.is_empty() {
        cfg.volumes.extend(volumes);
    }
    let mut note = None;
    if !no_tailnet {
        if tailnet.is_empty() {
            tailnet = wsp_host::InProcess::new().tailnet_ips().map(|out| tailnet_addrs(&out)).unwrap_or_default();
            if tailnet.is_empty() {
                note = Some("no tailnet address (tailscale did not answer); serving on loopback only");
            }
        }
        cfg.addrs.extend(tailnet);
    }
    cfg.addrs.dedup();
    debug_assert_eq!(cfg.addrs[0], IpAddr::V4(Ipv4Addr::LOCALHOST));

    let manager = Manager::start(cfg).map_err(|e| e.to_string())?;
    if let Some(note) = note {
        manager.note(note);
    }
    manager.wait();
    Ok(())
}
