//! `wsp-seats`: the seats, the second cut (`docs/components.md` §5, wsp-441); see `MAP.md`. Depends on
//! `wsp-engine`, never the other way: the engine reaches the seats only through the `Seats` impl the root
//! installs (`cycle::install_seats(&cmd_spawn::Fleet)`, wsp-542). Every engine module is re-exported here at its
//! old path (`crate::cycle`, `crate::cmd_task`, …), and the root re-exports these at theirs, so no caller changed.

pub use wsp_engine::{base, choices, input, risk, util};
pub use wsp_engine::{agent_kind, caption, checks, decisions, governors, lands, lapse, lease, log, message, model, resolve, sharing, store, tags, verbs};
pub use wsp_engine::{at_stop, check_tree, cmd_checkout, cmd_verify, gc, greens, guard, overlap, throttle, treecopy, warm};
pub use wsp_engine::{fake, far, host, host_tcp, launchd, manager, place, place_compound, place_super, systemd, tunnel};
// The shared-work add-on is reached through `addon` alone: the spoke (`wsp-spoke`) is never named here.
pub use wsp_engine::{
    addon, address, affected, after, artefact, attempt, attention, auto_close, barrier, barrier_replay, barrier_run, build_output, by_merge, check, claims, classify,
    closing, cmd_initiative, cmd_mandate, cmd_message, cmd_migrate, cmd_submit, cmd_task, cmd_whoami,
    cmd_worklist, cold, cycle, deliver, did, disk, door, duplicate, ending, exit, fold, follow_up, history, hold, hold_close,
    initiative_run, inner, leg_seat, leg_tree, legs, list_sweep, liveness, local_trust, mirror, orders, outputs, owed, packet, packet_gen, pickup, piece_close,
    piece_seat, pieces, prose, record_workspace, refusals, repair, retro_yours, room, scope, scratch, sent, size_check, split, stage, stand_down,
    starts, stepping, stuck, tell_log, untaken, verification, verifier_packet, waiting, waits, watch_levels, worklist,
};

/// The test kit at the path every test module uses: engine's store-backed fixtures, and the git repository
/// fixture from this crate's own `wsp-testkit` (a dev-dependency; layering keeps it out of engine's normal ones).
#[cfg(test)]
pub mod testkit {
    pub use wsp_engine::testkit::{isolated_store, store};
    pub use wsp_testkit::{git, repo, repo_at};
}

/// What a seats test that opens a real `state.db` calls first: layering keeps the sqlite crate out of engine's
/// normal dependencies, so engine's `orders::tests::open_db` takes the open from a dependent (d1 on wsp-441).
#[cfg(test)]
pub fn install_db() {
    orders::tests::install_db(|path, domain| std::sync::Arc::new(wsp_store_sqlite::SqliteStore::open(path, domain).unwrap()));
}

pub mod agent_commands;
pub mod cmd_agent;
pub mod cmd_attend;
pub mod cmd_brief;
pub mod cmd_govern;
pub mod cmd_install;
pub mod cmd_machine;
pub mod cmd_project;
pub mod cmd_resume;
pub mod cmd_role;
pub mod cmd_sandbox;
pub mod cmd_spawn;
pub mod cmd_stamp;
pub mod cmd_watch;
pub mod kind_attention;
pub mod kind_data;
pub mod memory;
pub mod seat_block;
pub mod wake;
