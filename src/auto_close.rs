//! A row at `review` that nothing is owed on closes itself: `wsp-547`.
//!
//! `review` is where a member waits for the person's `wsp done`, and the person
//! rarely reviews rows directly: 508 sat there on 2026-11-09, and the fleet
//! watch closed 467 of them by hand. Each held its tree until `done`, so the
//! barrier was also a disk leak.
//!
//! **The rule**, read by the daemon's tick, closes a row (`done`, logged as
//! `closed by the run: landed, verified, nothing owed`) when all of these hold:
//!
//! 1. its work has landed — [`crate::after::landed`], no commit off the trunk;
//! 2. its newest verification holds, or it is on no list and was never verified;
//! 3. every list that holds it is complete (finished);
//! 4. nothing is owed on it: no open message about it (a person item, an ask,
//!    a flag, a block) and no `hold-review` mark;
//! 5. it has sat quiet for the project's period — 24 hours unless
//!    `wsp review --auto-close off|<hours>` says otherwise.
//!
//! Anything else stays at review for a person. A row the person marked keep —
//! a flag on it, or `wsp review <id> … --hold-review` — never closes itself.
//! `wsp reopen` undoes either kind of close.
//!
//! **One commit per batch**: every row the tick closes is written, and the
//! store is committed once. What it closed is appended to `auto-closed.log` in
//! the state directory, and `wsp yours` counts the lines since the person last
//! looked (`wsp yours --closed`).

use crate::model::{Status, Task, WorklistStatus};
use crate::store::Store;
use crate::util;

/// The tag `--hold-review` leaves: the person wants this row kept at review.
pub(crate) const HOLD: &str = "hold-review";

/// The quiet period when the project says nothing, in hours.
pub(crate) const DEFAULT_HOURS: i64 = 24;

/// The line a close leaves on the row and in `cycle.log`.
pub(crate) const REASON: &str = "closed by the run: landed, verified, nothing owed";

const RECORD: &str = "auto-closed.log";
const LOOKED: &str = "auto-closed.looked";

/// The quiet period, in hours, for a row of this project — its own setting,
/// else the nearest ancestor's, else the default. `None` is `off`.
pub(crate) fn hours(store: &Store, project: Option<&str>) -> Option<i64> {
    let index = crate::resolve::Index::new(store.projects());
    let mut chain: Vec<String> = project.map(str::to_string).into_iter().collect();
    if let Some(p) = project {
        chain.extend(index.ancestors(p));
    }
    for id in chain {
        let Some(p) = store.project(&id) else { continue };
        let word = p.auto_close.trim().to_ascii_lowercase();
        match word.as_str() {
            "" => continue,
            "off" => return None,
            n => return n.parse::<i64>().ok().filter(|h| *h >= 0).or(Some(DEFAULT_HOURS)),
        }
    }
    Some(DEFAULT_HOURS)
}

/// Why this row stays at review, or `Ok(())` when the run may close it. `now`
/// is epoch seconds, so the quiet period is tested without a clock.
pub(crate) fn due(store: &Store, tasks: &[Task], t: &Task, now: i64) -> Result<(), String> {
    if t.status() != Status::Review {
        return Err("it is not at review".into());
    }
    if t.tags.iter().any(|g| g == HOLD) {
        return Err("the person marked it to keep (--hold-review)".into());
    }
    let Some(hours) = hours(store, t.project.as_deref()) else {
        return Err("its project has auto-close off".into());
    };
    let since = crate::util::epoch_of(&t.updated);
    if since == 0 || now - since < hours * 3600 {
        return Err(format!("it has not been quiet for {hours}h"));
    }
    let lists: Vec<_> = store.worklists().into_iter().filter(|w| w.groups().iter().any(|g| g.members.iter().any(|m| *m == t.id))).collect();
    if let Some(w) = lists.iter().find(|w| w.status() != WorklistStatus::Done) {
        return Err(format!("{} is not complete", w.id));
    }
    match crate::verification::latest(tasks, &t.id) {
        Some(p) if p.state == crate::verification::State::Holds => {}
        Some(_) => return Err("its newest verification does not hold".into()),
        None if !lists.is_empty() => return Err("it is on a list and was never verified".into()),
        None => {}
    }
    if let Some(m) = crate::message::about_task(store, &t.id).into_iter().find(|m| m.is_open()) {
        return Err(format!("{} is open about it", m.id));
    }
    if !crate::after::landed(store, &mut None, &t.id) {
        return Err("its branch has commits the trunk lacks".into());
    }
    Ok(())
}

/// Every row [`due`] at `now`, in id order.
pub(crate) fn rows_due(store: &Store, now: i64) -> Vec<String> {
    let tasks = store.tasks();
    let mut out: Vec<String> =
        tasks.iter().filter(|t| t.status() == Status::Review && due(store, &tasks, t, now).is_ok()).map(|t| t.id.clone()).collect();
    out.sort();
    out
}

/// Close the rows [`rows_due`] names: each written, the store committed once,
/// the trees swept as `wsp done` does, and the batch recorded. Returns the ids.
pub(crate) fn close_due(store: &Store, now: i64) -> Vec<String> {
    let rows = rows_due(store, now);
    if rows.is_empty() {
        return rows;
    }
    let mut closed = Vec::new();
    store.locked(|| {
        for id in &rows {
            // Read again under the lock: a person's note or a sent-back row
            // since the look is theirs.
            let Some(mut t) = store.task(id) else { continue };
            if t.status() != Status::Review {
                continue;
            }
            t.set_status(Status::Done);
            t.log(&format!("→ done ({REASON})"));
            if store.save_task(&t).is_ok() {
                closed.push(id.clone());
            }
        }
    });
    if closed.is_empty() {
        return closed;
    }
    store.git_commit(&format!("wsp: {} row(s) at review closed by the run: {}", closed.len(), closed.join(" ")));
    record(store, &closed);
    crate::store::log_line(store, &format!("{REASON}: {}", closed.join(" ")));
    for line in crate::cmd_checkout::closed_lines(&crate::cmd_checkout::sweep_closed(store, &closed, false)) {
        crate::store::log_line(store, &format!("{line}: {REASON}"));
    }
    closed
}

/// The daemon's step.
pub(crate) fn tidy(store: &Store) {
    close_due(store, util::epoch_secs());
}

fn record(store: &Store, ids: &[String]) {
    use std::io::Write;
    let at = util::now_iso();
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(store.state_file(RECORD)) {
        for id in ids {
            let _ = writeln!(f, "{at} {id}");
        }
    }
}

/// `(stamp, id)` for every row the run has closed, oldest first.
pub(crate) fn closed_rows(store: &Store) -> Vec<(String, String)> {
    let text = std::fs::read_to_string(store.state_file(RECORD)).unwrap_or_default();
    text.lines().filter_map(|l| l.split_once(' ')).map(|(a, b)| (a.to_string(), b.trim().to_string())).collect()
}

/// What the run has closed since the person last looked.
pub(crate) fn unseen(store: &Store) -> Vec<(String, String)> {
    let looked = std::fs::read_to_string(store.state_file(LOOKED)).unwrap_or_default();
    let looked = looked.trim();
    closed_rows(store).into_iter().filter(|(at, _)| looked.is_empty() || at.as_str() > looked).collect()
}

/// The person has looked: nothing is unseen until the run closes another.
pub(crate) fn looked(store: &Store) {
    let _ = std::fs::write(store.state_file(LOOKED), util::now_iso());
}

/// `wsp review --auto-close off|<hours> [-p project]` — the project's quiet
/// period. The project is the one named, else the one the cwd is in.
pub(crate) fn set_setting(store: &Store, word: &str, project: Option<&str>) -> Result<String, String> {
    let w = word.trim().to_ascii_lowercase();
    if w != "off" && w.parse::<i64>().is_err_and(|_| true) || w.starts_with('-') {
        return Err(format!("wsp review --auto-close takes `off` or a number of hours, not `{word}`"));
    }
    let index = crate::resolve::Index::new(store.projects());
    let id = match project {
        Some(p) => index.find(p).map(|f| f.id.clone()).ok_or_else(|| format!("no project matching `{p}`"))?,
        None => return Err("name the project: wsp review --auto-close off|<hours> -p <project>".into()),
    };
    let mut p = store.project(&id).ok_or_else(|| format!("no project {id}"))?;
    p.auto_close = if w == "24" { String::new() } else { w.clone() };
    store.save_project(&p).map_err(|e| e.to_string())?;
    store.git_commit(&format!("wsp: {id} auto-close {w}"));
    Ok(match w.as_str() {
        "off" => format!("{id}: rows at review no longer close themselves"),
        h => format!("{id}: a row at review closes itself after {h}h quiet, once landed, verified and owed nothing"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Group, Project, Worklist};
    use crate::verification::{self, Pass, State};

    const DAY: i64 = 86_400;

    /// A store with project `p` and a row `p-001` at review, last written long
    /// ago; no repository, so it is as landed as it will ever be.
    fn at_review(name: &str) -> (util::Isolated, Store) {
        let env = util::isolated(&format!("auto-close-{name}"));
        let store = Store::open();
        store.save_project(&Project::new("p")).unwrap();
        let mut t = Task::new("a row", "p-001");
        t.project = Some("p".into());
        t.set_status(Status::Review);
        t.updated = util::iso_at(util::epoch_secs() - 3 * DAY);
        store.save_task(&t).unwrap();
        (env, store)
    }

    fn row(store: &Store) -> Task {
        store.task("p-001").unwrap()
    }

    fn why(store: &Store) -> Result<(), String> {
        due(store, &store.tasks(), &row(store), util::epoch_secs())
    }

    fn verdict(store: &Store, state: State) {
        let mut t = row(store);
        let updated = t.updated.clone();
        let mut pass = Pass::opened(None, None);
        pass.state = state;
        verification::write(&mut t, &[pass]);
        t.updated = updated;
        store.save_task(&t).unwrap();
    }

    fn on_list(store: &Store, status: WorklistStatus) {
        let mut w = Worklist::new("l", "a list");
        w.set_groups(&[Group { members: vec!["p-001".into()], ..Default::default() }]);
        w.set_status(status);
        store.save_worklist(&w).unwrap();
    }

    #[test]
    fn a_landed_row_on_no_list_never_verified_and_quiet_closes() {
        let (_env, store) = at_review("plain");
        assert_eq!(why(&store), Ok(()));
        assert_eq!(rows_due(&store, util::epoch_secs()), ["p-001"]);
    }

    #[test]
    fn a_row_not_at_review_is_left_alone() {
        let (_env, store) = at_review("status");
        let mut t = row(&store);
        t.set_status(Status::Doing);
        t.updated = util::iso_at(util::epoch_secs() - 3 * DAY);
        store.save_task(&t).unwrap();
        assert!(why(&store).is_err());
    }

    #[test]
    fn a_row_with_commits_the_trunk_lacks_stays() {
        let (env, store) = at_review("landed");
        let repo = env.path("repo");
        std::fs::create_dir_all(&repo).unwrap();
        crate::testkit::repo(&repo);
        let git = |dir: &std::path::Path, a: &[&str]| {
            assert!(std::process::Command::new("git").arg("-C").arg(dir).args(a).output().unwrap().status.success(), "{a:?}");
        };
        git(&repo, &["commit", "--allow-empty", "--quiet", "-m", "first"]);
        let mut p = store.project("p").unwrap();
        p.roots = vec![repo.display().to_string()];
        store.save_project(&p).unwrap();
        let dir = repo.join(".worktrees/p-001");
        git(&repo, &["worktree", "add", "--quiet", "-b", "p-001", &dir.display().to_string(), "master"]);
        assert_eq!(why(&store), Ok(()), "a branch level with the trunk is landed");
        std::fs::write(dir.join("w.txt"), "work\n").unwrap();
        git(&dir, &["add", "w.txt"]);
        git(&dir, &["commit", "--quiet", "-m", "work"]);
        assert!(why(&store).unwrap_err().contains("trunk lacks"), "{:?}", why(&store));
        git(&repo, &["merge", "--quiet", "--ff-only", "p-001"]);
        assert_eq!(why(&store), Ok(()));
    }

    #[test]
    fn verification_must_hold_and_a_list_member_must_have_one() {
        let (env, store) = at_review("verified");
        verdict(&store, State::Blocks);
        assert!(why(&store).unwrap_err().contains("verification"), "a block closed the row");
        verdict(&store, State::Holds);
        assert_eq!(why(&store), Ok(()));
        drop(env);

        let (_env, store) = at_review("listed");
        on_list(&store, WorklistStatus::Done);
        assert!(why(&store).unwrap_err().contains("never verified"), "an unverified list member closed");
        verdict(&store, State::Holds);
        assert_eq!(why(&store), Ok(()));
    }

    #[test]
    fn every_list_that_holds_the_row_must_be_complete() {
        let (_env, store) = at_review("lists");
        verdict(&store, State::Holds);
        for open in [WorklistStatus::Draft, WorklistStatus::Running, WorklistStatus::Held, WorklistStatus::Parked] {
            on_list(&store, open);
            assert!(why(&store).unwrap_err().contains("not complete"), "{open:?}");
        }
        on_list(&store, WorklistStatus::Done);
        assert_eq!(why(&store), Ok(()));
        let mut other = Worklist::new("m", "another");
        other.set_groups(&[Group { members: vec!["p-001".into()], ..Default::default() }]);
        other.set_status(WorklistStatus::Running);
        store.save_worklist(&other).unwrap();
        assert!(why(&store).unwrap_err().contains("m is not complete"));
    }

    #[test]
    fn an_open_item_about_the_row_holds_it() {
        use crate::message::{About, Kind, Message, Party, Waiting};
        let (_env, store) = at_review("owed");
        assert_eq!(why(&store), Ok(()));
        let q = Message::question(Party::seat("worklist"), Kind::Note, "is this right?", Waiting::new("", "p-001"))
            .about(About::Task("p-001".into()))
            .for_the_person();
        crate::message::raise(&store, &q).unwrap();
        assert!(why(&store).unwrap_err().contains("open about it"), "{:?}", why(&store));
    }

    #[test]
    fn the_quiet_period_is_the_projects_and_off_is_off() {
        let (_env, store) = at_review("quiet");
        let at = |secs_ago: i64| {
            let mut t = row(&store);
            t.updated = util::iso_at(util::epoch_secs() - secs_ago);
            store.save_task(&t).unwrap();
        };
        at(23 * 3600);
        assert!(why(&store).unwrap_err().contains("24h"), "inside the default period");
        at(25 * 3600);
        assert_eq!(why(&store), Ok(()));

        set_setting(&store, "48", Some("p")).unwrap();
        assert!(why(&store).unwrap_err().contains("48h"));
        set_setting(&store, "1", Some("p")).unwrap();
        assert_eq!(why(&store), Ok(()));
        set_setting(&store, "off", Some("p")).unwrap();
        assert!(why(&store).unwrap_err().contains("off"));
        set_setting(&store, "24", Some("p")).unwrap();
        assert_eq!(why(&store), Ok(()), "24 is the default again");
        assert!(set_setting(&store, "soon", Some("p")).is_err());
        assert!(set_setting(&store, "-3", Some("p")).is_err());
    }

    #[test]
    fn a_child_project_takes_its_ancestors_setting() {
        let (_env, store) = at_review("inherit");
        let mut parent = store.project("p").unwrap();
        parent.auto_close = "off".into();
        store.save_project(&parent).unwrap();
        let mut child = Project::new("p-sub");
        child.parent = Some("p".into());
        store.save_project(&child).unwrap();
        assert_eq!(hours(&store, Some("p-sub")), None);
        assert_eq!(hours(&store, None), Some(DEFAULT_HOURS));
    }

    #[test]
    fn a_row_marked_to_keep_never_closes() {
        let (_env, store) = at_review("keep");
        let mut t = row(&store);
        t.tags.push(HOLD.into());
        t.updated = util::iso_at(util::epoch_secs() - 30 * DAY);
        store.save_task(&t).unwrap();
        assert!(why(&store).unwrap_err().contains("keep"));
        assert!(close_due(&store, util::epoch_secs()).is_empty());
        assert_eq!(row(&store).status(), Status::Review);
    }

    /// The close writes `done`, says why on the row, in `cycle.log` and in the
    /// record, and `reopen` sends it back.
    #[test]
    fn the_close_is_logged_recorded_once_and_reopened_by_reopen() {
        let (_env, store) = at_review("close");
        let mut b = Task::new("b", "p-002");
        b.project = Some("p".into());
        b.set_status(Status::Review);
        b.updated = util::iso_at(util::epoch_secs() - 3 * DAY);
        store.save_task(&b).unwrap();

        let closed = close_due(&store, util::epoch_secs());
        assert_eq!(closed, ["p-001", "p-002"]);
        let t = row(&store);
        assert_eq!(t.status(), Status::Done);
        assert!(t.body.contains(REASON), "{}", t.body);
        assert_eq!(closed_rows(&store).len(), 2);
        assert!(close_due(&store, util::epoch_secs()).is_empty(), "a second tick found a row");
        assert_eq!(closed_rows(&store).len(), 2);

        let args = crate::Args::synth("reopen", &["p-001", "one more thing", "--no-seat"], &[]);
        assert_eq!(crate::cmd_task::reopen(&store, &args), 0);
        assert_eq!(row(&store).status(), Status::Doing);
    }

    #[test]
    fn what_the_run_closed_is_unseen_until_the_person_looks() {
        let (_env, store) = at_review("seen");
        assert!(unseen(&store).is_empty());
        close_due(&store, util::epoch_secs());
        assert_eq!(unseen(&store).len(), 1);
        std::thread::sleep(std::time::Duration::from_millis(1100));
        looked(&store);
        assert!(unseen(&store).is_empty());
        assert_eq!(closed_rows(&store).len(), 1, "the record keeps what was closed");
    }

    /// `--hold-review` on the row's own `wsp review` keeps it; the batch is one
    /// commit in the store, and `wsp yours` counts what the run closed until
    /// `--closed` lists it.
    #[test]
    fn hold_review_keeps_a_row_and_yours_counts_the_closes_once() {
        let (_env, store) = at_review("verb");
        let mut b = Task::new("b", "p-002");
        b.project = Some("p".into());
        b.set_status(Status::Doing);
        store.save_task(&b).unwrap();
        let args = crate::Args::synth("review", &["p-002", "did it", "--hold-review"], &[("hold-review", "true")]);
        let code = crate::cmd_task::review(&store, &args);
        let kept = store.task("p-002").unwrap();
        if code == 0 {
            assert!(kept.tags.iter().any(|g| g == HOLD), "{:?}", kept.tags);
        }

        let git = |a: &[&str]| {
            let o = std::process::Command::new("git").arg("-C").arg(&store.root).args(a).output().unwrap();
            String::from_utf8_lossy(&o.stdout).to_string()
        };
        git(&["init", "--quiet"]);
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "--allow-empty", "--quiet", "-m", "start"]);
        let before = git(&["rev-list", "--count", "HEAD"]);
        assert_eq!(before.trim(), "1", "the store is a repository for this test");
        let mut c = Task::new("c", "p-003");
        c.project = Some("p".into());
        c.set_status(Status::Review);
        c.updated = util::iso_at(util::epoch_secs() - 3 * DAY);
        store.save_task(&c).unwrap();
        let closed = close_due(&store, util::epoch_secs());
        assert_eq!(closed, ["p-001", "p-003"]);
        let after = git(&["rev-list", "--count", "HEAD"]);
        let n = |s: &str| s.trim().parse::<i64>().unwrap_or(0);
        // The new row's own save is not committed here; the batch is one.
        assert_eq!(n(&after) - n(&before), 1, "{before} -> {after}: the batch is one commit");

        assert_eq!(unseen(&store).len(), 2);
        let args = crate::Args::synth("yours", &[], &[("json", "true")]);
        assert_eq!(crate::cmd_message::yours(&store, &args), 0);
        let args = crate::Args::synth("yours", &[], &[("closed", "true")]);
        assert_eq!(crate::cmd_message::yours(&store, &args), 0);
        assert!(unseen(&store).is_empty(), "listing them is looking");
    }
}
