//! The `--json` projections, byte for byte (wsp-305): the record before the
//! code that draws them moves into `crates/view`, and the guard while it does.
//!
//! A `wsp sandbox --fake` store, seeded at one pinned instant (`WSP_TEST_NOW`,
//! read by `util::epoch_secs` under a test build), holds two projects; open,
//! blocked, done, inbox and archived rows with tags, refs, `after`, a caption,
//! legs and a `## Verification`; a running list with two groups; a machine; a
//! claim; and spend ledger lines. Each command below prints, under the same
//! pinned instant, exactly the bytes in `tests/fixtures/view/<name>.json`,
//! once the sandbox's own directory is written `<dir>`.
//!
//! Every live reading is pinned too. The machine's (cores, load, memory,
//! thermal, idle) by `WSP_TEST_MACHINE` (`throttle::pinned`), its name by
//! `HOSTNAME`. No daemon runs, so `wip` reads the files (`WSP_READ=files`), and
//! its seats come from this machine's manager (`WSP_HOST=manager`): a fake one
//! here, on loopback, that answers `hello` and `seats` with a fixed reply.
//!
//! `WSP_VIEW_BLESS=1 cargo test --test view_golden` writes the fixtures from
//! what the commands print now; only for a change that means to move them.
//!
//! The API's view verbs (wsp-307) are held to the same bytes: with `wsp
//! daemon` serving the same store under the same pins on an ephemeral port,
//! `throttle`, `spend` and `show` asked through `wsp-client` and printed as the
//! CLI prints are exactly what `throttle --json`, `spend --json` and `show <id>
//! --json` print, and a `show` of nothing is `not_found` with the CLI's
//! sentence.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const WSP: &str = env!("CARGO_BIN_EXE_wsp");

/// 2026-11-18T11:06:40Z: every stamp the seeding writes and every age the
/// commands print is read against it.
const NOW: &str = "1795000000";

/// The machine every reading sees: 8 cores at a quiet load, memory and swap to
/// spare, no thermal limit, nobody's idle time to read (`throttle::pinned`).
const MACHINE: &str = r#"{"cores": 8, "load": [1.5, 1.25, 1.0], "mem_free": 60, "swap": [512, 4096], "thermal": 100}"#;

/// What `HOSTNAME` says this machine is called.
const HOST: &str = "golden-host";

/// The seat this machine's manager runs: the one holding beta-003's claim.
const SEAT: &str = "cpd-7";

/// What is captured: the fixture's name and the command line.
const CAPTURES: &[(&str, &[&str])] = &[
    ("ls", &["ls", "--json"]),
    ("ls-all", &["ls", "--all", "--json"]),
    ("wip", &["wip", "--json"]),
    ("worklist-ls", &["worklist", "ls", "--json"]),
    ("machine-ls", &["machine", "ls", "--json"]),
    ("throttle", &["throttle", "--json"]),
    ("spend", &["spend", "--json"]),
    ("show-open", &["show", "alpha-001", "--json"]),
    ("show-archived", &["show", "alpha-003", "--json"]),
];

struct Sandbox {
    name: String,
    outer: BTreeMap<String, String>,
    env: BTreeMap<String, String>,
    state: PathBuf,
    dir: PathBuf,
}

/// `wsp` with none of this seat's own `WSP_*`, `COMPOUND_*` or `CLAUDE*`, at
/// the pinned instant.
fn bare(env: &BTreeMap<String, String>) -> Command {
    let mut c = Command::new(WSP);
    for (k, _) in std::env::vars().filter(|(k, _)| k.starts_with("WSP_") || k.starts_with("COMPOUND_") || k.starts_with("CLAUDE")) {
        c.env_remove(k);
    }
    c.envs(env)
        .env("WSP_TEST_NOW", NOW)
        .env("WSP_TEST_MACHINE", MACHINE)
        .env("HOSTNAME", HOST)
        .env("TZ", "UTC")
        .env("WSP_READ", "files")
        .env("WSP_HOST", "manager");
    c
}

impl Sandbox {
    /// `tag` keeps the tests of this file, which run at once, apart.
    fn up(tag: &str) -> Sandbox {
        // Short: the fake backend's unix socket lives under it, and macOS caps that path at 104 bytes.
        let dir = PathBuf::from("/tmp").join(format!("wv{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let outer: BTreeMap<String, String> = [
            ("WSP_HOME".to_string(), dir.join("home").display().to_string()),
            ("WSP_STATE".to_string(), dir.join("state").display().to_string()),
        ]
        .into();
        bare(&outer).arg("init").output().unwrap();
        let name = "golden".to_string();
        let out = bare(&outer).args(["sandbox", "--fake", "--json", "--name", &name]).output().unwrap();
        let v: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("sandbox json: {e}: {}", String::from_utf8_lossy(&out.stderr)));
        let env: BTreeMap<String, String> =
            v["env"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string())).collect();
        let state = PathBuf::from(&env["WSP_STATE"]);
        let sb = Sandbox { name, outer, env, state, dir };
        sb.ok(&["init"]);
        sb
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = bare(&self.env);
        c.args(args);
        c
    }

    /// Run a seeding verb and insist it succeeded.
    fn ok(&self, args: &[&str]) -> String {
        let o = self.cmd(args).output().unwrap();
        let said = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
        assert!(o.status.success(), "wsp {}: {said}", args.join(" "));
        said
    }

    /// A seeding verb that reads its text on stdin.
    fn ok_in(&self, args: &[&str], input: &str) {
        let mut c = self.cmd(args);
        c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = c.spawn().unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        let o = child.wait_with_output().unwrap();
        assert!(o.status.success(), "wsp {}: {}{}", args.join(" "), String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    }

    fn store(&self) -> wsp_store::store::Store {
        wsp_store::store::Store::at(PathBuf::from(&self.env["WSP_HOME"]), self.state.clone())
    }

    /// A command's stdout, with the sandbox's directory written `<dir>`.
    fn capture(&self, args: &[&str]) -> String {
        let o = self.cmd(args).output().unwrap();
        assert!(o.status.success(), "wsp {}: {}", args.join(" "), String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap().replace(&self.dir.display().to_string(), "<dir>")
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = bare(&self.outer).args(["sandbox", "rm", &self.name]).output();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The store every capture reads.
fn seeded(tag: &str) -> Sandbox {
    use wsp_store::model::{self, Status, Worklist, WorklistStatus};

    let sb = Sandbox::up(tag);
    let store = sb.store();

    sb.ok(&["project", "add", "alpha", "--name", "Alpha"]);
    sb.ok(&["project", "add", "beta", "--name", "Beta"]);

    // alpha-001: open, tagged, high, a ref and an authored caption; a leg and a
    // verification pass in its body.
    sb.ok(&["add", "Open row", "-p", "alpha", "-t", "ui", "-t", "core", "--prio", "high", "--ref", "docs/a.md", "--caption", "the open one"]);
    // alpha-002: rests on alpha-001.
    sb.ok(&["add", "Second row", "-p", "alpha", "--after", "alpha-001"]);
    // alpha-003: done, then archived.
    sb.ok(&["add", "Gone row", "-p", "alpha", "-t", "old"]);
    // beta-001: blocked, with its reason.
    sb.ok(&["add", "Blocked row", "-p", "beta", "-t", "ops"]);
    sb.ok(&["block", "beta-001", "waiting on the vendor"]);
    // beta-002: done, not archived.
    sb.ok(&["add", "Done row", "-p", "beta"]);
    // beta-003: doing, claimed by a seat.
    sb.ok(&["add", "Doing row", "-p", "beta", "--prio", "low"]);
    // inbox-001: no project.
    sb.ok(&["add", "Inbox row"]);

    sb.ok_in(&["leg", "add", "alpha-001", "found", "--order", "-"], "Lay the ground.\n");
    sb.ok_in(&["leg", "add", "alpha-001", "build", "--order", "-"], "Build on it.\n");

    let mut t = store.task("alpha-001").expect("alpha-001 was added");
    model::set_section_in(
        &mut t.body,
        "Verification",
        "- 2026-11-18T10:00:00Z holds · read 0123456789ab · pane cpd-9 · on claude opus high · decided 2026-11-18T10:30:00Z · ended\n  the build holds",
    );
    store.save_task(&t).unwrap();

    for id in ["alpha-003", "beta-002"] {
        let mut t = store.task(id).unwrap();
        t.set_status(Status::Done);
        store.save_task(&t).unwrap();
    }
    let mut t = store.task("beta-003").unwrap();
    t.set_status(Status::Doing);
    store.save_task(&t).unwrap();
    let gone = store.task("alpha-003").unwrap();
    store.archive_task(&gone).unwrap();

    // A running list with two groups.
    let mut w = Worklist::new("list-a", "A list");
    w.set_status(WorklistStatus::Running);
    let g1 = model::Group { members: vec!["beta-003".into(), "beta-002".into()], ..model::Group::default() };
    let g2 = model::Group { members: vec!["alpha-002".into()], ..model::Group::default() };
    w.set_groups(&[g1, g2]);
    store.save_worklist(&w).unwrap();

    // A second machine, on record and never seen.
    sb.ok(&["machine", "add", "mb2", "mb2.local"]);

    // The doing row's seat: its claim, and the pane bound to it.
    store.set_claim("beta-003", json!({ "workspace": SEAT }));
    store.set_binding(SEAT, json!({ "pane": SEAT, "task_id": "beta-003" }));

    // The seat's clock: what the fold leaves when the manager's `turn` (ended,
    // at NOW) reaches it, the one source of `wip`'s `agents[].since` since
    // wsp-030 (`fold::state_since`; seat-since.json's sampling is gone). The
    // fake manager streams no events, so the sandbox stages the folded record.
    {
        use wsp_wire::host::Host as _;
        let seats = wsp_wire::place_compound::Compound::at(sb.state.join(wsp_wire::place_compound::SEATS));
        let dir = seats.dir_of(&wsp_wire::place::Seat::new(SEAT)).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let at = wsp_base::util::iso_at(NOW.parse().unwrap());
        wsp_wire::host::LOCAL.say(&dir, &json!({ "state": "idle", "at": at, "epoch": 1, "from": "manager" }));
    }

    // The stop ledger: seats on the claimed and the landed rows' days, in the
    // window and out of it.
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(sb.state.join("spent.jsonl")).unwrap();
    for (seat, ended, model, micros, output) in [
        ("cpd-7", "2026-11-18T09:40:00Z", "claude-opus-5-5", 3_000_000u64, 100u64),
        ("cpd-9", "2026-11-18T10:20:00Z", "claude-sonnet-5-5", 500_000, 50),
        ("cpd-8", "2026-11-10T09:00:00Z", "claude-haiku-4-5", 250_000, 7),
        ("cpd-6", "2026-09-10T09:00:00Z", "claude-opus-5-5", 9_000_000, 1),
    ] {
        let rec = json!({ "seat": seat, "ended": ended, "session_id": "s", "model": model, "turns": 3, "input": 10, "output": output, "cache_read": 1000, "cache_write": 5, "cost": micros });
        writeln!(f, "{rec}").unwrap();
    }
    fake_manager(&sb.state);
    sb
}

/// This machine's manager, faked: a loopback listener that answers `hello` as
/// a manager running [`SEAT`] and `seats` with that seat idle and the host's
/// facts, and refuses anything else. On record in the sandbox's state, as
/// `wsp install` leaves a real one.
fn fake_manager(state: &Path) {
    use wsp_host_api::*;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let local = wsp_wire::manager::Local {
        host: HOST.into(),
        addr: std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        port,
        exec_dir: state.join("exec"),
    };
    wsp_wire::manager::write_record(state, &local, "golden-token").unwrap();
    let answer = |req: Option<ReqId>, msg: Message| {
        let mut line = serde_json::to_string(&Frame { id: None, req, msg }).unwrap();
        line.push('\n');
        line
    };
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let mut out = stream.try_clone().unwrap();
                for line in BufReader::new(stream).lines().map_while(Result::ok) {
                    let Ok(frame) = serde_json::from_str::<Frame>(&line) else { continue };
                    let reply = match frame.msg {
                        Message::Hello(_) => Message::Hello(Hello::Manager(ManagerHello {
                            host: HostId(HOST.into()),
                            protocol: PROTOCOL,
                            version: "golden".into(),
                            compound_sup: WireRange { min: 9, max: 9 },
                            compound_render: WireRange { min: 1, max: 1 },
                            journal_head: None,
                            seats: vec![SeatEpoch { seat: SeatId(SEAT.into()), epoch: Epoch(1) }],
                            capacity: Capacity { seats: 8, build: 2, check: 2 },
                        })),
                        Message::Seats(_) => Message::SeatsList(SeatsList {
                            seats: vec![SeatRow {
                                seat: SeatId(SEAT.into()),
                                epoch: Epoch(1),
                                state: SeatState::Idle,
                                rss_bytes: 300 * 1024 * 1024,
                                shells: 1,
                                last_turn: None,
                                last_word: None,
                            }],
                            host: Some(HostFacts {
                                load_centi: 150,
                                cpus: 8,
                                memory: MemoryPressure::Normal,
                                swap_used_bytes: 512 * 1024 * 1024,
                                disks: vec![DiskFree { volume: "/".into(), free_bytes: 500 << 30, total_bytes: 1000 << 30 }],
                                thermal: Thermal::Nominal,
                                load5_centi: Some(125),
                                load15_centi: Some(100),
                                speed_limit_pct: Some(100),
                                hid_idle_secs: None,
                                free_memory_bytes: None,
                                pageouts: None,
                            }),
                        }),
                        _ => Message::Refused(Refused { why: Refusal::Unsupported, detail: "the golden manager answers hello and seats".into() }),
                    };
                    if out.write_all(answer(frame.req, reply).as_bytes()).is_err() {
                        break;
                    }
                }
            });
        }
    });
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/view").join(format!("{name}.json"))
}

#[test]
fn every_json_projection_prints_the_recorded_bytes() {
    // The store calls made from here stamp at the same instant the commands do.
    std::env::set_var("WSP_TEST_NOW", NOW);
    std::env::set_var("HOSTNAME", HOST);
    let sb = seeded("");
    let bless = std::env::var_os("WSP_VIEW_BLESS").is_some_and(|v| !v.is_empty() && v != "0");
    let mut moved = Vec::new();
    for (name, args) in CAPTURES {
        let now = sb.capture(args);
        let path = fixture(name);
        if bless {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &now).unwrap();
            continue;
        }
        let was = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e} (WSP_VIEW_BLESS=1 writes it)", path.display()));
        if was != now {
            moved.push(format!("── wsp {} (tests/fixtures/view/{name}.json) ──\n{now}", args.join(" ")));
        }
    }
    assert!(moved.is_empty(), "the --json output moved from its recording:\n{}", moved.join("\n"));
}

/// A `wsp daemon`, killed when dropped.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn the_apis_throttle_spend_and_show_are_the_bytes_the_cli_prints() {
    use wsp_client::{Client, ViewError};

    std::env::set_var("WSP_TEST_NOW", NOW);
    std::env::set_var("HOSTNAME", HOST);
    let sb = seeded("a");
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let daemon = sb.cmd(&["daemon"]).env("WSP_API_PORT", port.to_string()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    let _daemon = Daemon(daemon);
    let wait = Duration::from_secs(120);
    let end = Instant::now() + wait;
    while !sb.state.join("api.json").exists() {
        assert!(Instant::now() < end, "the daemon wrote no api.json within {wait:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut c = Client::dial(&sb.state, wsp_store_contract::Principal::Wsp("view-golden".into()), wait).expect("the daemon's API answers");
    c.set_timeout(wait);

    let printed = |args: &[&str]| {
        let o = sb.cmd(args).output().unwrap();
        assert!(o.status.success(), "wsp {}: {}", args.join(" "), String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap()
    };
    type Ask = fn(&mut Client) -> Result<Value, ViewError>;
    let asks: [(&[&str], Ask); 4] = [
        (&["throttle", "--json"], |c| c.throttle()),
        (&["spend", "--json"], |c| c.spend()),
        (&["show", "alpha-001", "--json"], |c| c.show("alpha-001")),
        (&["show", "alpha-003", "--json"], |c| c.show("alpha-003")),
    ];
    for (args, ask) in asks {
        // The daemon ticks beside this, so the API's answer is read between two
        // of the CLI's, and compared once those two agree (nothing moved).
        let mut tries = 0;
        loop {
            let before = printed(args);
            let api = ask(&mut c).unwrap_or_else(|e| panic!("the API's {}: {e}", args[0]));
            let after = printed(args);
            if before != after {
                tries += 1;
                assert!(tries < 10, "wsp {} never printed the same twice running", args.join(" "));
                continue;
            }
            let drawn = serde_json::to_string_pretty(&api).unwrap() + "\n";
            assert_eq!(drawn, before, "the API's {} is not the bytes `wsp {}` prints", args[0], args.join(" "));
            break;
        }
    }

    // A show of nothing: the CLI says why and fails; the API is `not_found` with the same sentence.
    let o = sb.cmd(&["show", "gamma-404", "--json"]).output().unwrap();
    assert!(!o.status.success());
    match c.show("gamma-404") {
        Err(ViewError::Refused(f)) => {
            assert_eq!(f.code, wsp_api::Code::NotFound);
            assert_eq!(f.text + "\n", String::from_utf8_lossy(&o.stderr));
        }
        other => panic!("a show of nothing over the API gave {other:?}"),
    }
}
