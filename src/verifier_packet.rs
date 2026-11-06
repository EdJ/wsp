//! The verification packet a React row's verifier is spawned with
//! (`compounding-104`).
//!
//! From compounding-079's trial (`docs/verifier-trial/verdict.md` in the
//! compounding project): a verifier given the packet caught as many expected
//! blocks as one that reads the tree (5 of 7 each), at 0.45 of the tokens, half
//! the requests and a lower median wall. It did not cut dollars. This is
//! `spikes/verifier-trial/packet.py` made wsp's own, so that wsp builds it and
//! nobody does by hand: the overview, the stop clause, every commit message
//! oldest first, the seat's review summary, the row's check plan with the greens
//! ledger, the net diff with run output elided to a line, and the one command.
//!
//! **A React row is one whose changed files sit in a directory with a
//! `package.json` that declares `check:box`.** Any other row has no packet, and
//! its verifier's order is what it was; nothing is claimed for a Rust row.

use std::path::Path;

use crate::cmd_verify::git;
use crate::model::Task;
use crate::store::Store;

/// The packet is cut here: a verifier's brief is read whole, and a diff larger
/// than this is a row for the tree, not for the brief.
const CAP: usize = 160 * 1024;

/// A `.json` over this many changed lines is run output.
const BIG_JSON: u64 = 200;

/// What a verifier is told of a red, beside the packet (VS-c02 was a false
/// block on two 30 s timeouts at load 108).
pub(crate) const RERUN: &str = "**A red on a Playwright timeout, or on N25, is rerun once before it is a block.** \
     Run the one command again; only a second red of the same kind blocks. Say in the verdict that you reran it \
     and what the load was (`uptime`).";

/// Whether a changed path is run output, which the packet names in a line and
/// does not print: raw run logs, timelines, answers, drives, lockfiles, images.
pub(crate) fn is_run_output(path: &str, changed: u64) -> bool {
    let p = format!("/{path}");
    const HAS: [&str; 6] = ["/results/", "/spikes/react-ports/legs/", "-drive/", "-packets/", "/drive/", "package-lock.json"];
    const ENDS: [&str; 6] = ["/timeline.json", "/answer.txt", "/load.txt", ".png", ".jsonl", ".log"];
    HAS.iter().any(|h| p.contains(h)) || ENDS.iter().any(|e| p.ends_with(e)) || (p.ends_with(".json") && changed > BIG_JSON)
}

/// The directory of `files` that holds a `package.json` declaring `check:box`,
/// at `sha`: the React workspace the row works in. The first such directory a
/// changed file sits under, the nearest ancestor first.
pub(crate) fn workspace(repo: &Path, sha: &str, files: &[String]) -> Option<String> {
    for f in files {
        let mut dir = Path::new(f).parent();
        while let Some(d) = dir {
            let pkg = match d.as_os_str().is_empty() {
                true => "package.json".to_string(),
                false => format!("{}/package.json", d.display()),
            };
            if git(repo, &["show", &format!("{sha}:{pkg}")]).is_some_and(|t| t.contains("\"check:box\"")) {
                return Some(d.display().to_string());
            }
            dir = d.parent();
        }
    }
    None
}

/// The boxes `scripts/check.mjs` declares, as `(name, directory)`: its entries
/// are `name: [["glob"], "directory"]`.
pub(crate) fn boxes(check_mjs: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in check_mjs.lines() {
        let Some((name, rest)) = line.trim().split_once(": [[\"") else { continue };
        if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        let Some((_, dir)) = rest.split_once("\"], \"") else { continue };
        let Some((dir, _)) = dir.split_once('"') else { continue };
        out.push((name.to_string(), dir.to_string()));
    }
    out
}

/// The one command: the box check when the code sits in one box and only `src/`
/// and `e2e/` changed, else the whole check. `ws` is the workspace directory
/// (`""` for the root) and `files` the row's changed paths from the repo root.
pub(crate) fn command(ws: &str, files: &[String], check_mjs: &str) -> String {
    let pre = if ws.is_empty() { String::new() } else { format!("{ws}/") };
    let code_ext = [".ts", ".tsx", ".css", ".html", ".mjs", ".json"];
    let code: Vec<&String> = files
        .iter()
        .filter(|f| f.starts_with(&pre) && !f.starts_with(&format!("{pre}packet/")) && code_ext.iter().any(|e| f.ends_with(e)))
        .collect();
    let src: Vec<&&String> = code.iter().filter(|f| f.starts_with(&format!("{pre}src/")) || f.starts_with(&format!("{pre}e2e/"))).collect();
    let boxes = boxes(check_mjs);
    let hit: std::collections::BTreeSet<&str> = src
        .iter()
        .flat_map(|f| {
            let pre = &pre;
            boxes.iter().filter(move |(b, d)| f.starts_with(&format!("{pre}{d}/")) || ***f == format!("{pre}e2e/{b}.spec.ts")).map(|(b, _)| b.as_str())
        })
        .collect();
    // src/main.tsx, keys.ts, styles.css: wiring, in no box.
    let wiring = |f: &str| f.strip_prefix(&format!("{pre}src/")).is_some_and(|r| !r.contains('/'));
    let stray = src.iter().any(|f| {
        f.starts_with(&format!("{pre}src/")) && !wiring(f) && !boxes.iter().any(|(_, d)| f.starts_with(&format!("{pre}{d}/")))
    });
    if !boxes.is_empty() && !code.is_empty() && src.len() == code.len() && hit.len() == 1 && !stray {
        let b = hit.iter().next().copied().unwrap_or_default();
        let named: Vec<&str> = src
            .iter()
            .filter(|f| f.starts_with(&format!("{pre}src/")) && !f.contains(".test."))
            .map(|f| &f[pre.len()..])
            .collect();
        return format!("npm run check:box -- {b} {}", named.join(" ")).trim_end().to_string();
    }
    "npm run check".to_string()
}

/// The seat's own review line for this commit: the last one before the last
/// line that names the commit as ready or landed; failing that, the last review
/// line there is.
pub(crate) fn summary_at(log: &str, sha: &str) -> Option<String> {
    let lines: Vec<&str> = log.lines().collect();
    let short = &sha[..7.min(sha.len())];
    let named = lines.iter().rposition(|l| {
        [crate::repair::READY, crate::repair::LANDED].iter().any(|m| l.contains(&format!("{m} {short}")))
    });
    let review = |l: &str| l.strip_prefix("- ").and_then(|r| r.split_once(' ')).and_then(|(_, r)| r.strip_prefix("review: ")).map(str::to_string);
    let reviews: Vec<(usize, String)> = lines.iter().enumerate().filter_map(|(i, l)| review(l).map(|r| (i, r))).collect();
    let before = reviews.iter().rev().find(|(i, _)| named.is_some_and(|n| *i < n));
    before.or(reviews.last()).map(|(_, r)| r.clone())
}

/// The lines of the overview that state when it is done.
fn stop_lines(overview: &str) -> Vec<&str> {
    overview.lines().filter(|l| l.contains("Done when") || l.contains("Stop")).collect()
}

/// The packet for a row's verifier, and `None` for a row that is not a React
/// row (or whose commit, tree or history cannot be read).
pub(crate) fn build(store: &Store, t: &Task) -> Option<String> {
    let tg = crate::check::target(store, t, None).ok()?;
    let (repo, sha) = (tg.repo.clone(), tg.sha.clone());
    let mut commits: Vec<String> = git(
        &repo,
        &["log", "--format=%H", "-E", &format!("--grep=^{}([^0-9]|$)", t.id), &sha, "--max-count=40"],
    )?
    .split_whitespace()
    .map(String::from)
    .collect();
    commits.reverse();
    if commits.is_empty() {
        commits.push(sha.clone());
    }
    let base = git(&repo, &["rev-parse", &format!("{}^", commits[0])])?.trim().to_string();
    let mut files: Vec<String> = git(&repo, &["diff", "--name-only", &base, &sha])?.lines().map(String::from).collect();
    files.sort();
    let ws = workspace(&repo, &sha, &files)?;

    let mut out = String::new();
    out.push_str(&format!("# Verification packet · {} · ready at {}\n\n", t.id, &sha[..12.min(sha.len())]));
    out.push_str(
        "Everything you need to decide is below, so **do not read the tree for what the packet already holds**: \
         the overview, the stop clause, the commits, the seat's summary, the check plan and the diff. Read a file \
         only where the diff does not show enough of it. The tree is at the ready commit.\n\n",
    );
    let overview = t.section("Overview").unwrap_or_default();
    out.push_str(&format!("## Row\n\n{}\n\n## Stop clause\n\n", overview.trim()));
    let stop = stop_lines(&overview);
    match stop.is_empty() {
        true => out.push_str("The overview states no 'Done when' line: the bullets above are the brief.\n\n"),
        false => out.push_str(&format!("{}\n\n", stop.join("\n"))),
    }
    out.push_str("## Commit messages (oldest first)\n\n");
    for c in &commits {
        let m = git(&repo, &["log", "-1", "--format=%h %an %ad%n%B", "--date=short", c]).unwrap_or_default();
        out.push_str(&format!("```\n{}\n```\n", m.trim()));
    }
    let log = t.section("Log").unwrap_or_default();
    out.push_str(&format!(
        "\n## Summary given at review\n\n{}\n\n",
        summary_at(&log, &sha).unwrap_or_else(|| "(no review line is recorded for this commit)".into())
    ));
    out.push_str(&format!("## Declared checks at this commit\n\n```\n{}\n```\n\n", crate::check::plan_text(store, t, &tg).trim_end()));

    out.push_str(&format!(
        "## Diff\n\nNet diff of the {} file(s) this row touched, `{}..{}`.\n\n",
        files.len(),
        &base[..12.min(base.len())],
        &sha[..12.min(sha.len())]
    ));
    let stat = git(&repo, &["diff", "--stat=140", &base, &sha]).unwrap_or_default();
    out.push_str(&format!("```\n{}\n```\n\n", stat.trim()));
    let (mut elided, mut body) = (Vec::new(), String::new());
    for f in &files {
        let num = git(&repo, &["diff", "--numstat", &base, &sha, "--", f]).unwrap_or_default();
        let mut w = num.split_whitespace();
        let (a, b) = (w.next().and_then(|x| x.parse::<u64>().ok()), w.next().and_then(|x| x.parse::<u64>().ok()));
        let n = a.unwrap_or(0) + b.unwrap_or(0);
        if a.is_none() || b.is_none() || is_run_output(f, n) {
            elided.push(format!("`{f}` [{n}]"));
            continue;
        }
        body.push_str(&git(&repo, &["diff", "--no-color", "-M", &base, &sha, "--", f]).unwrap_or_default());
    }
    if !elided.is_empty() {
        out.push_str(&format!(
            "Elided as run output (changed lines in brackets): {}. `git show {} -- <path>` in the tree prints any of them.\n\n",
            elided.join(", "),
            &sha[..12.min(sha.len())]
        ));
    }
    if out.len() + body.len() > CAP {
        let room = CAP.saturating_sub(out.len());
        let mut cut = room.min(body.len());
        while !body.is_char_boundary(cut) {
            cut -= 1;
        }
        body.truncate(cut);
        body.push_str("\n… (the diff is cut here: `git diff` in the tree prints the rest)\n");
    }
    out.push_str(&format!("```diff\n{}\n```\n\n", body.trim_end()));

    let mjs = git(&repo, &["show", &format!("{sha}:{}", if ws.is_empty() { "scripts/check.mjs".into() } else { format!("{ws}/scripts/check.mjs") })])
        .unwrap_or_default();
    let tree = crate::cmd_checkout::member_tree(store, &t.id).unwrap_or_else(|| repo.clone());
    let there = if ws.is_empty() { crate::util::contract(&tree) } else { format!("{}/{ws}", crate::util::contract(&tree)) };
    out.push_str(&format!(
        "## The one command you may run\n\nIn `{there}`:\n```\n{}\n```\n\
         A verifier runs the box check, never the full suite.\n\n{RERUN}\n",
        command(&ws, &files, &mjs)
    ));
    Some(out)
}

/// The packet written whole to a file under the store, for the order that has
/// to be one line. `None` for a row with no packet, or when the file cannot be
/// written (the order then goes without it, as it did).
pub(crate) fn file(store: &Store, t: &Task) -> Option<std::path::PathBuf> {
    let packet = build(store, t)?;
    let dir = store.state_file("packets");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{}.md", t.id));
    std::fs::write(&path, packet).ok()?;
    Some(path)
}

/// What the one-line order says of the packet's file, and the rule for a red,
/// which is short enough to say in the line itself.
pub(crate) fn pointer(path: &Path) -> String {
    format!(
        "**Your verification packet is in `{}`: read it first, whole.** It is markdown with its lines kept (a diff \
         cannot be said on one line) and holds the overview, the stop clause, the commits, the seat's review summary, \
         the check plan, the diff and the one command, so do not read the tree for what it holds. {}",
        path.display(),
        RERUN
    )
}

/// A React row for a test: a repo under the store with a workspace `web`
/// declaring `check:box`, a project `pj` rooted there, and a row `pj-001` in
/// review at a commit that changes `web/src/pane/Pane.tsx` (one line out, one
/// in) and `pj-002` at one that changes `b.rs`. Returns the first row's commit.
#[cfg(test)]
pub(crate) fn react_fixture(store: &Store) -> String {
    use crate::model::{Project, Status};
    let repo = store.root.join("react-repo");
    let _ = std::fs::remove_dir_all(&repo);
    std::fs::create_dir_all(repo.join("web/src/pane")).unwrap();
    std::fs::create_dir_all(repo.join("web/scripts")).unwrap();
    let run = |a: &[&str]| {
        let o = std::process::Command::new("git").arg("-C").arg(&repo).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(a).output().unwrap();
        assert!(o.status.success(), "{a:?}: {}", String::from_utf8_lossy(&o.stderr));
    };
    run(&["init", "-q", "-b", "master"]);
    std::fs::write(repo.join("web/package.json"), "{\"scripts\":{\"check\":\"x\",\"check:box\":\"x\"}}").unwrap();
    std::fs::write(repo.join("web/scripts/check.mjs"), "const BOXES = {\n  pane: [[\"src/pane\"], \"src/pane\"],\n};\n").unwrap();
    std::fs::write(repo.join("web/src/pane/Pane.tsx"), "export function a() {\n  old();\n}\n").unwrap();
    std::fs::write(repo.join("b.rs"), "fn b() {}\n").unwrap();
    run(&["add", "."]);
    run(&["commit", "-qm", "init"]);
    std::fs::write(repo.join("web/src/pane/Pane.tsx"), "export function a() {\n  next();\n}\n").unwrap();
    run(&["commit", "-qam", "pj-001: the pane calls next"]);
    let react = crate::cmd_verify::git(&repo, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
    std::fs::write(repo.join("b.rs"), "fn b() { 1; }\n").unwrap();
    run(&["commit", "-qam", "pj-002: b does one"]);
    let rust = crate::cmd_verify::git(&repo, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let mut p = Project::new("pj");
    p.roots = vec![repo.display().to_string()];
    store.save_project(&p).unwrap();
    for (id, sha) in [("pj-001", &react), ("pj-002", &rust)] {
        let mut t = Task::new("a row", id);
        t.project = Some("pj".into());
        t.set_status(Status::Review);
        t.body = format!("## Overview\nMake it so.\n\nDone when: the pane calls next.\n\n## Log\n\n- 2026-11-06T10:00:00Z review: it is done\n- 2026-11-06T10:00:01Z {} {sha}\n", crate::repair::READY);
        store.save_task(&t).unwrap();
    }
    react
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world(tag: &str) -> (crate::util::Isolated, Store) {
        let env = crate::util::isolated(&format!("vpk-{tag}"));
        (env, Store::open())
    }

    /// `compounding-104`: a React row's verifier is handed its packet, and the
    /// order that is typed into a seat as one line names the packet's file
    /// instead of folding the diff into itself.
    #[test]
    fn a_react_rows_verifier_gets_its_packet_whole_and_the_one_line_order_names_the_file() {
        let (_env, store) = world("react");
        let sha = react_fixture(&store);
        let t = store.find_task("pj-001").unwrap();

        let order = crate::cycle::verifier_order(&store, &t);
        assert!(order.contains("# Verification packet · pj-001"), "{order}");
        assert!(order.contains("Done when: the pane calls next."), "the stop clause");
        assert!(order.contains("pj-001: the pane calls next"), "the commit message");
        assert!(order.contains("it is done"), "the seat's review summary");
        assert!(order.contains("\n-  old();\n+  next();\n"), "the diff keeps its lines: {order}");
        assert!(order.contains("npm run check:box -- pane src/pane/Pane.tsx"), "the one command: {order}");
        assert!(order.contains("rerun") && order.contains("N25"), "{order}");

        let line = crate::cycle::verifier_order_line(&store, &t);
        assert!(!line.contains('\n'), "what the spawn types is one line: {line:?}");
        assert!(!line.contains("old();"), "and does not fold the diff into itself");
        let path = store.state_file("packets").join("pj-001.md");
        assert!(line.contains(&path.display().to_string()) && line.contains("read it first"), "it names the file: {line}");
        assert!(line.contains("N25"), "and carries the rule for a red");
        let packet = std::fs::read_to_string(&path).unwrap();
        assert!(packet.contains("\n-  old();\n+  next();\n") && packet.contains(&sha[..12]), "the file is the packet, newlines kept: {packet}");
    }

    /// Nothing is claimed for a row that is not React.
    #[test]
    fn a_rust_rows_verifier_gets_no_packet_and_the_same_one_line_order() {
        let (_env, store) = world("rust");
        react_fixture(&store);
        let t = store.find_task("pj-002").unwrap();
        let order = crate::cycle::verifier_order(&store, &t);
        assert!(!order.contains("Verification packet"), "{order}");
        let line = crate::cycle::verifier_order_line(&store, &t);
        assert_eq!(line, crate::cmd_task::fold(&order));
        assert!(!store.state_file("packets").exists());
    }

    const MJS: &str = "const BOXES = {\n  pane: [[\"src/pane\"], \"src/pane\"],\n  tabs: [[\"src/tabs\"], \"src/tabs\"],\n};\n";

    fn f(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn run_output_is_elided_and_code_is_not() {
        assert!(is_run_output("spikes/x/results/a.txt", 3));
        assert!(is_run_output("web/package-lock.json", 3));
        assert!(is_run_output("a/shot.png", 0));
        assert!(is_run_output("a/big.json", 201) && !is_run_output("a/big.json", 200));
        assert!(!is_run_output("web/src/pane/Pane.tsx", 900));
    }

    #[test]
    fn boxes_are_read_off_the_check_script() {
        assert_eq!(boxes(MJS), vec![("pane".to_string(), "src/pane".to_string()), ("tabs".to_string(), "src/tabs".to_string())]);
    }

    #[test]
    fn one_box_with_only_src_and_e2e_is_the_box_check_and_anything_else_the_full_one() {
        let ws = "web";
        let one = f(&["web/src/pane/Pane.tsx", "web/src/pane/Pane.test.tsx", "web/e2e/pane.spec.ts"]);
        assert_eq!(command(ws, &one, MJS), "npm run check:box -- pane src/pane/Pane.tsx");
        let two = f(&["web/src/pane/Pane.tsx", "web/src/tabs/Tabs.tsx"]);
        assert_eq!(command(ws, &two, MJS), "npm run check");
        let outside = f(&["web/src/pane/Pane.tsx", "web/vite.config.ts"]);
        assert_eq!(command(ws, &outside, MJS), "npm run check", "a file outside src/ and e2e/ is not one box");
        let stray = f(&["web/src/pane/Pane.tsx", "web/src/other/X.tsx"]);
        assert_eq!(command(ws, &stray, MJS), "npm run check", "a source in no box is not one box");
        assert_eq!(command(ws, &f(&["web/src/main.tsx", "web/src/pane/Pane.tsx"]), MJS), "npm run check:box -- pane src/main.tsx src/pane/Pane.tsx", "wiring rides with the box");
        assert_eq!(command(ws, &f(&["docs/a.md"]), MJS), "npm run check", "no code the checks cover");
        assert_eq!(command(ws, &one, ""), "npm run check", "no boxes declared");
    }

    #[test]
    fn the_summary_is_the_review_line_before_the_ready_line() {
        let log = "- 2026-11-05 review: first\n- 2026-11-05 wsp: ready abcdef1234\n- 2026-11-05 review: later\n";
        assert_eq!(summary_at(log, "abcdef1234567").as_deref(), Some("first"));
        assert_eq!(summary_at(log, "9999999").as_deref(), Some("later"), "no line names the commit: the last review");
        assert_eq!(summary_at("- 2026-11-05 doing\n", "abcdef1"), None);
    }

    #[test]
    fn a_workspace_is_found_by_its_package_json_declaring_check_box() {
        let dir = std::env::temp_dir().join(format!("wsp-vp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("web/src")).unwrap();
        std::fs::write(dir.join("web/package.json"), "{\"scripts\":{\"check:box\":\"node x\"}}").unwrap();
        std::fs::write(dir.join("web/src/a.ts"), "1").unwrap();
        std::fs::write(dir.join("b.rs"), "1").unwrap();
        let run = |a: &[&str]| {
            std::process::Command::new("git").arg("-C").arg(&dir).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(a).output().unwrap();
        };
        run(&["init", "-q"]);
        run(&["add", "."]);
        run(&["commit", "-qm", "x"]);
        let sha = git(&dir, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        assert_eq!(workspace(&dir, &sha, &f(&["web/src/a.ts"])).as_deref(), Some("web"));
        assert_eq!(workspace(&dir, &sha, &f(&["b.rs"])), None, "a Rust row has no packet");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
