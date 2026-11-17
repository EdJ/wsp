//! `wsp-manager`: run the agent manager in the foreground (launchd keeps it
//! running; wsp-024 installs it).
//!
//! ```text
//! wsp-manager [--exec-dir DIR] [--host ID] [--port N]
//!             [--tailnet ADDR]... [--no-tailnet] [--volume PATH]...
//! ```
//!
//! It binds loopback and this host's tailnet addresses (from `tailscale ip`,
//! or each `--tailnet`), and nothing else. Without a tailnet it serves on
//! loopback alone and logs that it does.

use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::process::ExitCode;

use wsp_manager::{tailnet_addrs, Config, Manager, DEFAULT_PORT};

const USAGE: &str = "usage: wsp-manager [--exec-dir DIR] [--host ID] [--port N] [--tailnet ADDR]... [--no-tailnet] [--volume PATH]...";

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("wsp-manager: {why}");
            ExitCode::FAILURE
        }
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
