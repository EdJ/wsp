//! The greens ledger (`wsp-342`): which check passed at which commit, so a later
//! tier does not run again what an earlier one proved.
//!
//! One append-only file per repository, `greens/<project>-<repo>.tsv`, a line
//! per pass: `sha  environment  took-secs  iso-time  who  flaky  command`. The
//! key is the full commit and the exact command; nothing is inferred (a release
//! green is not a debug green, and no green is carried over a rebase — `affected`
//! decides what reruns there). A cold green satisfies a warm ask, never the
//! reverse. Lines older than [`KEEP_DAYS`] are dropped when the file is next
//! appended to, and a line that does not read is skipped.

use std::path::{Path, PathBuf};

use crate::cmd_verify::git;
use crate::store::Store;
use crate::util;

const KEEP_DAYS: i64 = 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Env {
    Warm,
    Cold,
}

impl Env {
    fn word(self) -> &'static str {
        match self {
            Env::Warm => "warm",
            Env::Cold => "cold",
        }
    }
}

/// A pass the ledger holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Green {
    pub env: Env,
    pub took: u64,
    pub when: String,
    pub who: String,
    pub flaky: bool,
}

pub(crate) struct Greens {
    file: PathBuf,
}

/// What a run needs to reuse and record greens: the ledger, the commit the tree
/// stands at, the environment, and who is asking.
pub(crate) struct Reuse<'a> {
    pub greens: &'a Greens,
    pub sha: &'a str,
    pub env: Env,
    pub who: &'a str,
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\t', "\\t").replace('\n', "\\n")
}

impl Greens {
    pub(crate) fn open(store: &Store, project: &str, repo: &Path) -> Greens {
        let name = repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        Greens { file: store.state_file("greens").join(format!("{project}-{name}.tsv")) }
    }

    fn lines(&self) -> Vec<String> {
        std::fs::read_to_string(&self.file).map(|t| t.lines().map(str::to_string).collect()).unwrap_or_default()
    }

    /// `(sha, command, green)` for a line that reads.
    fn parse(line: &str) -> Option<(&str, String, Green)> {
        let mut f = line.splitn(7, '\t');
        let sha = f.next().filter(|s| s.len() == 40)?;
        let env = match f.next()? {
            "warm" => Env::Warm,
            "cold" => Env::Cold,
            _ => return None,
        };
        let took = f.next()?.parse().ok()?;
        let when = f.next().filter(|w| util::epoch_of(w) > 0)?.to_string();
        let who = f.next()?.to_string();
        let flaky = f.next()? == "flaky";
        let command = f.next()?.to_string();
        Some((sha, command, Green { env, took, when, who, flaky }))
    }

    /// The newest green of exactly this command at exactly this commit that
    /// satisfies `env`: a cold green satisfies either, a warm one only a warm ask.
    pub(crate) fn lookup(&self, sha: &str, command: &str, env: Env) -> Option<Green> {
        let want = escape(command);
        self.lines()
            .iter()
            .rev()
            .filter_map(|l| Self::parse(l).map(|(s, c, g)| (s.to_string(), c, g)))
            .find(|(s, c, g)| s == sha && *c == want && (env == Env::Warm || g.env == Env::Cold))
            .map(|(_, _, g)| g)
    }

    pub(crate) fn record(&self, sha: &str, command: &str, env: Env, took: u64, who: &str, flaky: bool) {
        let Some(dir) = self.file.parent() else { return };
        let _ = std::fs::create_dir_all(dir);
        let keep: Vec<String> =
            self.lines().into_iter().filter(|l| Self::parse(l).is_some_and(|(_, _, g)| util::age_days(&g.when) < KEEP_DAYS)).collect();
        let mut text = keep.join("\n");
        if !text.is_empty() {
            text.push('\n');
        }
        let who = escape(who).replace(' ', "_");
        text.push_str(&format!(
            "{sha}\t{}\t{took}\t{}\t{who}\t{}\t{}\n",
            env.word(),
            util::now_iso(),
            if flaky { "flaky" } else { "-" },
            escape(command)
        ));
        let tmp = self.file.with_extension("tsv.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &self.file);
        }
    }
}

/// Whether `tree` is clean at HEAD and HEAD is `sha`; ignored files do not count.
pub(crate) fn clean_at(tree: &Path, sha: &str) -> bool {
    let Some(head) = git(tree, &["rev-parse", "HEAD"]) else { return false };
    head.trim() == sha && git(tree, &["status", "--porcelain"]).is_some_and(|s| s.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "0123456789012345678901234567890123456789";

    fn ledger(name: &str) -> (util::Isolated, Greens) {
        let env = util::isolated(name);
        let greens = Greens::open(&Store::open(), "pj", Path::new("/x/repo"));
        (env, greens)
    }

    #[test]
    fn a_green_is_found_by_its_commit_and_exact_command_and_nothing_else() {
        let (_s, g) = ledger("greens-key");
        g.record(SHA, "cargo test", Env::Warm, 101, "wsp-1", false);
        let found = g.lookup(SHA, "cargo test", Env::Warm).unwrap();
        assert_eq!((found.took, found.who.as_str(), found.flaky), (101, "wsp-1", false));
        assert!(g.lookup(SHA, "cargo test --release", Env::Warm).is_none());
        assert!(g.lookup(&SHA.replace('0', "f"), "cargo test", Env::Warm).is_none());
    }

    #[test]
    fn a_cold_green_satisfies_a_warm_ask_and_a_warm_green_does_not_satisfy_a_cold_one() {
        let (_s, g) = ledger("greens-env");
        g.record(SHA, "warm one", Env::Warm, 1, "a", false);
        g.record(SHA, "cold one", Env::Cold, 1, "a", false);
        assert!(g.lookup(SHA, "cold one", Env::Warm).is_some());
        assert!(g.lookup(SHA, "warm one", Env::Cold).is_none());
        assert!(g.lookup(SHA, "cold one", Env::Cold).is_some());
    }

    #[test]
    fn a_multiline_command_and_a_flake_round_trip_and_unreadable_lines_are_skipped() {
        let (_s, g) = ledger("greens-lines");
        std::fs::create_dir_all(g.file.parent().unwrap()).unwrap();
        std::fs::write(&g.file, "garbage\n\t\t\n").unwrap();
        g.record(SHA, "a\n\tb", Env::Warm, 2, "barrier l 1", true);
        let found = g.lookup(SHA, "a\n\tb", Env::Warm).unwrap();
        assert!(found.flaky);
        assert_eq!(found.who, "barrier_l_1");
        assert_eq!(g.lines().len(), 1, "the garbage line is dropped when the file is appended to");
    }

    #[test]
    fn lines_older_than_thirty_days_are_dropped_on_the_next_append() {
        let (_s, g) = ledger("greens-old");
        std::fs::create_dir_all(g.file.parent().unwrap()).unwrap();
        let old = util::iso_at(util::epoch_secs() - 31 * 86_400);
        std::fs::write(&g.file, format!("{SHA}\twarm\t5\t{old}\tx\t-\told\n")).unwrap();
        assert!(g.lookup(SHA, "old", Env::Warm).is_some(), "still read until the next append");
        g.record(SHA, "new", Env::Warm, 1, "x", false);
        assert!(g.lookup(SHA, "old", Env::Warm).is_none());
        assert!(g.lookup(SHA, "new", Env::Warm).is_some());
    }
}
