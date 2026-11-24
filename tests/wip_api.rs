//! `wip --json` through the state core's API (wsp-076, `docs/state-core.md`
//! 4.5 and 4.6 (c)).
//!
//! A `wsp sandbox --fake` store with a `wsp daemon` serving its mirror, folding
//! a loopback `wsp-manager` that runs one seat (a fake `compound-sup`: nothing
//! spawns claude). The store holds a row in each state, a running list, an open
//! ask and that seat. Then:
//!
//! 1. `wip --json` under `WSP_READ=api` and under `WSP_READ=files` print the
//!    same bytes, once the mirror has caught up with the files, but for the
//!    two moment readings (`load`, `idle_memory`), which both paths read live
//!    and which are checked well-formed and masked;
//! 2. under `api` the call log holds no child for the call;
//! 3. with the daemon stopped, `api` answers from the files within the dial's
//!    budget, says so once on stderr, and prints what `files` prints;
//! 4. `WSP_READ=files` never dials: `api.json` names a listener that counts
//!    what it accepts, and it accepts nothing.
//!
//! `ls --json --all` is held to the same four (wsp-374), but it reads the
//! API by default and `WSP_READ=files` forces the files.
//!
//! 1 and 2 run twice, with the throttle `on` and on its default `auto`, and a
//! live build holder in a slot (what every shimmed cargo build leaves): the
//! throttle line names the build on both paths, and under `auto` says in force
//! or away by the person's idle time, read by `ioreg` on the files path and
//! from the manager's beat under api. The idle seconds may move between the
//! two reads, so their number alone is masked; the in-force/away word is
//! compared. The seats' rss and shells and the host's disks come from the
//! manager's `seats` reply on both paths.

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use wsp_client::{Client, Item};
use wsp_manager::{Config, Manager, TOKEN_FILE};
use wsp_store_contract::{ListFilter, Principal, RowFilter};
use wsp_view::reading;
use wsp_wire::host::{Host, Spawn, Tcp, LOCAL};
use wsp_wire::manager::{install_record, Local};

const WSP: &str = env!("CARGO_BIN_EXE_wsp");
const HOST: &str = "test-host";
const SEAT: &str = "cpd-wa1";

/// compound-sup as far as a spawn and the manager's `vitals` use it (as in
/// tests/wip_manager.rs). `spawn` becomes the session, in a group of its own,
/// and lives while this test does.
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

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

struct Sandbox {
    name: String,
    outer: BTreeMap<String, String>,
    env: BTreeMap<String, String>,
    state: PathBuf,
    daemon: Option<Child>,
    dir: PathBuf,
}

/// `wsp` with none of this seat's own `WSP_*`, `COMPOUND_*` or `CLAUDE*`.
fn bare(env: &BTreeMap<String, String>) -> Command {
    let mut c = Command::new(WSP);
    for (k, _) in std::env::vars().filter(|(k, _)| k.starts_with("WSP_") || k.starts_with("COMPOUND_") || k.starts_with("CLAUDE")) {
        c.env_remove(k);
    }
    c.envs(env);
    c
}

impl Sandbox {
    fn up(tag: &str) -> Sandbox {
        // Short: the fake backend's unix socket lives under it, and macOS caps that path at 104 bytes.
        let dir = PathBuf::from("/tmp").join(format!("wa-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let outer: BTreeMap<String, String> = [
            ("WSP_HOME".to_string(), dir.join("home").display().to_string()),
            ("WSP_STATE".to_string(), dir.join("state").display().to_string()),
        ]
        .into();
        bare(&outer).arg("init").output().unwrap();
        let name = format!("a{tag}");
        let out = bare(&outer).args(["sandbox", "--fake", "--json", "--name", &name]).output().unwrap();
        let v: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("sandbox json: {e}: {}", String::from_utf8_lossy(&out.stderr)));
        let env: BTreeMap<String, String> =
            v["env"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string())).collect();
        let state = PathBuf::from(&env["WSP_STATE"]);
        let sb = Sandbox { name, outer, env, state, daemon: None, dir };
        sb.wsp(&["init"]);
        sb
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = bare(&self.env);
        c.args(args).env("WSP_HOST", "manager");
        c
    }

    fn wsp(&self, args: &[&str]) -> String {
        let o = self.cmd(args).output().unwrap();
        String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr)
    }

    fn start_daemon(&mut self, port: u16) {
        let mut c = self.cmd(&["daemon"]);
        c.env("WSP_API_PORT", port.to_string()).stdout(Stdio::null()).stderr(Stdio::null());
        self.daemon = Some(c.spawn().unwrap());
    }

    fn stop_daemon(&mut self) {
        if let Some(mut d) = self.daemon.take() {
            let _ = d.kill();
            let _ = d.wait();
        }
    }

    fn client(&self, wait: Duration) -> Client {
        let end = Instant::now() + wait;
        while !self.state.join("api.json").exists() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(50));
        }
        Client::dial(&self.state, Principal::Wsp("test".into()), wait).expect("the daemon's API answers")
    }

    fn store(&self) -> wsp_store::store::Store {
        wsp_store::store::Store::at(PathBuf::from(&self.env["WSP_HOME"]), self.state.clone())
    }

    /// `wsp wip --json` under `WSP_READ=read`: its output, how long it took,
    /// and the children its call line counted.
    fn wip(&self, read: &str) -> (Output, Duration, Value) {
        let start = Instant::now();
        let out = self.cmd(&["wip", "--json"]).env("WSP_READ", read).output().expect("wsp runs");
        let took = start.elapsed();
        assert!(out.status.success(), "wsp wip under {read}: {}", String::from_utf8_lossy(&out.stderr));
        serde_json::from_slice::<Value>(&out.stdout)
            .unwrap_or_else(|e| panic!("wip --json under {read}: {e}: {}", String::from_utf8_lossy(&out.stdout)));
        (out, took, self.children_of_last_wip())
    }

    fn children_of_last_wip(&self) -> Value {
        let calls = |f: &str| std::fs::read_to_string(self.state.join(f)).unwrap_or_default();
        let line = calls("polls.jsonl")
            .lines()
            .chain(calls("calls.jsonl").lines())
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|l| l["verb"] == "wip")
            .last()
            .expect("wip's call line");
        line["children"].clone()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        self.stop_daemon();
        let _ = bare(&self.outer).args(["sandbox", "rm", &self.name]).output();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// An open question from `seat`, which waits on it while it works `row`.
fn ask_message(id: &str, text: &str, seat: &str, row: &str) -> wsp_store::message::Message {
    use wsp_store::message::{Kind, Message, Party, Waiting};
    let mut m = Message::question(
        Party::Pane { pane: seat.into(), workspace: seat.into() },
        Kind::Direction,
        text,
        Waiting::new(seat, row),
    );
    m.id = id.into();
    m
}

fn snapshot_with(client: &Client, ready: impl Fn(&Value) -> bool) -> Value {
    let mut feed = client.subscribe(None);
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut last = Value::Null;
    loop {
        match feed.next_within(Duration::from_millis(500)) {
            Some(Item::State { state, .. }) => {
                let v = serde_json::to_value(&state).unwrap();
                if ready(&v) {
                    return v;
                }
                assert!(Instant::now() < deadline, "the snapshot never held the seeded store: {v}");
                last = v;
                feed = client.subscribe(None);
            }
            _ if Instant::now() > deadline => panic!("the snapshot never held the seeded store: {last}"),
            _ => {}
        }
    }
}

/// The moment readings, which stay out of the contract (the person's decision
/// on wsp-076): `load` and `idle_memory` are read live on both paths, and an
/// agent's `held` and wait `line` count seconds since a moment (`elapsed_masked`),
/// so their values may move between two calls. Each is checked present and well-formed
/// and masked in the text; every other byte is compared.
fn masked(out: &Output) -> String {
    masked_text(&String::from_utf8_lossy(&out.stdout))
}

/// [`masked`] over `wip --json`'s text.
fn masked_text(text: &str) -> String {
    let json: Value = serde_json::from_str(text).unwrap();
    let load = &json["load"];
    assert!(json.get("load").is_some(), "no load key: {json}");
    assert!(load.is_null() || load.as_str().is_some_and(is_warning_line), "load is not a warning line: {load}");
    let mem = &json["idle_memory"];
    assert!(
        mem["over"].is_boolean() && mem["panes"].is_array() && mem["total_kb"].is_u64() && mem["unread"].is_u64() && mem["warn_kb"].is_u64(),
        "idle_memory is not well-formed: {mem}"
    );
    let mut lines = Vec::new();
    let mut skipping = false;
    for line in text.lines() {
        if skipping {
            skipping = !line.starts_with("  }");
            continue;
        }
        if line.starts_with("  \"throttle\": ") {
            lines.push(idle_masked(line));
        } else if line.starts_with("  \"load\": ") {
            lines.push("  \"load\": <masked>".to_string());
        } else if let Some(rest) = line.strip_prefix("  \"idle_memory\": ") {
            skipping = rest == "{";
            lines.push("  \"idle_memory\": <masked>".to_string());
        } else if let Some(m) = elapsed_masked(line) {
            lines.push(m);
        } else {
            lines.push(line.to_string());
        }
    }
    lines.join("\n")
}

/// The `load` key is `Throttle::warning()`: the load sentence, the thermal
/// sentence, or both joined by "; " with the load first. So a well-formed line
/// opens with one of the two sentences and carries more than the opening.
fn is_warning_line(line: &str) -> bool {
    const LOAD: &str = "load is ";
    const THERMAL: &str = "the CPU is thermally limited to ";
    match line.strip_prefix(LOAD) {
        Some(rest) => !rest.is_empty(),
        None => line.strip_prefix(THERMAL).is_some_and(|rest| !rest.is_empty()),
    }
}

#[test]
fn the_load_key_accepts_every_sentence_warning_can_produce() {
    let load = "load is 12.3 (1m) and 11.0 (5m) on 8 cores, over 1.5x for a while — hold off heavy runs; no wsp-run build is among it";
    let thermal = "the CPU is thermally limited to 70% of its clock (under 80%), so builds are held to 1 at once and 2 jobs each in every mode — more parallel builds add heat and finish fewer; fewer agents at once would help: `wsp machine set seat agents=2`";
    assert!(is_warning_line(load), "the load sentence");
    assert!(is_warning_line(thermal), "the thermal sentence");
    assert!(is_warning_line(&format!("{load}; {thermal}")), "the two joined");
    for bad in ["", "load is ", "the CPU is thermally limited to ", "cpu clock 70%", "3.2"] {
        assert!(!is_warning_line(bad), "accepted {bad:?}");
    }
}

/// An agent's `held` and its wait's `line` count whole seconds since a moment
/// (`held 3s`, `waiting on a person 3s: …`), so two reads a second apart
/// differ; each is masked with its key, indentation and comma kept.
fn elapsed_masked(line: &str) -> Option<String> {
    let body = line.trim_start();
    let indent = &line[..line.len() - body.len()];
    let comma = if body.ends_with(',') { "," } else { "" };
    if body.starts_with("\"held\": ") {
        return Some(format!("{indent}\"held\": <masked>{comma}"));
    }
    if body.starts_with("\"line\": \"waiting on ") {
        return Some(format!("{indent}\"line\": <masked>{comma}"));
    }
    None
}

/// The throttle line with the person's idle seconds (`idle 3s`, `away 2h`) and
/// the CPU clock (`cpu clock 75%`) masked: the files path reads them at its call
/// and the api path from the manager's last beat, so either may move between
/// the two; the in-force/away word around them is kept.
fn idle_masked(line: &str) -> String {
    let mut out = line.to_string();
    for (open, close) in [("(idle ", ")"), ("away ", " (back"), ("cpu clock ", "%")] {
        if let Some(i) = out.find(open) {
            let from = i + open.len();
            if let Some(n) = out[from..].find(close) {
                out.replace_range(from..from + n, "<n>");
            }
        }
    }
    out
}

/// Every child the call line counted, by name.
fn ran(children: &Value) -> Vec<String> {
    children.as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default()
}

/// A listener that counts the connections it accepts, until the test ends.
fn counting_listener() -> (String, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap().to_string();
    let n = Arc::new(AtomicUsize::new(0));
    let seen = n.clone();
    std::thread::spawn(move || {
        for s in l.incoming() {
            if s.is_ok() {
                seen.fetch_add(1, Ordering::SeqCst);
            }
        }
    });
    (addr, n)
}

/// 1 and 2 under one throttle mode: the same bytes both ways once the mirror
/// has caught up (the files are the truth, and the mirror follows them within
/// a pass), no child under api on any try, and the throttle line, idle
/// seconds masked, the same on both paths and naming the live build.
fn same_both_ways(sb: &Sandbox, mode: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    let (api, files) = loop {
        let (api, _, children) = sb.wip("api");
        let (files, _, _) = sb.wip("files");
        assert_eq!(ran(&children), Vec::<String>::new(), "wip under api with the throttle {mode} ran {children}");
        assert!(api.stderr.is_empty(), "wip under api fell back: {}", String::from_utf8_lossy(&api.stderr));
        if masked(&api) == masked(&files) || Instant::now() > deadline {
            break (api, files);
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    assert_eq!(masked(&api), masked(&files), "wip --json with the throttle {mode}, under api (left) and files (right), the moment readings masked");
    let line = |o: &Output| serde_json::from_slice::<Value>(&o.stdout).unwrap()["throttle"].as_str().unwrap_or_default().to_string();
    let (a, f) = (line(&api), line(&files));
    assert_eq!(idle_masked(&a), idle_masked(&f), "the throttle line with the throttle {mode}");
    assert!(a.starts_with(&format!("throttle: {mode} — ")), "the throttle line under api: {a}");
    assert!(a.contains(" · building: ") && a.contains("`cargo build`"), "the live build under api: {a}");
    // Where ioreg reads the idle time for the files path, the beat carries it.
    if mode == "auto" && !f.contains("unreadable") {
        assert!(!a.contains("unreadable"), "the beat's idle time went unread under api: {a} (files: {f})");
    }
}

/// A seeded sandbox with a daemon serving it, a loopback manager running one
/// seat on the doing row, and a client to the daemon's API, once the mirror
/// holds all of it: what `wip_reads_the_same_through_the_api_and_the_files`
/// and the census reads (wsp-743) each start from. Dropping it ends what it
/// started.
///
/// One at a time: nine sandboxes, each with a daemon and a manager, started
/// at once left the manager's spawn unanswered past its 1 s on this machine,
/// so each holds [`ONE_SEEDED`] until it is dropped, after all it started.
struct Seeded {
    sb: Sandbox,
    client: Client,
    session: u32,
    manager: Option<Manager>,
    /// Writes the build hold again: a hold is live only while its file is younger than the start grace.
    hold_build: Box<dyn Fn()>,
    _one: std::sync::MutexGuard<'static, ()>,
}

static ONE_SEEDED: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl Drop for Seeded {
    fn drop(&mut self) {
        self.sb.wsp(&["despawn", SEAT]);
        LOCAL.signal_group(self.session, "KILL");
        if let Some(m) = self.manager.take() {
            m.stop();
        }
    }
}

fn seeded(tag: &str) -> Seeded {
    use wsp_store::model::{self, Status, Worklist, WorklistStatus};

    // A test that failed holding it poisons it; the next one still runs.
    let one = ONE_SEEDED.lock().unwrap_or_else(|e| e.into_inner());
    let mut sb = Sandbox::up(tag);
    let store = sb.store();

    // A row in each state.
    let states = [
        Status::Inbox,
        Status::Todo,
        Status::Doing,
        Status::Blocked,
        Status::Parked,
        Status::Review,
        Status::Done,
    ];
    let mut ids = Vec::new();
    for (i, s) in states.iter().enumerate() {
        sb.wsp(&["add", &format!("row {i} {s:?}")]);
        let id = format!("inbox-{:03}", i + 1);
        let mut t = store.task(&id).unwrap_or_else(|| panic!("{id} was added"));
        t.set_status(*s);
        store.save_task(&t).unwrap();
        ids.push(id);
    }
    let doing = ids[2].clone();

    // A running list with two groups.
    let mut w = Worklist::new("list-a", "A list");
    w.set_status(WorklistStatus::Running);
    let mut g1 = model::Group::default();
    g1.members = vec![ids[2].clone(), ids[6].clone()];
    let mut g2 = model::Group::default();
    g2.members = vec![ids[1].clone()];
    w.set_groups(&[g1, g2]);
    store.save_worklist(&w).unwrap();

    // An open ask, from the seat below, which waits on it.
    store.save_message(&ask_message("m-1", "may I?", SEAT, &doing)).unwrap();
    // For the census's flags (wsp-743): a question only the person may close,
    // and a note to the person from the same seat.
    let mut theirs = ask_message("m-2", "which way?", SEAT, &doing);
    theirs.for_person = true;
    store.save_message(&theirs).unwrap();
    let mut note = wsp_store::message::Message::new(
        wsp_store::message::Party::Pane { pane: SEAT.into(), workspace: SEAT.into() },
        wsp_store::message::Kind::Note,
        "the build is green",
    );
    note.id = "m-3".into();
    store.save_message(&note).unwrap();
    // For spend: one seat's banked burn, ended now, as `bank_burn` writes it.
    let burn = serde_json::json!({
        "seat": SEAT, "ended": wsp_base::util::now_iso(), "session_id": "s", "transcript": "", "model": "claude-opus-5-5",
        "turns": 3, "input": 10, "output": 20, "cache_read": 30, "cache_write": 40, "cost": 1_250_000,
    });
    std::fs::write(sb.state.join("spent.jsonl"), format!("{burn}\n")).unwrap();
    // For ls's legs: the todo row runs as two.
    let order = sb.dir.join("order.md");
    std::fs::write(&order, "look first\n").unwrap();
    for leg in ["scout", "finish"] {
        sb.wsp(&["leg", "add", &ids[1], leg, "--order", &order.display().to_string()]);
    }

    // A live build holder in a slot, as every shimmed cargo build leaves one.
    // A hold is live by its job (wsp-052, wsp-057), not a pid: this job is in
    // no job table, so the hold is its holder's while its file is younger than
    // the start grace, and it is written again before each read below. No
    // `since`, so its line does not tick between reads.
    let builds = sb.state.join("builds");
    std::fs::create_dir_all(&builds).unwrap();
    let job = wsp_host_api::JobKey { row: wsp_host_api::RowId(doing.clone()), attempt: 0, job: "throttle:build@wip-api".into() };
    let holder = serde_json::json!({ "pid": std::process::id(), "task": doing, "cmd": "cargo build", "tree": sb.dir.display().to_string(), "job": job });
    let hold_build: Box<dyn Fn()> = Box::new(move || std::fs::write(builds.join("slot-0"), holder.to_string()).unwrap());
    hold_build();

    // The loopback manager, on record in the sandbox's state.
    let sup = sb.dir.join("sup");
    wsp_testkit::script(&sup, &FAKE_SUP.replace("__OWNER__", &std::process::id().to_string()));
    let exec = sb.dir.join("exec");
    let mport = free_port();
    let mut cfg = Config::new(&exec, HOST);
    cfg.port = mport;
    cfg.sup = Some(sup);
    // A beat a second: the `seats` reply carries the host's facts as the last beat read them.
    cfg.beat = Duration::from_secs(1);
    let manager = Manager::start(cfg).expect("the manager starts on loopback");
    let token = wsp_host_api::Token(std::fs::read_to_string(exec.join(TOKEN_FILE)).unwrap().trim().to_string());
    let mut local = Local::new(&exec, mport);
    local.host = HOST.into();
    install_record(&sb.state, &local, Duration::from_secs(5)).unwrap();

    // The daemon, folding the manager and serving the mirror.
    sb.start_daemon(free_port());
    let client = sb.client(Duration::from_secs(20));

    // One seat the manager runs, on the doing row.
    let tcp = Tcp::new(local.socket(), token, wsp_host_api::Role::Cli, "test");
    let args: Vec<String> = vec![];
    let spawn = Spawn {
        key: wsp_host_api::Key { row: wsp_host_api::RowId(doing.clone()), attempt: 0 },
        seat: SEAT,
        kind: None,
        cwd: sb.dir.display().to_string(),
        env: BTreeMap::new(),
        program: "true",
        args: &args,
    };
    let session = tcp.spawn_session(&spawn).expect("the manager spawns the seat");
    // Its agent took a turn and ended it, as Claude Code's hooks post it: a
    // seat that has turned is idle to the manager and to the fold alike (until
    // then the manager reads it idle on its vitals and the fold, which hears
    // no vitals, reads it starting).
    let epoch = manager.seats().into_iter().find(|r| r.seat.0 == SEAT).expect("the manager runs the seat").epoch;
    let dial = wsp_host::hooks::Dial::Manager { addr: local.socket(), token_file: exec.join(TOKEN_FILE) };
    let payload = serde_json::json!({ "session_id": format!("sess-{SEAT}") });
    for hook in ["UserPromptSubmit", "Stop"] {
        let sent = wsp_host::hooks::report(&dial, SEAT, epoch, hook, &payload);
        assert!(matches!(sent, wsp_host::hooks::Sent::Taken(_)), "the manager took the {hook} hook");
    }

    let n_rows = ids.len();
    snapshot_with(&client, |v| {
        let n = |k: &str| v[k].as_object().map_or(0, |o| o.len());
        n("rows") == n_rows
            && n("lists") == 1
            && ["m-1", "m-2", "m-3"].iter().all(|m| v["asks"].get(m).is_some())
            && v["seats"].get(SEAT).is_some()
            && v["claims"].get(&doing).is_some()
    });

    Seeded { sb, client, session: session.pid, manager: Some(manager), hold_build, _one: one }
}

#[test]
fn wip_reads_the_same_through_the_api_and_the_files() {
    let mut seed = seeded("wip");
    // 1 and 2, with the throttle on (neither path reads the idle time) and
    // on its default auto (both do, from ioreg and from the beat).
    for mode in ["on", "auto"] {
        std::fs::write(seed.sb.state.join("throttle.json"), serde_json::json!({ "mode": mode }).to_string()).unwrap();
        (seed.hold_build)();
        same_both_ways(&seed.sb, mode);
    }
    let sb = &mut seed.sb;
    // The seat as wip draws it under api.
    let (api, _, _) = sb.wip("api");
    let json: Value = serde_json::from_slice(&api.stdout).unwrap();
    // The idle seat's rss came live from the manager under api too.
    let panes = json["idle_memory"]["panes"].as_array().cloned().unwrap_or_default();
    assert!(panes.iter().any(|p| p["seat"] == SEAT && p["kb"].as_u64().is_some_and(|k| k > 0)), "the seat's rss under api: {json}");
    assert!(json.to_string().contains(SEAT), "the manager's seat is in wip: {json}");
    let agent = json["agents"].as_array().and_then(|a| a.iter().find(|a| a["pane"] == SEAT)).cloned().unwrap_or_default();
    assert!(!agent["waiting"].is_null(), "the seat waits on its open ask: {agent}");
    assert!(json.to_string().contains("list-a"), "the running list is in wip: {json}");

    // 3. With the daemon stopped: the files, within the dial's budget, said
    // once, and the same json.
    sb.stop_daemon();
    let (files, files_took, _) = sb.wip("files");
    let (fell, fell_took, children) = sb.wip("api");
    let said = String::from_utf8_lossy(&fell.stderr);
    assert_eq!(said.matches("wsp: wip reads the files").count(), 1, "the fallback is said once: {said}");
    assert_eq!(said.lines().count(), 1, "one line on stderr: {said}");
    assert_eq!(masked(&fell), masked(&files), "the fallback's json, the moment readings masked");
    assert!(
        fell_took < files_took + Duration::from_millis(200) + Duration::from_millis(300),
        "the fallback took {fell_took:?} against the files' {files_took:?}"
    );
    // The fallback is the files path: what it runs is the files path's.
    let _ = children;

    // 4. WSP_READ=files never dials for what it reads from the files. Under
    // WSP_ORDERS=store, the default since wsp-358, the orders live only in the
    // store, so there the files path's one dial is the orders read (wsp-127):
    // the files read is counted with the orders on files, then the orders' dial.
    let (addr, accepted) = counting_listener();
    std::fs::write(sb.state.join("api.json"), serde_json::json!({ "addrs": [addr] }).to_string()).unwrap();
    let files_only = sb.cmd(&["wip", "--json"]).env("WSP_READ", "files").env("WSP_ORDERS", "files").output().expect("wsp runs");
    assert!(files_only.status.success(), "wsp wip under files: {}", String::from_utf8_lossy(&files_only.stderr));
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(accepted.load(Ordering::SeqCst), 0, "WSP_READ=files dialled");
    sb.wip("files");
    std::thread::sleep(Duration::from_millis(200));
    let orders_dials = accepted.load(Ordering::SeqCst);
    assert!(orders_dials >= 1, "under WSP_ORDERS=store the files path reads the orders through the API");
    // And the listener counts: under api the same call dials it.
    sb.wip("api");
    std::thread::sleep(Duration::from_millis(200));
    assert!(accepted.load(Ordering::SeqCst) > orders_dials, "the counting listener counted nothing under api");
    // 5. WSP_READ unset is the API: the same listener counts a dial, and the
    // output is what `WSP_READ=api` printed.
    let before = accepted.load(Ordering::SeqCst);
    let unset = sb.cmd(&["wip", "--json"]).env_remove("WSP_READ").output().expect("wsp runs");
    assert!(unset.status.success(), "wsp wip with WSP_READ unset: {}", String::from_utf8_lossy(&unset.stderr));
    std::thread::sleep(Duration::from_millis(200));
    assert!(accepted.load(Ordering::SeqCst) > before, "WSP_READ unset did not dial the API");
}

// ------------------------------------------------ ls --json --all (wsp-374)

/// `wsp ls --json --all` under `WSP_READ=read` (unset where empty): its
/// output and how long it took.
fn ls_all(sb: &Sandbox, read: &str) -> (Output, Duration) {
    let mut c = sb.cmd(&["ls", "--json", "--all"]);
    match read {
        "" => c.env_remove("WSP_READ"),
        r => c.env("WSP_READ", r),
    };
    let start = Instant::now();
    let out = c.output().expect("wsp runs");
    let took = start.elapsed();
    assert!(out.status.success(), "wsp ls --json --all under {read:?}: {}", String::from_utf8_lossy(&out.stderr));
    (out, took)
}

/// The children `ls`'s last call line counted.
fn children_of_last_ls(sb: &Sandbox) -> Value {
    let calls = |f: &str| std::fs::read_to_string(sb.state.join(f)).unwrap_or_default();
    calls("polls.jsonl")
        .lines()
        .chain(calls("calls.jsonl").lines())
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|l| l["verb"] == "ls")
        .last()
        .map(|l| l["children"].clone())
        .expect("ls's call line")
}

/// A listener that accepts and then says nothing, holding each connection
/// until the test ends: a silent dial.
fn silent_listener() -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for s in l.incoming().flatten() {
            held.push(s);
        }
    });
    addr
}

/// `ls --json --all` reads the daemon's API by default (wsp-374): the rows'
/// heads and the `tasks` reading, the same bytes as `WSP_READ=files` once the
/// reading has caught up with the files, with no child started and nothing
/// said; with the daemon stopped, or its address silent, it prints the files'
/// bytes within the dial's 200 ms and says why once; and `WSP_READ=files`
/// never dials.
#[test]
fn ls_json_all_reads_the_same_through_the_api_and_the_files() {
    let mut seed = seeded("ls");
    let sb = &mut seed.sb;

    // 1 and 2: the same bytes, no child, no fallback. The reading is taken
    // again every 30 s, so until it has, the API path may fall back and say so.
    let deadline = Instant::now() + Duration::from_secs(70);
    let (api, files) = loop {
        let (api, _) = ls_all(sb, "");
        let children = children_of_last_ls(sb);
        let (files, _) = ls_all(sb, "files");
        let same = api.stdout == files.stdout && api.stderr.is_empty();
        if same {
            assert_eq!(ran(&children), Vec::<String>::new(), "ls --json --all through the API ran {children}");
        }
        if same || Instant::now() > deadline {
            break (api, files);
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    assert!(api.stderr.is_empty(), "ls --json --all never read the API: {}", String::from_utf8_lossy(&api.stderr));
    assert_eq!(
        String::from_utf8_lossy(&api.stdout),
        String::from_utf8_lossy(&files.stdout),
        "ls --json --all through the API (left) and off the files (right)"
    );
    let rows: Value = serde_json::from_slice(&api.stdout).unwrap();
    assert_eq!(rows.as_array().map(Vec::len), Some(7), "every seeded row: {rows}");
    assert!(rows.as_array().unwrap().iter().any(|r| r["legs"].as_array().is_some_and(|l| l.len() == 2)), "a row with legs: {rows}");

    // A filtered ls reads the files, and says nothing of it.
    let doing = sb.cmd(&["ls", "--json", "--all", "-s", "doing"]).env_remove("WSP_READ").output().unwrap();
    assert!(doing.status.success() && doing.stderr.is_empty(), "ls -s doing: {}", String::from_utf8_lossy(&doing.stderr));

    // 3. With the daemon stopped (its api.json left behind): the files, said
    // once, within the dial's budget of what the files take.
    sb.stop_daemon();
    let (files, files_took) = ls_all(sb, "files");
    let fell_back = |sb: &Sandbox, what: &str| {
        let (fell, took) = ls_all(sb, "");
        let said = String::from_utf8_lossy(&fell.stderr);
        assert_eq!(said.matches("wsp: ls reads the files").count(), 1, "{what}: the fallback is said once: {said}");
        assert_eq!(said.lines().count(), 1, "{what}: one line on stderr: {said}");
        assert_eq!(fell.stdout, files.stdout, "{what}: the fallback prints the files' bytes");
        assert!(
            took < files_took + Duration::from_millis(200) + Duration::from_millis(300),
            "{what}: the fallback took {took:?} against the files' {files_took:?}"
        );
    };
    fell_back(sb, "a failed dial");

    // A silent one: an address that accepts and never welcomes.
    std::fs::write(sb.state.join("api.json"), serde_json::json!({ "addrs": [silent_listener()] }).to_string()).unwrap();
    fell_back(sb, "a silent dial");

    // 4. WSP_READ=files never dials.
    let (addr, accepted) = counting_listener();
    std::fs::write(sb.state.join("api.json"), serde_json::json!({ "addrs": [addr] }).to_string()).unwrap();
    let (forced, _) = ls_all(sb, "files");
    assert!(forced.stderr.is_empty(), "ls under files said: {}", String::from_utf8_lossy(&forced.stderr));
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(accepted.load(Ordering::SeqCst), 0, "WSP_READ=files dialled");
    // And the listener counts: unset, the same call dials it.
    ls_all(sb, "");
    std::thread::sleep(Duration::from_millis(200));
    assert!(accepted.load(Ordering::SeqCst) > 0, "the counting listener counted nothing with WSP_READ unset");
}

// ------------------------------------------------- the census reads (wsp-743)
//
// web-chrome's census reads tasks, flags, agents, worklists, machines,
// throttle, spend and wip with no `wsp` child: through wsp-client, each reply
// fed through the wsp-view function the CLI prints with. Each test below pins
// one read: on the seeded store, what the client's reply draws is the JSON the
// CLI's `--json` prints. The daemon takes each reading again on its period
// (10 s, tasks 30 s, spend 60 s), so each polls until the two agree or its
// minute is up, and then compares. Moments the CLI reads live at its call
// (load, idle time, memory, clocks) are masked, as in `masked` above.

/// The CLI's `--json` for `args`, parsed, off the files (`ls --json --all`
/// reads the API by default since wsp-374, and these compare against the files).
fn cli_json(sb: &Sandbox, args: &[&str]) -> Value {
    let out = sb.cmd(args).env("WSP_READ", "files").output().expect("wsp runs");
    assert!(out.status.success(), "wsp {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("wsp {args:?}: {e}: {}", String::from_utf8_lossy(&out.stdout)))
}

/// One reading's shape, from the API; `None` until the daemon has written it.
/// A macro, as this crate names no `serde`: the shape is the one its use takes.
macro_rules! reading_of {
    ($client:expr, $name:expr) => {{
        let got = $client.readings(&[$name]).expect("the readings verb answers");
        got.get($name).and_then(|r| reading::of(&r.json))
    }};
}

/// Polls until `drawn` (from the API) equals `cli` (the CLI's `--json`), or a
/// minute passes, and then compares the two.
fn draws_what_the_cli_prints<T: PartialEq + std::fmt::Debug>(what: &str, mut drawn: impl FnMut() -> Option<T>, mut cli: impl FnMut() -> T) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let c = cli();
        let d = drawn();
        if d.as_ref() == Some(&c) || Instant::now() > deadline {
            assert_eq!(d, Some(c), "{what}: drawn from the API (left) and the CLI's --json (right)");
            return;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// `value` with each of `keys` (at the top) replaced by its kind: a moment
/// the CLI reads at its own call, kept present and of the same type.
fn moments_masked(mut value: Value, keys: &[&str]) -> Value {
    for k in keys {
        if let Some(v) = value.get_mut(*k) {
            let kind = match v {
                Value::Null => "null",
                Value::Bool(_) => "bool",
                Value::Number(_) => "number",
                Value::String(_) => "string",
                Value::Array(_) => "array",
                Value::Object(_) => "object",
            };
            *v = Value::String(format!("<{kind}>"));
        }
    }
    value
}

/// tasks: `rows {heads: true}` and the `tasks` reading, drawn by
/// `task::ls_of_reading` (`task::ls_json_row` in the reading's order), are
/// `ls --json --all`.
#[test]
fn the_census_tasks_through_the_api_are_ls_json_all() {
    let mut seed = seeded("tasks");
    let Seeded { sb, client, .. } = &mut seed;
    let mut heads_seen = Vec::new();
    draws_what_the_cli_prints(
        "tasks",
        || {
            let heads = client.rows(RowFilter { heads: true, ..Default::default() }).expect("rows answers");
            let t: reading::Tasks = reading_of!(client, reading::TASKS)?;
            heads_seen = heads.clone();
            serde_json::from_str::<Value>(&wsp_view::task::ls_of_reading(&heads, &t)?).ok()
        },
        || cli_json(sb, &["ls", "--json", "--all"]),
    );
    assert!(heads_seen.iter().all(|r| !r.fields.contains_key("body") && !r.fields.contains_key("prose")), "heads carry no text");
    let ls = cli_json(sb, &["ls", "--json", "--all"]);
    assert_eq!(ls.as_array().map(Vec::len), Some(7), "every seeded row: {ls}");
    assert!(ls.as_array().unwrap().iter().any(|r| r["legs"].as_array().is_some_and(|l| l.len() == 2)), "a row with legs: {ls}");
}

/// agents and wip (attention): the `wip` reading, drawn by
/// `wip::census_json`, is `wip --json`, its moments masked as `masked` does.
#[test]
fn the_census_agents_and_wip_through_the_api_are_wip_json() {
    let mut seed = seeded("agents");
    let Seeded { sb, client, .. } = &mut seed;
    draws_what_the_cli_prints(
        "agents and wip",
        || {
            let c: wsp_view::wip::Census = reading_of!(client, reading::WIP)?;
            Some(masked_text(&serde_json::to_string_pretty(&wsp_view::wip::census_json(&c)).unwrap()))
        },
        || {
            let out = sb.cmd(&["wip", "--json"]).output().expect("wsp runs");
            masked_text(&String::from_utf8_lossy(&out.stdout))
        },
    );
    let wip = cli_json(sb, &["wip", "--json"]);
    assert!(wip["agents"].as_array().is_some_and(|a| a.iter().any(|a| a["pane"] == SEAT)), "the seat is an agent: {wip}");
}

/// worklists: the `worklists` reading, drawn by `worklist::list_json`, is
/// `worklist ls --json` (host-core's worklists read). `list ls --json`, which
/// web-chrome's feed.rs names beside it, is `ls --json` in this build.
#[test]
fn the_census_worklists_through_the_api_are_worklist_ls_json() {
    let mut seed = seeded("lists");
    let Seeded { sb, client, .. } = &mut seed;
    draws_what_the_cli_prints(
        "worklists",
        || Some(wsp_view::worklist::list_json(&reading_of!(client, reading::WORKLISTS).map(|v: Vec<wsp_view::worklist::Listing>| v)?)),
        || cli_json(sb, &["worklist", "ls", "--json"]),
    );
    let ls = cli_json(sb, &["worklist", "ls", "--json"]);
    assert!(ls.to_string().contains("list-a"), "the running list: {ls}");
    // Lists as heads beside it: each without its text.
    let heads = client.lists(ListFilter { heads: true }).unwrap();
    assert!(heads.len() == 1 && !heads[0].fields.contains_key("body"), "{heads:?}");
}

/// `wsp worklist show --all --json` under `read` (unset for the default):
/// its output, how long it took, and the children its call line counted.
fn show_all(sb: &Sandbox, read: Option<&str>) -> (Output, Duration, Value) {
    let mut c = sb.cmd(&["worklist", "show", "--all", "--json"]);
    // The orders stay on the files, so that a dial is the read's own.
    c.env("WSP_ORDERS", "files");
    if let Some(r) = read {
        c.env("WSP_READ", r);
    }
    let start = Instant::now();
    let out = c.output().expect("wsp runs");
    let took = start.elapsed();
    assert!(out.status.success(), "worklist show --all under {read:?}: {}", String::from_utf8_lossy(&out.stderr));
    let calls = |f: &str| std::fs::read_to_string(sb.state.join(f)).unwrap_or_default();
    let line = calls("polls.jsonl")
        .lines()
        .chain(calls("calls.jsonl").lines())
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|l| l["verb"] == "worklist" && l["sub"] == "show")
        .last()
        .expect("show's call line");
    (out, took, line["children"].clone())
}

/// wsp-375: `worklist show --all --json` reads the lists through the API by
/// default (`lists {heads: true}`, what the mirror wrote beside each head) and
/// prints the files' bytes, with no child; a silent dial reads the files within
/// the dial's budget and says so once; `WSP_READ=files` never dials.
#[test]
fn worklist_show_all_reads_the_same_through_the_api_and_the_files() {
    let mut seed = seeded("show");
    let sb = &mut seed.sb;
    // 1 and 2: the same bytes once the mirror has caught up, no child, no fallback.
    let deadline = Instant::now() + Duration::from_secs(60);
    let (api, files) = loop {
        let (api, _, children) = show_all(sb, None);
        let (files, _, _) = show_all(sb, Some("files"));
        assert_eq!(ran(&children), Vec::<String>::new(), "show --all through the API ran {children}");
        assert!(api.stderr.is_empty(), "show --all fell back: {}", String::from_utf8_lossy(&api.stderr));
        if api.stdout == files.stdout || Instant::now() > deadline {
            break (api, files);
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    assert_eq!(String::from_utf8_lossy(&api.stdout), String::from_utf8_lossy(&files.stdout), "show --all --json, api (left) and files (right)");
    let v: Value = serde_json::from_slice(&api.stdout).unwrap();
    let list = v.as_array().and_then(|a| a.iter().find(|l| l["worklist"]["id"] == "list-a")).cloned().unwrap_or_default();
    assert_eq!(list["groups"].as_array().map(Vec::len), Some(2), "the running list, its groups whole: {v}");

    // 3. A silent dial (a listener that never welcomes): the files, within the
    // dial's budget, said once, and the same bytes.
    sb.stop_daemon();
    let (addr, accepted) = counting_listener();
    std::fs::write(sb.state.join("api.json"), serde_json::json!({ "addrs": [addr] }).to_string()).unwrap();
    let (files, files_took, _) = show_all(sb, Some("files"));
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(accepted.load(Ordering::SeqCst), 0, "WSP_READ=files dialled");
    let (fell, fell_took, _) = show_all(sb, None);
    let said = String::from_utf8_lossy(&fell.stderr);
    assert_eq!(said.matches("wsp: worklist show --all reads the files").count(), 1, "the fallback is said once: {said}");
    assert_eq!(said.lines().count(), 1, "one line on stderr: {said}");
    assert_eq!(fell.stdout, files.stdout, "the fallback prints the files' bytes");
    assert!(
        fell_took < files_took + Duration::from_millis(200) + Duration::from_millis(300),
        "the fallback took {fell_took:?} against the files' {files_took:?}"
    );
    std::thread::sleep(Duration::from_millis(200));
    assert!(accepted.load(Ordering::SeqCst) >= 1, "the default read dialled the listener");
}

/// machines: the `machines` reading, drawn by `machine::list_json`, is
/// `machine ls --json`.
#[test]
fn the_census_machines_through_the_api_are_machine_ls_json() {
    let mut seed = seeded("machines");
    let Seeded { sb, client, .. } = &mut seed;
    draws_what_the_cli_prints(
        "machines",
        || Some(wsp_view::machine::list_json(&reading_of!(client, reading::MACHINES)?)),
        || cli_json(sb, &["machine", "ls", "--json"]),
    );
}

/// The throttle's moments: what the machine reads at the call (load, idle,
/// memory, clock) and what follows from them. The rest (mode, cores, the
/// slot and job settings, builds, agents) is compared.
const THROTTLE_MOMENTS: [&str; 10] =
    ["idle_secs", "in_force", "load", "load_warning", "memory", "cpu_speed_limit", "thermally_limited", "suggested_agents", "slots", "jobs"];

/// throttle: the `throttle` reading, drawn by `throttle::json`, is
/// `throttle --json`, its moments masked.
#[test]
fn the_census_throttle_through_the_api_is_throttle_json() {
    let mut seed = seeded("throttle");
    let Seeded { sb, client, .. } = &mut seed;
    draws_what_the_cli_prints(
        "throttle",
        || Some(moments_masked(wsp_view::throttle::json(&reading_of!(client, reading::THROTTLE)?), &THROTTLE_MOMENTS)),
        || moments_masked(cli_json(sb, &["throttle", "--json"]), &THROTTLE_MOMENTS),
    );
}

/// spend: the `spend` reading, drawn by `spend::report` at the reader's clock,
/// is `spend --json` at the CLI's.
#[test]
fn the_census_spend_through_the_api_is_spend_json() {
    let mut seed = seeded("spend");
    let Seeded { sb, client, .. } = &mut seed;
    // The CLI's clock at its call, which the reader draws at: `cli` runs first.
    let now = std::cell::Cell::new(0);
    draws_what_the_cli_prints(
        "spend",
        || {
            let s: reading::Spend = reading_of!(client, reading::SPEND)?;
            Some(wsp_view::spend::report(&s.lines, &s.places(), s.landed, now.get()))
        },
        || {
            now.set(wsp_base::util::epoch_secs());
            cli_json(sb, &["spend", "--json"])
        },
    );
    let spend = cli_json(sb, &["spend", "--json"]);
    assert_eq!(spend["totals"]["today"]["turns"], 3, "the banked burn is counted: {spend}");
}

/// The CLI's `--json` for `args`, as the bytes it prints.
fn cli_text(sb: &Sandbox, args: &[&str]) -> String {
    let out = sb.cmd(args).output().expect("wsp runs");
    assert!(out.status.success(), "wsp {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("utf-8")
}

/// The seeded store and, for `yours --json`'s `closed`, one more question of
/// the person's, answered by the person just now.
fn seeded_flags(tag: &str) -> Seeded {
    let seed = seeded(tag);
    let mut asked = ask_message("m-4", "ship it?", SEAT, "inbox-003");
    asked.for_person = true;
    seed.sb.store().save_message(&asked).unwrap();
    seed.sb.wsp(&["answer", "m-4", "--as-person", "yes"]);
    seed
}

/// flags, the hands (wsp-745): the `flags` reading's `person`, drawn by
/// `flag::person_json`, is `flag --person --json` byte for byte.
#[test]
fn the_census_flags_through_the_api_are_flag_person_json() {
    let mut seed = seeded_flags("flags-person");
    let Seeded { sb, client, .. } = &mut seed;
    draws_what_the_cli_prints(
        "flag --person",
        || {
            let f: wsp_view::flag::Flags = reading_of!(client, reading::FLAGS)?;
            Some(format!("{}\n", serde_json::to_string_pretty(&wsp_view::flag::person_json(&f.person)).unwrap()))
        },
        || cli_text(sb, &["flag", "--person", "--json"]),
    );
    let hands = cli_json(sb, &["flag", "--person", "--json"]);
    let ids: Vec<&str> = hands.as_array().into_iter().flatten().filter_map(|m| m["id"].as_str()).collect();
    assert!(ids.contains(&"m-2") && ids.contains(&"m-3"), "the person's question and note: {hands}");
}

/// flags, the questions (wsp-745): the `flags` reading, drawn by
/// `flag::yours_json`, is `yours --json` byte for byte, a closed question
/// included.
#[test]
fn the_census_flags_through_the_api_are_yours_json() {
    let mut seed = seeded_flags("flags-yours");
    let Seeded { sb, client, .. } = &mut seed;
    draws_what_the_cli_prints(
        "yours",
        || {
            let f: wsp_view::flag::Flags = reading_of!(client, reading::FLAGS)?;
            Some(format!("{}\n", serde_json::to_string_pretty(&wsp_view::flag::yours_json(&f)).unwrap()))
        },
        || cli_text(sb, &["yours", "--json"]),
    );
    let yours = cli_json(sb, &["yours", "--json"]);
    let items: Vec<&str> = yours["items"].as_array().into_iter().flatten().filter_map(|m| m["id"].as_str()).collect();
    // m-1 too: a question with no route is the person's, since nobody else has it.
    assert_eq!(items, ["m-1", "m-2"], "the person's open questions: {yours}");
    assert_eq!(yours["review"], 1, "the review row: {yours}");
    let closed: Vec<&str> = yours["closed"].as_array().into_iter().flatten().filter_map(|m| m["id"].as_str()).collect();
    assert_eq!(closed, ["m-4"], "the question answered just now: {yours}");
}
