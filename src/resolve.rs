//! Project resolution and tag inheritance.
//!
//! Five workspaces share `~/git/Easter`, so cwd alone can never identify a
//! project. The precedence chain below is the fix.

use std::collections::{BTreeMap, HashSet};

use crate::model::{Project, Status, Task};
use crate::util;

pub struct Index {
    pub projects: Vec<Project>,
}

impl Index {
    pub fn new(projects: Vec<Project>) -> Index {
        Index { projects }
    }

    pub fn get(&self, id: &str) -> Option<&Project> {
        self.projects.iter().find(|p| p.id == id)
    }

    /// Accepts an exact id, a case-insensitive name, or a unique prefix.
    pub fn find(&self, needle: &str) -> Option<&Project> {
        let n = needle.trim().to_ascii_lowercase();
        if n.is_empty() {
            return None;
        }
        if let Some(p) = self.projects.iter().find(|p| p.id.to_ascii_lowercase() == n) {
            return Some(p);
        }
        if let Some(p) = self.projects.iter().find(|p| p.name.to_ascii_lowercase() == n) {
            return Some(p);
        }
        let matches: Vec<&Project> = self
            .projects
            .iter()
            .filter(|p| p.id.to_ascii_lowercase().starts_with(&n))
            .collect();
        if matches.len() == 1 {
            return Some(matches[0]);
        }
        None
    }

    /// Own tags plus every ancestor's, nearest first, de-duplicated.
    pub fn effective_tags(&self, id: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut guard: HashSet<String> = HashSet::new();
        let mut cur = Some(id.to_string());
        while let Some(cid) = cur {
            if !guard.insert(cid.clone()) {
                break; // parent cycle; doctor reports it
            }
            let Some(p) = self.get(&cid) else { break };
            for t in &p.tags {
                if seen.insert(t.clone()) {
                    out.push(t.clone());
                }
            }
            cur = p.parent.clone();
        }
        out
    }

    pub fn ancestors(&self, id: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut guard: HashSet<String> = HashSet::new();
        let mut cur = self.get(id).and_then(|p| p.parent.clone());
        while let Some(c) = cur {
            if !guard.insert(c.clone()) {
                break;
            }
            out.push(c.clone());
            cur = self.get(&c).and_then(|p| p.parent.clone());
        }
        out
    }

    /// A project and everything beneath it.
    pub fn subtree(&self, id: &str) -> Vec<String> {
        let mut out = vec![id.to_string()];
        let mut i = 0;
        while i < out.len() {
            let cur = out[i].clone();
            for p in &self.projects {
                if p.parent.as_deref() == Some(cur.as_str()) && !out.contains(&p.id) {
                    out.push(p.id.clone());
                }
            }
            i += 1;
        }
        out
    }

    pub fn children(&self, id: Option<&str>) -> Vec<&Project> {
        let mut kids: Vec<&Project> =
            self.projects.iter().filter(|p| p.parent.as_deref() == id).collect();
        kids.sort_by(|a, b| a.id.cmp(&b.id));
        kids
    }

    /// Roots are projects with no parent, or whose parent is missing.
    pub fn roots(&self) -> Vec<&Project> {
        let mut out: Vec<&Project> = self
            .projects
            .iter()
            .filter(|p| match &p.parent {
                None => true,
                Some(parent) => self.get(parent).is_none(),
            })
            .collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// Longest-prefix match of `cwd` against every project root.
    pub fn project_for_cwd(&self, cwd: &str) -> Option<String> {
        let target = util::real(cwd);
        let mut best: Option<(usize, String)> = None;
        for p in &self.projects {
            for r in &p.roots {
                let root = util::real(r);
                if target.starts_with(&root) {
                    let depth = root.components().count();
                    if best.as_ref().map(|(d, _)| depth > *d).unwrap_or(true) {
                        best = Some((depth, p.id.clone()));
                    }
                }
            }
        }
        best.map(|(_, id)| id)
    }

    /// Last-resort inference from a workspace label like `Trance Video`.
    /// Deliberately conservative: longest matching project name wins, and a
    /// match must cover a whole word.
    pub fn project_for_label(&self, label: &str) -> Option<String> {
        let l = label.to_ascii_lowercase();
        let words: Vec<&str> = l.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
        let mut best: Option<(usize, String)> = None;
        for p in &self.projects {
            for cand in [p.id.to_ascii_lowercase(), p.name.to_ascii_lowercase()] {
                if cand.is_empty() {
                    continue;
                }
                let hit = words.iter().any(|w| *w == cand) || l.starts_with(&cand);
                if hit && best.as_ref().map(|(len, _)| cand.len() > *len).unwrap_or(true) {
                    best = Some((cand.len(), p.id.clone()));
                }
            }
        }
        best.map(|(_, id)| id)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Counts {
    pub open: usize,
    pub doing: usize,
    pub blocked: usize,
    pub review: usize,
    pub done: usize,
}

/// Per-project counts, rolled up so a parent includes its children.
pub fn counts_by_project(index: &Index, tasks: &[Task]) -> BTreeMap<String, Counts> {
    let mut direct: BTreeMap<String, Counts> = BTreeMap::new();
    for t in tasks {
        let key = t.project.clone().unwrap_or_else(|| "".to_string());
        let c = direct.entry(key).or_default();
        match t.status() {
            Status::Done => c.done += 1,
            Status::Doing => {
                c.doing += 1;
                c.open += 1;
            }
            Status::Blocked => {
                c.blocked += 1;
                c.open += 1;
            }
            Status::Review => {
                c.review += 1;
                c.open += 1;
            }
            _ => c.open += 1,
        }
    }

    let mut rolled: BTreeMap<String, Counts> = BTreeMap::new();
    for p in &index.projects {
        let mut total = Counts::default();
        for id in index.subtree(&p.id) {
            if let Some(c) = direct.get(&id) {
                total.open += c.open;
                total.doing += c.doing;
                total.blocked += c.blocked;
                total.review += c.review;
                total.done += c.done;
            }
        }
        rolled.insert(p.id.clone(), total);
    }
    if let Some(c) = direct.get("") {
        rolled.insert("".to_string(), *c);
    }
    rolled
}

/// Where a pane/workspace/cwd sits, and how we decided.
pub struct Resolution {
    pub project: Option<String>,
    pub source: &'static str,
}

/// Pin value meaning "deliberately no project". Distinct from an absent pin,
/// which only means nothing has been decided.
pub const TOP_LEVEL: &str = "(top)";

pub fn resolve(
    index: &Index,
    pins: &BTreeMap<String, String>,
    binding_project: Option<String>,
    workspace_id: Option<&str>,
    workspace_label: Option<&str>,
    cwd: Option<&str>,
) -> Resolution {
    if let Some(ws) = workspace_id {
        if let Some(p) = pins.get(ws) {
            if p == TOP_LEVEL {
                return Resolution { project: None, source: "top" };
            }
            if index.get(p).is_some() {
                return Resolution { project: Some(p.clone()), source: "pin" };
            }
        }
    }
    if let Some(p) = binding_project {
        if index.get(&p).is_some() {
            return Resolution { project: Some(p), source: "binding" };
        }
    }
    if let Some(c) = cwd {
        if let Some(p) = index.project_for_cwd(c) {
            return Resolution { project: Some(p), source: "cwd" };
        }
    }
    if let Some(l) = workspace_label {
        if let Some(p) = index.project_for_label(l) {
            return Resolution { project: Some(p), source: "label" };
        }
    }
    Resolution { project: None, source: "none" }
}
