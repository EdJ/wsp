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

mod view_seed;

use view_seed::{fixture, seeded, HOST, NOW};

/// What is captured: the fixture's name and the command line.
const CAPTURES: &[(&str, &[&str])] = &[
    ("ls", &["ls", "--json"]),
    ("ls-all", &["ls", "--all", "--json"]),
    ("wip", &["wip", "--json"]),
    ("worklist-ls", &["worklist", "ls", "--json"]),
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
    let sb = seeded();
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
