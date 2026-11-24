//! Lands by merge (`wsp-350`, EdJ/wsp#6): a project, or one list, whose work
//! reaches the trunk only when a reviewer merges the row's PR.
//!
//! **Why.** In such a project wsp put unreviewed work on the local trunk by two
//! routes: the "at review and not on the trunk" notice told the seat to `wsp
//! land`, and repair's idle-seat land did it with nobody typing anything. Later
//! groups then built on a local master nobody else had.
//!
//! **Where it is said.** `lands: by-merge` on the project (`wsp project set <p>
//! lands=by-merge`), and `lands: by-merge|direct` on a list, which beats the
//! project for that list's members (`wsp worklist edit <l> --lands …`). Absent
//! everywhere is today's local land, so nothing stored before this changes.
//!
//! **What reads it.** `wsp land` refuses and names the PR; the verifier's hold
//! does not land; repair's idle-seat land passes the row by; and the notice
//! says the row waits for its PR to merge rather than ordering a land.

use crate::log::LANDED;
use crate::model::{project_of, Task, WorklistStatus};
use crate::store::Store;

/// The word for a project or list whose rows land by a PR's merge.
pub const BY_MERGE: &str = "by-merge";
/// The word a list uses to take its members back to the local land.
pub const DIRECT: &str = "direct";

/// The marker the notice and the refusal share: what a by-merge row at review
/// is waiting on. Golden in the tests; a reader greps for it.
pub const WAITING: &str = "waiting for the PR to merge";

/// The note on a landing line naming the branch tip it covers: a squash or
/// rebase merge leaves that tip off the trunk, and [`covers`] reads it.
pub const MERGED: &str = "· merged";

/// Whether a landing on the row already covers the branch tip `tip`.
pub fn covers(t: &Task, tip: &str) -> bool {
    let note = format!("{MERGED} {}", tip.trim());
    t.section("Log").is_some_and(|log| log.lines().any(|l| l.contains(LANDED) && l.contains(&note)))
}

/// `Some(true)` for by merge, `Some(false)` for the local land, `None` for a
/// word this build does not know (read as nobody having said).
pub fn word(w: &str) -> Option<bool> {
    match w.trim().to_ascii_lowercase().as_str() {
        "by-merge" | "by_merge" | "merge" | "pr" => Some(true),
        "direct" | "local" | "land" => Some(false),
        _ => None,
    }
}

/// Who said a row lands by merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Said {
    /// A list holding the row, by its `lands:`.
    List(String),
    /// The row's project, by its `lands:`.
    Project(String),
}

impl Said {
    /// `the list <id>` / `the project <id>`, for a sentence.
    pub fn words(&self) -> String {
        match self {
            Said::List(l) => format!("the list {l}"),
            Said::Project(p) => format!("the project {p}"),
        }
    }
}

/// Whether `task` lands by merge, and who said so; `None` is the local land.
///
/// A list that holds the row and has not ended speaks first, in slug order,
/// either way (`direct` takes a by-merge project's row back); then the row's
/// project. A list that never said leaves it to the project.
pub fn said(store: &Store, task: &str) -> Option<Said> {
    for w in store.worklists() {
        if matches!(w.status(), WorklistStatus::Done) || w.is_cancelled() {
            continue;
        }
        let Some(m) = w.lands else { continue };
        if w.groups().iter().any(|g| g.members.iter().any(|x| x == task)) {
            return m.then(|| Said::List(w.id.clone()));
        }
    }
    let t = store.find_task(task)?;
    let p = store.project(&project_of(&t)?)?;
    (word(&p.lands) == Some(true)).then(|| Said::Project(p.id))
}

/// Whether `task` lands only by a PR's merge.
pub fn by_merge(store: &Store, task: &str) -> bool {
    said(store, task).is_some()
}

/// What `wsp land` says for a by-merge row instead of landing: the PR to merge,
/// when one is known, else the branch it is to come from.
pub fn refusal(task: &str, by: &Said, branch: &str, pr: Option<&str>) -> String {
    let merge = match pr {
        Some(pr) => format!("merge its PR, {pr}"),
        None => format!("merge the PR from branch {branch} (no open PR was found for it: push the branch and open one)"),
    };
    format!(
        "wsp: {task} lands by merge ({}), so `wsp land` does not put it on the local trunk — {merge}; \
         the row is {WAITING}, and wsp lands it when the merge is on the remote's trunk",
        by.words()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Project, Task, Worklist};

    fn row(store: &Store, id: &str, project: &str) {
        let mut p = store.project(project).unwrap_or_else(|| Project::new(project));
        p.id = project.into();
        store.save_project(&p).unwrap();
        let mut t = Task::new("a row", id);
        t.project = Some(project.into());
        store.save_task(&t).unwrap();
    }

    #[test]
    fn the_project_says_and_a_list_beats_it_either_way() {
        let store = crate::testkit::store("lands-said");
        row(&store, "p-001", "p");
        assert_eq!(said(&store, "p-001"), None, "nobody said: the local land, as before");

        let mut p = store.project("p").unwrap();
        p.lands = BY_MERGE.into();
        store.save_project(&p).unwrap();
        assert_eq!(store.project("p").unwrap().lands, "by-merge", "the field survives the file");
        assert_eq!(said(&store, "p-001"), Some(Said::Project("p".into())));

        let mut w = Worklist::new("l", "a list");
        w.body = "## Groups\n- 1  p-001\n".into();
        w.set_status(WorklistStatus::Running);
        w.lands = Some(false);
        store.save_worklist(&w).unwrap();
        assert_eq!(store.worklist("l").unwrap().lands, Some(false), "the list's word survives the file");
        assert_eq!(said(&store, "p-001"), None, "the list said direct, over the project");

        p.lands.clear();
        store.save_project(&p).unwrap();
        w.lands = Some(true);
        store.save_worklist(&w).unwrap();
        assert_eq!(said(&store, "p-001"), Some(Said::List("l".into())), "the list said by merge, over a silent project");
    }

    #[test]
    fn the_refusal_names_the_pr_or_the_branch() {
        let by = Said::Project("p".into());
        let r = refusal("p-001", &by, "p-001", Some("https://github.com/o/r/pull/7"));
        assert!(r.contains("merge its PR, https://github.com/o/r/pull/7"), "{r}");
        assert!(r.contains(WAITING), "{r}");
        let r = refusal("p-001", &by, "p-001", None);
        assert!(r.contains("the PR from branch p-001"), "{r}");
    }
}
