//! Is the work a follow-up names already somebody's? `wsp-565`.
//!
//! The `signing` list's barrier check held on a help-fixture miss and filed
//! `wsp-531` for it, when `wsp-527` — filed by an earlier barrier for exactly
//! that fix, with the same failing test — was in flight and landed a minute
//! later. `wsp-531`'s seat put back, byte for byte, lines already on the trunk:
//! 28 minutes of the list's wall. Nothing at `followup` time looked.
//!
//! This is the looking. A row is read for what it *names* — the files and the
//! failing tests in its prose, and its title's words — and held against every
//! other row that is open, or that landed after the check's tree was cut. Three
//! signals, any one of which is enough:
//!
//! - **a test** the two both name (a long snake-case identifier);
//! - **a file**: one the row names that an open row's prose also covers whole,
//!   or that a landed row's diff touched;
//! - **a close title**: most of the significant words the same.
//!
//! The other half is [`moved`]: a row whose seat is about to start in a tree
//! cut before the trunk changed a file the row names is told so in its order.

use std::collections::BTreeSet;
use std::path::Path;

use crate::model::{Status, Task};
use crate::store::Store;

/// The sections a row's own account of the work is in. Not the Log or the
/// notes: those name every file anybody ever touched.
const SECTIONS: &[&str] = &["Overview", "Evidence", "Fix", "Done when", "Checks", "Details"];

/// A row that already covers a follow-up, and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub row: String,
    pub status: String,
    pub why: String,
}

impl Found {
    /// `wsp-527 (landed at 6dc95d2): touches fixtures/help.txt`.
    pub fn line(&self) -> String {
        format!("{} ({}): {}", self.row, self.status, self.why)
    }
}

/// What a row names.
#[derive(Debug, Default, Clone)]
pub struct Names {
    pub files: BTreeSet<String>,
    pub tests: BTreeSet<String>,
    pub words: BTreeSet<String>,
}

fn is_file(tok: &str) -> bool {
    const EXTS: &[&str] = &["rs", "md", "txt", "toml", "json", "sh", "lock", "yaml", "yml", "py", "js", "ts", "html", "css"];
    if tok.len() < 4 || tok.contains("..") || tok.starts_with('.') && !tok.contains('/') {
        return false;
    }
    let Some((stem, ext)) = tok.rsplit_once('.') else { return false };
    !stem.is_empty()
        && EXTS.contains(&ext)
        && stem.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '.'))
}

fn is_test(tok: &str) -> bool {
    tok.len() >= 15
        && tok.matches('_').count() >= 3
        && tok.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !tok.starts_with('_')
}

const STOP: &[&str] = &[
    "that", "this", "with", "from", "into", "when", "what", "each", "have", "does", "should", "which", "their", "there", "about", "after",
    "before", "row", "rows", "wsp",
];

/// The files, tests and title words a row names.
pub fn names(t: &Task) -> Names {
    let mut text = String::new();
    for s in SECTIONS {
        if let Some(body) = t.section(s) {
            text.push_str(&body);
            text.push('\n');
        }
    }
    let mut n = Names::default();
    let split = |c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/'));
    for tok in text.split(split).chain(t.title.split(split)) {
        let tok = tok.trim_matches(|c: char| c == '.' || c == '-' || c == '/');
        if is_file(tok) {
            n.files.insert(tok.to_string());
        } else if is_test(tok) {
            n.tests.insert(tok.to_string());
        }
    }
    for w in t.title.split(|c: char| !c.is_ascii_alphabetic()) {
        let w = w.to_ascii_lowercase();
        if w.len() >= 4 && !STOP.contains(&w.as_str()) {
            n.words.insert(w);
        }
    }
    n
}

/// Most of the significant words the same: three or more shared, and at least
/// 60% of the smaller title.
fn close_titles(a: &Names, b: &Names) -> bool {
    let shared = a.words.intersection(&b.words).count();
    let smaller = a.words.len().min(b.words.len());
    shared >= 3 && shared * 10 >= smaller * 6
}

/// The open or newly landed rows that already cover `row`.
///
/// `since` is when the check's tree was cut (`checks::Check::at`); a row that
/// landed before it is already in that tree and is not a duplicate of anything
/// found there. `None` — a list held, with no check open — looks at open rows
/// only. `skip` is the check's own row, and the rows the follow-up is itself a
/// follow-up of.
pub fn of(store: &Store, row: &Task, tasks: &[Task], since: Option<&str>, skip: &[String]) -> Vec<Found> {
    let mine = names(row);
    if mine.files.is_empty() && mine.tests.is_empty() && mine.words.len() < 3 {
        return Vec::new();
    }
    let candidate = |t: &Task| -> Option<bool> {
        if t.id == row.id || skip.contains(&t.id) || t.title.starts_with(crate::cycle::BARRIER_TITLE) {
            return None;
        }
        match t.status() {
            Status::Todo | Status::Doing | Status::Review | Status::Blocked => Some(false),
            Status::Done => since.filter(|s| t.updated.as_str() >= *s).map(|_| true),
            Status::Inbox | Status::Parked => None,
        }
    };
    let others: Vec<(&Task, bool)> = tasks.iter().filter_map(|t| candidate(t).map(|landed| (t, landed))).collect();
    let landed_ids: Vec<String> = others.iter().filter(|(_, l)| *l).map(|(t, _)| t.id.clone()).collect();
    let touched = if landed_ids.is_empty() { Vec::new() } else { crate::worklist::touched(store, &landed_ids).landed };

    let mut out = Vec::new();
    for (t, landed) in others {
        let theirs = names(t);
        let mut why: Vec<String> = Vec::new();
        if let Some(test) = mine.tests.intersection(&theirs.tests).next() {
            why.push(format!("names the same test, {test}"));
        }
        let diff = touched.iter().find(|l| l.member == t.id);
        let in_diff: Vec<&String> = diff.map(|l| mine.files.iter().filter(|f| l.files.contains(f)).collect()).unwrap_or_default();
        if let Some(f) = in_diff.first() {
            why.push(format!("touches {f}"));
        } else if !mine.files.is_empty() && mine.files.is_subset(&theirs.files) {
            let f = mine.files.iter().next().unwrap();
            why.push(format!("covers {f}"));
        }
        if close_titles(&mine, &theirs) {
            why.push("a close title".to_string());
        }
        if why.is_empty() {
            continue;
        }
        let status = if landed {
            match diff.and_then(|l| l.commit.as_deref()) {
                Some(c) => format!("landed at {}", &c[..c.len().min(7)]),
                None => "landed".to_string(),
            }
        } else {
            t.status().as_str().to_string()
        };
        out.push(Found { row: t.id.clone(), status, why: why.join(", ") });
    }
    out
}

/// The files `task` names that the trunk has changed since the tree at `dir`
/// was cut, each with the trunk commit that changed it. Empty where the tree
/// is not the task's own, git cannot read it, or the trunk has not moved.
pub fn moved(store: &Store, task: &str, dir: &Path) -> Vec<(String, String)> {
    if dir.file_name().and_then(|n| n.to_str()) != Some(task) {
        return Vec::new();
    }
    let Some(t) = store.find_task(task) else { return Vec::new() };
    // The trunk's branch, asked of the trunk: asked of a linked tree it is the
    // tree's own branch.
    let Some(trunk) = crate::cmd_verify::toplevel(dir)
        .and_then(|top| crate::cmd_checkout::trunk(&top))
        .and_then(|t| crate::cmd_checkout::trunk_branch(&t))
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for f in names(&t).files {
        let range = format!("HEAD..{trunk}");
        let log = crate::cmd_verify::git(dir, &["log", "--format=%h", "-1", &range, "--", &f]);
        if let Some(c) = log.map(|s| s.trim().to_string()).filter(|c| !c.is_empty()) {
            out.push((f, c));
        }
    }
    out
}

/// What the order of a seat starting in a tree the trunk has moved under says;
/// empty where it has not. One ASCII line with a leading space, as
/// `cmd_spawn::inherited` is, since some kinds take the order in argv.
pub fn moved_note(store: &Store, task: &str, cwd: Option<&str>) -> String {
    let Some(cwd) = cwd.map(crate::util::expand) else { return String::new() };
    let hit = moved(store, task, Path::new(&cwd));
    if hit.is_empty() {
        return String::new();
    }
    let list: Vec<String> = hit.iter().map(|(f, c)| format!("{f} ({c})")).collect();
    format!(
        " The trunk has changed files this row names since your tree was cut: {}. Rebase onto the trunk or read \
         `git log -p` on them first - the fix you were filed for may already be there.",
        list.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, title: &str, status: &str, overview: &str) -> Task {
        let mut t = Task::new(title, id);
        t.status_raw = status.into();
        crate::model::set_section_in(&mut t.body, "Overview", overview);
        t
    }

    #[test]
    fn a_row_names_its_files_and_tests_and_not_prose() {
        let t = row("a-1", "t", "todo", "Fix `fixtures/help.txt` and src/cmd.rs:12; the e.g. case. Test a_follow_up_ends_the_check_that_filed_it.");
        let n = names(&t);
        assert_eq!(n.files.iter().cloned().collect::<Vec<_>>(), ["fixtures/help.txt", "src/cmd.rs"]);
        assert_eq!(n.tests.iter().cloned().collect::<Vec<_>>(), ["a_follow_up_ends_the_check_that_filed_it"]);
    }

    /// A seat started on a row whose files the trunk has already changed is
    /// told so in its order, naming the file and the commit; a row whose files
    /// it has not is told nothing.
    #[test]
    fn a_seat_in_a_tree_the_trunk_has_moved_under_is_told_which_files_in_its_order() {
        let (env, store) = crate::testkit::isolated_store("moved-order");
        let top = env.path("repo");
        std::fs::create_dir_all(&top).unwrap();
        let git = |dir: &Path, args: &[&str]| {
            let ok = wsp_base::calls::Command::new("git").arg("-C").arg(dir).args(args).env_remove("GIT_INDEX_FILE").status().unwrap();
            assert!(ok.success(), "git {args:?}");
        };
        git(&top, &["init", "--quiet", "-b", "master"]);
        git(&top, &["config", "user.email", "t@t"]);
        git(&top, &["config", "user.name", "t"]);
        std::fs::write(top.join("help.txt"), "one\n").unwrap();
        git(&top, &["add", "."]);
        git(&top, &["commit", "--quiet", "-m", "one"]);
        let tree = env.path("wsp-900");
        git(&top, &["worktree", "add", "--quiet", "-b", "wsp-900", tree.to_str().unwrap()]);
        let row = row("wsp-900", "fix the help", "doing", "Fix help.txt.");
        store.save_task(&row).unwrap();
        let cwd = tree.display().to_string();
        assert_eq!(moved_note(&store, "wsp-900", Some(&cwd)), "", "the trunk has not moved");

        std::fs::write(top.join("help.txt"), "two\n").unwrap();
        git(&top, &["commit", "--quiet", "-am", "two"]);
        let said = moved_note(&store, "wsp-900", Some(&cwd));
        assert!(said.contains("help.txt (") && said.contains("trunk has changed"), "{said}");
        assert!(said.is_ascii() && !said.contains('\n'), "{said:?}");
    }

    #[test]
    fn titles_with_most_words_shared_are_close_and_short_ones_never() {
        let a = names(&row("a-1", "fixtures help text carries reap old help lines", "todo", ""));
        let b = names(&row("a-2", "help fixtures text still carries old reap lines", "todo", ""));
        assert!(close_titles(&a, &b), "{:?} {:?}", a.words, b.words);
        let c = names(&row("a-3", "wl-001", "todo", ""));
        assert!(!close_titles(&c, &c));
    }
}
