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

mod view_seed;

use std::net::TcpListener;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use view_seed::{fixture, seeded, HOST, NOW};

/// What is captured: the fixture's name and the command line.
const CAPTURES: &[(&str, &[&str])] = &[
    ("ls", &["ls", "--json"]),
    ("ls-all", &["ls", "--all", "--json"]),
    ("wip", &["wip", "--json"]),
    ("worklist-ls", &["worklist", "ls", "--json"]),
    ("worklist-show", &["worklist", "show", "list-a", "--json"]),
    ("worklist-show-all", &["worklist", "show", "--all", "--json"]),
    ("machine-ls", &["machine", "ls", "--json"]),
    ("flag-person", &["flag", "--person", "--json"]),
    ("yours", &["yours", "--json"]),
    ("yours-alpha", &["yours", "-p", "alpha", "--json"]),
    ("yours-beta", &["yours", "-p", "beta", "--json"]),
    ("throttle", &["throttle", "--json"]),
    ("spend", &["spend", "--json"]),
    ("show-open", &["show", "alpha-001", "--json"]),
    ("show-archived", &["show", "alpha-003", "--json"]),
];

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
