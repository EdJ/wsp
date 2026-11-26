//! `wsp-engine`: the engine and the seats, the phase 2 cut (`docs/components.md` §5, wsp-440); see `MAP.md`.
//! Depends on `wsp-base`, `wsp-store`, `wsp-wire` and `wsp-trees`. The root re-exports each module at its old
//! path (`crate::cycle`, `crate::cmd_spawn`, …), so no caller changed.

pub use wsp_base::{choices, input, risk, util};
pub use wsp_store::{agent_kind, caption, checks, decisions, governors, lands, lapse, lease, log, message, model, resolve, sharing, store, tags, verbs};
pub use wsp_trees::{at_stop, check_tree, cmd_checkout, cmd_verify, gc, greens, guard, overlap, throttle, treecopy, warm};
pub use wsp_wire::{fake, far, host, host_tcp, launchd, manager, place, place_compound, place_super, systemd, tunnel};

pub mod base;

/// The test kit at the path every test module uses: the store-backed fixtures beside `Store` in `wsp-store`,
/// the git repository fixture in `wsp-testkit`.
#[cfg(test)]
pub mod testkit {
    pub use wsp_store::testkit::{isolated_store, store};
    pub use wsp_testkit::{git, repo, repo_at};
}

pub mod addon;
pub mod address;
pub mod affected;
pub mod after;
pub mod agent_commands;
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
pub mod classify;
pub mod closing;
pub mod cmd_agent;
pub mod cmd_allocate;
pub mod cmd_attend;
pub mod cmd_brief;
pub mod cmd_govern;
pub mod cmd_hub;
pub mod cmd_initiative;
pub mod cmd_install;
pub mod cmd_login;
pub mod cmd_machine;
pub mod cmd_mandate;
pub mod cmd_message;
pub mod cmd_migrate;
pub mod cmd_project;
pub mod cmd_publish;
pub mod cmd_resume;
pub mod cmd_role;
pub mod cmd_sandbox;
pub mod cmd_spawn;
pub mod cmd_stamp;
pub mod cmd_submit;
pub mod cmd_task;
pub mod cmd_watch;
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
// After a `stale` (wsp-176): the merge, and the push and stale glue; called by the sync worker (wsp-464, wsp-469).
#[allow(dead_code)]
pub mod hub_cache;
pub mod hub_journal;
pub mod hub_lease;
pub mod hub_sync;
pub mod inner;
pub mod kind_attention;
pub mod kind_data;
pub mod leg_seat;
pub mod leg_tree;
pub mod legs;
pub mod list_sweep;
pub mod liveness;
pub mod local_trust;
pub mod memory;
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
#[allow(dead_code)]
pub mod reconcile;
pub mod record_workspace;
pub mod refusals;
pub mod repair;
pub mod retro_yours;
pub mod room;
pub mod scope;
pub mod scratch;
pub mod size_check;
pub mod split;
pub mod spoke;
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
pub mod wake;
pub mod worklist;
