//! `wsp mandate` — standing direction for a workspace.
//!
//! A claim says what an agent is doing now. A mandate says what it is *for*,
//! which is the question an agent has to answer for itself every time it
//! finishes something. Without one, a session records faithfully what it was
//! told and then stops; with one, it can take the next piece of work without
//! being asked again.
//!
//! Kept beside the claims and keyed on the workspace, because a workspace is
//! the unit a person points at a piece of work — and because it has to survive
//! a herdr restart. A direction you have to repeat every morning is not
//! standing direction, it is a reminder.

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

/// The project this workspace is mandated to work, if any.
pub fn current(store: &Store, workspace: Option<&str>) -> Option<String> {
    from_map(&store.mandates(), workspace?)
}

/// The same answer, from a map somebody has already read.
///
/// The panel holds every mandate in its snapshot and asks about twenty panes
/// per refresh; going back to the file for each of them would be twenty reads
/// of one small map. Kept here rather than open-coded there so the host rule
/// below has exactly one home.
pub fn from_map(mandates: &std::collections::BTreeMap<String, serde_json::Value>, workspace: &str) -> Option<String> {
    let rec = mandates.get(workspace)?;
    // A mandate names a machine as well as a workspace: workspace ids are
    // herdr's and mean nothing on another host, the same reason a claim
    // carries one.
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
    let index = Index::new(store.projects());
    let p = Paint::new();

    if args.has("clear") {
        let had = current(store, Some(&ws));
        let removed = store.clear_mandate(&ws);
        store.log_event("mandate-cleared", json!({ "workspace": ws, "project": had }));
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
        let held = current(store, Some(&ws));
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
        eprintln!("wsp: no such project `{needle}`");
        return 1;
    };

    store.set_mandate(
        &ws,
        json!({
            "project": proj.id,
            "host": util::hostname(),
            "set_at": util::now_iso(),
        }),
    );
    store.log_event("mandate-set", json!({ "workspace": ws, "project": proj.id }));

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
