//! The `--json` projections over `State` alone (wsp-308): what web-chrome's
//! fold of the daemon's `State` derives, byte for byte what the CLI prints.
//!
//! The store is `tests/view_golden.rs`'s (`tests/view_seed`). `wsp state
//! --json` prints the contract `State` the files make, by the mirror's own
//! projections (`mirror::state_of`); this test reads it back as a
//! `wsp_store_contract::State`, hands it to each `wsp-view` fn below with the
//! pinned clock, and asserts the bytes equal both what the CLI prints for the
//! same projection and `tests/fixtures/view/<name>.json` — the recording
//! `view_golden` keeps. The CLI prints through the same fn, so the first is
//! the wiring and the second is that the output did not move.
//!
//! **Later legs add a row to [`PROJECTIONS`]**: the fixture's name, the
//! command line, and the `wsp-view` fn over `&State` and the clock.

mod view_seed;

use view_seed::{fixture, seeded, HOST, NOW};
use wsp_store_contract::State;

/// A projection over `State`: the fixture's name, the CLI's command line, and
/// the `wsp-view` fn that prints it (without the newline `println!` adds).
type Projection = (&'static str, &'static [&'static str], fn(&State, i64) -> String);

const PROJECTIONS: &[Projection] = &[
    ("ls-all", &["ls", "--all", "--json"], wsp_view::task::ls_all),
    ("wip", &["wip", "--json"], wip),
    ("worklist-ls", &["worklist", "ls", "--json"], worklist_ls),
    ("worklist-show", &["worklist", "show", "list-a", "--json"], worklist_show),
    ("worklist-show-all", &["worklist", "show", "--all", "--json"], worklist_show_all),
    ("machine-ls", &["machine", "ls", "--json"], machine_ls),
    ("flag-person", &["flag", "--person", "--json"], flag_person),
    ("yours", &["yours", "--json"], yours),
    ("yours-alpha", &["yours", "-p", "alpha", "--json"], yours_alpha),
    ("yours-beta", &["yours", "-p", "beta", "--json"], yours_beta),
];

fn wip(state: &State, now: i64) -> String {
    serde_json::to_string_pretty(&wsp_view::wip::of_state(state, now)).unwrap()
}

fn worklist_ls(state: &State, now: i64) -> String {
    serde_json::to_string_pretty(&wsp_view::worklist::ls_of_state(state, now)).unwrap()
}

fn worklist_show(state: &State, now: i64) -> String {
    serde_json::to_string_pretty(&wsp_view::worklist::show_of_state(state, "list-a", now).expect("list-a is in the State")).unwrap()
}

fn worklist_show_all(state: &State, now: i64) -> String {
    serde_json::to_string_pretty(&wsp_view::worklist::show_all_of_state(state, now, false)).unwrap()
}

fn machine_ls(state: &State, now: i64) -> String {
    wsp_view::machine::ls_of_state(state, now).to_string()
}

fn flag_person(state: &State, now: i64) -> String {
    serde_json::to_string_pretty(&wsp_view::asks::flags_person(state, now)).unwrap()
}

fn yours(state: &State, now: i64) -> String {
    serde_json::to_string_pretty(&wsp_view::asks::yours(state, now, None)).unwrap()
}

fn yours_alpha(state: &State, now: i64) -> String {
    serde_json::to_string_pretty(&wsp_view::asks::yours(state, now, Some("alpha"))).unwrap()
}

fn yours_beta(state: &State, now: i64) -> String {
    serde_json::to_string_pretty(&wsp_view::asks::yours(state, now, Some("beta"))).unwrap()
}

#[test]
fn each_projection_over_state_prints_the_clis_bytes_and_the_recording() {
    std::env::set_var("WSP_TEST_NOW", NOW);
    std::env::set_var("HOSTNAME", HOST);
    let sb = seeded("s");
    let out = sb.cmd(&["state", "--json"]).output().unwrap();
    assert!(out.status.success(), "wsp state --json: {}", String::from_utf8_lossy(&out.stderr));
    let state: State = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("wsp state --json is a State: {e}"));
    assert!(!state.rows.is_empty(), "the seeded rows are in the State");
    let now: i64 = NOW.parse().unwrap();
    let dir = sb.dir.display().to_string();
    let mut moved = Vec::new();
    for (name, args, view) in PROJECTIONS {
        let drawn = format!("{}\n", view(&state, now)).replace(&dir, "<dir>");
        let cli = sb.capture(args);
        let path = fixture(name);
        let was = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e} (view_golden records it)", path.display()));
        if drawn != cli {
            moved.push(format!("── {name}: wsp-view over State ≠ wsp {} ──\nview:\n{drawn}\ncli:\n{cli}", args.join(" ")));
        } else if drawn != was {
            moved.push(format!("── {name}: wsp-view over State ≠ tests/fixtures/view/{name}.json ──\n{drawn}"));
        }
    }
    assert!(moved.is_empty(), "{}", moved.join("\n"));
}
