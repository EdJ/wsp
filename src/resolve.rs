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

    /// Which project a tag reaches a task from — the nearest one up the chain
    /// that carries it of its own.
    ///
    /// [`effective_tags`](Index::effective_tags) answers *what* is inherited
    /// and drops *where from* on the floor, which is the half you need when the
    /// answer is "you cannot take that off here". A task under `render` reads
    /// `rust herdr` and owns neither; being told the tag comes from `wsp` is
    /// the difference between a refusal and a refusal you can act on.
    pub fn tag_source(&self, project: &str, tag: &str) -> Option<String> {
        let mut guard: HashSet<String> = HashSet::new();
        let mut cur = Some(project.to_string());
        while let Some(cid) = cur {
            if !guard.insert(cid.clone()) {
                break; // parent cycle; doctor reports it
            }
            let p = self.get(&cid)?;
            if p.tags.iter().any(|t| t == tag) {
                return Some(p.id.clone());
            }
            cur = p.parent.clone();
        }
        None
    }

    /// Where a project's work lives on disk: its own first root, or failing
    /// that the nearest ancestor's.
    ///
    /// Inherited for the same reason tags are. The backlog is split into
    /// `wsp/render` and `wsp/data`, and neither has a checkout of its own —
    /// they are two halves of one tree, and saying so twice more would be two
    /// more paths to keep true. Without the walk, opening a workspace for a
    /// task in either landed it wherever the panel happened to be installed.
    pub fn root_of(&self, id: &str) -> Option<String> {
        let mut guard: HashSet<String> = HashSet::new();
        let mut cur = Some(id.to_string());
        while let Some(cid) = cur {
            if !guard.insert(cid.clone()) {
                break; // parent cycle; doctor reports it
            }
            let p = self.get(&cid)?;
            if let Some(r) = p.roots.first() {
                return Some(r.clone());
            }
            cur = p.parent.clone();
        }
        None
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
    /// Open, and deliberately not being worked. Counted apart from `blocked`
    /// so a surface can show what wants an answer without showing what wants
    /// nothing — the two were one number, and the badge read as five things
    /// waiting on you when four of them were waiting on a date.
    pub parked: usize,
    pub review: usize,
    pub done: usize,
}

/// Fold one task into a tally. Shared by the project rollup and the sub-task
/// rollup so the two can never disagree about what `open` means.
fn tally(c: &mut Counts, t: &Task) {
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
        // Open: parked work is unfinished work, and everything that walks the
        // sub-tree to ask whether it can finish a parent, sweep a worktree or
        // resolve a bare suffix has to keep seeing it. What parking changes is
        // where it sorts and how loudly it draws, not whether it exists.
        Status::Parked => {
            c.parked += 1;
            c.open += 1;
        }
        Status::Review => {
            c.review += 1;
            c.open += 1;
        }
        _ => c.open += 1,
    }
}

/// The tasks directly beneath one, in id order.
pub fn children_of<'a>(tasks: &'a [Task], id: &str) -> Vec<&'a Task> {
    let mut kids: Vec<&Task> = tasks.iter().filter(|t| t.parent.as_deref() == Some(id)).collect();
    kids.sort_by(|a, b| a.id.cmp(&b.id));
    kids
}

/// Every task beneath one, through the whole sub-tree. Excludes the task
/// itself.
///
/// Cycle-safe: `parent` is a plain string field and the store is edited by
/// several agents at once, so a loop is a thing that can exist on disk. It is
/// `doctor`'s job to report one, and nobody else's job to hang on it.
pub fn descendants_of(tasks: &[Task], id: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: Vec<String> = vec![id.to_string()];
    let mut queue: Vec<String> = vec![id.to_string()];
    while let Some(cur) = queue.pop() {
        for kid in children_of(tasks, &cur) {
            if seen.contains(&kid.id) {
                continue;
            }
            seen.push(kid.id.clone());
            queue.push(kid.id.clone());
            out.push(kid.id.clone());
        }
    }
    out
}

/// Everything beneath a task, counted through the whole sub-tree.
///
/// Walks by way of `descendants_of` so that what `done` refuses over and what
/// re-parenting carries can never disagree about what "beneath" means.
pub fn counts_under(tasks: &[Task], id: &str) -> Counts {
    let mut total = Counts::default();
    for kid in descendants_of(tasks, id) {
        if let Some(t) = tasks.iter().find(|t| t.id == kid) {
            tally(&mut total, t);
        }
    }
    total
}

/// Reading order: each task followed by its own children, with the depth to
/// indent it by.
///
/// A task whose parent is not in `tasks` is a root here. Filtering — by
/// project, by status, by tag — must never drop a child on the floor, and one
/// indented under a parent that is not on screen is just a row that looks
/// broken. Order within a level is the order it was given.
pub fn nest(tasks: &[Task]) -> Vec<(Task, usize)> {
    const MAX_DEPTH: usize = 6;
    let present = |id: &str| tasks.iter().any(|t| t.id == id);
    let mut out: Vec<(Task, usize)> = Vec::new();
    let mut done: Vec<String> = Vec::new();

    fn walk(
        tasks: &[Task],
        parent: &str,
        depth: usize,
        out: &mut Vec<(Task, usize)>,
        done: &mut Vec<String>,
    ) {
        if depth > MAX_DEPTH {
            return;
        }
        for kid in tasks.iter().filter(|t| t.parent.as_deref() == Some(parent)) {
            if done.contains(&kid.id) {
                continue;
            }
            done.push(kid.id.clone());
            out.push((kid.clone(), depth));
            walk(tasks, &kid.id, depth + 1, out, done);
        }
    }

    for t in tasks {
        let rooted = match &t.parent {
            None => true,
            Some(p) => !present(p),
        };
        if rooted && !done.contains(&t.id) {
            done.push(t.id.clone());
            out.push((t.clone(), 0));
            walk(tasks, &t.id, 1, &mut out, &mut done);
        }
    }
    // Anything left is inside a cycle. Show it flat rather than not at all.
    for t in tasks {
        if !done.contains(&t.id) {
            out.push((t.clone(), 0));
        }
    }
    out
}

/// Per-project counts, rolled up so a parent includes its children.
pub fn counts_by_project(index: &Index, tasks: &[Task]) -> BTreeMap<String, Counts> {
    let mut direct: BTreeMap<String, Counts> = BTreeMap::new();
    for t in tasks {
        let key = t.project.clone().unwrap_or_else(|| "".to_string());
        tally(direct.entry(key).or_default(), t);
    }

    let mut rolled: BTreeMap<String, Counts> = BTreeMap::new();
    for p in &index.projects {
        let mut total = Counts::default();
        for id in index.subtree(&p.id) {
            if let Some(c) = direct.get(&id) {
                total.open += c.open;
                total.doing += c.doing;
                total.blocked += c.blocked;
                total.parked += c.parked;
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

/// The work in hand, in the two places it is written down.
///
/// A binding says which task a *pane* is working. A claim says which task a
/// *workspace* is working: the same fact one level up, and the only thing that
/// tells ten shells sharing one folder apart. They travel together because
/// resolution reads them together, and as one value rather than two arguments
/// of the same type, which are a swap waiting to happen at a call site.
#[derive(Default, Clone)]
pub struct Held {
    pub binding: Option<String>,
    pub claim: Option<String>,
}


/// The project of the task this workspace holds, if it holds one.
///
/// Matched on the workspace id first and its label second — the label is what
/// survives a workspace being rebuilt under a new id, which is the same
/// fallback `reconcile` uses to find a claim's pane again. A claim made on
/// another machine says nothing about this one, and a claim on work that is
/// finished is not work in hand: both are passed over. Of what is left, the
/// most recent claim wins, because a workspace that has moved on from one task
/// to the next is doing the second.
pub fn claimed_project(
    claims: &BTreeMap<String, serde_json::Value>,
    tasks: &[Task],
    workspace_id: Option<&str>,
    workspace_label: Option<&str>,
) -> Option<String> {
    let host = util::hostname();
    let mut best: Option<(&str, String)> = None;
    for (task_id, c) in claims {
        let get = |k: &str| c.get(k).and_then(|v| v.as_str()).unwrap_or("");
        if !get("host").is_empty() && get("host") != host {
            continue;
        }
        let names_it = match (workspace_id, workspace_label) {
            (Some(id), _) if !id.is_empty() && get("workspace_id") == id => true,
            (_, Some(l)) if !l.is_empty() && get("workspace_label") == l => true,
            _ => false,
        };
        if !names_it {
            continue;
        }
        let Some(t) = tasks.iter().find(|t| &t.id == task_id) else { continue };
        if !t.status().is_open() {
            continue;
        }
        let Some(project) = t.project.clone() else { continue };
        let at = get("claimed_at");
        if best.as_ref().map(|(seen, _)| at > *seen).unwrap_or(true) {
            best = Some((get("claimed_at"), project));
        }
    }
    best.map(|(_, p)| p)
}

pub fn resolve(
    index: &Index,
    pins: &BTreeMap<String, String>,
    held: Held,
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
    if let Some(p) = held.binding {
        if index.get(&p).is_some() {
            return Resolution { project: Some(p), source: "binding" };
        }
    }
    // Above the directory, deliberately. A shell started in the folder ten
    // workspaces share is standing there in the shallowest sense — the claim is
    // what somebody said this workspace is doing, and it is the reason the
    // folder is not the answer.
    if let Some(p) = held.claim {
        if index.get(&p).is_some() {
            return Resolution { project: Some(p), source: "claim" };
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Parked work is open work that wants nothing. Both halves matter: a
    /// parent with a parked child still has a child, so `wsp done` goes on
    /// refusing and a worktree sweep goes on leaving it alone — and the count
    /// that surfaces draw in red stops including it, which is what stopped a
    /// project reading as five things waiting on you when four of them were
    /// waiting on a date.
    #[test]
    fn parked_counts_as_open_and_not_as_waiting_on_anyone() {
        let mut kid = task("t-2", "vst", "parked");
        kid.parent = Some("t-1".into());
        let mut owed = task("t-3", "vst", "blocked");
        owed.parent = Some("t-1".into());
        let tasks = vec![task("t-1", "vst", "doing"), kid, owed];

        let c = counts_under(&tasks, "t-1");
        assert_eq!(c.open, 2, "parked work is unfinished, and a parent cannot be finished over it");
        assert_eq!(c.parked, 1);
        assert_eq!(c.blocked, 1, "the two are counted apart, or neither number means anything");
    }

    /// The world this exists for: eleven workspaces whose shells all started in
    /// one folder, each holding a claim on work in a different project.
    fn index() -> Index {
        let mut vst = Project::new("vst");
        vst.roots = vec!["/home/ed/claude/vst".into()];
        let mut trance = Project::new("trance");
        trance.parent = Some("vst".into());
        trance.roots = vec!["/home/ed/claude/trance".into()];
        Index::new(vec![vst, trance])
    }

    fn task(id: &str, project: &str, status: &str) -> Task {
        let mut t = Task::new("Trance Video", id);
        t.project = Some(project.into());
        t.status_raw = status.into();
        t
    }

    fn claim(ws: &str, label: &str, at: &str) -> serde_json::Value {
        json!({
            "workspace_id": ws,
            "workspace_label": label,
            "claimed_at": at,
            "host": util::hostname(),
        })
    }

    fn claims(pairs: &[(&str, serde_json::Value)]) -> BTreeMap<String, serde_json::Value> {
        pairs.iter().map(|(t, c)| ((*t).to_string(), c.clone())).collect()
    }

    fn held(cl: &BTreeMap<String, serde_json::Value>, tasks: &[Task], ws: &str, label: &str) -> Held {
        Held { binding: None, claim: claimed_project(cl, tasks, Some(ws), Some(label)) }
    }

    /// A shell in the folder ten workspaces share is standing there in the
    /// shallowest sense. What the workspace is holding is what it is doing.
    #[test]
    fn a_claim_places_a_pane_the_directory_cannot() {
        let cl = claims(&[("t-1", claim("w7", "Trance Video", "2026-08-14T10:00:00Z"))]);
        let tasks = vec![task("t-1", "trance", "doing")];
        let r = resolve(
            &index(),
            &BTreeMap::new(),
            held(&cl, &tasks, "w7", "Trance Video"),
            Some("w7"),
            Some("Trance Video"),
            Some("/home/ed/claude/vst"),
        );
        assert_eq!(r.project.as_deref(), Some("trance"));
        assert_eq!(r.source, "claim");

        // And with nothing claimed it is the folder again, unchanged.
        let r = resolve(
            &index(),
            &BTreeMap::new(),
            Held::default(),
            Some("w7"),
            Some("Trance Video"),
            Some("/home/ed/claude/vst"),
        );
        assert_eq!(r.project.as_deref(), Some("vst"));
        assert_eq!(r.source, "cwd");
    }

    /// A pane working something of its own is not doing what the workspace
    /// around it is doing. The narrower fact wins, as it always has.
    #[test]
    fn a_panes_own_binding_beats_the_workspaces_claim() {
        let cl = claims(&[("t-1", claim("w7", "Trance Video", "2026-08-14T10:00:00Z"))]);
        let tasks = vec![task("t-1", "trance", "doing")];
        let mut h = held(&cl, &tasks, "w7", "Trance Video");
        h.binding = Some("vst".into());
        let r = resolve(&index(), &BTreeMap::new(), h, Some("w7"), Some("Trance Video"), None);
        assert_eq!(r.project.as_deref(), Some("vst"));
        assert_eq!(r.source, "binding");
    }

    /// A pin is a statement about what a workspace *is*, and it outranks every
    /// statement about what it is doing.
    #[test]
    fn a_pin_still_beats_a_claim() {
        let cl = claims(&[("t-1", claim("w7", "Trance Video", "2026-08-14T10:00:00Z"))]);
        let tasks = vec![task("t-1", "trance", "doing")];
        let pins: BTreeMap<String, String> = [("w7".to_string(), "vst".to_string())].into();
        let r = resolve(
            &index(),
            &pins,
            held(&cl, &tasks, "w7", "Trance Video"),
            Some("w7"),
            Some("Trance Video"),
            None,
        );
        assert_eq!(r.project.as_deref(), Some("vst"));
        assert_eq!(r.source, "pin");
    }

    /// Finished work is not work in hand. A claim nobody released would
    /// otherwise go on placing a workspace for as long as the file survived.
    #[test]
    fn a_claim_on_finished_work_places_nothing() {
        let cl = claims(&[("t-1", claim("w7", "Trance Video", "2026-08-14T10:00:00Z"))]);
        let tasks = vec![task("t-1", "trance", "done")];
        assert_eq!(claimed_project(&cl, &tasks, Some("w7"), Some("Trance Video")), None);
    }

    /// Claims are machine-local — a workspace id on the laptop means nothing
    /// here — and the store is shared between both.
    #[test]
    fn a_claim_from_another_machine_is_not_this_machines_business() {
        let mut c = claim("w7", "Trance Video", "2026-08-14T10:00:00Z");
        c["host"] = json!("somebody-elses-mac");
        let cl = claims(&[("t-1", c)]);
        let tasks = vec![task("t-1", "trance", "doing")];
        assert_eq!(claimed_project(&cl, &tasks, Some("w7"), Some("Trance Video")), None);
    }

    /// A workspace rebuilt under a new id keeps its label, which is why
    /// `reconcile` looks it up both ways and why this does too.
    #[test]
    fn a_claim_is_found_again_by_label_when_the_id_has_changed() {
        let cl = claims(&[("t-1", claim("w7", "Trance Video", "2026-08-14T10:00:00Z"))]);
        let tasks = vec![task("t-1", "trance", "doing")];
        assert_eq!(
            claimed_project(&cl, &tasks, Some("w22"), Some("Trance Video")).as_deref(),
            Some("trance")
        );
    }

    /// Two claims on one workspace is a workspace that moved from one task to
    /// the next and left the first behind. It is doing the second.
    #[test]
    fn the_most_recent_of_two_claims_is_the_one_being_worked() {
        let cl = claims(&[
            ("t-1", claim("w7", "Trance Video", "2026-08-14T10:00:00Z")),
            ("t-2", claim("w7", "Trance Video", "2026-08-15T09:00:00Z")),
        ]);
        let tasks = vec![task("t-1", "vst", "doing"), task("t-2", "trance", "doing")];
        assert_eq!(
            claimed_project(&cl, &tasks, Some("w7"), Some("Trance Video")).as_deref(),
            Some("trance")
        );
    }

    /// The walk that makes the tag picker honest. A task under `render` reads
    /// `rust herdr` in `wsp show` and owns neither: `render` owns nothing
    /// either, and both come from `wsp`, two levels up. Answering "inherited"
    /// without answering "from where" leaves the picker able to refuse and
    /// unable to say what to do instead.
    #[test]
    fn a_tag_is_traced_to_the_project_that_actually_carries_it() {
        let mut wsp = Project::new("wsp");
        wsp.tags = vec!["rust".into(), "herdr".into()];
        let mut render = Project::new("render");
        render.parent = Some("wsp".into());
        let mut data = Project::new("data");
        data.parent = Some("wsp".into());
        data.tags = vec!["rust".into()];
        let index = Index::new(vec![wsp, render, data]);

        assert_eq!(index.tag_source("render", "rust").as_deref(), Some("wsp"));
        assert_eq!(index.tag_source("render", "herdr").as_deref(), Some("wsp"));
        // Nearest wins: `data` carries `rust` of its own, so that is where a
        // task under it would be sent to change it.
        assert_eq!(index.tag_source("data", "rust").as_deref(), Some("data"));
        assert_eq!(index.tag_source("data", "herdr").as_deref(), Some("wsp"));
        // A tag nobody up the chain carries is the task's own, or nobody's.
        assert_eq!(index.tag_source("render", "dsp"), None);
        assert_eq!(index.tag_source("nosuch", "rust"), None);
    }
}
