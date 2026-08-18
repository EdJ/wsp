//! Splitting the panel into a workspace, and taking it back out.
//!
//! Never `layout.apply`: herdr rebuilds the whole tree from that call and every
//! pane in it gets a fresh terminal, which takes down any agent running in the
//! workspace. So this reaches for `pane.split` and swaps the new pane into the
//! narrow slot itself.
//!
//! # Why this is still here
//!
//! It is the old way. A forked herdr draws the panel itself, in a sidebar that
//! belongs to no workspace — see [`super::surface`] — and against that herdr a
//! panel split into a pane is a second panel beside the real one, costing a
//! column of somebody's screen for a copy of what is already on it.
//!
//! **It stays while the fork is unproven, and it stays whole.** The fork is one
//! person's build of one terminal: it can be rolled back to an upstream release
//! in a minute, and the wsp that talks to that release is this same binary —
//! stage 1 of the conversion measured it, and nothing in `src/protocol`,
//! `src/api` or the schema differs between the two. So the fallback is real
//! rather than notional, and `--all` stays with it: against un-forked herdr
//! there genuinely is one panel per workspace, and a fallback missing the
//! command that sets it up on a fresh machine is half a fallback.
//!
//! What does *not* stay is both at once. [`install_if_adopted`] refuses while a
//! surface is drawing, because that is the one path nobody chooses — the plugin
//! fires `workspace.created` and a panel appears in a workspace somebody's
//! agent was opened in, dragging the screen with it. Typing `wsp panel install`
//! is a person asking, and gets what they asked for with a word about the
//! surface that is already up.

use std::collections::HashSet;

use serde_json::json;

use crate::herdr;
use crate::store::Store;
use crate::util::shell_quote;

use super::{BOARD_LABEL, FULL_LABEL, PANEL_LABEL, VIEW_LABEL};

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
    /// The tab it is in. A workspace's panes span every tab it has, so this is
    /// what tells the panel's own detail pane from one belonging to a board or
    /// to the fullscreen panel two tabs over.
    pub(super) tab: String,
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
                tab: p.get("tab_id").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            })
        })
        .collect())
}

/// Is this herdr label one of ours — the sidebar itself, or one of the three
/// surfaces it opens? Furniture, in [`super`]'s word for it, and the answer to
/// both "what may this be split off" and "what is there to take back out".
pub(super) fn is_ours(label: &str) -> bool {
    [PANEL_LABEL, VIEW_LABEL, BOARD_LABEL, FULL_LABEL].contains(&label)
}

/// The pane a panel should be split off: the widest one that is not ours.
/// Width comes from herdr's layout, because "first in the list" is an
/// arbitrary answer that happens to be right only when there is one pane.
pub(super) fn widest<'a>(ws_id: &str, panes: &'a [PaneInfo]) -> Option<&'a PaneInfo> {
    // Everything of ours, not just the two that live in this tab: `pane.list`
    // is per *workspace*, so a board or a fullscreen panel open in another tab
    // is in this list too, and splitting the sidebar off one of those would put
    // it in a tab nobody has the sidebar open in.
    let candidates: Vec<&PaneInfo> = panes.iter().filter(|p| !is_ours(&p.label)).collect();
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

    // **This swap steals the screen, and there is no way to ask it not to.**
    // `handle_pane_swap` calls `switch_workspace_tab` unconditionally and
    // `pane.swap` is the one mutation in herdr's API with no `focus` field —
    // `workspace.create`, `pane.split`, `pane.move` and `layout.apply` all have
    // one, defaulting to false. So a workspace opened deliberately in the
    // background is dragged onto the screen 34 ms later by its own panel being
    // installed, which is the whole of why `robustness-069` looked unfixed
    // after `spawn` stopped opting into focus. Measured, with the chain and the
    // line, on `fork-002`; nothing above can be reordered around it, because
    // `SplitDirection` is `right|down` and `pane.move` refuses a move within
    // one tab.
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
///
/// And not at all while a surface is drawing. This is the only path that
/// installs a panel nobody asked for at that moment, so it is the only one
/// where "there is already a panel on this screen" has to win: under a forked
/// herdr with the plugin still linked, every `O` and every `wsp spawn` would
/// otherwise put a second panel into the workspace it just made — and put it
/// there through the swap that steals the screen, so an agent opened in the
/// background arrives in front of you.
///
/// The state file is left exactly as it is. A person who goes back to upstream
/// herdr has their workspaces still listed and their panels still installed;
/// nothing here decides that the fork is permanent.
pub fn install_if_adopted(store: &Store, ws_id: &str) {
    if panels_state(store).is_empty() {
        return;
    }
    if crate::daemon::surface_drawing(&store.state) {
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

    // Said, not refused. `wsp panel install` is somebody typing it, and the
    // fork is one build of one terminal — the reason to run both at once is
    // that you are checking one against the other, which is most of what this
    // fallback is for.
    if crate::daemon::surface_drawing(&store.state) {
        eprintln!("wsp: a surface is already drawing the sidebar — installing a panel beside it");
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

/// Take the panel back out of a workspace, or out of all of them.
///
/// **By label, not by the state file.** This used to walk `panels.json` and
/// close the pane id recorded there, which is the same as trusting a note we
/// wrote to still describe the room: a panel installed by a wsp that crashed
/// before it saved, a workspace herdr restored with the pane in it and no entry
/// for it, or an id that has since been reused, are all panes standing in front
/// of somebody with nothing here able to see them. `render-074` is that: `wsp
/// panel uninstall -w w1` answered `removed from 0 workspace(s)` about a pane
/// that was on the screen, and it took `herdr pane close` to shift.
///
/// So what is closed is what herdr says is there wearing one of our labels, and
/// the state file is only the record that is tidied afterwards.
///
/// And a zero says which zero it is. "There is no panel in this workspace" and
/// "there is no such workspace" were one line and one exit code, which reads as
/// the first while being the second — the reading that sends somebody looking
/// for a pane wsp has already told them it removed.
pub fn uninstall(store: &Store, args: &crate::Args) -> i32 {
    if !herdr::available() {
        eprintln!("wsp: no herdr socket");
        return 1;
    }
    let mut panels = panels_state(store);
    let only = args.get("workspace");

    // Where to look: every workspace herdr has, plus any the state file still
    // names. A workspace that has gone took its panes with it, but its entry is
    // ours to forget, and a run that never visits it never forgets it.
    let mut targets: Vec<String> =
        herdr::workspaces().unwrap_or_default().into_iter().map(|w| w.id).collect();
    for ws in panels.keys() {
        if !targets.contains(ws) {
            targets.push(ws.clone());
        }
    }
    if let Some(want) = &only {
        targets.retain(|ws| ws == want);
        if targets.is_empty() {
            eprintln!(
                "wsp: no workspace `{want}` — herdr does not list it, and nothing is recorded there"
            );
            return 1;
        }
    }

    let mut cleared = 0;
    let mut closed = 0;
    let mut bare: Vec<String> = Vec::new();
    let mut failed: Vec<String> = Vec::new();

    for ws in &targets {
        let panes = match list_panes(ws) {
            Ok(panes) => panes,
            // Said, never swallowed. This is the other silent zero: a
            // workspace we could not look inside is not a workspace with
            // nothing in it.
            Err(e) => {
                failed.push(format!("{ws}: {e}"));
                continue;
            }
        };
        // The recorded id as well as the labels, for the one pane a label
        // cannot find: a rename by hand, or a herdr that lost the label across
        // a restart. It costs one comparison and it closes a pane that would
        // otherwise be nobody's to close.
        let recorded = panels.get(ws).cloned();
        let mut here = 0;
        let mine = |p: &&PaneInfo| is_ours(&p.label) || Some(&p.id) == recorded.as_ref();
        for pane in panes.iter().filter(mine) {
            match herdr::call("pane.close", json!({ "pane_id": pane.id })) {
                Ok(_) => here += 1,
                Err(e) => failed.push(format!("{}: {e}", pane.id)),
            }
        }
        panels.remove(ws);
        closed += here;
        if here > 0 {
            cleared += 1;
        } else {
            bare.push(ws.clone());
        }
    }
    save_panels(store, &panels);

    if args.json() {
        println!(
            "{}",
            json!({ "removed": cleared, "panes": closed, "empty": bare, "failed": failed })
        );
    } else if closed > 0 {
        println!("panel removed from {cleared} workspace(s), {closed} pane(s) closed");
    } else if let Some(want) = &only {
        println!("nothing of ours is open in {want} — no panel to remove");
    } else {
        println!("nothing of ours is open in any of {} workspace(s)", targets.len());
    }
    for f in &failed {
        eprintln!("wsp: {f}");
    }
    i32::from(!failed.is_empty())
}

/// Close the panel panes an older wsp left standing, and say how many.
///
/// The conversion to the fork is what this is for. herdr restores the pane
/// *layout* it had before the restart, so every workspace that had a panel
/// installed comes back with a pane in the sidebar's slot — and nothing refills
/// it, because [`install_if_adopted`] stands down while a surface is drawing,
/// which is correct and is why they are empty. What a person sees on their
/// first fork is the new sidebar working beside a dead pane that should not be
/// there, and reads it as the fork being broken. It is the last thing standing
/// between a conversion and a clean first impression (`render-074`).
///
/// **A husk is a pane of ours with no wsp left in it**, which is the honest
/// test rather than "a pane of ours while a surface is up". `panel install` is
/// deliberately still allowed beside a surface — one build of one terminal is
/// the fallback, and checking one against the other is most of what it is for —
/// so a panel somebody has just asked for must survive this. It does: the panel
/// `exec`s over its pane's shell and `exec`s again over itself on reload, so a
/// live one is always a `wsp` with that pane's id in its environment. See
/// [`crate::daemon::wsp_panes`], which is also where a process list that will
/// not answer stops this dead rather than emptying every workspace.
///
/// This machine only, for the same reason: the evidence is a local `ps`, and a
/// far machine's pane labelled `wsp` is one this cannot see the process for.
///
/// `panels.json` is deliberately left alone. It records that the panel was
/// adopted here, not that these particular panes exist, and somebody who rolls
/// back to an upstream herdr should find their panels install again as they
/// always did — nothing here decides that the fork is permanent.
pub(super) fn sweep_husks(state: &std::path::Path) -> usize {
    let Some(manned) = crate::daemon::wsp_panes(state) else {
        return 0;
    };
    let Ok(workspaces) = herdr::workspaces() else {
        return 0;
    };
    let mut closed = 0;
    for ws in workspaces.iter().filter(|w| herdr::host_of(&w.id).is_none()) {
        let Ok(panes) = list_panes(&ws.id) else { continue };
        for pane in husks(&panes, &manned) {
            if herdr::call("pane.close", json!({ "pane_id": pane })).is_ok() {
                closed += 1;
            }
        }
    }
    closed
}

/// Which of a workspace's panes are husks: ours by label, with nothing of ours
/// running in them.
pub(super) fn husks<'a>(panes: &'a [PaneInfo], manned: &HashSet<String>) -> Vec<&'a str> {
    panes
        .iter()
        .filter(|p| is_ours(&p.label) && !manned.contains(&p.id))
        .map(|p| p.id.as_str())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(id: &str, label: &str) -> PaneInfo {
        PaneInfo { id: id.into(), label: label.into(), tab: "t1".into() }
    }

    fn manned(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    /// The conversion, exactly as it was found: herdr restored the pane and
    /// nothing refilled it, so the label says panel and no process backs it.
    /// That pane is what a person sees beside their new sidebar, and it is the
    /// whole of what the sweep is for.
    #[test]
    fn a_pane_of_ours_with_nothing_running_in_it_is_a_husk() {
        let panes = vec![pane("w1:p1", ""), pane("w1:p6G", PANEL_LABEL)];

        assert_eq!(husks(&panes, &manned(&[])), vec!["w1:p6G"]);
    }

    /// `panel install` beside a surface is somebody checking one against the
    /// other, and it is the fallback the fork is allowed to be rolled back to.
    /// A sweep that closed it would take the panel out from under them on the
    /// next restart, with a message about panes an older wsp left behind.
    #[test]
    fn a_panel_somebody_is_still_running_survives_the_sweep() {
        let panes = vec![pane("w1:p1", ""), pane("w1:p2", PANEL_LABEL)];

        assert!(husks(&panes, &manned(&["w1:p2"])).is_empty());
    }

    /// A pane with nothing running in it is the *normal* state of a pane: a
    /// shell at a prompt, an agent that has finished, a window somebody left
    /// open. Only the label makes one ours, and only a pane of ours may be
    /// closed by anything a person did not type.
    #[test]
    fn an_idle_pane_that_is_not_ours_is_not_a_husk() {
        let panes = vec![pane("w1:p1", ""), pane("w1:p4", "shell")];

        assert!(husks(&panes, &manned(&[])).is_empty());
    }

    /// The three surfaces the panel opens are furniture like the sidebar, and
    /// a conversion leaves them behind the same way. A board two tabs over
    /// with nothing drawing it is a pane nothing will ever reclaim.
    #[test]
    fn the_detail_the_board_and_the_full_tree_are_swept_like_the_sidebar() {
        let panes = vec![
            pane("w1:p2", PANEL_LABEL),
            pane("w1:p3", VIEW_LABEL),
            pane("w1:p7", BOARD_LABEL),
            pane("w1:p9", FULL_LABEL),
        ];

        assert_eq!(husks(&panes, &manned(&["w1:p2"])).len(), 3);
    }
}
