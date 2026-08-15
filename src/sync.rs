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
    let live_panes: Vec<String> = herdr::panes()
        .unwrap_or_default()
        .iter()
        .map(|p| p.pane_id.clone())
        .collect();
    let reaped = store.reap_bindings(&live_panes);
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
