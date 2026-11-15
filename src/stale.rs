//! A refusal from an installed wsp that a landed row has since fixed says so
//! (`compounding-622`).
//!
//! The installed binary is built at one commit and master moves on. A seat
//! refused for something the code at master already allows reads the refusal as
//! the rule, not as a lag: on retro-8 a sent-back retro row could not file
//! because the install predated `wsp-627`, and the governor first took it for a
//! real refusal and waited 126 minutes on it. So the refusal names the lag: the
//! installed commit, how far master is ahead of it, and the landed rows since
//! that touched the source the refusal came from.
//!
//! "The source the refusal came from" is the file list the caller passes. A
//! row that never touched those files is not said to fix anything — then the
//! note is the gap alone, and says the refusal stands.

use std::path::Path;

use crate::cmd_verify::git;
use crate::store::Store;

/// The installed build against master, for one refusal.
#[derive(Debug, PartialEq, Eq)]
pub struct Gap {
    pub installed: String,
    pub master: String,
    /// Commits master has that the installed build was not built from.
    pub behind: u64,
    /// Landed rows since, newest first, each as (row id, short commit): only
    /// those whose commit touched the refusing files.
    pub rows: Vec<(String, String)>,
    pub files: Vec<String>,
}

/// `wsp-627: a sent-back…` → `wsp-627`.
fn row_of(subject: &str) -> Option<&str> {
    let (id, _) = subject.split_once(':')?;
    let (slug, n) = id.rsplit_once('-')?;
    let named = !slug.is_empty() && slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    (named && !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())).then_some(id)
}

/// The gap between `installed` and `master` in `root`, over the commits that
/// touched `files` (all of them when `files` is empty). `None` when master has
/// nothing the install lacks, or either end is not a commit in `root`.
pub fn gap_in(root: &Path, installed: &str, master: &str, files: &[&str]) -> Option<Gap> {
    let range = format!("{installed}..{master}");
    let behind: u64 = git(root, &["rev-list", "--count", &range])?.trim().parse().ok()?;
    if behind == 0 {
        return None;
    }
    let mut argv = vec!["log", "--format=%h%x09%s", &range];
    if !files.is_empty() {
        argv.push("--");
        argv.extend_from_slice(files);
    }
    let mut rows: Vec<(String, String)> = Vec::new();
    for line in git(root, &argv).unwrap_or_default().lines() {
        let Some((sha, subject)) = line.split_once('\t') else { continue };
        let Some(id) = row_of(subject) else { continue };
        if !rows.iter().any(|(r, _)| r == id) {
            rows.push((id.to_string(), sha.to_string()));
        }
    }
    let master = git(root, &["rev-parse", "--short", master])?.trim().to_string();
    Some(Gap {
        installed: installed.to_string(),
        master,
        behind,
        rows,
        files: files.iter().map(|f| f.to_string()).collect(),
    })
}

/// What a refusal appends: `the installed wsp (13f6ab0) predates wsp-627
/// (dca4e72) — master 9ab12cd is 33 commits ahead; `wsp install` from master
/// takes it`.
pub fn sentence(g: &Gap) -> String {
    let ahead = format!("master {} is {} commit{} ahead", g.master, g.behind, if g.behind == 1 { "" } else { "s" });
    if g.rows.is_empty() {
        let where_ = match g.files.as_slice() {
            [] => "this".to_string(),
            f => f.join(", "),
        };
        return format!(
            "the installed wsp ({}) is behind master — {ahead}, but no landed row since touches {where_}, so the refusal stands",
            g.installed
        );
    }
    let shown: Vec<String> = g.rows.iter().take(3).map(|(r, s)| format!("{r} ({s})")).collect();
    let more = match g.rows.len() {
        n if n > 3 => format!(" and {} more", n - 3),
        _ => String::new(),
    };
    format!(
        "the installed wsp ({}) predates {}{more} — {ahead}; a landed row since may already allow this, `wsp install` from master takes it",
        g.installed,
        shown.join(", ")
    )
}

fn master_of(root: &Path) -> Option<&'static str> {
    ["master", "main"].into_iter().find(|b| git(root, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{b}")]).is_some())
}

/// The note for a refusal that came from `files`, or `None` when the install
/// is current, cannot say what it was built from, or no declared root knows its
/// commit.
pub fn note(store: &Store, files: &[&str]) -> Option<String> {
    note_at(store, crate::base::stamp::COMMIT, files)
}

/// [`note`] for an install built at `installed`, so a test can name the commit.
pub fn note_at(store: &Store, installed: &str, files: &[&str]) -> Option<String> {
    if installed.is_empty() {
        return None;
    }
    for p in store.projects() {
        for r in &p.roots {
            let root = crate::util::real(r);
            if git(&root, &["cat-file", "-e", &format!("{installed}^{{commit}}")]).is_none() {
                continue;
            }
            let master = master_of(&root)?;
            return gap_in(&root, installed, master, files).map(|g| sentence(&g));
        }
    }
    None
}

/// The refusal `msg`, with the lag appended on a second line when there is one.
pub fn refusal(store: &Store, msg: &str, files: &[&str]) -> String {
    refusal_at(store, crate::base::stamp::COMMIT, msg, files)
}

fn refusal_at(store: &Store, installed: &str, msg: &str, files: &[&str]) -> String {
    match note_at(store, installed, files) {
        Some(n) => format!("{msg}\nwsp: {n}"),
        None => msg.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(tag: &str) -> (std::path::PathBuf, String) {
        let dir = std::env::temp_dir().join(format!("wsp-stale-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let g = |a: &[&str]| {
            let o = std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
                .args(a)
                .output()
                .unwrap();
            assert!(o.status.success(), "{a:?}: {}", String::from_utf8_lossy(&o.stderr));
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
        g(&["init", "-q", "-b", "master"]);
        std::fs::write(dir.join("src/retro_run.rs"), "a").unwrap();
        std::fs::write(dir.join("src/other.rs"), "a").unwrap();
        g(&["add", "."]);
        g(&["commit", "-qm", "base"]);
        let installed = g(&["rev-parse", "--short", "HEAD"]);
        std::fs::write(dir.join("src/other.rs"), "b").unwrap();
        g(&["commit", "-qam", "wsp-600: other"]);
        std::fs::write(dir.join("src/retro_run.rs"), "b").unwrap();
        g(&["commit", "-qam", "wsp-627: a sent-back retro row can file"]);
        (dir, installed)
    }

    #[test]
    fn a_refusal_from_a_file_a_landed_row_changed_says_the_install_predates_that_row() {
        let (dir, installed) = repo("fixes");
        let g = gap_in(&dir, &installed, "master", &["src/retro_run.rs"]).expect("two commits ahead");
        assert_eq!(g.behind, 2);
        assert_eq!(g.rows.len(), 1);
        let s = sentence(&g);
        assert!(s.contains(&format!("the installed wsp ({installed}) predates wsp-627 (")), "{s}");
        assert!(s.contains("2 commits ahead"), "{s}");
        assert!(!s.contains("wsp-600"), "a row that never touched the refusing file was named: {s}");
    }

    #[test]
    fn a_gap_no_landed_row_closes_says_the_refusal_stands() {
        let (dir, installed) = repo("stands");
        let g = gap_in(&dir, &installed, "master", &["src/missing.rs"]).unwrap();
        let s = sentence(&g);
        assert!(s.contains("no landed row since touches src/missing.rs, so the refusal stands"), "{s}");
        assert!(!s.contains("predates"), "{s}");
    }

    #[test]
    fn an_install_at_master_says_nothing() {
        let (dir, _) = repo("current");
        assert_eq!(gap_in(&dir, "master", "master", &["src/retro_run.rs"]), None);
    }

    #[test]
    fn a_refusal_names_the_row_a_declared_root_shows_the_install_predates() {
        let (dir, installed) = repo("store");
        let (_env, store) = crate::testkit::isolated_store("stale-refusal");
        let mut p = crate::model::Project::new("wsp");
        p.roots = vec![dir.display().to_string()];
        store.save_project(&p).unwrap();
        let said = refusal_at(&store, &installed, "wsp: e has no retro running", &["src/retro_run.rs"]);
        let mut lines = said.lines();
        assert_eq!(lines.next(), Some("wsp: e has no retro running"));
        let note = lines.next().expect("the refusal did not name the lag");
        assert!(note.contains(&format!("the installed wsp ({installed}) predates wsp-627")), "{note}");
        // An install that cannot say where it came from adds nothing.
        assert_eq!(refusal_at(&store, "", "m", &["src/retro_run.rs"]), "m");
    }

    #[test]
    fn only_a_row_prefix_names_a_row() {
        assert_eq!(row_of("wsp-627: a thing"), Some("wsp-627"));
        assert_eq!(row_of("compounding-622: x"), Some("compounding-622"));
        assert_eq!(row_of("Merge branch: x"), None);
        assert_eq!(row_of("fix: x"), None);
    }
}
