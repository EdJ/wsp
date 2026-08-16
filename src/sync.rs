//! Projection of the durable store into herdr's sidebar tokens.

use std::collections::HashMap;

use serde_json::Value;

use crate::herdr;
use crate::resolve::{self, Index};
use crate::store::Store;
use crate::util;

/// 6h, refreshed by the daemon well inside herdr's 24h ceiling.
pub const TTL_MS: u64 = 6 * 60 * 60 * 1000;

#[derive(Default)]
pub struct Cache {
    sent: HashMap<String, String>,
}

impl Cache {
    pub fn changed(&mut self, key: &str, value: &str) -> bool {
        match self.sent.get(key) {
            Some(prev) if prev == value => false,
            _ => {
                self.sent.insert(key.to_string(), value.to_string());
                true
            }
        }
    }
    pub fn clear(&mut self) {
        self.sent.clear();
    }
}

pub struct Report {
    pub workspaces: usize,
    pub panes: usize,
    pub reaped: usize,
}

/// Which bindings survive a sync: the ones herdr just named, plus every one on
/// a machine that did not answer.
///
/// **Unreachable is not empty**, one layer over `reconcile --reap`, and the same
/// judgement rather than a second one — `answered_by_machine` and `may_reap` are
/// its, and `sync` hands them pane ids instead of workspace ids. A binding is
/// keyed on a pane, so the evidence entitling us to drop one is a pane list from
/// the machine that pane is on. No pane list, no reap; and when panes fan out
/// across machines (t-260816-037) this partitions itself, because a qualified
/// id carries the machine it came from.
///
/// The case that made this urgent has both halves at once: a `wsp daemon`
/// started by a sandbox's herdr, holding an empty herdr and the *live* store
/// (t-260816-076). Every machine says nothing, so nothing is reaped — where
/// before, every binding on the seat went. A herdr restarting says the same
/// thing for a moment, which is why the daemon's own startup `reconcile` has
/// never been allowed to reap either.
fn kept_bindings<'a>(
    live_panes: &[String],
    bound_panes: impl IntoIterator<Item = &'a String>,
    answered: &std::collections::BTreeMap<&str, usize>,
) -> Vec<String> {
    let mut keep: Vec<String> = live_panes.to_vec();
    keep.extend(
        bound_panes
            .into_iter()
            .filter(|p| !crate::cmd_agent::may_reap(answered, p))
            .cloned(),
    );
    keep
}

/// Compute and push tokens. `force` ignores the change cache (used on the
/// periodic TTL refresh).
pub fn sync(store: &Store, cache: &mut Cache, force: bool) -> std::io::Result<Report> {
    if force {
        cache.clear();
    }

    let index = Index::new(store.projects());
    let tasks = store.tasks();
    let counts = resolve::counts_by_project(&index, &tasks);
    let pins = store.pins();
    let bindings = store.bindings();
    let claims = store.claims();

    let workspaces = herdr::workspaces()?;
    let agents = herdr::agents()?;

    // Every pane, not just the ones running an agent. Reaping against
    // `agent.list` meant a binding to a plain shell was destroyed by the very
    // next sync — including the one `claim` runs on its way out, so claiming
    // from a shell silently undid itself.
    //
    // `Err` is not an empty list. It used to be — `unwrap_or_default()` — and
    // an empty list drops every binding in the store, so one `pane.list` that
    // timed out unbound every agent on the seat. Silence is not evidence that
    // the panes went away.
    let reaped = match herdr::panes() {
        Err(_) => 0,
        Ok(panes) => {
            let live: Vec<String> = panes.iter().map(|p| p.pane_id.clone()).collect();
            let answered =
                crate::cmd_agent::answered_by_machine(live.iter().map(|s| s.as_str()));
            let keep = kept_bindings(&live, bindings.keys(), &answered);
            store.reap_bindings(&keep)
        }
    };
    let bindings = if reaped > 0 { store.bindings() } else { bindings };

    // pane -> task
    let task_for_pane = |pane: &str| -> Option<&crate::model::Task> {
        bindings
            .get(pane)
            .and_then(|b: &Value| b.get("task_id"))
            .and_then(|t| t.as_str())
            .and_then(|id| tasks.iter().find(|t| t.id == id))
    };

    let mut ws_count = 0;
    for ws in &workspaces {
        // A workspace's cwd comes from any agent living in it.
        let cwd = agents
            .iter()
            .find(|a| a.workspace_id == ws.id && !a.cwd.is_empty())
            .map(|a| a.cwd.clone());

        // A binding on any pane in this workspace also identifies the project.
        let bound_project = agents
            .iter()
            .filter(|a| a.workspace_id == ws.id)
            .find_map(|a| task_for_pane(&a.pane_id).and_then(|t| t.project.clone()));

        let r = resolve::resolve(
            &index,
            &pins,
            resolve::Held {
                binding: bound_project,
                claim: resolve::claimed_project(&claims, &tasks, Some(&ws.id), Some(&ws.label)),
            },
            Some(&ws.id),
            Some(&ws.label),
            cwd.as_deref(),
        );

        let (proj, tags, c) = match &r.project {
            Some(id) => {
                // Nearest tags first, capped by count rather than characters —
                // a mid-word ellipsis reads as noise in a narrow sidebar.
                let tags = index.effective_tags(id);
                let shown: Vec<String> = tags.iter().take(3).cloned().collect();
                (
                    Some(id.clone()),
                    Some(shown.join("·")).filter(|s| !s.is_empty()),
                    counts.get(id).copied().unwrap_or_default(),
                )
            }
            None => (None, None, Default::default()),
        };

        let tokens: Vec<(&str, Option<String>)> = vec![
            ("proj", proj.clone()),
            ("tags", tags),
            ("todo", nonzero(c.open)),
            ("doing", nonzero(c.doing)),
            ("blocked", nonzero(c.blocked)),
            ("review", nonzero(c.review)),
        ];

        let fingerprint = tokens
            .iter()
            .map(|(k, v)| format!("{k}={}", v.clone().unwrap_or_default()))
            .collect::<Vec<_>>()
            .join(",");
        if force || cache.changed(&format!("ws:{}", ws.id), &fingerprint) {
            let _ = herdr::report_workspace_tokens(&ws.id, &tokens, TTL_MS);
            ws_count += 1;
        }
    }

    let mut pane_count = 0;
    for a in &agents {
        let t = task_for_pane(&a.pane_id);
        let tokens: Vec<(&str, Option<String>)> = vec![
            ("task", t.map(|t| util::truncate(&t.title, 44))),
            ("taskid", t.map(|t| t.id.clone())),
            // `render/109`: which piece of work this is, in the ten columns a
            // narrow sidebar has for it, and what you would type to open it.
            // The pane's label leads with the same thing — this is for a row
            // that would rather carry it on its own.
            ("scope", t.map(crate::cmd_agent::task_scope)),
            ("tstatus", t.map(|t| t.status().as_str().to_string())),
        ];
        let fingerprint = tokens
            .iter()
            .map(|(k, v)| format!("{k}={}", v.clone().unwrap_or_default()))
            .collect::<Vec<_>>()
            .join(",");
        if force || cache.changed(&format!("pane:{}", a.pane_id), &fingerprint) {
            let _ = herdr::report_pane_tokens(&a.pane_id, &tokens, TTL_MS);
            pane_count += 1;
        }
    }

    Ok(Report { workspaces: ws_count, panes: pane_count, reaped })
}

fn nonzero(n: usize) -> Option<String> {
    if n == 0 {
        None
    } else {
        Some(n.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bound(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| (*s).to_string()).collect()
    }

    /// The line this task was filed about: an empty pane list dropped **every**
    /// binding in the store, and there are two ordinary ways to get one — herdr
    /// answering with nothing while it restores a session, and a wsp pointed at
    /// an empty herdr while holding the live store.
    ///
    /// Asserted as "what survives" rather than "what is reaped", because the
    /// failure was that the survivors were nobody.
    #[test]
    fn a_herdr_that_named_no_panes_reaps_no_bindings() {
        let bindings = bound(&["w0:p1", "w1:p3", "w2:p7"]);
        let answered = crate::cmd_agent::answered_by_machine(std::iter::empty());
        let keep = kept_bindings(&[], bindings.iter(), &answered);
        for b in &bindings {
            assert!(keep.contains(b), "{b} was unbound by a herdr that said nothing");
        }
    }

    /// …and when it does answer, a binding whose pane is gone still goes. The
    /// guard has to be the difference between silence and an answer, not a
    /// blanket refusal — otherwise a pane that exited keeps its binding for
    /// ever and the sidebar goes on naming a task nobody is holding.
    #[test]
    fn a_pane_that_is_gone_from_a_machine_that_answered_still_goes() {
        let bindings = bound(&["w0:p1", "w1:p3"]);
        let live = bound(&["w0:p1", "w0:p2"]);
        let answered = crate::cmd_agent::answered_by_machine(live.iter().map(|s| s.as_str()));
        let keep = kept_bindings(&live, bindings.iter(), &answered);

        assert!(keep.contains(&"w0:p1".to_string()), "a live pane lost its binding");
        assert!(!keep.contains(&"w1:p3".to_string()), "a pane that is gone kept its binding");
    }

    /// One machine's silence must not reap another machine's bindings. This is
    /// the executor case, and it is why the judgement is shared with
    /// `reconcile` rather than written a second time here: the seat answering
    /// says nothing whatever about mb2.
    #[test]
    fn silence_from_one_machine_does_not_unbind_another() {
        let bindings = bound(&["w0:p1", "w0:p9", "w0:p1@mb2"]);
        let live = bound(&["w0:p1"]);
        let answered = crate::cmd_agent::answered_by_machine(live.iter().map(|s| s.as_str()));
        let keep = kept_bindings(&live, bindings.iter(), &answered);

        assert!(keep.contains(&"w0:p1".to_string()));
        assert!(!keep.contains(&"w0:p9".to_string()), "the seat answered, so its dead pane goes");
        assert!(
            keep.contains(&"w0:p1@mb2".to_string()),
            "mb2 said nothing and its agent was unbound anyway"
        );
    }
}
