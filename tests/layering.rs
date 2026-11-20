//! The layering ratchet (`wsp-400`, `docs/components.md` §5 `p0`).
//!
//! Parses `src/` the way `docs/spikes/components/graph.py` does: each
//! `src/*.rs` and `src/<dir>/` is one module, comments and strings are
//! stripped, everything from the first `#[cfg(test)] mod` on is test code and
//! not counted, and an edge is a `crate::m` path or a `use crate::{…}` list,
//! with the symbol named. Each module's component comes from the table under
//! "The map the layering test reads" in `docs/components.md`; the table's row
//! order is the stack, lowest first.
//!
//! An edge from a module to one in a higher component is upward. Every upward
//! edge and symbol the code has today is listed in `docs/components-upward.txt`.
//! The test fails on an upward edge or symbol not listed (a new one), on a listed
//! one the code no longer has (delete the line: the list only shrinks), and on a
//! line the trunk's copy of the list does not have.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

type Edges = BTreeMap<String, BTreeMap<String, BTreeSet<String>>>;

const MAP_HEADING: &str = "### The map the layering test reads";
const ALLOW: &str = "docs/components-upward.txt";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Module name → its files.
fn modules(src: &Path) -> BTreeMap<String, Vec<PathBuf>> {
    let mut out: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    let mut stack = vec![src.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                let rel = p.strip_prefix(src).unwrap();
                let first = rel.components().next().unwrap().as_os_str().to_string_lossy();
                let top = first.strip_suffix(".rs").unwrap_or(&first).to_string();
                out.entry(top).or_default().push(p);
            }
        }
    }
    out
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Product code and test code: split at the first line that starts a
/// `#[cfg(test)]` module.
fn split_test(s: &str) -> (&str, &str) {
    let mut at = 0;
    for line in s.split_inclusive('\n') {
        if let Some(rest) = s[at..].strip_prefix("#[cfg(test)]") {
            let trimmed = rest.trim_start();
            let gap = &rest[..rest.len() - trimmed.len()];
            if gap.contains('\n') {
                let t = trimmed
                    .strip_prefix("pub(crate) ")
                    .or_else(|| trimmed.strip_prefix("pub "))
                    .unwrap_or(trimmed);
                if let Some(t) = t.strip_prefix("mod ") {
                    let n = t.chars().take_while(|&c| is_word(c)).map(char::len_utf8).sum::<usize>();
                    if n > 0 && t[n..].starts_with(" {") {
                        return (&s[..at], &s[at..]);
                    }
                }
            }
        }
        at += line.len();
    }
    (s, "")
}

/// Block comments, then line comments and string literals (roughly, as the
/// spike does).
fn strip(s: &str) -> String {
    let mut no_block = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("/*") {
        match rest[i + 2..].find("*/") {
            Some(j) => {
                no_block.push_str(&rest[..i]);
                rest = &rest[i + 2 + j + 2..];
            }
            None => break,
        }
    }
    no_block.push_str(rest);
    let mut out = Vec::new();
    for line in no_block.split('\n') {
        let b: Vec<char> = line.chars().collect();
        let (mut i, mut instr, mut res) = (0, false, String::new());
        while i < b.len() {
            let c = b[i];
            if instr {
                if c == '\\' {
                    i += 2;
                    continue;
                }
                if c == '"' {
                    instr = false;
                }
                i += 1;
                continue;
            }
            if c == '"' {
                instr = true;
                i += 1;
                continue;
            }
            if c == '/' && b.get(i + 1) == Some(&'/') {
                break;
            }
            res.push(c);
            i += 1;
        }
        out.push(res);
    }
    out.join("\n")
}

struct Refs<'a> {
    top: &'a BTreeSet<String>,
    out: BTreeMap<String, BTreeSet<String>>,
}

impl Refs<'_> {
    fn yield_ref(&mut self, path: &str) {
        let mut path = path.trim();
        // ` as name` at the end
        if let Some(i) = path.rfind(" as ") {
            let name = path[i + 4..].trim_start();
            if !name.is_empty() && name.chars().all(is_word) {
                path = path[..i].trim_end();
            }
        }
        let segs: Vec<&str> = path.split("::").filter(|x| !x.is_empty()).collect();
        let Some(&first) = segs.first() else { return };
        if self.top.contains(first) || first == "main" {
            let sym = segs.get(1).copied().unwrap_or("*");
            self.out.entry(first.to_string()).or_default().insert(sym.to_string());
        } else {
            self.out.entry("main".into()).or_default().insert(first.to_string());
        }
    }

    fn expand(&mut self, prefix: &str, body: &str) {
        let mut items = Vec::new();
        let (mut depth, mut cur) = (0i32, String::new());
        for ch in body.chars() {
            if ch == '{' {
                depth += 1;
            }
            if ch == '}' {
                depth -= 1;
            }
            if ch == ',' && depth == 0 {
                items.push(cur.trim().to_string());
                cur.clear();
            } else {
                cur.push(ch);
            }
        }
        if !cur.trim().is_empty() {
            items.push(cur.trim().to_string());
        }
        for it in items {
            match (it.find('{'), it.ends_with('}')) {
                (Some(i), true) => {
                    let inner = &it[i + 1..it.len() - 1];
                    self.expand(&format!("{prefix}{}", &it[..i]), inner);
                }
                _ => self.yield_ref(&format!("{prefix}{it}")),
            }
        }
    }
}

/// Every `use<ws>crate::` in `code`: the byte offset just past `crate::`.
fn uses(code: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = code[from..].find("use") {
        let at = from + i;
        from = at + 3;
        let rest = &code[at + 3..];
        let t = rest.trim_start();
        if t.len() == rest.len() {
            continue;
        }
        if let Some(after) = t.strip_prefix("crate::") {
            out.push(code.len() - after.len());
        }
    }
    out
}

fn word(s: &str) -> &str {
    let n = s.chars().take_while(|&c| is_word(c)).map(char::len_utf8).sum();
    &s[..n]
}

fn refs(code: &str, top: &BTreeSet<String>) -> BTreeMap<String, BTreeSet<String>> {
    let mut r = Refs { top, out: BTreeMap::new() };
    for at in uses(code) {
        let rest = &code[at..];
        // use crate::{…};
        if let Some(body) = rest.strip_prefix('{') {
            if let Some(end) = body.find("};") {
                r.expand("", &body[..end]);
            }
        }
        // use crate::a::b[::*][ as x];
        let p = rest.chars().take_while(|&c| is_word(c) || c == ':').map(char::len_utf8).sum::<usize>();
        if p > 0 {
            let mut path = &rest[..p];
            let mut tail = &rest[p..];
            if path.ends_with("::") && tail.starts_with('*') {
                path = &rest[..p + 1];
                tail = &rest[p + 1..];
            } else if tail.starts_with("::*") {
                path = &rest[..p + 3];
                tail = &rest[p + 3..];
            }
            let ok = tail.starts_with(';') || {
                let t = tail.trim_start();
                t.len() < tail.len()
                    && t.strip_prefix("as").is_some_and(|t2| {
                        let t3 = t2.trim_start();
                        let w = word(t3);
                        t3.len() < t2.len() && !w.is_empty() && t3[w.len()..].starts_with(';')
                    })
            };
            if ok && !path.ends_with("::") {
                r.yield_ref(path);
            }
        }
        // use crate::m::{…};
        let m = word(rest);
        if !m.is_empty() {
            if let Some(body) = rest[m.len()..].strip_prefix("::{") {
                if let Some(end) = body.find("};") {
                    r.expand(&format!("{m}::"), &body[..end]);
                }
            }
        }
    }
    // inline crate::a[::b]
    let mut from = 0;
    while let Some(i) = code[from..].find("crate::") {
        let at = from + i;
        from = at + 7;
        if code[..at].chars().next_back().is_some_and(|c| is_word(c) || c == ':') {
            continue;
        }
        let a = word(&code[at + 7..]);
        if a.is_empty() {
            continue;
        }
        let after = &code[at + 7 + a.len()..];
        let b = after.strip_prefix("::").map(word).filter(|w| !w.is_empty());
        if top.contains(a) {
            r.out.entry(a.into()).or_default().insert(b.unwrap_or("*").into());
        } else {
            r.out.entry("main".into()).or_default().insert(a.into());
        }
    }
    r.out
}

/// Product edges: module → module → symbols.
fn product_edges(srcs: &[PathBuf]) -> (BTreeSet<String>, Edges) {
    let mut mods: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for src in srcs {
        for (m, files) in modules(src) {
            // A crate's `lib.rs` only declares its modules.
            if m != "lib" {
                mods.entry(m).or_default().extend(files);
            }
        }
    }
    let top: BTreeSet<String> = mods.keys().cloned().collect();
    let mut edges = Edges::new();
    for (m, files) in &mods {
        for f in files {
            let raw = fs::read_to_string(f).unwrap();
            let (prod, _test) = split_test(&raw);
            for (b, syms) in refs(&strip(prod), &top) {
                if &b != m {
                    edges.entry(m.clone()).or_default().entry(b).or_default().extend(syms);
                }
            }
        }
    }
    (top, edges)
}

/// The stack, lowest first, and module → component, from `docs/components.md`.
fn component_map(doc: &str) -> (Vec<String>, BTreeMap<String, String>) {
    let mut lines = doc.lines().skip_while(|l| l.trim() != MAP_HEADING).skip(1);
    let mut order = Vec::new();
    let mut map = BTreeMap::new();
    let mut seen_table = false;
    for l in lines.by_ref() {
        let l = l.trim();
        if !l.starts_with('|') {
            if seen_table {
                break;
            }
            continue;
        }
        seen_table = true;
        let cells: Vec<&str> = l.trim_matches('|').split('|').map(str::trim).collect();
        if cells.len() < 2 || cells[0] == "component" || cells[0].starts_with("---") {
            continue;
        }
        let c = cells[0].trim_matches('`').to_string();
        for m in cells[1].split(',') {
            let m = m.trim().trim_matches('`');
            if !m.is_empty() {
                let old = map.insert(m.to_string(), c.clone());
                assert!(old.is_none(), "{m} is in two components in the map in docs/components.md");
            }
        }
        order.push(c);
    }
    assert!(!order.is_empty(), "no table under {MAP_HEADING:?} in docs/components.md");
    (order, map)
}

/// `from -> to: sym, sym` lines; `#` starts a comment.
fn parse_allow(text: &str) -> BTreeSet<(String, String, String)> {
    let mut out = BTreeSet::new();
    for l in text.lines() {
        let l = l.split('#').next().unwrap().trim();
        if l.is_empty() {
            continue;
        }
        let (edge, syms) = l.split_once(':').unwrap_or_else(|| panic!("{ALLOW}: no `:` in {l:?}"));
        let (a, b) = edge.split_once("->").unwrap_or_else(|| panic!("{ALLOW}: no `->` in {l:?}"));
        for s in syms.split(',') {
            let s = s.trim();
            if !s.is_empty() {
                out.insert((a.trim().to_string(), b.trim().to_string(), s.to_string()));
            }
        }
    }
    out
}

fn render_allow(up: &BTreeSet<(String, String, String)>, comp: &BTreeMap<String, String>) -> String {
    let mut by: BTreeMap<(&str, &str), Vec<&str>> = BTreeMap::new();
    for (a, b, s) in up {
        by.entry((a, b)).or_default().push(s);
    }
    let mut out = String::new();
    for ((a, b), syms) in by {
        out.push_str(&format!("{a} -> {b}: {}  # {} -> {}\n", syms.join(", "), comp[a], comp[b]));
    }
    out
}

#[test]
fn no_new_upward_edge_and_the_allowlist_only_shrinks() {
    let root = root();
    let doc = fs::read_to_string(root.join("docs/components.md")).unwrap();
    let (order, comp) = component_map(&doc);
    let rank: BTreeMap<&str, usize> = order.iter().enumerate().map(|(i, c)| (c.as_str(), i)).collect();
    for c in comp.values() {
        assert!(rank.contains_key(c.as_str()));
    }
    let (mods, edges) = product_edges(&[
        root.join("src"),
        root.join("crates/base/src"),
        root.join("crates/host/src"),
        root.join("crates/store/src"),
        root.join("crates/wire/src"),
        root.join("crates/trees/src"),
    ]);

    let unmapped: Vec<&String> = mods.iter().filter(|m| !comp.contains_key(*m)).collect();
    assert!(
        unmapped.is_empty(),
        "modules with no component: {unmapped:?}; put each in the table under {MAP_HEADING:?} in docs/components.md"
    );

    // Distinct cross-component symbols per pair, and the upward edges.
    let mut pairs: BTreeMap<(&str, &str), BTreeSet<String>> = BTreeMap::new();
    let mut up = BTreeSet::new();
    for (a, bs) in &edges {
        for (b, syms) in bs {
            let (ca, cb) = (comp[a].as_str(), comp[b].as_str());
            if ca == cb {
                continue;
            }
            pairs.entry((ca, cb)).or_default().extend(syms.iter().map(|s| format!("{b}::{s}")));
            if rank[cb] > rank[ca] {
                for s in syms {
                    up.insert((a.clone(), b.clone(), s.clone()));
                }
            }
        }
    }
    let n = order.len();
    let total: usize = pairs.values().map(BTreeSet::len).sum();
    let up_edges: BTreeSet<(&str, &str)> = up.iter().map(|(a, b, _)| (a.as_str(), b.as_str())).collect();
    println!(
        "layering: {total} distinct cross-component symbols over {} of {} pairs; {} upward edges, {} symbols",
        pairs.len(),
        n * (n - 1),
        up_edges.len(),
        up.len()
    );
    for ((a, b), s) in &pairs {
        let dir = if rank[b] > rank[a] { "up" } else { "" };
        println!("  {a:>9} -> {b:<9} {:4} {dir}", s.len());
    }

    let allow_path = root.join(ALLOW);
    let allow_text = fs::read_to_string(&allow_path).unwrap_or_default();
    let allow = parse_allow(&allow_text);
    let new: Vec<_> = up.difference(&allow).collect();
    let gone: Vec<_> = allow.difference(&up).collect();
    if !new.is_empty() || !gone.is_empty() {
        let fresh = std::env::var_os("WSP_SCRATCH").map(PathBuf::from).map(|d| d.join("components-upward.txt"));
        if let Some(f) = &fresh {
            let _ = fs::write(f, render_allow(&up, &comp));
        }
        panic!(
            "the layering ratchet:\n{}{}{}",
            if new.is_empty() {
                String::new()
            } else {
                format!(
                    "new upward uses (a lower component using a higher one; move the symbol down or pass it in):\n{}",
                    new.iter().map(|(a, b, s)| format!("  {a} ({}) -> {b} ({}): {s}\n", comp[a], comp[b])).collect::<String>()
                )
            },
            if gone.is_empty() {
                String::new()
            } else {
                format!(
                    "gone from the code, so delete them from {ALLOW} (it only shrinks):\n{}",
                    gone.iter().map(|(a, b, s)| format!("  {a} -> {b}: {s}\n")).collect::<String>()
                )
            },
            fresh.map(|f| format!("the list as the code has it now: {}\n", f.display())).unwrap_or_default()
        );
    }

    // The trunk's list bounds this one: no line it lacks.
    let trunk = std::process::Command::new("git")
        .args(["show", &format!("master:{ALLOW}")])
        .current_dir(&root)
        .output();
    if let Ok(o) = trunk {
        if o.status.success() {
            let theirs = parse_allow(&String::from_utf8_lossy(&o.stdout));
            let grown: Vec<_> = allow.difference(&theirs).collect();
            assert!(
                grown.is_empty(),
                "{ALLOW} has upward uses the trunk's copy does not; the list only shrinks:\n{}",
                grown.iter().map(|(a, b, s)| format!("  {a} -> {b}: {s}\n")).collect::<String>()
            );
        }
    }
}

/// The workspace crates a manifest names as `path` dependencies, `name ->
/// directory`: under `[dependencies]`, `[build-dependencies]` and their
/// `[target.….…]` forms, and `[dev-dependencies]` too when `dev`. Both the
/// inline form (`name = { path = "…" }`) and the table form
/// (`[dependencies.name]` then `path = "…"`) are read.
fn path_deps(manifest: &Path, dev: bool) -> BTreeMap<String, PathBuf> {
    let text = fs::read_to_string(manifest).unwrap_or_else(|e| panic!("{}: {e}", manifest.display()));
    let dir = manifest.parent().unwrap();
    let kinds: &[&str] = if dev { &["dependencies", "build-dependencies", "dev-dependencies"] } else { &["dependencies", "build-dependencies"] };
    let path_in = |v: &str| -> Option<String> {
        let at = v.find("path")?;
        let rest = v[at + 4..].trim_start().strip_prefix('=')?.trim_start().strip_prefix('"')?;
        Some(rest[..rest.find('"')?].to_string())
    };
    let mut out = BTreeMap::new();
    // The section's kind, and the crate a `[dependencies.name]` table is about.
    let mut section: Option<Option<String>> = None;
    for line in text.lines() {
        let l = line.split('#').next().unwrap().trim();
        if let Some(h) = l.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
            // `target.<spec>.<kind>[.<name>]`: the spec may hold dots, so the
            // kind is found as a whole dotted segment.
            let h = match h.strip_prefix("target.") {
                Some(t) => kinds
                    .iter()
                    .find_map(|k| {
                        t.match_indices(&format!(".{k}"))
                            .map(|(i, _)| &t[i + 1..])
                            .find(|r| r.len() == k.len() || r[k.len()..].starts_with('.'))
                    })
                    .unwrap_or(""),
                None => h,
            };
            section = kinds.iter().find_map(|k| {
                if h == *k {
                    Some(None)
                } else {
                    h.strip_prefix(&format!("{k}.")).map(|n| Some(n.trim_matches('"').to_string()))
                }
            });
            continue;
        }
        match &section {
            Some(None) => {
                if let Some((name, v)) = l.split_once('=') {
                    if let Some(p) = path_in(v) {
                        out.insert(name.trim().trim_matches('"').to_string(), dir.join(p));
                    }
                }
            }
            Some(Some(name)) => {
                if let Some(p) = l.starts_with("path").then(|| path_in(l)).flatten() {
                    out.insert(name.clone(), dir.join(p));
                }
            }
            None => {}
        }
    }
    out
}

/// Every workspace crate the package at `dir` reaches through its path
/// dependencies, its own dev-dependencies included, each with the chain that
/// reaches it.
fn reached_from(dir: &Path, name: &str) -> BTreeMap<String, Vec<String>> {
    let manifest = dir.join("Cargo.toml");
    let text = fs::read_to_string(&manifest).unwrap();
    assert!(
        text.lines().any(|l| l.trim() == format!("name = \"{name}\"")),
        "{} is no longer the package {name}",
        dir.display()
    );
    let mut reached: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut stack: Vec<(PathBuf, Vec<String>, bool)> = vec![(manifest, vec![name.into()], true)];
    while let Some((manifest, via, dev)) = stack.pop() {
        for (dep, at) in path_deps(&manifest, dev).into_iter().filter(|(n, _)| n != name) {
            let mut chain = via.clone();
            chain.push(dep.clone());
            if reached.insert(dep.clone(), chain.clone()).is_none() {
                stack.push((at.join("Cargo.toml"), chain, false));
            }
        }
    }
    println!("{name} reaches: {:?}", reached.keys().collect::<Vec<_>>());
    reached
}

/// The chains in `reached` that end in a crate not in `may`.
fn beyond(reached: &BTreeMap<String, Vec<String>>, may: &[&str]) -> String {
    reached
        .iter()
        .filter(|(n, _)| !may.contains(&n.as_str()))
        .map(|(_, chain)| format!("  {}\n", chain.join(" -> ")))
        .collect()
}

/// `wsp-host`, the in-process host, is what the agent manager links, and the
/// manager links nothing of the store (`docs/agent-manager.md` 2.3, R1;
/// `docs/components.md` R1). So the crate depends on `wsp-base` and
/// `wsp-host-api` (whose types its TCP carrier and the beat's facts speak) and
/// on nothing else of the workspace, directly or through them, its own tests
/// included: never `wsp-store`, `wsp-store-contract` or `wsp-wire` (wsp-020).
#[test]
fn the_host_crate_links_neither_the_store_nor_the_wire() {
    const MAY: &[&str] = &["wsp-base", "wsp-host-api"];
    let bad = beyond(&reached_from(&root().join("crates/host"), "wsp-host"), MAY);
    assert!(
        bad.is_empty(),
        "wsp-host must link only {MAY:?} of the workspace (the agent manager links it and nothing of the store; \
         docs/agent-manager.md 2.3, docs/components.md R1):\n{bad}"
    );
}

/// `wsp-manager`, the agent manager, depends on `wsp-host-api`, `wsp-base` and
/// `wsp-host`, and on nothing of the store or the wire (2.3 R1, d1 Q3; wsp-021).
#[test]
fn the_manager_links_no_store_and_no_wire() {
    const MAY: &[&str] = &["wsp-base", "wsp-host", "wsp-host-api"];
    let bad = beyond(&reached_from(&root().join("crates/manager"), "wsp-manager"), MAY);
    assert!(
        bad.is_empty(),
        "wsp-manager must link only {MAY:?} of the workspace (docs/agent-manager.md 2.3 R1):\n{bad}"
    );
}

#[test]
fn the_manifest_reader_finds_path_dependencies_in_each_form() {
    let d = std::env::temp_dir().join(format!("wsp-layering-manifest-{}", std::process::id()));
    fs::create_dir_all(&d).unwrap();
    let m = d.join("Cargo.toml");
    fs::write(
        &m,
        "[package]\nname = \"x\"\npath = \"not-a-dep\"\n\n[dependencies]\nserde_json = \"1\"\nwsp-base = { path = \"../base\" }\n\
         \n[dependencies.wsp-store]\nversion = \"0.1\"\npath = \"../store\"\n\
         \n[target.'cfg(unix)'.dependencies]\nwsp-wire = { path = \"../wire\", features = [\"x\"] }\n\
         \n[target.x86_64-apple-darwin.build-dependencies]\nwsp-trees = { path = \"../trees\" }\n\
         \n[dev-dependencies]\nwsp-testkit = { path = \"../testkit\" } # a comment\n\n[features]\ntest-support = []\n",
    )
    .unwrap();
    let names = |dev| path_deps(&m, dev).into_keys().collect::<Vec<_>>();
    assert_eq!(names(false), ["wsp-base", "wsp-store", "wsp-trees", "wsp-wire"]);
    assert_eq!(names(true), ["wsp-base", "wsp-store", "wsp-testkit", "wsp-trees", "wsp-wire"]);
    assert_eq!(path_deps(&m, false)["wsp-base"], d.join("../base"));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn the_parser_reads_use_lists_paths_and_skips_comments_strings_and_tests() {
    let top: BTreeSet<String> = ["store", "model", "util"].iter().map(|s| s.to_string()).collect();
    let src = "use crate::store::{Store, lock as l};\nuse crate::{model::Task, util::{a, b}};\n\
               use crate::util::*;\n// crate::model::Hidden\nlet s = \"crate::model::InString\";\n\
               /* crate::model::Block */ let x = crate::model::Status::Open; crate::Args::new();\n\
               #[cfg(test)]\nmod tests {\n    use crate::model::TestOnly;\n}\n";
    let (prod, test) = split_test(src);
    assert!(test.contains("TestOnly") && !prod.contains("TestOnly"));
    let r = refs(&strip(prod), &top);
    let s = |m: &str| r[m].iter().cloned().collect::<Vec<_>>();
    // `crate::store::{` is also an inline path with no symbol, as in the spike
    assert_eq!(s("store"), ["*", "Store", "lock"]);
    assert_eq!(s("model"), ["Status", "Task"]);
    assert_eq!(s("util"), ["*", "a", "b"]);
    assert_eq!(s("main"), ["Args"]);
}

/// Every `Cargo.toml` of the workspace: the root's and each member's.
fn manifests() -> Vec<PathBuf> {
    let root = root();
    let mut out = vec![root.join("Cargo.toml")];
    let mut dirs: Vec<PathBuf> = fs::read_dir(root.join("crates")).unwrap().map(|e| e.unwrap().path()).collect();
    dirs.sort();
    out.extend(dirs.into_iter().map(|d| d.join("Cargo.toml")).filter(|m| m.exists()));
    out
}

/// True when the manifest declares `dep` as a dependency of any kind: `dep = …` under a
/// dependencies section, or a `[…dependencies.dep]` table, `[target.….…]` forms included.
fn declares(manifest: &str, dep: &str) -> bool {
    let mut in_deps = false;
    for line in manifest.lines() {
        let l = line.split('#').next().unwrap().trim();
        if let Some(h) = l.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
            let h = h.trim();
            if h.ends_with(&format!("dependencies.{dep}")) || h.ends_with(&format!("dependencies.\"{dep}\"")) {
                return true;
            }
            in_deps = h.ends_with("dependencies");
        } else if in_deps {
            if let Some((name, _)) = l.split_once('=') {
                if name.trim().trim_matches('"') == dep {
                    return true;
                }
            }
        }
    }
    false
}

/// D2 (`docs/state-core.md` section 6) admits `rusqlite` as the second external dependency, in
/// `wsp-store-sqlite` only: no other manifest of the workspace names it, and the crate's own
/// manifest does, pinned exactly with `bundled`. The crate also links none of `wsp-store`,
/// `wsp-wire`, `wsp-host` or `wsp-manager`, directly or through its path dependencies; the
/// contract and `wsp-base` may join at step 1 (wsp-045).
#[test]
fn rusqlite_is_the_sqlite_crates_alone_and_that_crate_links_no_store_wire_host_or_manager() {
    let sqlite = root().join("crates/store-sqlite/Cargo.toml");
    let mut holders = vec![];
    for m in manifests() {
        if declares(&fs::read_to_string(&m).unwrap(), "rusqlite") {
            holders.push(m.strip_prefix(root()).unwrap().to_path_buf());
        }
    }
    assert_eq!(
        holders,
        [PathBuf::from("crates/store-sqlite/Cargo.toml")],
        "rusqlite is a dependency of wsp-store-sqlite only (D2, docs/state-core.md section 6)"
    );
    let text = fs::read_to_string(&sqlite).unwrap();
    assert!(
        text.lines().any(|l| l.contains("rusqlite") && l.contains("\"=") && l.contains("bundled")),
        "wsp-store-sqlite pins rusqlite exactly (`=`) with `bundled`"
    );

    // Normal dependencies: the contract alone. With dev-dependencies, the suite and what it may link.
    let normal: Vec<String> = path_deps(&sqlite, false).into_keys().collect();
    assert_eq!(normal, ["wsp-store-contract"], "wsp-store-sqlite's normal dependencies reach wsp-store-contract alone");
    const MAY: &[&str] = &["wsp-store-contract", "wsp-store-conformance", "wsp-testkit"];
    let bad = beyond(&reached_from(&root().join("crates/store-sqlite"), "wsp-store-sqlite"), MAY);
    assert!(
        bad.is_empty(),
        "wsp-store-sqlite must link only {MAY:?} of the workspace (dev-dependencies included), never wsp-store, \
         wsp-wire, wsp-host or wsp-manager (docs/state-core.md section 6):\n{bad}"
    );
}

/// No manifest of the workspace but the root's names `wsp-store-sqlite`: nothing but the root
/// binary links the backend.
#[test]
fn no_manifest_but_the_roots_names_the_sqlite_crate() {
    let root_manifest = root().join("Cargo.toml");
    for m in manifests() {
        if m == root_manifest {
            continue;
        }
        let text = fs::read_to_string(&m).unwrap();
        let own = text.lines().any(|l| l.trim() == "name = \"wsp-store-sqlite\"");
        assert!(
            own || !text.contains("wsp-store-sqlite"),
            "{} names wsp-store-sqlite; only the root manifest may (docs/state-core.md section 6)",
            m.display()
        );
    }
}

/// `wsp-store-conformance`, the suite of 2.3, depends on `wsp-store-contract` and `wsp-testkit`
/// and on nothing else of the workspace, directly or through them, its own tests included: no
/// backend, so never `wsp-store-sqlite`, `wsp-store`, `wsp-wire`, `wsp-host` or `wsp-manager`
/// (`docs/state-core.md` section 6; wsp-047). A backend's tests depend on the suite, not the other
/// way round. (`rusqlite` is held off by the test above.)
#[test]
fn the_conformance_suite_links_the_contract_and_the_test_kit_only() {
    const MAY: &[&str] = &["wsp-store-contract", "wsp-testkit"];
    let bad = beyond(&reached_from(&root().join("crates/store-conformance"), "wsp-store-conformance"), MAY);
    assert!(
        bad.is_empty(),
        "wsp-store-conformance must link only {MAY:?} of the workspace (docs/state-core.md section 6):\n{bad}"
    );
}

/// Every dependency a manifest names, of any kind (normal, build, dev, and their `[target.….…]`
/// forms), in the inline form (`name = …`) or the table form (`[dependencies.name]`).
fn deps_named(manifest: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut in_deps = false;
    for line in manifest.lines() {
        let l = line.split('#').next().unwrap().trim();
        if let Some(h) = l.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
            let h = h.trim();
            in_deps = h.ends_with("dependencies");
            if let Some((_, name)) = h.rsplit_once("dependencies.") {
                out.insert(name.trim_matches('"').to_string());
            }
        } else if in_deps {
            if let Some((name, _)) = l.split_once('=') {
                out.insert(name.trim().trim_matches('"').to_string());
            }
        }
    }
    out
}

/// `wsp-api`, the API's wire types, is a contract crate (R2, R5): it depends on
/// `wsp-store-contract` and on nothing else of the workspace, directly or through it, its own
/// tests included (`docs/state-core.md` section 6; wsp-071). Its only other dependencies are
/// `serde` and `serde_json`, so no IO crate, and its source names no IO of the standard library:
/// a carrier is `wsp-server`'s or `wsp-client`'s.
#[test]
fn the_api_links_the_contract_alone_and_does_no_io() {
    const MAY: &[&str] = &["wsp-store-contract"];
    let dir = root().join("crates/api");
    let bad = beyond(&reached_from(&dir, "wsp-api"), MAY);
    assert!(
        bad.is_empty(),
        "wsp-api must link only {MAY:?} of the workspace (docs/state-core.md section 6):\n{bad}"
    );

    let named = deps_named(&fs::read_to_string(dir.join("Cargo.toml")).unwrap());
    let extra: Vec<&String> =
        named.iter().filter(|d| !["serde", "serde_json", "wsp-store-contract", "wsp-api"].contains(&d.as_str())).collect();
    assert!(extra.is_empty(), "wsp-api depends on serde, serde_json and the contract alone, not {extra:?}");

    const IO: &[&str] = &[
        "std::fs", "std::net", "std::process", "std::io", "std::os", "std::env", "std::thread", "File::", "TcpStream",
        "TcpListener", "UnixStream",
    ];
    let mut found = vec![];
    for e in fs::read_dir(dir.join("src")).unwrap() {
        let p = e.unwrap().path();
        let code = strip(&fs::read_to_string(&p).unwrap());
        for io in IO.iter().filter(|io| code.contains(*io)) {
            found.push(format!("{}: {io}", p.strip_prefix(root()).unwrap().display()));
        }
    }
    assert!(found.is_empty(), "wsp-api does no IO (R2):\n{}", found.join("\n"));
}

/// `wsp-view`, the CLI's `--json` projections (wsp-305), is pure: it links `wsp-store-contract`
/// and nothing else of the workspace, directly or through it, its own tests included, so
/// web-chrome can link it and draw what the CLI prints from its own fold. Its only other
/// dependency is `serde_json`, and its source names no IO of the standard library: what it
/// projects (the records, the live readings, the clock) is handed in by the caller.
#[test]
fn the_view_links_the_contract_alone_and_does_no_io() {
    const MAY: &[&str] = &["wsp-store-contract"];
    let dir = root().join("crates/view");
    let bad = beyond(&reached_from(&dir, "wsp-view"), MAY);
    assert!(bad.is_empty(), "wsp-view must link only {MAY:?} of the workspace (wsp-305):\n{bad}");

    let named = deps_named(&fs::read_to_string(dir.join("Cargo.toml")).unwrap());
    let extra: Vec<&String> =
        named.iter().filter(|d| !["serde_json", "wsp-store-contract", "wsp-view"].contains(&d.as_str())).collect();
    assert!(extra.is_empty(), "wsp-view depends on serde_json and the contract alone, not {extra:?}");

    const IO: &[&str] = &[
        "std::fs", "std::net", "std::process", "std::io", "std::os", "std::env", "std::thread", "std::time",
        "SystemTime", "Instant", "File::", "TcpStream", "TcpListener", "UnixStream",
    ];
    let mut found = vec![];
    for e in fs::read_dir(dir.join("src")).unwrap() {
        let p = e.unwrap().path();
        let code = strip(&fs::read_to_string(&p).unwrap());
        for io in IO.iter().filter(|io| code.contains(*io)) {
            found.push(format!("{}: {io}", p.strip_prefix(root()).unwrap().display()));
        }
    }
    assert!(found.is_empty(), "wsp-view does no IO and reads no clock: its inputs are handed in (wsp-305):\n{}", found.join("\n"));
}

/// `wsp-machines`, section 6's machines (wsp-118), is pure: it links `wsp-store-contract` and
/// nothing else of the workspace, directly or through it, its own tests included, so a machine is
/// tested as two functions over a `State` with no store behind it. Its only other dependency is
/// `serde_json` (an event's payload), and its source names no IO of the standard library and reads
/// no clock: `decide` and `evolve` see only the state, the command and the event they are handed.
#[test]
fn the_machines_link_the_contract_alone_and_do_no_io() {
    const MAY: &[&str] = &["wsp-store-contract"];
    let dir = root().join("crates/machines");
    let bad = beyond(&reached_from(&dir, "wsp-machines"), MAY);
    assert!(bad.is_empty(), "wsp-machines must link only {MAY:?} of the workspace (docs/state-core.md section 6):\n{bad}");

    let named = deps_named(&fs::read_to_string(dir.join("Cargo.toml")).unwrap());
    let extra: Vec<&String> =
        named.iter().filter(|d| !["serde_json", "wsp-store-contract", "wsp-machines"].contains(&d.as_str())).collect();
    assert!(extra.is_empty(), "wsp-machines depends on serde_json and the contract alone, not {extra:?}");

    const IO: &[&str] = &[
        "std::fs", "std::net", "std::process", "std::io", "std::os", "std::env", "std::thread", "std::time",
        "SystemTime", "Instant", "File::", "TcpStream", "TcpListener", "UnixStream",
    ];
    let mut found = vec![];
    let mut files = 0;
    let mut stack = vec![dir.join("src")];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            files += 1;
            let code = strip(&fs::read_to_string(&p).unwrap());
            for io in IO.iter().filter(|io| code.contains(*io)) {
                found.push(format!("{}: {io}", p.strip_prefix(root()).unwrap().display()));
            }
        }
    }
    assert!(files >= 2, "crates/machines/src holds lib.rs and orders.rs at least");
    assert!(found.is_empty(), "wsp-machines does no IO and reads no clock (docs/state-core.md section 6):\n{}", found.join("\n"));
}

/// `wsp-server`, the state core's server, goes through `Store` only (`docs/state-core.md`
/// section 6; wsp-073): its normal dependencies are `wsp-api` and `wsp-store-contract`, and with
/// its tests it reaches only those and `wsp-store-conformance` (for `MemStore`) of the workspace,
/// so nothing that does OS or store-file work: never `wsp-store-sqlite` ("not the sqlite crate,
/// except in main"), `wsp-store`, `wsp-wire`, `wsp-host` or `wsp-manager`. Its one external
/// dependency is `serde_json`.
#[test]
fn the_server_links_the_api_and_the_contract_and_nothing_that_does_os_or_store_file_work() {
    let dir = root().join("crates/server");
    let normal: Vec<String> = path_deps(&dir.join("Cargo.toml"), false).into_keys().collect();
    assert_eq!(normal, ["wsp-api", "wsp-store-contract"], "wsp-server's normal dependencies are the api and the contract");

    const MAY: &[&str] = &["wsp-api", "wsp-store-contract", "wsp-store-conformance"];
    let bad = beyond(&reached_from(&dir, "wsp-server"), MAY);
    assert!(
        bad.is_empty(),
        "wsp-server must link only {MAY:?} of the workspace, its tests included (docs/state-core.md section 6):\n{bad}"
    );

    let named = deps_named(&fs::read_to_string(dir.join("Cargo.toml")).unwrap());
    let extra: Vec<&String> = named
        .iter()
        .filter(|d| !["serde_json", "wsp-api", "wsp-store-contract", "wsp-store-conformance"].contains(&d.as_str()))
        .collect();
    assert!(extra.is_empty(), "wsp-server depends on serde_json, the api and the contract alone, not {extra:?}");
}

/// `wsp-client`, the state core's client, speaks the protocol and nothing else: its normal
/// dependencies are `wsp-api` and `wsp-store-contract`, so that web-chrome, which links it by path,
/// links no more of wsp than that (`docs/state-core.md` section 6; wsp-074). Its tests may also
/// reach `wsp-server` and `wsp-store-conformance` (a loopback server over `MemStore`), never the
/// store, the wire, the host, the manager or the sqlite crate. Its one external dependency is
/// `serde_json`.
#[test]
fn the_client_links_the_api_and_the_contract_only_and_the_server_in_tests_alone() {
    let dir = root().join("crates/client");
    let normal: Vec<String> = path_deps(&dir.join("Cargo.toml"), false).into_keys().collect();
    assert_eq!(normal, ["wsp-api", "wsp-store-contract"], "wsp-client's normal dependencies are the api and the contract");

    const MAY: &[&str] = &["wsp-api", "wsp-store-contract", "wsp-server", "wsp-store-conformance"];
    let bad = beyond(&reached_from(&dir, "wsp-client"), MAY);
    assert!(
        bad.is_empty(),
        "wsp-client must link only {MAY:?} of the workspace, its tests included (docs/state-core.md section 6):\n{bad}"
    );

    let named = deps_named(&fs::read_to_string(dir.join("Cargo.toml")).unwrap());
    let extra: Vec<&String> = named
        .iter()
        .filter(|d| !["serde_json", "wsp-api", "wsp-store-contract", "wsp-server", "wsp-store-conformance"].contains(&d.as_str()))
        .collect();
    assert!(extra.is_empty(), "wsp-client depends on serde_json, the api and the contract alone, not {extra:?}");

    // The server is a dev-dependency: a normal dependency would link it into web-chrome.
    let manifest = fs::read_to_string(dir.join("Cargo.toml")).unwrap();
    let before_dev = manifest.split("[dev-dependencies]").next().unwrap();
    assert!(!before_dev.contains("wsp-server"), "wsp-server is a dev-dependency of wsp-client, never a normal one");
}

#[test]
fn the_dependency_lister_reads_each_manifest_form() {
    let m = "[package]\nname = \"x\"\n\n[dependencies]\nserde = { version = \"1\" }\n\"serde_json\" = \"1\"\n\
             \n[dependencies.wsp-store-contract]\npath = \"../store-contract\"\n\
             \n[target.'cfg(unix)'.dependencies]\nlibc = \"0.2\" # a comment\n\
             \n[dev-dependencies]\n# tokio = \"1\"\nwsp-api = { path = \".\" }\n\n[features]\ntest-support = []\n";
    let names: Vec<String> = deps_named(m).into_iter().collect();
    assert_eq!(names, ["libc", "serde", "serde_json", "wsp-api", "wsp-store-contract"]);
}

#[test]
fn the_rusqlite_reader_finds_each_manifest_form_and_not_a_lookalike() {
    assert!(declares("[dependencies]\nrusqlite = \"0.32\"\n", "rusqlite"));
    assert!(declares("[dev-dependencies]\nrusqlite = { version = \"0.32\" }\n", "rusqlite"));
    assert!(declares("[target.'cfg(unix)'.dependencies]\n\"rusqlite\" = \"0.32\"\n", "rusqlite"));
    assert!(declares("[dependencies.rusqlite]\nversion = \"0.32\"\n", "rusqlite"));
    assert!(!declares("[dependencies]\nrusqlite-extra = \"1\"\n", "rusqlite"));
    assert!(!declares("[package]\nname = \"rusqlite\"\n", "rusqlite"));
    assert!(!declares("[dependencies]\n# rusqlite = \"0.32\"\n", "rusqlite"));
}
