//! How a claim reads, and who else holds a row (wsp-438): the claim and its
//! ended trace as one line ([`claim_line`], [`worked_line`], [`claim_where`]),
//! the tier clause a claim line carries ([`spawned_at`]), and the one rule for
//! "somebody else has this" ([`live_holders`]).
//!
//! Pure over the records, and moved down out of `cmd_agent` so the row and list
//! verbs (`cmd_task`, `cmd_worklist`, `leg_seat`) read them without reaching up
//! into the seats. `cmd_agent` re-exports each, so its paths still name them.

use crate::util;

/// One pane row, as the views in this file read one. Nothing fills it live
/// (`wsp-120`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Pane {
    pub pane_id: String,
    pub label: String,
    pub workspace_id: String,
    pub agent: String,
    pub agent_status: String,
    pub cwd: String,
    pub title: String,
    pub focused: bool,
}

/// Every pane holding `task` that is not `me`, and that a live agent is
/// driving.
///
/// The one definition of "somebody else has this". `claim` refuses on it and
/// `next` declines to name it, and the two have to be the same rule: when they
/// disagreed, `next` named work `claim` would refuse, so an agent told to find
/// its own work was sent straight into a wall with nothing to try instead.
///
/// A *dead* pane's binding is not a holder — that is exactly the stale state a
/// re-claim exists to clear. Nor is a shell's: a person at a terminal can be
/// asked to move, and refusing on their behalf helps nobody.
pub fn live_holders<'a>(
    bindings: &std::collections::BTreeMap<String, serde_json::Value>,
    panes_now: &'a [Pane],
    task: &str,
    me: Option<&str>,
) -> Vec<&'a Pane> {
    panes_now
        .iter()
        .filter(|p| !p.agent.is_empty())
        .filter(|p| Some(p.pane_id.as_str()) != me)
        .filter(|p| {
            bindings
                .get(&p.pane_id)
                .and_then(|b| b.get("task_id"))
                .and_then(|x| x.as_str())
                == Some(task)
        })
        .collect()
}

/// `Trance Video · 3h12m` — a claim as one line.
///
/// Both this and `worked_line` join what they have and skip what they do not,
/// because every part is optional: a claim made outside a seat has no label, and
/// one made before the clock was recorded has no duration. Formatting them with
/// fixed separators left `" · 3s · to prio1-001"` hanging off nothing.
pub fn claim_line(c: &serde_json::Value) -> String {
    let get = |k: &str| c.get(k).and_then(|x| x.as_str()).unwrap_or("");
    let mut parts: Vec<String> = Vec::new();
    match (get("workspace_label"), get("workspace_id")) {
        ("", "") => {}
        ("", id) => parts.push(id.to_string()),
        (label, "") => parts.push(label.to_string()),
        (label, id) => parts.push(format!("{label} ({id})")),
    }
    let held = util::since(get("claimed_at"));
    if held > 0 {
        parts.push(util::duration_human(held));
    }
    parts.join(" · ")
}

/// `Trance Video · 3h12m · to render-004` — the claim that ended.
pub fn worked_line(w: &serde_json::Value) -> String {
    let get = |k: &str| w.get(k).and_then(|x| x.as_str()).unwrap_or("");
    let mut parts: Vec<String> = Vec::new();
    match (get("workspace_label"), get("workspace_id")) {
        ("", id) if !id.is_empty() => parts.push(id.to_string()),
        (label, _) if !label.is_empty() => parts.push(label.to_string()),
        _ => {}
    }
    let spent = w.get("seconds").and_then(|x| x.as_i64()).unwrap_or(0);
    if spent > 0 {
        parts.push(util::duration_human(spent));
    }
    match get("handed_to") {
        "" => parts.push(get("reason").to_string()),
        next => parts.push(format!("to {next}")),
    }
    parts.join(" · ")
}

/// Where a claim says the work is, for one line of output.
///
/// The id rather than the label, which is the opposite of what a list wants.
/// A workspace holding a task is *named after* it — `002 · the probe task
/// claimed onto anot…` — so the label restates the id already in the sentence
/// and arrives elided, with the ellipsis landing mid-clause. The id is the part
/// the reader cannot reconstruct: it says which screen to go and look at.
pub fn claim_where(claim: &serde_json::Value) -> String {
    let get = |k: &str| claim.get(k).and_then(|v| v.as_str()).unwrap_or("");
    let (label, id) = (get("workspace_label"), get("workspace_id"));
    match (id.is_empty(), label.is_empty()) {
        (false, _) => id.to_string(),
        (true, false) => label.to_string(),
        (true, true) => "somewhere it did not record".to_string(),
    }
}

/// The tier a claim was made at, as a clause on the line that records the
/// claim — and nothing at all when none was stated.
///
/// **A clause and not a line of its own**, which is the whole reason it is
/// shaped like this. `wsp brief` hands an arriving agent the last four log
/// lines and nothing more; a record that spent one of those four on the tier
/// would push whatever direction was written on the task out of the brief, on
/// every spawn, for ever. The claim line was already the one that says an
/// attempt started, so this is the same sentence finishing itself.
///
/// **`default` is a word here and an absence in the flag**, and the asymmetry
/// is deliberate. `--effort high` with no `--model` is a real thing to type,
/// so the pair has to be printable with one half missing; `default/high` says
/// *whatever the settings file says, at high effort*, which is precisely what
/// was asked for and reads back unambiguously. What it does **not** do is
/// name the model — because at the moment of the claim wsp does not know it,
/// and the record that does is [`crate::agent_kind::Ran`], read off the
/// transcript when the claim ends.
///
/// Empty for a spawn that stated neither, so the ordinary claim line is
/// byte-for-byte what it was — the same compatibility rule `--model` itself
/// keeps, and the reason nothing in the store needs migrating.
pub fn spawned_at(model: Option<&str>, effort: Option<&str>) -> String {
    match (model, effort) {
        (None, None) => String::new(),
        (m, e) => format!(
            " · spawned at {}/{}",
            m.unwrap_or("default"),
            e.unwrap_or("default")
        ),
    }
}
