//! `wip` reads `seats` from the manager (`docs/agent-manager.md` 4.3, M1's
//! group 5; wsp-027).
//!
//! The `wsp` binary runs `wip --json` against a loopback `wsp-manager` that
//! runs one seat (a fake `compound-sup`: nothing spawns claude), and the call
//! log it writes on exit is read for the children it ran:
//!
//! - under `WSP_HOST=manager` there is no `ps`, `lsof`, `compound-render` or
//!   `df` among them, and the seat the manager runs is in the answer;
//! - under `WSP_HOST=inproc` the same call does run `ps`, so the log is seen
//!   to count what it should;
//! - with the manager stopped, `wip` under `manager` says the manager is
//!   unreachable rather than answering with nothing, still within the second
//!   and still with none of those children;
//! - the json's keys are the same under both.

use std::collections::BTreeMap;
use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::Value;
use wsp_manager::{Config, Manager, TOKEN_FILE};
use wsp_wire::host::{Host, Spawn, Tcp, LOCAL};
use wsp_wire::manager::{install_record, Local};

/// compound-sup as far as a spawn and the manager's `vitals` use it. `spawn`
/// becomes the session, in a group of its own, and lives while this test does.
const FAKE_SUP: &str = r#"#!/bin/sh
OWNER=__OWNER__
verb=$1; shift
case "$verb" in
wire-version) echo "9 9" ;;
spawn)
  label=
  while [ $# -gt 0 ]; do
    case "$1" in
      --label) label=$2; shift 2 ;;
      --agent|--cwd|--ephemeral|--cols|--rows) shift 2 ;;
      --) shift; break ;;
      *) shift ;;
    esac
  done
  run=${COMPOUND_RUN_DIR:?}
  mkdir -p "$run"
  name=s$$
  echo "$$" > "$run/$name.sock"
  printf '%s' "$label" > "$run/$name.label"
  printf 'sup %s\npid %s\nsocket %s\nrender %s\n' "$$" "$$" "$run/$name.sock" "$run/$name.rsock"
  exec perl -MPOSIX -e 'POSIX::setsid(); exec @ARGV or die' sh -c "trap 'exit 0' TERM; while kill -0 $OWNER 2>/dev/null; do sleep 0.2; done" </dev/null >/dev/null 2>&1
  ;;
vitals)
  pid=$(cat "$1" 2>/dev/null)
  if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    echo "renderer_attached false"; echo "output_at 0"
  else
    echo "no session at $1" >&2; exit 1
  fi
  ;;
*) echo "fake compound-sup: no verb $verb" >&2; exit 2 ;;
esac
"#;

/// What `wip` must not run under the manager.
const PROBES: &[&str] = &["ps", "lsof", "df", "compound-render"];

fn place() -> PathBuf {
    let base = std::env::var_os("WSP_SCRATCH").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("wm-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("state")).unwrap();
    fs::create_dir_all(dir.join("home")).unwrap();
    let sup = dir.join("sup");
    wsp_testkit::script(&sup, &FAKE_SUP.replace("__OWNER__", &std::process::id().to_string()));
    dir
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// `wsp` on this test's store and state, as no seat.
fn wsp(state: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_wsp"));
    for (k, _) in std::env::vars().filter(|(k, _)| k.starts_with("WSP_") || k.starts_with("COMPOUND_") || k.starts_with("CLAUDE")) {
        cmd.env_remove(k);
    }
    cmd.env("WSP_STATE", state).env("WSP_HOME", state.parent().unwrap().join("home"));
    cmd
}

/// `wsp wip --json` under `host`, as no seat, and the call line it wrote.
fn wip(state: &Path, host: &str) -> (Value, Value, Duration) {
    let calls = |f: &str| fs::read_to_string(state.join(f)).unwrap_or_default();
    let before = calls("polls.jsonl").lines().count() + calls("calls.jsonl").lines().count();
    let start = Instant::now();
    let out = wsp(state).args(["wip", "--json"]).env("WSP_HOST", host).output().expect("wsp runs");
    let took = start.elapsed();
    assert!(out.status.success(), "wsp wip under {host}: {}", String::from_utf8_lossy(&out.stderr));
    let json: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)));
    let lines: Vec<Value> = calls("polls.jsonl")
        .lines()
        .chain(calls("calls.jsonl").lines())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    assert_eq!(lines.len(), before + 1, "one call line per wip");
    let line = lines.into_iter().rev().find(|l| l["verb"] == "wip").expect("wip's call line");
    (json, line["children"].clone(), took)
}

fn ran<'a>(children: &'a Value, probes: &[&'a str]) -> Vec<&'a str> {
    probes.iter().copied().filter(|p| children.get(p).is_some()).collect()
}

fn keys(v: &Value) -> Vec<String> {
    v.as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default()
}

#[test]
fn wip_under_the_manager_runs_no_probe_and_a_stopped_manager_reads_silent() {
    let dir = place();
    let state = dir.join("state");
    let exec = dir.join("exec");
    let port = free_port();
    let mut cfg = Config::new(&exec, "test-host");
    cfg.port = port;
    cfg.sup = Some(dir.join("sup"));
    let manager = Manager::start(cfg).expect("the manager starts on loopback");
    let token = wsp_host_api_token(&exec);
    let mut local = Local::new(&exec, port);
    local.host = "test-host".into();
    install_record(&state, &local, Duration::from_secs(5)).unwrap();
    let init = wsp(&state).arg("init").output().unwrap();
    assert!(init.status.success(), "wsp init: {}", String::from_utf8_lossy(&init.stderr));

    // One seat the manager runs.
    let tcp = Tcp::new(local.socket(), token, wsp_host_api::Role::Cli, "test");
    let args: Vec<String> = vec![];
    let spawn = Spawn { key: wsp_host_api::Key { row: wsp_host_api::RowId("wsp-027".into()), attempt: 0 }, seat: "cpd-wm1", kind: None, cwd: dir.display().to_string(), env: BTreeMap::new(), program: "true", args: &args };
    let session = tcp.spawn_session(&spawn).expect("the manager spawns the seat");

    // Under the manager: the seat, and no probe.
    // `wip` gives the manager `SILENT` (1 s) to answer and reads it silent
    // past that, which a loaded machine overruns (the same wait wsp-693 made
    // in repair's census test): asked again until the manager is heard,
    // bounded generously, and only a heard answer is held to having the seat.
    let deadline = Instant::now() + Duration::from_secs(120);
    let (json, children, took) = loop {
        let got = wip(&state, "manager");
        if got.0["unreachable"].is_null() {
            break got;
        }
        assert!(Instant::now() < deadline, "wip never heard the manager within 120s: {}", got.0);
    };
    let agents = json["agents"].as_array().cloned().unwrap_or_default();
    assert!(agents.iter().any(|a| a["pane"] == "cpd-wm1"), "the manager's seat is in wip: {json}");
    assert_eq!(ran(&children, PROBES), Vec::<&str>::new(), "wip under the manager ran {children}");
    assert!(json["unreachable"].is_null(), "the manager answered: {json}");
    assert!(took < Duration::from_secs(10), "{took:?}");

    // In process: the same call does run `ps`, so the log counts children.
    let (inproc, kids, _) = wip(&state, "inproc");
    assert!(ran(&kids, &["ps"]) == ["ps"], "wip in process runs ps: {kids}");
    // The shape is the same: the keys the UI polls are there under both.
    let (k_mgr, k_in) = (keys(&json), keys(&inproc));
    assert_eq!(k_mgr, k_in, "the json's keys under manager and inproc");

    // Stopped: silent, never empty.
    manager.stop();
    let (json, children, took) = wip(&state, "manager");
    let said = json["unreachable"].as_str().unwrap_or_default();
    assert!(said.contains("this machine's manager") && said.contains("host silent"), "a stopped manager is said: {json}");
    assert_eq!(ran(&children, PROBES), Vec::<&str>::new(), "wip with the manager stopped ran {children}");
    assert!(took < Duration::from_secs(10), "{took:?}");

    LOCAL.signal_group(session.pid, "KILL");
    let _ = fs::remove_dir_all(&dir);
}

fn wsp_host_api_token(exec: &Path) -> wsp_host_api::Token {
    wsp_host_api::Token(fs::read_to_string(exec.join(TOKEN_FILE)).unwrap().trim().to_string())
}
