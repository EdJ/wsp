//! Splitting the panel into a workspace, and taking it back out.
//!
//! Never `layout.apply`: herdr rebuilds the whole tree from that call and every
//! pane in it gets a fresh terminal, which takes down any agent running in the
//! workspace. So this reaches for `pane.split` and swaps the new pane into the
//! narrow slot itself.

use std::collections::HashSet;

use serde_json::json;

use crate::herdr;
use crate::store::Store;

use super::{PANEL_LABEL, VIEW_LABEL};

pub(super) fn panel_command() -> Vec<String> {
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "wsp".into());
    vec![exe, "panel".into()]
}

pub(super) fn panels_state(store: &Store) -> std::collections::BTreeMap<String, String> {
    std::fs::read_to_string(store.state.join("panels.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .map(|m| {
            m.into_iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn save_panels(store: &Store, map: &std::collections::BTreeMap<String, String>) {
    let obj: serde_json::Map<String, serde_json::Value> = map
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    let _ = std::fs::create_dir_all(&store.state);
    let _ = crate::store::write_atomic(
        &store.state.join("panels.json"),
        &serde_json::to_string_pretty(&serde_json::Value::Object(obj)).unwrap_or_default(),
    );
}

pub(super) struct PaneInfo {
    pub(super) id: String,
    pub(super) label: String,
}

pub(super) fn list_panes(ws_id: &str) -> Result<Vec<PaneInfo>, String> {
    let r = herdr::call("pane.list", json!({ "workspace_id": ws_id })).map_err(|e| e.to_string())?;
    let arr = r.get("panes").and_then(|p| p.as_array()).cloned().unwrap_or_default();
    Ok(arr
        .iter()
        // The server has honoured the filter, but a stray pane from another
        // workspace here would mean splitting the wrong tree.
        .filter(|p| p.get("workspace_id").and_then(|x| x.as_str()) == Some(ws_id))
        .filter_map(|p| {
            Some(PaneInfo {
                id: p.get("pane_id")?.as_str()?.to_string(),
                label: p.get("label").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            })
        })
        .collect())
}

/// The pane a panel should be split off: the widest one that is not ours.
/// Width comes from herdr's layout, because "first in the list" is an
/// arbitrary answer that happens to be right only when there is one pane.
pub(super) fn widest<'a>(ws_id: &str, panes: &'a [PaneInfo]) -> Option<&'a PaneInfo> {
    let mine = |p: &PaneInfo| p.label == PANEL_LABEL || p.label == VIEW_LABEL;
    let candidates: Vec<&PaneInfo> = panes.iter().filter(|p| !mine(p)).collect();
    if candidates.len() <= 1 {
        return candidates.into_iter().next();
    }
    let widths: std::collections::BTreeMap<String, u64> = herdr::call(
        "pane.layout",
        json!({ "pane_id": candidates[0].id, "workspace_id": ws_id }),
    )
    .ok()
    .and_then(|r| r.get("layout").and_then(|l| l.get("panes").cloned()))
    .and_then(|p| p.as_array().cloned())
    .unwrap_or_default()
    .iter()
    .filter_map(|p| {
        let id = p.get("pane_id")?.as_str()?.to_string();
        let w = p.get("rect")?.get("width")?.as_u64()?;
        Some((id, w))
    })
    .collect();

    candidates.into_iter().max_by_key(|p| widths.get(&p.id).copied().unwrap_or(0))
}

/// The store this panel is talking to, as environment for anything we spawn.
///
/// A tab or workspace herdr creates starts a fresh shell, which inherits
/// nothing from us — so a panel pointed at a non-default store would open
/// editors pointed at the default one. They would then fail to find the task
/// and take the tab down with them, which looks exactly like the key not
/// working.
pub(super) fn store_env() -> serde_json::Map<String, serde_json::Value> {
    let mut env = serde_json::Map::new();
    for key in ["WSP_HOME", "WSP_STATE", "WSP_NO_COMMIT"] {
        if let Ok(v) = std::env::var(key) {
            if !v.is_empty() {
                env.insert(key.to_string(), json!(v));
            }
        }
    }
    env
}

pub(super) fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `pane.split` starts the user's shell and takes no command, so the panel is
/// `exec`d over it — which also means quitting the panel closes the pane
/// instead of leaving a bare prompt behind.
pub(super) fn launch_panel(pane: &str) {
    let cmd = panel_command().iter().map(|a| shell_quote(a)).collect::<Vec<_>>().join(" ");
    let _ = herdr::call("pane.rename", json!({ "pane_id": pane, "label": "wsp" }));
    let _ = herdr::call(
        "pane.send_text",
        json!({ "pane_id": pane, "text": format!("exec {cmd}\n") }),
    );
}

/// Install into one workspace. Returns Ok(true) when a panel was added,
/// Ok(false) when the workspace already had one.
///
/// This used to build a whole layout tree and hand it to `layout.apply`, with
/// the agent's pane re-referenced by id on the assumption that herdr would
/// carry it across. It does not: the tree is rebuilt from scratch and every
/// pane in it gets a fresh terminal, which killed every running agent. So:
/// `pane.split`, which adds one pane beside a target and touches nothing else.
pub fn install_one(store: &Store, ws_id: &str, ratio: f64) -> Result<bool, String> {
    let panes = list_panes(ws_id)?;
    let mut panels = panels_state(store);

    if let Some(existing) = panels.get(ws_id) {
        if panes.iter().any(|p| p.id == *existing) {
            return Ok(false);
        }
    }
    // A pane labelled `wsp` is ours even when the state file lost track of it
    // — a crash between the split and the save would otherwise install twice.
    if let Some(orphan) = panes.iter().find(|p| p.label == "wsp") {
        panels.insert(ws_id.to_string(), orphan.id.clone());
        save_panels(store, &panels);
        return Ok(false);
    }
    // Never split one of our own panes. A leftover view pane used to be a
    // candidate, and splitting it gave the panel 22% of an already-narrow
    // column — seven usable characters.
    let Some(target) = widest(ws_id, &panes) else {
        return Ok(false);
    };
    let before: HashSet<&str> = panes.iter().map(|p| p.id.as_str()).collect();

    // Splitting `right` at `ratio` leaves the *target* holding `ratio` and puts
    // the new pane in the remainder, so the sidebar arrives on the wrong side
    // at the wrong width. Swapping the two afterwards lands the panel in the
    // narrow left slot without disturbing either process.
    let res = herdr::call(
        "pane.split",
        json!({
            "direction": "right",
            "target_pane_id": target.id,
            "ratio": ratio,
            "focus": false,
        }),
    )
    .map_err(|e| e.to_string())?;

    let new_pane = res
        .get("pane")
        .and_then(|p| p.get("pane_id"))
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            let after = list_panes(ws_id).ok()?;
            after.into_iter().find(|p| !before.contains(p.id.as_str())).map(|p| p.id)
        })
        .ok_or_else(|| "split reported no new pane".to_string())?;

    let _ = herdr::call(
        "pane.swap",
        json!({ "source_pane_id": target.id, "target_pane_id": new_pane }),
    );
    launch_panel(&new_pane);

    panels.insert(ws_id.to_string(), new_pane);
    save_panels(store, &panels);
    Ok(true)
}

/// Auto-install for a newly created workspace — but only once the panel is
/// actually in use, so linking the plugin never surprises anyone.
pub fn install_if_adopted(store: &Store, ws_id: &str) {
    if panels_state(store).is_empty() {
        return;
    }
    let _ = install_one(store, ws_id, 0.22);
}

pub fn install(store: &Store, args: &crate::Args) -> i32 {
    if !herdr::available() {
        eprintln!("wsp: no herdr socket");
        return 1;
    }
    let ratio: f64 = args.get("ratio").and_then(|r| r.parse().ok()).unwrap_or(0.22);
    let only = args.get("workspace").or_else(|| herdr::Env::read().workspace_id);
    let all = args.has("all");

    // Without `--all` this installs into exactly one workspace. Run outside a
    // herdr pane there is no such workspace, and letting the filter fall
    // through would quietly install into every one of them.
    if !all && only.is_none() {
        eprintln!("wsp: not inside a herdr pane — pass --workspace <id> or --all");
        return 1;
    }

    let workspaces = herdr::workspaces().unwrap_or_default();
    let mut added = 0;
    let mut skipped: Vec<String> = Vec::new();

    for ws in &workspaces {
        if !all {
            if let Some(target) = &only {
                if &ws.id != target {
                    continue;
                }
            }
        }
        match install_one(store, &ws.id, ratio) {
            Ok(true) => added += 1,
            Ok(false) => {}
            Err(e) => skipped.push(format!("{}: {e}", ws.label)),
        }
    }

    if args.json() {
        println!("{}", json!({ "installed": added, "skipped": skipped }));
    } else {
        println!("panel installed in {added} workspace(s)");
        for sk in &skipped {
            println!("  skipped {sk}");
        }
    }
    0
}

pub fn uninstall(store: &Store, args: &crate::Args) -> i32 {
    let mut panels = panels_state(store);
    let only = args.get("workspace");
    let mut removed = 0;
    let targets: Vec<(String, String)> = panels
        .iter()
        .filter(|(ws, _)| only.as_ref().map(|o| *ws == o).unwrap_or(true))
        .map(|(a, b)| (a.clone(), b.clone()))
        .collect();

    for (ws, pane) in targets {
        if herdr::call("pane.close", json!({ "pane_id": pane })).is_ok() {
            removed += 1;
        }
        // The view pane is ours too. Leaving it behind orphans a pane nothing
        // will reclaim, and the next install would try to split it.
        if let Ok(ps) = list_panes(&ws) {
            for v in ps.iter().filter(|p| p.label == VIEW_LABEL) {
                let _ = herdr::call("pane.close", json!({ "pane_id": v.id }));
            }
        }
        panels.remove(&ws);
    }
    save_panels(store, &panels);

    if args.json() {
        println!("{}", json!({ "removed": removed }));
    } else {
        println!("panel removed from {removed} workspace(s)");
    }
    0
}
