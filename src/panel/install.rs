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
//!
//! # What is written down, and what is swept
//!
//! [`Panels`] is the file, and it holds two facts with opposite lifetimes: that
//! the panel is adopted on this machine, which nothing but a person withdraws,
//! and which pane holds it in each workspace, which the workspace closing ends.
//! [`Held`] is the second one, and it carries herdr's `terminal_id` beside the
//! pane id because a pane id that resolves is not evidence that our pane is
//! still behind it. [`reap_panels`] sweeps the perishable half, from
//! `reconcile --reap`. All three doc comments are worth reading before touching
//! [`install_one`]; the argument underneath them is rule 1 of
//! [`crate::arrange`].

use std::collections::HashSet;

use serde_json::json;

use crate::store::Store;

use super::{BOARD_LABEL, FULL_LABEL, PANEL_LABEL, VIEW_LABEL};

pub(super) fn panel_command() -> Vec<String> {
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "wsp".into());
    vec![exe, "panel".into()]
}

/// What wsp put in one workspace: the pane, and what the runtime said was
/// behind that pane at the moment it was recorded.
///
/// The witness is `robustness-089`'s, one file over from where it was argued.
/// [`crate::arrange::Held`] is the same record in the port that has no
/// implementor yet; this is the one that is load-bearing today, and rule 1 in
/// `src/arrange.rs` is the argument for both — a handle that still resolves is
/// not evidence that what wsp bound is still behind it. herdr hands out the
/// witness for free as `terminal_id`, on every `pane.list` row.
pub(super) struct Held {
    pub(super) pane: String,
    /// herdr's `terminal_id`. Empty for a record written before this field
    /// existed, and for a herdr that does not send one.
    pub(super) witness: String,
}

impl Held {
    /// Whether this live pane is the pane the panel was installed into.
    ///
    /// **Disagreement is the test, not agreement**, exactly as in
    /// [`crate::arrange::Held::corroborated_by`] and for the same reason: two
    /// witnesses that differ are proof the pane changed under us, and a witness
    /// missing on either side is proof of nothing. Every entry in every file on
    /// disk today predates the field, so a rule that demanded agreement would
    /// install a second panel into every workspace that already had one, the
    /// first time a new wsp ran.
    pub(super) fn corroborated_by(&self, live: &PaneInfo) -> bool {
        self.witness.is_empty() || live.terminal.is_empty() || self.witness == live.terminal
    }
}

/// What `panels.json` holds — and it holds two facts with different lifetimes.
///
/// **Adoption is durable; the pane holding a panel is perishable.** `adopted`
/// is that somebody has installed a panel on this machine, which is the only
/// question [`install_if_adopted`] asks and stays true until they take them all
/// back out. `held` is which pane is holding the panel in each workspace, which
/// stops being true the moment that workspace closes.
///
/// It used to be one fact: a flat `{workspace: pane}` map, with adoption
/// spelled as *the map is not empty*. That reading is what made the map
/// unsweepable, and it went unswept — 147 entries on this machine, not one of
/// them a workspace herdr still listed, because forgetting them would have
/// silently turned the plugin's auto-install off (`robustness-090`). Two facts
/// written down apart, and [`reap_panels`] sweeps exactly one of them.
///
/// A wsp rolled back to before this reads the new shape as no entries at all —
/// `adopted` is a bool and `panels` an object, and the old reader kept only
/// string values. It costs the auto-install until the next `panel install`, and
/// not a duplicate panel: [`install_one`]'s orphan branch adopts the pane
/// already wearing our label rather than splitting a second one beside it.
#[derive(Default)]
pub(super) struct Panels {
    pub(super) adopted: bool,
    pub(super) held: std::collections::BTreeMap<String, Held>,
}

impl Panels {
    /// Record a pane as this workspace's panel, with whatever is behind it now.
    /// Installing anything at all is also the adoption.
    pub(super) fn remember(&mut self, ws_id: &str, pane: &PaneInfo) {
        self.adopted = true;
        self.held.insert(
            ws_id.to_string(),
            Held { pane: pane.id.clone(), witness: pane.terminal.clone() },
        );
    }
}

pub(super) fn panels_state(store: &Store) -> Panels {
    read_panels(
        &std::fs::read_to_string(store.state.join("panels.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .unwrap_or(serde_json::Value::Null),
    )
}

/// Both shapes, because every file on disk is still the old one and none of
/// them is ours to rewrite before something installs. A flat map is read the
/// way it was always read: the entries, no witnesses, and adopted iff there is
/// anything in it — which is what that map meant.
pub(super) fn read_panels(v: &serde_json::Value) -> Panels {
    let Some(obj) = v.as_object() else { return Panels::default() };
    let Some(held) = obj.get("panels").and_then(|p| p.as_object()) else {
        let held: std::collections::BTreeMap<String, Held> = obj
            .iter()
            .filter_map(|(k, v)| {
                Some((k.clone(), Held { pane: v.as_str()?.to_string(), witness: String::new() }))
            })
            .collect();
        return Panels { adopted: !held.is_empty(), held };
    };
    Panels {
        adopted: obj.get("adopted").and_then(|a| a.as_bool()).unwrap_or(false),
        held: held
            .iter()
            .filter_map(|(k, v)| {
                Some((
                    k.clone(),
                    Held {
                        pane: v.get("pane")?.as_str()?.to_string(),
                        witness: v
                            .get("witness")
                            .and_then(|w| w.as_str())
                            .unwrap_or_default()
                            .to_string(),
                    },
                ))
            })
            .collect(),
    }
}

/// Split from the write so the file's shape is a test rather than a claim.
pub(super) fn write_panels(p: &Panels) -> serde_json::Value {
    let held: serde_json::Map<String, serde_json::Value> = p
        .held
        .iter()
        .map(|(ws, h)| (ws.clone(), json!({ "pane": h.pane, "witness": h.witness })))
        .collect();
    json!({ "adopted": p.adopted, "panels": serde_json::Value::Object(held) })
}

pub(super) fn save_panels(store: &Store, panels: &Panels) {
    let _ = std::fs::create_dir_all(&store.state);
    let _ = crate::store::write_atomic(
        &store.state.join("panels.json"),
        &serde_json::to_string_pretty(&write_panels(panels)).unwrap_or_default(),
    );
}

/// Forget the panel records whose workspace herdr no longer has.
///
/// **The perishable half only.** [`Panels::adopted`] survives this, which is
/// the whole reason it is a field: on the machine this was measured on the
/// sweep takes the file from 147 entries to none, and a person who has had a
/// panel installed for a month should not find the plugin quietly stopped.
///
/// **Why `reconcile --reap` and not `sync`** (decided on `robustness-090`).
/// Two reasons, and they point the same way. The key is a workspace id, so this
/// is the judgement `reconcile --reap` already makes about claims and seats,
/// with the same `may_reap` guard on the same evidence — `sync` reaps on pane
/// ids and would need a second rule for workspaces, and two rules for *is this
/// machine answering* is how they drift apart. And `sync` runs on every daemon
/// tick, including the tick one second after herdr came back, when a session
/// still being restored looks exactly like a mass closure. Destroying a record
/// is asked for here, never automatic; robustness-015 is what the other way cost.
pub(crate) fn reap_panels(
    store: &Store,
    workspaces: &[String],
    answered: &std::collections::BTreeMap<&str, usize>,
) -> usize {
    let mut panels = panels_state(store);
    let gone = forgettable(panels.held.keys(), workspaces, answered);
    if gone.is_empty() {
        return 0;
    }
    for ws in &gone {
        panels.held.remove(ws);
    }
    save_panels(store, &panels);
    gone.len()
}

/// Which recorded workspaces may be forgotten: the ones a machine that has been
/// heard from no longer lists.
///
/// [`crate::cmd_agent::may_reap`] and not a test of its own. An unreachable
/// machine reports no workspaces, which reads exactly like a machine with
/// nothing open on it, and a sweep that cannot tell them apart empties the
/// record on the first network blip.
pub(super) fn forgettable<'a>(
    recorded: impl IntoIterator<Item = &'a String>,
    live: &[String],
    answered: &std::collections::BTreeMap<&str, usize>,
) -> Vec<String> {
    recorded
        .into_iter()
        .filter(|ws| crate::cmd_agent::may_reap(answered, ws))
        .filter(|ws| !live.iter().any(|l| l == *ws))
        .cloned()
        .collect()
}

pub(super) struct PaneInfo {
    pub(super) id: String,
    pub(super) label: String,
    /// The tab it is in. A workspace's panes span every tab it has, so this is
    /// what tells the panel's own detail pane from one belonging to a board or
    /// to the fullscreen panel two tabs over.
    pub(super) tab: String,
    /// What is behind it: herdr's `terminal_id`, minted per pane and never
    /// handed out twice, including across the restart that hands the *pane* id
    /// out again. [`Held::corroborated_by`] is the only reader.
    pub(super) terminal: String,
}

pub(super) fn list_panes(_ws_id: &str) -> Result<Vec<PaneInfo>, String> {
    Err("no herdr socket".to_string())
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
pub(super) fn widest<'a>(_ws_id: &str, panes: &'a [PaneInfo]) -> Option<&'a PaneInfo> {
    // Everything of ours, not just the two that live in this tab: `pane.list`
    // is per *workspace*, so a board or a fullscreen panel open in another tab
    // is in this list too, and splitting the sidebar off one of those would put
    // it in a tab nobody has the sidebar open in.
    let candidates: Vec<&PaneInfo> = panes.iter().filter(|p| !is_ours(&p.label)).collect();
    // No layout to measure: no host runs herdr, so every width reads as 0 and
    // the last candidate wins.
    candidates.into_iter().last()
}

/// `pane.split` starts the user's shell and takes no command, so the panel is
/// `exec`d over it — which also means quitting the panel closes the pane
/// instead of leaving a bare prompt behind.
pub(super) fn launch_panel(_pane: &str) {}

/// Install into one workspace. Returns Ok(true) when a panel was added,
/// Ok(false) when the workspace already had one.
///
/// This used to build a whole layout tree and hand it to `layout.apply`, with
/// the agent's pane re-referenced by id on the assumption that herdr would
/// carry it across. It does not: the tree is rebuilt from scratch and every
/// pane in it gets a fresh terminal, which killed every running agent. So:
/// `pane.split`, which adds one pane beside a target and touches nothing else.
pub fn install_one(_store: &Store, ws_id: &str, _ratio: f64) -> Result<bool, String> {
    list_panes(ws_id).map(|_| false)
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
    if !panels_state(store).adopted {
        return;
    }
    if crate::daemon::surface_drawing(&store.state) {
        return;
    }
    let _ = install_one(store, ws_id, 0.22);
}

pub fn install(_store: &Store, _args: &crate::Args) -> i32 {
    eprintln!("wsp: no herdr socket");
    1
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
pub fn uninstall(_store: &Store, _args: &crate::Args) -> i32 {
    eprintln!("wsp: no herdr socket");
    1
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
pub(super) fn sweep_husks(_state: &std::path::Path) -> usize {
    0
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
        behind(pane_of(id, label), "")
    }

    /// A pane with something herdr can name behind it, which is every pane a
    /// live herdr reports. The bare [`pane`] above is the other case — a herdr
    /// with no `terminal_id` to give — and is what the husk tests want.
    fn pane_of(id: &str, label: &str) -> PaneInfo {
        PaneInfo {
            id: id.into(),
            label: label.into(),
            tab: "t1".into(),
            terminal: format!("term_{id}"),
        }
    }

    fn behind(mut p: PaneInfo, terminal: &str) -> PaneInfo {
        p.terminal = terminal.into();
        p
    }

    fn held(pane: &str, witness: &str) -> Held {
        Held { pane: pane.into(), witness: witness.into() }
    }

    /// What [`crate::cmd_agent::answered_by_machine`] makes of a list of live
    /// ids: every one of these is on this seat, so one entry keyed `""`.
    fn answered(ids: &[&str]) -> std::collections::BTreeMap<&'static str, usize> {
        let mut m = std::collections::BTreeMap::new();
        for _ in ids {
            *m.entry("").or_insert(0) += 1;
        }
        m
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

    /// The defect, in the shape it was found in. `w10` was closed long ago,
    /// herdr handed the id out again, and the new one has been split once — so
    /// `w10:p2` resolves and belongs to somebody else. Answering "already has a
    /// panel" to that is what left those workspaces without one.
    #[test]
    fn a_recorded_pane_that_something_else_is_behind_is_not_our_panel() {
        let record = held("w10:p2", "term_before_the_restart");

        assert!(!record.corroborated_by(&pane_of("w10:p2", "claude")));
    }

    /// Same workspace, same pane, still ours. The witness must not cost an
    /// install every time somebody runs one.
    #[test]
    fn the_pane_we_installed_into_is_still_the_pane_we_installed_into() {
        let live = pane_of("w1:p2", PANEL_LABEL);

        assert!(held("w1:p2", &live.terminal).corroborated_by(&live));
    }

    /// Every entry on disk when this was written had no witness, and a herdr
    /// that sends no `terminal_id` writes none either. Absence is not
    /// disagreement in both directions — a rule that demanded agreement would
    /// split a second panel into every workspace that already had one, the
    /// first time a new wsp ran.
    #[test]
    fn a_record_with_no_witness_is_believed_exactly_as_before() {
        assert!(held("w1:p2", "").corroborated_by(&pane_of("w1:p2", PANEL_LABEL)));
        assert!(held("w1:p2", "term_x").corroborated_by(&behind(pane_of("w1:p2", ""), "")));
    }

    /// The file every machine has today, read by the wsp that comes after. The
    /// entries survive and so does the adoption, because a flat map with
    /// anything in it is what adoption used to be spelled as.
    #[test]
    fn the_flat_file_on_every_machine_today_still_reads_as_adopted() {
        let old = serde_json::json!({ "w0": "w0:p3P", "w10": "w10:p2" });

        let p = read_panels(&old);

        assert!(p.adopted);
        assert_eq!(p.held.len(), 2);
        assert_eq!(p.held["w10"].pane, "w10:p2");
        assert_eq!(p.held["w10"].witness, "", "an older wsp had no witness to write");
    }

    /// And the new shape round-trips, witness included.
    #[test]
    fn a_pane_and_what_was_behind_it_survive_being_written_down() {
        let mut p = Panels::default();
        p.remember("w1", &pane_of("w1:p2", PANEL_LABEL));

        let back = read_panels(&write_panels(&p));

        assert!(back.adopted, "installing anything at all is the adoption");
        assert_eq!(back.held["w1"].pane, "w1:p2");
        assert_eq!(back.held["w1"].witness, "term_w1:p2");
    }

    /// The whole reason adoption is a field of its own. On the machine this was
    /// measured on the sweep empties the map, and somebody who has had a panel
    /// installed for a month must not find the plugin quietly stopped.
    #[test]
    fn a_file_swept_down_to_nothing_is_still_an_adopted_one() {
        let swept = Panels { adopted: true, held: Default::default() };

        assert!(read_panels(&write_panels(&swept)).adopted);
    }

    /// The rot: a record for a workspace herdr no longer lists, on a machine
    /// that answered. Nothing swept these, and the file reached 147 entries
    /// against six live workspaces.
    #[test]
    fn a_record_for_a_workspace_that_is_gone_is_forgotten() {
        let recorded = vec!["w1".to_string(), "w10".to_string()];
        let live = vec!["w1".to_string()];

        let gone = forgettable(recorded.iter(), &live, &answered(&["w1"]));

        assert_eq!(gone, vec!["w10".to_string()]);
    }

    /// Unreachable is not empty, one file over. A machine that said nothing has
    /// not said its workspaces are gone, and a sweep that cannot tell those
    /// apart empties the record on the first network blip — robustness-015 is
    /// what that cost the bindings.
    #[test]
    fn a_machine_that_said_nothing_keeps_every_record_on_it() {
        let recorded = vec!["w1".to_string(), "w10".to_string()];

        let gone = forgettable(recorded.iter(), &[], &answered(&[]));

        assert!(gone.is_empty(), "no workspace list is not a workspace list of none");
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
