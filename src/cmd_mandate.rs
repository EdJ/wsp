//! `wsp mandate` — standing direction for a workspace.
//!
//! A claim says what an agent is doing now. A mandate says what it is *for*,
//! which is the question an agent has to answer for itself every time it
//! finishes something. Without one, a session records faithfully what it was
//! told and then stops; with one, it can take the next piece of work without
//! being asked again.
//!
//! Kept beside the claims and keyed on the agent (`compound-092` stage B) —
//! because it has to survive a herdr restart, which a claim or a binding does
//! not, and an agent's row does. A store on disk outlives a release, so a
//! mandate a pre-migration store still holds — keyed on the workspace, the
//! shape this used before — reads correctly for one release too; see
//! [`from_map`]. A direction you have to repeat every morning is not standing
//! direction, it is a reminder.

use serde_json::json;

use crate::herdr;
use crate::resolve::Index;
use crate::store::Store;
use crate::util::{self, Paint};
use crate::Args;

/// Whether work in `project` is inside the mandate on `mandated`.
///
/// True both ways along the ancestor chain, and the second direction is the
/// one that matters: a sub-project that declares no roots of its own — as
/// `data` and `render` do — can only ever be worked from inside its parent's,
/// so an agent standing in `wsp` under a mandate on `data` is exactly where it
/// should be. Reading containment one way round would have called that out of
/// scope, and would have passed every test anyone thought to write, because
/// the obvious test is a mandate on a project with a root.
pub fn in_scope(index: &Index, mandated: &str, project: Option<&str>) -> bool {
    let Some(project) = project else { return false };
    if project == mandated {
        return true;
    }
    index.subtree(mandated).iter().any(|p| p == project)
        || index.subtree(project).iter().any(|p| p == mandated)
}

/// The project this agent — or, failing that, this workspace — is mandated to
/// work, if any.
pub fn current(store: &Store, agent: Option<&str>, workspace: Option<&str>) -> Option<String> {
    from_map(&store.mandates(), agent, workspace)
}

/// The same answer, from a map somebody has already read.
///
/// The panel holds every mandate in its snapshot and asks about twenty panes
/// per refresh; going back to the file for each of them would be twenty reads
/// of one small map. Kept here rather than open-coded there so the host rule
/// below has exactly one home.
///
/// # Reading a store two releases wide
///
/// `mandates.json` on disk can carry both shapes at once for one release: rows
/// a pre-`compound-092` writer keyed on a workspace id, sitting beside rows
/// this release keys on an agent id. `store::mandates()` hands back a plain
/// map with no memory of which writer wrote which row, so this is the one
/// place that decides — `place::is_agent_id` tells the two shapes apart,
/// exactly as it tells an agent id apart from a task id. The agent, being the
/// shape a writer here now always uses, is tried first; the workspace is the
/// fallback a store written before this release still needs.
pub fn from_map(
    mandates: &std::collections::BTreeMap<String, serde_json::Value>,
    agent: Option<&str>,
    workspace: Option<&str>,
) -> Option<String> {
    let rec = agent
        .filter(|a| crate::place::is_agent_id(a))
        .and_then(|a| mandates.get(a))
        .or_else(|| workspace.and_then(|w| mandates.get(w)))?;
    // A mandate names a machine as well as its key: a workspace id is
    // herdr's and means nothing on another host, the same reason a claim
    // carries one — and an agent id, though it needs no such check to stay
    // out of another host's id space, was set from a process on some one
    // host and the direction it recorded is a fact about that host's room.
    let host = rec.get("host").and_then(|x| x.as_str()).unwrap_or("");
    if !host.is_empty() && host != util::hostname() {
        return None;
    }
    rec.get("project").and_then(|x| x.as_str()).map(|s| s.to_string())
}

pub fn mandate(store: &Store, args: &Args) -> i32 {
    let env = herdr::Env::read();
    let Some(ws) = args.get("workspace").or(env.workspace_id.clone()) else {
        eprintln!("wsp: no workspace — pass -w, or run inside herdr");
        return 2;
    };
    // The environment's own pane answers for `ws` only when `ws` is the room
    // this process is standing in — named explicitly (the panel always
    // executes the mandate it picks inside the pane it names, so this is the
    // common case) or left to fall back to the environment above. An explicit
    // `-w` naming some *other* room says nothing about who, if anyone, is
    // sitting there — the same rule `cmd_govern::govern` applies to the pane
    // it stamps on a record — so it gets no pane at all. A read (report,
    // clear) still works from outside a room by its legacy workspace key;
    // only setting a new mandate needs a seat to attach an agent's identity
    // to, and that need is checked where it bites, below.
    let pane = (env.workspace_id.as_deref() == Some(ws.as_str()))
        .then(|| env.pane_id.clone())
        .flatten()
        .filter(|p| !p.is_empty());
    // Read-only: whoever already holds this seat, with no minting. Minting
    // only happens where a mandate is actually being set — see there for why.
    let agent = pane.as_deref().and_then(|p| store.agent_in_seat(p));
    let index = Index::new(store.projects());
    let p = Paint::new();

    if args.has("clear") {
        let had = current(store, agent.as_deref(), Some(&ws));
        let removed = store.clear_mandate(agent.as_deref(), &ws);
        store.log_event("mandate-cleared", json!({ "workspace": ws, "agent": agent, "project": had }));
        if args.json() {
            println!("{}", json!({ "workspace": ws, "cleared": removed, "was": had }));
        } else if let Some(was) = had {
            println!("{} {}", p.dim("mandate cleared —"), was);
        } else {
            println!("{}", p.dim("no mandate on this workspace"));
        }
        return 0;
    }

    // No argument: say what the standing direction is, and nothing else. A
    // command that reports is a command an agent can run without deciding to
    // change anything.
    let Some(needle) = args.rest.first().cloned() else {
        let held = current(store, agent.as_deref(), Some(&ws));
        if args.json() {
            println!("{}", json!({ "workspace": ws, "project": held }));
            return 0;
        }
        match held {
            Some(proj) => {
                println!("{} {}", p.cyan("▸"), p.bold(&proj));
                println!("  {}", p.dim("take work here without asking · wsp mandate --clear to stop"));
            }
            None => println!("{}", p.dim("no mandate — this workspace works what it is given")),
        }
        return 0;
    };

    let Some(proj) = index.find(&needle) else {
        eprintln!("wsp: no such project `{needle}`{}", crate::util::dash_hint(&needle));
        return 1;
    };

    // The ordering question a workspace-keyed mandate never had to answer: a
    // mandate is set on a room before anybody is in it, and under the new key
    // that is a room with no agent row yet. `agent_for_seat` answers it the
    // same way `claim` answers it for itself. A `-w` naming a room this
    // process is not standing in has no pane to found that identity on, so it
    // cannot set — only read or clear — a mandate.
    let Some(seat) = pane.as_deref() else {
        eprintln!("wsp: mandate can only be set from the room it names — run it there, or omit -w");
        return 2;
    };
    let agent_id = agent.unwrap_or_else(|| store.agent_for_seat(seat));

    store.set_mandate(
        &agent_id,
        json!({
            "project": proj.id,
            "host": util::hostname(),
            "set_at": util::now_iso(),
        }),
    );
    store.log_event("mandate-set", json!({ "workspace": ws, "agent": agent_id, "project": proj.id }));

    if args.json() {
        println!("{}", json!({ "workspace": ws, "project": proj.id }));
    } else {
        println!("{} {}", p.cyan("▸"), p.bold(&proj.id));
        let open = store
            .tasks()
            .iter()
            .filter(|t| t.status().is_open())
            .filter(|t| in_scope(&index, &proj.id, t.project.as_deref()))
            .count();
        println!(
            "  {}",
            p.dim(&format!("{open} open · take work here without asking · wsp next to start"))
        );
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> (crate::util::Isolated, Store) {
        let env = crate::util::isolated(tag);
        let store = Store::open();
        store.ensure_dirs().unwrap();
        store.save_project(&crate::model::Project::new("wsp")).unwrap();
        (env, store)
    }

    /// **The barrier itself**: a store written before this migration —
    /// `mandates.json` keyed on a workspace id, the only shape that ever
    /// existed before `compound-092` stage B — read back through the real
    /// verb, not asserted against `from_map` directly.
    #[test]
    fn a_mandate_a_pre_migration_store_wrote_still_reports_through_the_verb() {
        let (_env, store) = scratch("mandate-legacy-report");
        store.set_mandate("w1", json!({ "project": "wsp", "host": util::hostname() }));

        let args = Args::synth("mandate", &[], &[("workspace", "w1")]);
        assert_eq!(mandate(&store, &args), 0);
        assert_eq!(current(&store, None, Some("w1")).as_deref(), Some("wsp"));
    }

    /// The same fixture, cleared through the real verb — a clear that only
    /// knew the new shape would leave a pre-migration mandate stuck.
    #[test]
    fn wsp_mandate_clear_removes_a_legacy_row_too() {
        let (_env, store) = scratch("mandate-legacy-clear");
        store.set_mandate("w1", json!({ "project": "wsp", "host": util::hostname() }));

        let args = Args::synth("mandate", &[], &[("workspace", "w1"), ("clear", "")]);
        assert_eq!(mandate(&store, &args), 0);
        assert!(store.mandates().is_empty(), "the legacy key is gone, not left behind");
        assert_eq!(current(&store, None, Some("w1")), None);
    }

    /// A mandate written new is keyed on the agent, never on the workspace it
    /// was named with — "writers move outright" is a behaviour, not a
    /// sentence in a doc comment.
    #[test]
    fn a_new_mandate_is_keyed_on_the_agent_never_on_the_workspace() {
        let (_env, store) = scratch("mandate-new-shape");
        store.set_agent("a-abc-p1", json!({ "seat": "w1:p1", "started": "2026-09-19T00:00:00Z" }));

        let agent_id = store.agent_for_seat("w1:p1");
        assert_eq!(agent_id, "a-abc-p1", "the seat's existing row is reused, not re-minted");

        store.set_mandate(&agent_id, json!({ "project": "wsp", "host": util::hostname() }));
        let m = store.mandates();
        assert!(m.contains_key("a-abc-p1"));
        assert!(!m.contains_key("w1"), "the workspace name never becomes a key");
    }

    /// The ordering question the row exists to answer: a mandate set on a
    /// seat with no agent row yet mints one right there, the same mint site
    /// `claim` uses — so a claim made afterwards in the same pane finds and
    /// reuses this row rather than minting a second one, and the mandate is
    /// already attached to the agent that claim goes on to use.
    #[test]
    fn a_mandate_on_an_empty_seat_mints_the_agent_a_claim_will_later_reuse() {
        let (_env, store) = scratch("mandate-mints-agent");
        assert!(store.agents_held().is_empty(), "nobody has claimed here yet");

        let minted = store.agent_for_seat("w1:p1");
        assert!(crate::place::is_agent_id(&minted), "{minted}");
        assert_eq!(
            store.agents_held().get(&minted).and_then(|a| a.get("seat")).and_then(|s| s.as_str()),
            Some("w1:p1")
        );

        // A second call — standing in for the claim that follows — reuses the
        // same row rather than minting a second one.
        assert_eq!(store.agent_for_seat("w1:p1"), minted);
        assert_eq!(store.agents_held().len(), 1, "one row, not two");
    }

    /// `from_map` prefers the agent-keyed row over a legacy workspace-keyed
    /// one for the same room, when a store somehow carries both — the shape a
    /// writer here now always uses is the one that answers.
    #[test]
    fn from_map_prefers_the_agent_shaped_key_over_the_legacy_workspace_one() {
        let mut mandates = std::collections::BTreeMap::new();
        mandates.insert("w1".to_string(), json!({ "project": "stale" }));
        mandates.insert("a-xyz-p1".to_string(), json!({ "project": "current" }));

        assert_eq!(
            from_map(&mandates, Some("a-xyz-p1"), Some("w1")).as_deref(),
            Some("current")
        );
        // And the legacy fallback still answers when there is no agent row —
        // the case a pane with nobody claimed onto it is in.
        assert_eq!(from_map(&mandates, None, Some("w1")).as_deref(), Some("stale"));
    }
}
