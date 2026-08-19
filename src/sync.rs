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

/// Which pane keys survive a sync: the ones herdr just named, plus every one on
/// a machine that did not answer.
///
/// Written for the bindings and asked for `said.json` too, because the question
/// is about the key rather than the record — anything filed under a pane id is
/// entitled to survive on the same evidence, and two rules for that would be
/// two chances to reap on silence.
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

/// The census, as rows that can be offered back: one per running agent, with
/// what it would take to bring that agent back.
///
/// **Only panes with an agent and a session.** A shell is not an agent and has
/// no thread to resume; an agent herdr has not yet reported a session for is
/// one wsp cannot bring back, and a row promising otherwise would be a row
/// that fails when it is taken. Both are omitted rather than drawn dim,
/// because this list is a set of *offers*.
///
/// What each row carries is what `wsp resume` needs and nothing else: where it
/// was, what it was on, and how to say so to a person. It is a projection of
/// live state rather than a record — the file is overwritten whole on every
/// tick — so nothing here is durable and nothing else may read it as if it
/// were.
fn roster(
    panes: &[herdr::Pane],
    agents: &[herdr::Pane],
    bindings: &std::collections::BTreeMap<String, Value>,
    governors: &std::collections::BTreeMap<String, Value>,
) -> Vec<Value> {
    panes
        .iter()
        .filter(|p| !p.agent.is_empty() && !p.session_id.is_empty())
        .map(|p| {
            let cwd = match p.cwd.is_empty() {
                true => agents
                    .iter()
                    .find(|a| a.pane_id == p.pane_id && !a.cwd.is_empty())
                    .map(|a| a.cwd.as_str())
                    .unwrap_or_default(),
                false => p.cwd.as_str(),
            };
            serde_json::json!({
                "pane": p.pane_id,
                "workspace": p.workspace_id,
                "session": p.session_id,
                "cwd": cwd,
                "kind": p.agent,
                "label": p.label,
                "task": bindings
                    .get(&p.pane_id)
                    .and_then(|b| b.get("task_id"))
                    .and_then(Value::as_str),
                "seat": crate::cmd_govern::governs(governors, &p.workspace_id),
            })
        })
        .collect()
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
    let governors = store.governors();

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
            let dropped = store.reap_bindings(&keep);
            // The long names go with them, on the same evidence: `said.json` is
            // keyed on a pane exactly as the bindings are, so an unreachable
            // machine must not empty it either. Not counted in `reaped`, which
            // is a count of claims-to-panes and is read as one.
            let said = store.said();
            store.reap_said(&kept_bindings(&live, said.keys(), &answered));
            // The one reading of herdr that happens on every tick, and it
            // already carries `agent_session` on the rows that have one. A
            // binding cannot record the session at the moment it is written —
            // `spawn` claims before it starts the agent — so this is what makes
            // the field true rather than merely present. After the reap, so a
            // binding about to be dropped is not written to first.
            crate::cmd_agent::learn_sessions(
                store,
                panes.iter().map(|p| (p.pane_id.as_str(), p.session_id.as_str())),
            );
            // And the same reading again for the seats, off the same rows.
            // A custodian holds no claim, so nothing above this line records
            // its session anywhere — see `cmd_govern::learn_seats`, which is
            // the whole of `render-061`'s writer and costs one pass over a
            // list already in hand.
            crate::cmd_govern::learn_seats(
                store,
                panes.iter().map(|p| {
                    let cwd = match p.cwd.is_empty() {
                        // `pane.list` does not always carry a cwd; `agent.list`
                        // does, and it was already read above for the sidebar.
                        true => agents
                            .iter()
                            .find(|a| a.pane_id == p.pane_id && !a.cwd.is_empty())
                            .map(|a| a.cwd.as_str())
                            .unwrap_or_default(),
                        false => p.cwd.as_str(),
                    };
                    (p.workspace_id.as_str(), p.session_id.as_str(), cwd)
                }),
            );
            // And the census itself, kept so a restart can offer back what a
            // person was actually looking at. `render-061`: written here
            // because this is the same reading the agents section of the panel
            // is drawn from, on the same tick — so what the roster holds is
            // what was last on screen, and not a walk of anything older.
            store.set_roster(roster(&panes, &agents, &bindings, &governors));
            dropped
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

        // The position, where there is one. A workspace holding a custodial slot
        // is answerable for a project rather than working in it, and every
        // other token here describes work — measured on the live seat, the
        // sidebar said `scope=robustness/078` and nothing at all about the two
        // governorships that window was actually holding.
        let seat = crate::cmd_govern::governs(&governors, &ws.id);

        let tokens: Vec<(&str, Option<String>)> = vec![
            ("proj", proj.clone()),
            ("seat", seat.clone()),
            ("tags", tags),
            ("todo", nonzero(c.open)),
            ("doing", nonzero(c.doing)),
            ("blocked", nonzero(c.blocked)),
            ("parked", nonzero(c.parked)),
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

    // What is standing, for the one token here that is not about which work
    // this is. Read once for the whole sweep, off the derivation the daemon's
    // attention pass already made — see `attention::standing`, which is where
    // the argument for this token lives.
    let waiting = crate::attention::standing(store);

    let mut pane_count = 0;
    for a in &agents {
        let t = task_for_pane(&a.pane_id);
        // A custodian holds no task, so every token below was absent and the
        // sidebar had nothing whatever to draw for the one agent answerable for
        // the whole project — measured 2026-08-17, after the seat released the
        // task it had borrowed. What it is doing is the position, so that is
        // what it reports, and `scope` falls back to it for the same reason: the
        // ten columns a narrow sidebar keeps for "which piece of work is this"
        // are better spent on `wsp` than on nothing.
        let seat = crate::cmd_govern::governs(&governors, &a.workspace_id);
        let tokens: Vec<(&str, Option<String>)> = vec![
            ("task", t.map(|t| util::truncate(&t.title, 44))),
            ("taskid", t.map(|t| t.id.clone())),
            ("seat", seat.clone()),
            // `render-109`: which piece of work this is, in the ten columns a
            // narrow sidebar has for it, and what you would type to open it.
            // The pane's label leads with the same thing — this is for a row
            // that would rather carry it on its own.
            ("scope", t.map(crate::cmd_agent::task_scope).or_else(|| seat.clone())),
            ("tstatus", t.map(|t| t.status().as_str().to_string())),
            // **The one token that says somebody is waiting**, and the answer
            // to Ed's *"we don't see a flag or any similar notification in the
            // UI"*: every other token above describes which piece of work this
            // is, and an agent stopped with a question drew identically to one
            // mid-turn. The value is the signal's own word — `unanswered`,
            // `needs-a-person`, `review` — so the sidebar and `wsp watch` say
            // the same thing and a person learning one has learned both.
            //
            // Absent when nothing is standing, which is the common case and
            // costs the row nothing: `report_pane_tokens` is given `None` and
            // the sidebar has no column to keep.
            ("needs", t.and_then(|t| waiting.get(&t.id)).map(|w| (*w).to_string())),
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
