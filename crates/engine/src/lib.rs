//! `wsp-engine`: the engine, the phase 2 cut (`docs/components.md` §5, wsp-440; the seats are `wsp-seats`, wsp-441); see `MAP.md`.
//! Depends on `wsp-base`, `wsp-store`, `wsp-wire` and `wsp-trees`. The root re-exports each module at its old
//! path (`crate::cycle`, `crate::cmd_spawn`, …), so no caller changed.

pub use wsp_base::{choices, input, risk, util};
pub use wsp_store::{agent_kind, caption, checks, decisions, governors, lands, lapse, lease, log, message, model, resolve, sharing, store, tags, verbs};
pub use wsp_trees::{at_stop, check_tree, cmd_checkout, cmd_verify, gc, greens, guard, overlap, throttle, treecopy, warm};
pub use wsp_wire::{fake, far, host, host_tcp, launchd, manager, place, place_compound, place_super, systemd, tunnel};

pub mod base;

/// The test kit at the path every test module uses: the store-backed fixtures beside `Store` in `wsp-store`,
/// the git repository fixture in `wsp-testkit`. Only the first reach a dependent: layering keeps `wsp-testkit` a dev-dependency.
#[cfg(any(test, feature = "test-support"))]
pub mod testkit {
    pub use wsp_store::testkit::{isolated_store, store};
    #[cfg(test)]
    pub use wsp_testkit::{git, repo, repo_at};
}

pub mod addon;
pub mod address;
pub mod affected;
pub mod after;
pub mod artefact;
pub mod attempt;
pub mod attention;
pub mod auto_close;
pub mod barrier;
pub mod barrier_replay;
pub mod barrier_run;
pub mod build_output;
pub mod by_merge;
pub mod check;
pub mod claims;
pub mod classify;
pub mod closing;
pub mod cmd_initiative;
pub mod initiative_run;
pub mod cmd_mandate;
pub mod cmd_message;
pub mod cmd_migrate;
pub mod cmd_submit;
pub mod cmd_task;
pub mod cmd_whoami;
pub mod cmd_worklist;
pub mod cold;
pub mod cycle;
pub mod deliver;
pub mod did;
pub mod disk;
// The import door (wsp-175): its caller is the spoke's sync worker (wsp-464, wsp-469), which is not built yet.
#[allow(dead_code)]
pub mod door;
pub mod duplicate;
pub mod ending;
pub mod exit;
pub mod fold;
pub mod follow_up;
pub mod history;
pub mod hold;
pub mod hold_close;
pub mod inner;
pub mod leg_seat;
pub mod leg_tree;
pub mod legs;
pub mod list_sweep;
pub mod liveness;
pub mod local_trust;
pub mod mirror;
pub mod orders;
pub mod outputs;
pub mod owed;
pub mod packet;
pub mod packet_gen;
pub mod pickup;
pub mod piece_close;
pub mod piece_seat;
pub mod pieces;
pub mod prose;
#[allow(dead_code)]
pub mod record_workspace;
pub mod refusals;
pub mod repair;
pub mod retro_yours;
pub mod room;
pub mod scope;
pub mod scratch;
pub mod sent;
pub mod size_check;
pub mod split;
pub mod stage;
pub mod stand_down;
pub mod starts;
pub mod stepping;
pub mod stuck;
pub mod tell_log;
pub mod untaken;
pub mod verification;
pub mod verifier_packet;
pub mod waiting;
pub mod waits;
pub mod watch_levels;
pub mod worklist;
