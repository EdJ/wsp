//! wsp's side of the manager on this machine (`docs/agent-manager.md` 2.4 and
//! 3.3; wsp-024): the local machine record `wsp install` writes, and what
//! `wsp doctor` asks of the manager it names.
//!
//! The manager (`wsp-manager`) makes its token at first start, 0600 in its
//! `<exec_dir>`. wsp keeps a copy beside the address and port it was told to
//! dial, under `<state>/manager/`:
//!
//! - `local.json` — `{host, addr, port, exec_dir}`, what to dial;
//! - `local.secret` — the token, mode 0600, never in the committed store.
//!
//! Nothing here imports `wsp-manager`: [`DEFAULT_PORT`] is the number it
//! listens on unless told otherwise, and `wsp install` writes the number it
//! tells the process, so the record and the process agree by construction.

use std::fs;
use std::io::Write as _;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};
use wsp_host::Tcp;
use wsp_host_api::{
    ConnError, Connection, Frame, Hello, ManagerHello, Message, Refusal, Role, Token, WireRange, WspHello, PROTOCOL,
};

use crate::util;

/// The manager's binary, beside `wsp`.
pub const BINARY: &str = "wsp-manager";
/// The port a manager listens on unless told otherwise (`wsp-manager`'s own
/// `DEFAULT_PORT`).
pub const DEFAULT_PORT: u16 = 7487;
/// The token file in the manager's `<exec_dir>`.
pub const TOKEN_FILE: &str = "manager.token";

/// Where the manager keeps its state: `$WSP_MANAGER_DIR`, else
/// `~/.local/state/wsp-manager` (what the binary itself defaults to).
pub fn default_exec_dir() -> PathBuf {
    match std::env::var_os("WSP_MANAGER_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => util::home().join(".local/state/wsp-manager"),
    }
}

/// The local machine record: how to reach the manager on this machine.
#[derive(Debug, Clone, PartialEq)]
pub struct Local {
    pub host: String,
    pub addr: IpAddr,
    pub port: u16,
    pub exec_dir: PathBuf,
}

impl Local {
    /// This machine's manager as `wsp install` configures it: loopback on `port`.
    pub fn new(exec_dir: &Path, port: u16) -> Local {
        Local { host: util::hostname(), addr: IpAddr::V4(Ipv4Addr::LOCALHOST), port, exec_dir: exec_dir.to_path_buf() }
    }

    pub fn socket(&self) -> SocketAddr {
        SocketAddr::new(self.addr, self.port)
    }

    fn to_json(&self) -> Value {
        json!({"host": self.host, "addr": self.addr.to_string(), "port": self.port,
               "exec_dir": self.exec_dir.display().to_string()})
    }

    fn from_json(v: &Value) -> Option<Local> {
        Some(Local {
            host: v.get("host")?.as_str()?.to_string(),
            addr: v.get("addr")?.as_str()?.parse().ok()?,
            port: u16::try_from(v.get("port")?.as_u64()?).ok()?,
            exec_dir: PathBuf::from(v.get("exec_dir")?.as_str()?),
        })
    }
}

fn dir(state: &Path) -> PathBuf {
    state.join("manager")
}

pub fn record_path(state: &Path) -> PathBuf {
    dir(state).join("local.json")
}

pub fn secret_path(state: &Path) -> PathBuf {
    dir(state).join("local.secret")
}

/// Write the record and its secret file (0600). `token` is the manager's.
pub fn write_record(state: &Path, local: &Local, token: &str) -> Result<(), String> {
    let d = dir(state);
    fs::create_dir_all(&d).map_err(|e| format!("cannot make {}: {e}", util::contract(&d)))?;
    let secret = secret_path(state);
    let _ = fs::remove_file(&secret);
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&secret)
        .map_err(|e| format!("cannot write {}: {e}", util::contract(&secret)))?;
    f.write_all(format!("{token}\n").as_bytes()).map_err(|e| format!("cannot write the token: {e}"))?;
    crate::store::write_atomic(&record_path(state), &format!("{}\n", local.to_json()))
        .map_err(|e| format!("cannot write {}: {e}", util::contract(&record_path(state))))
}

/// Remove the record and the secret file.
pub fn remove_record(state: &Path) {
    let _ = fs::remove_file(secret_path(state));
    let _ = fs::remove_file(record_path(state));
}

/// The record and the token, if both are there.
pub fn read_record(state: &Path) -> Option<(Local, Token)> {
    let local = Local::from_json(&serde_json::from_str(&fs::read_to_string(record_path(state)).ok()?).ok()?)?;
    let token = fs::read_to_string(secret_path(state)).ok()?;
    let token = token.trim();
    (!token.is_empty()).then(|| (local, Token(token.to_string())))
}

/// The manager's token, read out of its `<exec_dir>`; waits up to `wait` for a
/// manager that was only just loaded to make it.
pub fn manager_token(exec_dir: &Path, wait: Duration) -> Option<String> {
    let path = exec_dir.join(TOKEN_FILE);
    let start = std::time::Instant::now();
    loop {
        if let Some(t) = fs::read_to_string(&path).ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()) {
            return Some(t);
        }
        if start.elapsed() >= wait {
            return None;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Copy the manager's token into the machine record's secret file, with the
/// address and port the manager was told to listen on.
pub fn install_record(state: &Path, local: &Local, wait: Duration) -> Result<(), String> {
    let token = manager_token(&local.exec_dir, wait).ok_or_else(|| {
        format!("no token in {} — the manager has not started (see its log in {}/log/)", util::contract(&local.exec_dir), util::contract(&local.exec_dir))
    })?;
    write_record(state, local, &token)
}

/// What a `hello` to the manager came back as.
#[derive(Debug)]
pub enum Probe {
    Answers(Box<ManagerHello>),
    /// It answered, and refused: the token is not its token, or the protocol.
    Refused(Refusal),
    /// Nothing answered, or what did was not a manager.
    Silent(String),
}

/// Dial `addr`, say `hello` with `token`, and read what comes back.
pub fn probe(addr: SocketAddr, token: &Token, wsp_version: &str) -> Probe {
    let mut conn = match Tcp::dial(addr, Duration::from_millis(500)) {
        Ok(c) => c,
        Err(e) => return Probe::Silent(format!("nothing answers at {addr} ({e})")),
    };
    let hello = Frame {
        id: None,
        req: None,
        msg: Message::Hello(Hello::Wsp(WspHello {
            role: Role::Cli,
            protocol: PROTOCOL,
            version: format!("wsp {wsp_version} (doctor)"),
            last_event: None,
            token: token.clone(),
        })),
    };
    if let Err(e) = conn.send(&hello) {
        return Probe::Silent(format!("{addr} did not take a hello ({e})"));
    }
    let out = loop {
        match conn.recv(Duration::from_secs(2)) {
            Ok(Some(Frame { msg: Message::Hello(Hello::Manager(m)), .. })) => break Probe::Answers(Box::new(m)),
            Ok(Some(Frame { msg: Message::Refused(r), .. })) => break Probe::Refused(r.why),
            Ok(Some(_)) => continue,
            Ok(None) => break Probe::Silent(format!("{addr} did not answer a hello")),
            Err(ConnError::Closed) => break Probe::Silent(format!("{addr} closed the connection on a hello")),
            Err(e) => break Probe::Silent(format!("{addr} answered something that is not a manager ({e:?})")),
        }
    };
    conn.shutdown();
    out
}

/// The installed `compound-sup` (`$COMPOUND_SUP`, `PATH`, `~/.local/bin`), if any.
pub fn installed_sup() -> Option<PathBuf> {
    wsp_host::InProcess::new().sup()
}

/// `wsp doctor`'s lines about the manager on this machine.
///
/// Returns `false` when there is no local record to read, saying nothing: the
/// caller then asks the older question of `compound-sup` alone. With a record,
/// this is the local half of the compound-wire check — the manager is what
/// speaks to `compound-sup` here, so the question is whether the installed
/// `compound-sup`'s wire is inside the range the manager names.
///
/// `sup` is the installed `compound-sup`, handed in rather than found, so a test
/// does not race the process's environment over `$COMPOUND_SUP`.
pub fn health(
    state: &Path,
    sup: Option<&Path>,
    wsp_version: &str,
    problems: &mut Vec<String>,
    notes: &mut Vec<String>,
) -> bool {
    let Some((local, token)) = read_record(state) else {
        return false;
    };
    health_of(&local, &token, sup, wsp_version, problems, notes);
    true
}

pub fn health_of(
    local: &Local,
    token: &Token,
    sup: Option<&Path>,
    wsp_version: &str,
    problems: &mut Vec<String>,
    notes: &mut Vec<String>,
) {
    let addr = local.socket();
    match probe(addr, token, wsp_version) {
        Probe::Silent(why) => problems.push(format!(
            "the manager does not answer: {why} — `wsp install --daemon` loads it, its log is {}/log/manager.log",
            util::contract(&local.exec_dir)
        )),
        Probe::Refused(Refusal::Unauthorised) => problems.push(format!(
            "the manager at {addr} refused wsp's token — manager/local.secret is not {}/{TOKEN_FILE}; `wsp install` copies it again",
            util::contract(&local.exec_dir)
        )),
        Probe::Refused(why) => problems.push(format!("the manager at {addr} refused wsp's hello: {why:?}")),
        Probe::Answers(m) => {
            notes.push(format!("manager {} answers hello at {addr} (version {}, protocol {})", m.host.0, m.version, m.protocol));
            if m.protocol != PROTOCOL {
                problems.push(format!(
                    "the manager speaks protocol {}, this wsp speaks {PROTOCOL} — `wsp install` puts them at one commit",
                    m.protocol
                ));
            }
            if m.version != wsp_version {
                problems.push(format!(
                    "the manager is version {}, this wsp is {wsp_version} — `wsp install` puts them at one commit",
                    m.version
                ));
            }
            sup_in_range(sup, m.compound_sup, problems, notes);
        }
    }
}

pub(crate) fn sup_in_range(sup: Option<&Path>, range: WireRange, problems: &mut Vec<String>, notes: &mut Vec<String>) {
    let Some(sup) = sup else {
        notes.push("compound-sup not found ($COMPOUND_SUP or PATH) — nothing to check against the manager's wire range".into());
        return;
    };
    match wsp_host::inproc::ask_wire_version(sup) {
        Some(v) if v < range.min || v > range.max => problems.push(format!(
            "{} speaks wire {v}, outside the manager's range {}..{} — rebuild whichever is behind (`cargo build`, then `wsp install`)",
            util::contract(sup),
            range.min,
            range.max
        )),
        Some(v) => notes.push(format!("compound-sup speaks wire {v}, inside the manager's {}..{}", range.min, range.max)),
        None => notes.push(format!(
            "{} does not answer `wire-version` — its wire cannot be checked against the manager's {}..{}",
            util::contract(sup),
            range.min,
            range.max
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use wsp_manager::{Config, Manager};

    fn _mode(p: &Path) -> u32 {
        fs::metadata(p).map(|m| m.permissions().mode() & 0o777).unwrap_or(0)
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("wsp-wire-manager-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// A `compound-sup` that answers `wire-version` with `reply`.
    fn fake_sup(dir: &Path, reply: &str) -> PathBuf {
        let p = dir.join("compound-sup");
        fs::write(&p, format!("#!/bin/sh\n[ \"$1\" = wire-version ] && echo '{reply}'\n")).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    /// A manager on loopback, an ephemeral port, in its own `<exec_dir>`; the
    /// record `wsp install` would have written for it, in its own state dir.
    fn running(name: &str) -> (Manager, PathBuf, PathBuf, Local) {
        let root = scratch(name);
        let exec = root.join("exec");
        let state = root.join("state");
        let mut cfg = Config::new(&exec, "testhost");
        cfg.port = 0;
        let m = Manager::start(cfg).expect("a manager starts on loopback");
        let mut local = Local::new(&exec, m.addrs()[0].port());
        local.host = "testhost".into();
        install_record(&state, &local, Duration::from_secs(2)).unwrap();
        (m, root, state, local)
    }

    #[test]
    fn the_record_keeps_the_managers_token_at_0600_beside_its_address() {
        let (m, root, state, local) = running("record");
        let (read, token) = read_record(&state).unwrap();
        assert_eq!(read, local);
        assert_eq!(Some(token.0), fs::read_to_string(local.exec_dir.join(TOKEN_FILE)).ok().map(|t| t.trim().to_string()));
        assert_eq!(_mode(&secret_path(&state)), 0o600);
        // Written again, it is still one file at 0600.
        install_record(&state, &local, Duration::from_secs(1)).unwrap();
        assert_eq!(_mode(&secret_path(&state)), 0o600);
        m.stop();
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn doctor_reads_a_loopback_manager_that_answers_with_the_token() {
        let (m, root, state, local) = running("answers");
        let version = wsp_manager::VERSION;
        let range = wsp_host::inproc::WIRE_VERSION;
        let sup = fake_sup(&root, &format!("{range} {range}"));

        let (mut problems, mut notes) = (vec![], vec![]);
        assert!(health(&state, Some(&sup), version, &mut problems, &mut notes));
        assert!(problems.is_empty(), "{problems:?}");
        assert!(notes.iter().any(|n| n.contains("answers hello") && n.contains("testhost")), "{notes:?}");
        assert!(notes.iter().any(|n| n.contains("inside the manager's")), "{notes:?}");

        // A version that is not the manager's, and a sup outside its range.
        let (mut problems, mut notes) = (vec![], vec![]);
        let off = fake_sup(&root, &format!("{} {}", range + 1, range + 1));
        health_of(&local, &read_record(&state).unwrap().1, Some(&off), "9.9.9", &mut problems, &mut notes);
        assert!(problems.iter().any(|p| p.contains("manager is version")), "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("outside the manager's range")), "{problems:?}");

        // Another token is a refusal, said as one.
        let (mut problems, mut notes) = (vec![], vec![]);
        health_of(&local, &Token("not-the-token".into()), Some(&sup), version, &mut problems, &mut notes);
        assert!(problems.iter().any(|p| p.contains("refused wsp's token")), "{problems:?}");
        m.stop();
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn doctor_says_a_manager_that_is_named_and_not_there_does_not_answer() {
        let (m, root, state, _) = running("gone");
        m.stop();
        let (mut problems, mut notes) = (vec![], vec![]);
        assert!(health(&state, None, wsp_manager::VERSION, &mut problems, &mut notes));
        assert!(problems.iter().any(|p| p.contains("the manager does not answer")), "{problems:?}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn with_no_record_doctor_says_nothing_and_leaves_the_question_to_the_caller() {
        let state = scratch("none");
        let (mut problems, mut notes) = (vec![], vec![]);
        assert!(!health(&state, None, "0.1.0", &mut problems, &mut notes));
        assert!(problems.is_empty() && notes.is_empty());
        let _ = fs::remove_dir_all(&state);
    }
}
