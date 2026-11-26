//! A crate that depends on engine, as `wsp-seats` will (wsp-544): every test-only item the seats modules'
//! tests name is reachable here, and engine knows it is in a test. Layering keeps `wsp-testkit` and
//! the sqlite crate out of engine's normal dependencies, so a dependent takes `testkit::repo_at` from its own
//! `wsp-testkit` and installs the `state.db` open.
#![allow(unused_imports)]

use wsp_engine::barrier::open as barrier_open;
use wsp_engine::cmd_message::tests::{item_json, scratch as message_scratch, task as message_task, two_questions};
use wsp_engine::cmd_task::tests::{far, parse, refused, released, reseated, scratch, verified_row, Absent, Reads};
use wsp_engine::cycle::tests::{list, reading, scratch as cycle_scratch, task, Exits, ENDED, KEPT, SAID, TOLD};
use wsp_engine::ending::tests::{alive, log_of, pass, released as ended_released, verified, Fake};
use wsp_engine::fold::mirror_tests::{frame, open};
use wsp_engine::legs::tests::{leg, parse as legs_parse, row, scratch as legs_scratch};
use wsp_engine::orders::tests::{db, ed, install_db, seat};
use wsp_engine::testkit::store;
use wsp_engine::refusals::tests::{ack, fail, open as refusing_open};
use wsp_engine::repair::tests::{Fake as RepairFake, Inproc, FAKE_SUP};
use wsp_engine::verification::record;
use wsp_engine::verification::tests::{at_the_leg_cap, scratch as verification_scratch, sent_back_by_check};
use wsp_engine::verifier_packet::react_fixture;
use wsp_engine::waiting::{Asks, On, Own, Shells};

#[test]
fn a_dependents_tests_read_as_a_test_and_reach_engines_fixtures() {
    // What `cycle::start_detached` returns early on: this harness is never handed `worklist advance`.
    assert!(wsp_engine::base::under_test());
    let _ = (Asks::one, Asks::held_one, Asks::waiter, Shells::one, Shells::own);
    install_db(|path, domain| std::sync::Arc::new(wsp_store_sqlite::SqliteStore::open(path, domain).unwrap()));
    let dir = std::env::temp_dir().join(format!("wsp-dependent-db-{}", std::process::id()));
    seat(&db(&dir, wsp_server::Mode::Owned), "cpd-1", wsp_store_contract::SeatState::Started);
    std::fs::remove_dir_all(&dir).unwrap();
}
