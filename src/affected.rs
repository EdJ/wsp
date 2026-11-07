//! Affected tests and what a test reads (`wsp-361`).
//!
//! Ed, 2026-11-03: per-ticket checks must stay cheap as suites grow, and the
//! heavy ones belong at the barrier. Two questions are answered here.
//!
//! - **Which targets a diff can reach** ([`derived`]). Rust: the crate graph
//!   from `cargo metadata --no-deps` — a crate a path changed in runs its tests,
//!   then its reverse dependents', nearest first; `Cargo.lock` or the root
//!   `Cargo.toml` reaches every crate. TypeScript: a changed source in a package
//!   that depends on Vitest runs `npx vitest related --run <files>`, Vitest's own
//!   module graph. Playwright specs are mapped by the project's own rules
//!   (`- name on <globs>: …`), as compounding-074's `check:box` does by hand; the
//!   rules' `{1}` captures are untouched.
//! - **What a target reads** ([`key`]): the input hash the greens ledger keys a
//!   pass by beside its commit, so a green is reused at any commit where what
//!   the command reads is unchanged. A plain `cargo` command (one line, nothing
//!   chained) reads its crates and their workspace path dependencies — each
//!   non-root crate's whole directory; for the root crate `Cargo.toml`,
//!   `build.rs`, `src/`, `tests/`, `benches/`, `examples/` and every file an
//!   `include_str!`/`include_bytes!` in them names — plus `Cargo.lock` and the
//!   root `Cargo.toml`. A leading `cd <dir> &&` (one, nothing else chained,
//!   `NAME=$VAR` prefixes allowed) puts the same read under `<dir>`, and for
//!   `npm run check:box -- <box> <paths>` it is `<dir>`'s tree id. Anything else
//!   reads the whole tree, so its key is the commit's tree: reused across a
//!   rebase that changed nothing, never more.
//!
//! The hash is of git's object ids at the commit, not of files on disk, so it
//! is the same whoever computes it and costs one `git ls-tree`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use wsp_base::calls::Command;

use crate::check::{Of, Step};
use crate::cmd_verify::git;

/// A workspace crate: its package name, its directory relative to the
/// workspace root (`""` for the root), and the workspace crates it depends on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Crate {
    pub name: String,
    pub dir: String,
    pub deps: Vec<String>,
}

/// The workspace's crates, from `cargo metadata --no-deps` in `tree`.
pub(crate) fn crates(tree: &Path) -> Option<Vec<Crate>> {
    crates_of(&metadata(tree)?)
}

/// `cargo metadata --no-deps` in `tree`.
fn metadata(tree: &Path) -> Option<serde_json::Value> {
    if !tree.join("Cargo.toml").exists() {
        return None;
    }
    let out = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1", "--offline"])
        .current_dir(tree)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

/// The crates in a `cargo metadata` answer.
pub(crate) fn crates_of(v: &serde_json::Value) -> Option<Vec<Crate>> {
    let root = PathBuf::from(v.get("workspace_root")?.as_str()?);
    let rel = |p: &Path| -> Option<String> { p.strip_prefix(&root).ok().map(|r| r.to_string_lossy().trim_end_matches('/').to_string()) };
    let mut found = Vec::new();
    let mut dirs: Vec<(String, String)> = Vec::new();
    for p in v.get("packages")?.as_array()? {
        let name = p.get("name")?.as_str()?.to_string();
        let manifest = PathBuf::from(p.get("manifest_path")?.as_str()?);
        let dir = rel(manifest.parent()?)?;
        dirs.push((dir.clone(), name.clone()));
        let deps: Vec<String> = p
            .get("dependencies")
            .and_then(|d| d.as_array())
            .map(|d| d.iter().filter_map(|d| d.get("path").and_then(|x| x.as_str()).and_then(|x| rel(Path::new(x)))).collect())
            .unwrap_or_default();
        found.push(Crate { name, dir, deps });
    }
    // A dependency is read by its directory; name it by its crate.
    for c in &mut found {
        c.deps = c.deps.iter().filter_map(|d| dirs.iter().find(|(dir, _)| dir == d).map(|(_, n)| n.clone())).collect();
        c.deps.sort();
        c.deps.dedup();
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    Some(found)
}

/// The crate a path belongs to: the one whose directory is its longest prefix.
pub(crate) fn owner<'a>(crates: &'a [Crate], path: &str) -> Option<&'a Crate> {
    crates
        .iter()
        .filter(|c| c.dir.is_empty() || path == c.dir || path.starts_with(&format!("{}/", c.dir)))
        .max_by_key(|c| c.dir.len())
}

/// The crates the changed paths reach, nearest first: those a path changed in
/// (distance 0), then their reverse dependents by distance. `Cargo.lock` or the
/// root `Cargo.toml` reaches every crate. Paths no crate owns reach nothing.
pub(crate) fn reached(crates: &[Crate], changed: &[String]) -> Vec<(String, usize)> {
    let everything = changed.iter().any(|p| p == "Cargo.lock" || p == "Cargo.toml");
    let mut out: Vec<(String, usize)> = Vec::new();
    let mut frontier: Vec<String> = if everything {
        crates.iter().map(|c| c.name.clone()).collect()
    } else {
        let mut f: Vec<String> = changed
            .iter()
            .filter(|p| !p.ends_with(".md"))
            .filter_map(|p| owner(crates, p))
            .filter(|c| {
                // The root crate owns every path; only its own sources reach it.
                !c.dir.is_empty() || changed.iter().any(|p| reads_root(p))
            })
            .map(|c| c.name.clone())
            .collect();
        f.sort();
        f.dedup();
        f
    };
    let mut distance = 0;
    while !frontier.is_empty() {
        for n in &frontier {
            out.push((n.clone(), distance));
        }
        let mut next: Vec<String> = crates
            .iter()
            .filter(|c| !out.iter().any(|(n, _)| *n == c.name) && c.deps.iter().any(|d| frontier.contains(d)))
            .map(|c| c.name.clone())
            .collect();
        next.sort();
        next.dedup();
        frontier = next;
        distance += 1;
    }
    out
}

fn reads_root(p: &str) -> bool {
    ["src/", "tests/", "benches/", "examples/"].iter().any(|d| p.starts_with(d)) || p == "build.rs"
}

/// The targets a tree's changed paths reach that no declared rule names:
/// `crate:<name>` per reached crate, nearest first, and `vitest:related` for
/// changed sources in a package that uses Vitest. Each is an `inner` or `row`
/// step a project need not declare.
pub(crate) fn derived(tree: &Path, changed: &[String]) -> Vec<(Step, usize)> {
    let mut out = Vec::new();
    if let Some(cs) = crates(tree) {
        let many = cs.len() > 1;
        for (name, d) in reached(&cs, changed) {
            let command = if many { format!("cargo test -p {name}") } else { "cargo test".to_string() };
            out.push((derived_step(&format!("crate:{name}"), &command), d));
        }
    }
    let sources: Vec<&String> = changed
        .iter()
        .filter(|p| [".ts", ".tsx", ".js", ".jsx", ".mts", ".vue", ".svelte"].iter().any(|e| p.ends_with(e)))
        .filter(|p| tree.join(p).exists())
        .filter(|p| !crate::check::captures("**/*.spec.*", p).is_some())
        .collect();
    let vitest = std::fs::read_to_string(tree.join("package.json")).is_ok_and(|t| t.contains("\"vitest\""));
    if vitest && !sources.is_empty() && sources.iter().all(|p| plain_path(p)) {
        let files: Vec<&str> = sources.iter().map(|s| s.as_str()).collect();
        out.push((derived_step("vitest:related", &format!("npx vitest related --run {}", files.join(" "))), 0));
    }
    out
}

fn plain_path(p: &str) -> bool {
    !p.is_empty() && p.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '@' | '+'))
}

fn derived_step(name: &str, command: &str) -> Step {
    Step {
        name: name.to_string(),
        command: command.to_string(),
        limit: std::time::Duration::from_secs(1200),
        of: Of::Rule,
        on: Vec::new(),
        barrier: false,
        end: false,
    }
}

/// Whether a command is one plain `cargo` invocation, so its inputs can be read
/// from the crate graph.
fn plain_cargo(command: &str) -> bool {
    let c = command.trim();
    c.starts_with("cargo ") && !c.contains('\n') && !c.chars().any(|ch| matches!(ch, ';' | '|' | '&' | '>' | '<' | '`' | '$' | '('))
}

/// The crates a plain cargo command builds: `-p`/`--package` names, else every
/// workspace crate.
fn named_crates(command: &str, crates: &[Crate]) -> Vec<String> {
    let words: Vec<&str> = command.split_whitespace().collect();
    let mut named = Vec::new();
    for (i, w) in words.iter().enumerate() {
        if *w == "--" {
            break;
        }
        if (*w == "-p" || *w == "--package") && i + 1 < words.len() {
            named.push(words[i + 1].to_string());
        } else if let Some(n) = w.strip_prefix("--package=").or_else(|| w.strip_prefix("-p").filter(|n| !n.is_empty())) {
            named.push(n.to_string());
        }
    }
    if named.is_empty() || words.iter().any(|w| *w == "--workspace" || *w == "--all") {
        return crates.iter().map(|c| c.name.clone()).collect();
    }
    named
}

/// What a command reads.
#[derive(Debug, PartialEq, Eq)]
enum Reads {
    /// These repository paths (files or directories).
    Paths(Vec<String>),
    /// Everything under one directory.
    Dir(String),
    /// Anything: the whole tree.
    Whole,
}

/// A command split at a leading `cd <dir> &&`: the directory (`""` when there
/// is none) and what runs there, with its `NAME=value` prefixes dropped. `None`
/// for a command that chains anything else, or whose directory is not a plain
/// relative path inside the tree.
fn scoped(command: &str) -> Option<(String, String)> {
    let c = command.trim();
    if c.contains('\n') || c.chars().any(|ch| matches!(ch, ';' | '|' | '>' | '<' | '`' | '(' | ')')) {
        return None;
    }
    let (dir, rest) = match c.split_once("&&") {
        None => (String::new(), c),
        Some((head, rest)) => {
            let mut words = head.split_whitespace().skip_while(|w| env_word(w));
            if words.next()? != "cd" {
                return None;
            }
            let d = words.next()?;
            if words.next().is_some() {
                return None;
            }
            let d = d.strip_prefix("./").unwrap_or(d).trim_end_matches('/');
            if !plain_path(d) || d.starts_with('/') || d.split('/').any(|p| p.is_empty() || p == "." || p == "..") {
                return None;
            }
            (d.to_string(), rest)
        }
    };
    if rest.contains('&') {
        return None;
    }
    let inner: Vec<&str> = rest.split_whitespace().skip_while(|w| env_word(w)).collect();
    Some((dir, inner.join(" ")))
}

/// `NAME=value` where the value is plain text or a `$VAR` such as `$WSP_SCRATCH`.
fn env_word(w: &str) -> bool {
    let Some((name, value)) = w.split_once('=') else { return false };
    !name.is_empty()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && value.chars().all(|c| plain_path(&c.to_string()) || matches!(c, '$' | '{' | '}' | ':' | ','))
}

/// `npm run check:box -- <box> <paths>` with paths that stay in the package.
fn check_box(inner: &str) -> bool {
    let Some(args) = inner.strip_prefix("npm run check:box") else { return false };
    args.split_whitespace().all(|a| a == "--" || (plain_path(a) && !a.starts_with('/') && !a.split('/').any(|p| p == "..")))
}

/// What a command reads.
fn reads(tree: &Path, command: &str) -> Reads {
    let Some((dir, inner)) = scoped(command) else { return Reads::Whole };
    if plain_cargo(&inner) && !inner.contains("--manifest-path") {
        return cargo_inputs(tree, &dir, &inner).map_or(Reads::Whole, Reads::Paths);
    }
    if !dir.is_empty() && check_box(&inner) {
        return Reads::Dir(dir);
    }
    Reads::Whole
}

/// The repository paths a command reads, or `None` for the whole tree or one
/// directory of it.
#[cfg(test)]
pub(crate) fn inputs(tree: &Path, command: &str) -> Option<Vec<String>> {
    match reads(tree, command) {
        Reads::Paths(p) => Some(p),
        _ => None,
    }
}

/// The paths a plain cargo command run in `base` (relative to `tree`) reads, or
/// `None` when they cannot be told: the workspace is not inside the tree, or
/// `cargo metadata` fails. A workspace rooted above `base` is read from its own
/// root; a path dependency outside the workspace but inside the tree adds its
/// directory (and its own such dependencies'); one outside the tree is not a
/// file of this repository, so a commit cannot key it.
fn cargo_inputs(tree: &Path, base: &str, command: &str) -> Option<Vec<String>> {
    let top = tree.canonicalize().ok()?;
    let meta = metadata(&tree.join(base))?;
    let root = PathBuf::from(meta.get("workspace_root")?.as_str()?);
    let root_rel = root.canonicalize().ok()?.strip_prefix(&top).ok()?.to_string_lossy().trim_end_matches('/').to_string();
    let cs = crates_of(&meta)?;
    let mut want = named_crates(command, &cs);
    if want.iter().any(|n| !cs.iter().any(|c| c.name == *n)) {
        return None;
    }
    let mut i = 0;
    while i < want.len() {
        if let Some(c) = cs.iter().find(|c| c.name == want[i]) {
            for d in &c.deps {
                if !want.contains(d) {
                    want.push(d.clone());
                }
            }
        }
        i += 1;
    }
    let at = |p: &str| match (root_rel.is_empty(), p.is_empty()) {
        (true, _) => p.to_string(),
        (false, true) => root_rel.clone(),
        (false, false) => format!("{root_rel}/{p}"),
    };
    let mut paths = vec![at("Cargo.lock"), at("Cargo.toml")];
    for c in cs.iter().filter(|c| want.contains(&c.name)) {
        if c.dir.is_empty() {
            for d in ["build.rs", "src", "tests", "benches", "examples"] {
                paths.push(at(d));
                paths.extend(included(tree, Path::new(&at(d))));
            }
        } else {
            paths.push(at(&c.dir));
        }
    }
    // Path dependencies that are not workspace members.
    let mut seen = vec![root.canonicalize().ok()?];
    let mut work = vec![(meta, Some(want))];
    while let Some((m, only)) = work.pop() {
        let Some(wr) = m.get("workspace_root").and_then(|r| r.as_str()).map(PathBuf::from) else { return None };
        let wr = wr.canonicalize().ok()?;
        for p in m.get("packages")?.as_array()? {
            if only.as_ref().is_some_and(|w| !p.get("name").and_then(|n| n.as_str()).is_some_and(|n| w.iter().any(|x| x == n))) {
                continue;
            }
            for d in p.get("dependencies").and_then(|d| d.as_array()).into_iter().flatten() {
                let Some(dp) = d.get("path").and_then(|x| x.as_str()) else { continue };
                let Ok(dp) = Path::new(dp).canonicalize() else { continue };
                let Ok(rel) = dp.strip_prefix(&top) else { continue };
                if rel.as_os_str().is_empty() {
                    return None;
                }
                if dp.starts_with(&wr) && dp != wr {
                    // Inside this workspace: a member is read by the crate graph, anything else by its directory.
                    paths.push(rel.to_string_lossy().into_owned());
                } else if !dp.starts_with(&wr) {
                    paths.push(rel.to_string_lossy().into_owned());
                    if !seen.iter().any(|s| dp.starts_with(s)) {
                        seen.push(dp.clone());
                        let m = metadata(&dp)?;
                        let above = m.get("workspace_root").and_then(|r| r.as_str()).and_then(|r| Path::new(r).canonicalize().ok());
                        if let Some(above) = above.filter(|a| a != &dp) {
                            if let Ok(r) = above.strip_prefix(&top) {
                                let r = r.to_string_lossy();
                                let at = |f: &str| if r.is_empty() { f.to_string() } else { format!("{r}/{f}") };
                                paths.push(at("Cargo.toml"));
                                paths.push(at("Cargo.lock"));
                            }
                        }
                        work.push((m, None));
                    }
                }
            }
        }
    }
    paths.sort();
    paths.dedup();
    Some(paths)
}

/// The files `include_str!`/`include_bytes!` literals under `rel` name,
/// relative to the tree.
fn included(tree: &Path, rel: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let full = tree.join(rel);
    if full.is_dir() {
        if let Ok(rd) = std::fs::read_dir(&full) {
            for e in rd.flatten() {
                out.extend(included(tree, &rel.join(e.file_name())));
            }
        }
        return out;
    }
    if rel.extension().and_then(|e| e.to_str()) != Some("rs") {
        return out;
    }
    let Ok(text) = std::fs::read_to_string(&full) else { return out };
    for mac in ["include_str!(\"", "include_bytes!(\""] {
        for (at, _) in text.match_indices(mac) {
            let rest = &text[at + mac.len()..];
            let Some(end) = rest.find('"') else { continue };
            let lit = &rest[..end];
            if let Some(p) = normal(&rel.parent().unwrap_or(Path::new("")).join(lit)) {
                out.push(p);
            }
        }
    }
    out
}

/// `a/b/../c` as `a/c`; `None` for a path that leaves the tree.
fn normal(p: &Path) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for c in p.components() {
        match c {
            std::path::Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
            std::path::Component::ParentDir => {
                parts.pop()?;
            }
            std::path::Component::CurDir => {}
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// The input hash of `command` at `sha`: the command and git's object ids for
/// what it reads. `None` when git cannot say. A `cd <dir> &&` in front scopes
/// the read to `<dir>`: a cargo command's crates there, or for `npm run
/// check:box` the directory's tree id (`node_modules` is untracked, so not in it).
pub(crate) fn key(tree: &Path, sha: &str, command: &str) -> Option<String> {
    let whole = || Some(format!("tree {}", git(tree, &["rev-parse", &format!("{sha}^{{tree}}")])?.trim()));
    let listing = match reads(tree, command) {
        Reads::Paths(paths) => {
            let mut args = vec!["ls-tree", sha, "--"];
            args.extend(paths.iter().map(String::as_str));
            git(tree, &args)?
        }
        Reads::Dir(dir) => match git(tree, &["rev-parse", &format!("{sha}:{dir}")]) {
            Some(id) => format!("dir {dir} {}", id.trim()),
            None => whole()?,
        },
        Reads::Whole => whole()?,
    };
    Some(digest(&format!("{command}\n{listing}")))
}

/// 128 bits of FNV-1a: two 64-bit passes from different offsets.
pub(crate) fn digest(text: &str) -> String {
    let pass = |mut h: u64| {
        for b in text.bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h
    };
    format!("{:016x}{:016x}", pass(0xcbf2_9ce4_8422_2325), pass(0x6c62_272e_07bb_0142))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws() -> Vec<Crate> {
        let c = |n: &str, d: &str, deps: &[&str]| Crate { name: n.into(), dir: d.into(), deps: deps.iter().map(|s| s.to_string()).collect() };
        vec![c("app", "", &["web"]), c("core", "crates/core", &[]), c("web", "crates/web", &["core"]), c("tool", "crates/tool", &[])]
    }

    fn paths(p: &[&str]) -> Vec<String> {
        p.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_changed_crate_reaches_itself_then_its_reverse_dependents_nearest_first() {
        let got = reached(&ws(), &paths(&["crates/core/src/lib.rs"]));
        assert_eq!(got, [("core".to_string(), 0), ("web".to_string(), 1), ("app".to_string(), 2)]);
        assert_eq!(reached(&ws(), &paths(&["crates/tool/src/main.rs"])), [("tool".to_string(), 0)]);
    }

    #[test]
    fn the_lockfile_reaches_every_crate_and_a_doc_or_stray_path_reaches_none() {
        assert_eq!(reached(&ws(), &paths(&["Cargo.lock"])).len(), 4);
        assert!(reached(&ws(), &paths(&["README.md", "docs/x.txt", "crates/core/NOTES.md"])).is_empty());
        assert_eq!(reached(&ws(), &paths(&["src/main.rs"])), [("app".to_string(), 0)]);
    }

    #[test]
    fn crates_and_their_path_dependencies_are_read_from_cargo_metadata() {
        let v = serde_json::json!({
            "workspace_root": "/w",
            "packages": [
                { "name": "app", "manifest_path": "/w/Cargo.toml", "dependencies": [ { "name": "core", "path": "/w/crates/core" }, { "name": "serde_json" } ] },
                { "name": "core", "manifest_path": "/w/crates/core/Cargo.toml", "dependencies": [] }
            ]
        });
        let got = crates_of(&v).unwrap();
        assert_eq!(got[0], Crate { name: "app".into(), dir: "".into(), deps: vec!["core".into()] });
        assert_eq!(got[1].dir, "crates/core");
    }

    #[test]
    fn only_a_plain_cargo_command_has_its_inputs_read_and_dash_p_names_the_crate() {
        assert!(plain_cargo("cargo test -p core"));
        assert!(!plain_cargo("cd x && cargo test"));
        assert!(!plain_cargo("npm test"));
        assert_eq!(named_crates("cargo test -p core -- --nocapture -p x", &ws()), ["core"]);
        assert_eq!(named_crates("cargo test --package=web", &ws()), ["web"]);
        assert_eq!(named_crates("cargo test", &ws()).len(), 4);
    }

    #[test]
    fn an_include_literal_is_resolved_against_its_file_and_kept_inside_the_tree() {
        assert_eq!(normal(Path::new("src/../fixtures/a.jsonl")).as_deref(), Some("fixtures/a.jsonl"));
        assert_eq!(normal(Path::new("src/panel/run.rs")).as_deref(), Some("src/panel/run.rs"));
        assert!(normal(Path::new("src/../../etc")).is_none());
    }

    fn repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wsp-affected-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let g = |a: &[&str]| assert!(Command::new("git").arg("-C").arg(&dir).args(a).output().unwrap().status.success());
        g(&["init", "-q"]);
        g(&["config", "user.email", "t@t"]);
        g(&["config", "user.name", "t"]);
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"pj\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
        std::fs::write(dir.join("src/main.rs"), "const D: &str = include_str!(\"../data/d.txt\");\nfn main() { print!(\"{D}\"); }\n").unwrap();
        std::fs::create_dir_all(dir.join("data")).unwrap();
        std::fs::write(dir.join("data/d.txt"), "one").unwrap();
        std::fs::write(dir.join("README.md"), "one").unwrap();
        g(&["add", "-A"]);
        g(&["commit", "-qm", "one"]);
        dir
    }

    fn commit(dir: &Path, file: &str, text: &str) -> String {
        std::fs::write(dir.join(file), text).unwrap();
        let g = |a: &[&str]| Command::new("git").arg("-C").arg(dir).args(a).output().unwrap();
        g(&["commit", "-qam", "x"]);
        String::from_utf8_lossy(&g(&["rev-parse", "HEAD"]).stdout).trim().to_string()
    }

    #[test]
    fn a_cargo_key_holds_across_a_commit_that_touched_nothing_it_reads_and_moves_with_one_that_did() {
        let dir = repo("key");
        let head = |d: &Path| String::from_utf8_lossy(&Command::new("git").arg("-C").arg(d).args(["rev-parse", "HEAD"]).output().unwrap().stdout).trim().to_string();
        let one = head(&dir);
        let k1 = key(&dir, &one, "cargo test").unwrap();
        let s1 = key(&dir, &one, "sh run.sh").unwrap();
        let two = commit(&dir, "README.md", "two");
        assert_eq!(key(&dir, &two, "cargo test").unwrap(), k1, "a doc is not what cargo test reads");
        assert_ne!(key(&dir, &two, "sh run.sh").unwrap(), s1, "a command wsp cannot read reads the whole tree");
        assert_ne!(key(&dir, &two, "cargo test --release").unwrap(), k1, "the command is part of the key");
        let three = commit(&dir, "data/d.txt", "three");
        assert_ne!(key(&dir, &three, "cargo test").unwrap(), k1, "an included file is read");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_leading_cd_scopes_what_a_command_reads_and_anything_else_stays_whole() {
        let sc = |c: &str| scoped(c);
        assert_eq!(sc("cd web && cargo test census::"), Some(("web".into(), "cargo test census::".into())));
        assert_eq!(sc("cd ./web/src-tauri/ && X=$WSP_SCRATCH/t cargo test"), Some(("web/src-tauri".into(), "cargo test".into())));
        assert_eq!(sc("cargo test"), Some((String::new(), "cargo test".into())));
        assert_eq!(sc("cd web && cargo test && rm x"), None);
        assert_eq!(sc("cd web; cargo test"), None);
        assert_eq!(sc("cd ../web && cargo test"), None);
        assert_eq!(sc("cd /web && cargo test"), None);
        assert_eq!(sc("cd a b && cargo test"), None);
        assert_eq!(sc("cd web && cargo test | tee x"), None);
        assert!(check_box("npm run check:box -- pane src/pane/Pane.tsx"));
        assert!(!check_box("npm run check:box -- pane ../x"));
        assert!(!check_box("npm run check"));
        let dir = Path::new("/nonexistent");
        assert_eq!(reads(dir, "cd web && npm run check:box -- pane src/a.tsx"), Reads::Dir("web".into()));
        assert_eq!(reads(dir, "cd web && sh ../run.sh"), Reads::Whole);
        assert_eq!(reads(dir, "npm run check:box -- pane src/a.tsx"), Reads::Whole);
    }

    #[test]
    fn a_cd_key_holds_across_a_commit_in_another_directory_and_moves_with_one_in_its_own() {
        let dir = repo("cd");
        let g = |a: &[&str]| assert!(Command::new("git").arg("-C").arg(&dir).args(a).output().unwrap().status.success());
        let head = || String::from_utf8_lossy(&Command::new("git").arg("-C").arg(&dir).args(["rev-parse", "HEAD"]).output().unwrap().stdout).trim().to_string();
        std::fs::create_dir_all(dir.join("web/src")).unwrap();
        std::fs::create_dir_all(dir.join("web/node_modules")).unwrap();
        std::fs::write(dir.join("web/Cargo.toml"), "[package]\nname = \"wj\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
        std::fs::write(dir.join("web/src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.join("web/package.json"), "{}").unwrap();
        g(&["add", "-A"]);
        g(&["commit", "-qm", "web"]);
        let one = head();
        let cargo = "cd web && cargo test census::";
        let npm = "cd web && npm run check:box -- pane src/a.tsx";
        let (kc, kn, kw) = (key(&dir, &one, cargo).unwrap(), key(&dir, &one, npm).unwrap(), key(&dir, &one, "cd web && sh run.sh").unwrap());
        let two = commit(&dir, "README.md", "two");
        let moved = commit(&dir, "data/d.txt", "two");
        assert_eq!(key(&dir, &two, cargo).unwrap(), kc, "a doc elsewhere is not what the cargo command reads");
        assert_eq!(key(&dir, &moved, npm).unwrap(), kn, "a file in another directory is not in web's tree");
        assert_ne!(key(&dir, &moved, "cd web && sh run.sh").unwrap(), kw, "a command wsp cannot read reads the whole tree");
        let three = commit(&dir, "web/src/main.rs", "fn main() { }\n");
        assert_ne!(key(&dir, &three, cargo).unwrap(), kc, "a source in the crate moves the key");
        assert_ne!(key(&dir, &three, npm).unwrap(), kn, "a file in the directory moves the key");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn crate_at(dir: &Path, rel: &str, name: &str, deps: &str) {
        std::fs::create_dir_all(dir.join(rel).join("src")).unwrap();
        std::fs::write(dir.join(rel).join("Cargo.toml"), format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[dependencies]\n{deps}")).unwrap();
        std::fs::write(dir.join(rel).join("src/lib.rs"), "pub fn f() {}\n").unwrap();
    }

    fn commit_all(dir: &Path) -> String {
        let g = |a: &[&str]| Command::new("git").arg("-C").arg(dir).args(a).output().unwrap();
        g(&["add", "-A"]);
        g(&["commit", "-qm", "y"]);
        String::from_utf8_lossy(&g(&["rev-parse", "HEAD"]).stdout).trim().to_string()
    }

    #[test]
    fn a_path_dependency_beside_the_directory_is_read_and_one_outside_the_repo_is_not() {
        let dir = repo("dep");
        let out = repo("outside");
        crate_at(&dir, "shared", "shared", "");
        crate_at(&out, "far", "far", "");
        crate_at(&dir, "web", "web", &format!("shared = {{ path = \"../shared\" }}\nfar = {{ path = \"{}\" }}\n", out.join("far").display()));
        let one = commit_all(&dir);
        let cmd = "cd web && cargo test";
        let k1 = key(&dir, &one, cmd).unwrap();
        match reads(&dir, cmd) {
            Reads::Paths(p) => assert!(p.iter().any(|x| x == "shared") && !p.iter().any(|x| x.contains("far")), "{p:?}"),
            other => panic!("{other:?}"),
        }
        let two = commit(&dir, "README.md", "two");
        assert_eq!(key(&dir, &two, cmd).unwrap(), k1, "a doc elsewhere is not read");
        std::fs::write(dir.join("shared/src/lib.rs"), "pub fn g() {}\n").unwrap();
        let three = commit_all(&dir);
        assert_ne!(key(&dir, &three, cmd).unwrap(), k1, "the sibling crate it depends on moved");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&out);
    }

    #[test]
    fn a_workspace_rooted_above_the_directory_is_read_from_its_root_and_an_unreadable_one_stays_whole() {
        let dir = repo("above");
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"pj\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[workspace]\nmembers = [\"web\"]\n").unwrap();
        crate_at(&dir, "web", "web", "");
        let one = commit_all(&dir);
        let cmd = "cd web && cargo test";
        let k1 = key(&dir, &one, cmd).unwrap();
        let two = commit(&dir, "README.md", "two");
        assert_eq!(key(&dir, &two, cmd).unwrap(), k1);
        let three = commit(&dir, "Cargo.toml", "[package]\nname = \"pj\"\nversion = \"0.2.0\"\nedition = \"2021\"\n[workspace]\nmembers = [\"web\"]\n");
        assert_ne!(key(&dir, &three, cmd).unwrap(), k1, "the root manifest is read");
        // No manifest to read the graph from: the whole tree.
        assert_eq!(reads(&dir.join("docs"), "cd web && cargo test"), Reads::Whole);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
