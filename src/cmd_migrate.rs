//! `wsp migrate` — off dated ids and into each project's own numbering space.
//!
//! The change this carries out is t-260815-024, and the reason for it is two
//! incidents on one day. An agent read `wsp add`'s answer of `t-260817-014`,
//! typed `013`, and wrote one task's overview onto a task in a *different
//! project* — a single-digit slip that crossed a project boundary invisibly,
//! because under the dated scheme nothing in an id says where its task lives.
//! Hours later Ed lost his own time to `022`, which by then named two open
//! tasks. The first cost an agent a minute and was recoverable from git. The
//! second cost a person their attention, and that is the thing this scheme
//! exists to protect.
//!
//! Under the new scheme those two ids are `wsp-013` and `robustness-014`, and
//! the same slip is an id that does not exist rather than one that does.
//!
//! # Why this is a command and not a script
//!
//! It has to be re-runnable, it has to be inspectable before it is believed,
//! and it has to leave the old ids resolving afterwards. A migration that
//! renames 267 tasks and rewrites 800-odd references to them is not something
//! to find out about halfway through, so `-n` plans it in full and applies
//! nothing.
//!
//! # What `--refs` will not touch
//!
//! A task id in a comment is a *reference*: it points at a task, and when the
//! task is renumbered the comment should follow it. A task id in code is
//! *data* — a fixture value, or a literal whose shape is the thing under test.
//! `cmd_brief::tests::task_ids_are_read_out_of_prose_and_only_at_a_word_boundary`
//! settles which of the two wins where they disagree: it is a test *about* the
//! shape of an id, so rewriting its literals changes what it tests.
//!
//! Run over `src` without that line drawn, this broke six tests on 2026-08-17
//! and twenty on 2026-08-19 — the defect unchanged, the surface grown, because
//! every test written since that keys a fixture on an id added one more. So
//! the rule is Ed's decision as it was worded, store prose and `src/*.rs`
//! *comments*, and the reading of a file that follows from it is [`Reads`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::model::Project;
use crate::store::Store;
use crate::util::Paint;
use crate::Args;

/// Is this an id from the dated scheme — `t-YYMMDD-NNN`?
///
/// Asked of the id rather than of a migration flag, so a store part-way
/// through, or one that gained a task between the plan and the apply, is
/// simply migrated the rest of the way. Re-running on a finished store is a
/// no-op rather than a second renumbering.
pub fn is_dated(id: &str) -> bool {
    let Some(rest) = id.strip_prefix("t-") else { return false };
    let Some((date, seq)) = rest.split_once('-') else { return false };
    date.len() == 6
        && seq.len() == 3
        && date.bytes().all(|b| b.is_ascii_digit())
        && seq.bytes().all(|b| b.is_ascii_digit())
}

/// Work out what every dated id should become.
///
/// Numbering runs in old-id order, which under the dated scheme is
/// chronological — so a project's new ids climb in the order its work was
/// filed, and `wsp-001` is the first task ever opened against wsp rather than
/// whichever one the directory happened to list first.
///
/// Archived tasks are numbered alongside live ones and not skipped. They share
/// the space, four of them are cited by live prose, and a number given to a
/// live task that an archived one already answers to is precisely the "one
/// name, two pieces of work" failure the archive already carries scars from.
fn plan(store: &Store) -> (BTreeMap<String, String>, BTreeMap<String, usize>) {
    let by_code: BTreeMap<String, String> =
        store.projects().into_iter().map(|p| (p.id.clone(), p.code().to_string())).collect();

    // Everything in the id space, live and archived, oldest first.
    let mut all: Vec<(String, Option<String>)> = store
        .tasks()
        .into_iter()
        .map(|t| (t.id, t.project))
        .chain(store.archived_tasks().into_iter().map(|t| (t.id, t.project)))
        .collect();
    all.sort_by(|a, b| a.0.cmp(&b.0));
    all.dedup_by(|a, b| a.0 == b.0);

    // Where each space has already got to, so a part-migrated store carries on
    // from where it stopped rather than colliding with what it did last time.
    let mut next: BTreeMap<String, usize> = BTreeMap::new();
    for (id, _) in &all {
        if let Some((code, n)) = id.rsplit_once('-') {
            if let Ok(n) = n.parse::<usize>() {
                let slot = next.entry(code.to_string()).or_insert(0);
                *slot = (*slot).max(n);
            }
        }
    }

    let mut map = BTreeMap::new();
    for (id, project) in &all {
        if !is_dated(id) {
            continue;
        }
        let code = match project {
            Some(p) => by_code.get(p).cloned().unwrap_or_else(|| p.clone()),
            None => crate::store::INBOX_CODE.to_string(),
        };
        let n = next.entry(code.clone()).or_insert(0);
        *n += 1;
        map.insert(id.clone(), format!("{code}-{:03}", *n));
    }
    (map, next)
}

pub fn run(store: &Store, args: &Args) -> i32 {
    if let Some(dir) = args.get("refs") {
        return refs(store, &dir, args);
    }

    let p = Paint::new();
    let (map, tops) = plan(store);
    if map.is_empty() {
        println!("nothing to migrate — every id is already in its project's space");
        return 0;
    }

    // What it will do, always, whether or not it is about to do it. A rename of
    // this size that only reports afterwards is one you cannot decline.
    let mut by_code: BTreeMap<String, Vec<(&String, &String)>> = BTreeMap::new();
    for (from, to) in &map {
        let code = to.rsplit_once('-').map(|(c, _)| c).unwrap_or(to);
        by_code.entry(code.to_string()).or_default().push((from, to));
    }
    for (code, rows) in &by_code {
        println!("{}  {} task(s)", p.bold(code), rows.len());
        for (from, to) in rows.iter().take(if args.has("all") { usize::MAX } else { 3 }) {
            println!("    {} → {}", p.dim(from), to);
        }
        if rows.len() > 3 && !args.has("all") {
            println!("    {}", p.dim(&format!("… and {} more", rows.len() - 3)));
        }
    }
    println!("\n{} task(s) across {} space(s)", map.len(), by_code.len());

    if args.has("n") || args.has("dry-run") {
        println!("{}", p.dim("nothing written — drop -n to apply"));
        return 0;
    }

    match store.rename_tasks(&map) {
        Ok((files, refs)) => {
            // The counters come last and are set from what was actually handed
            // out, not from what the plan intended — if the rename stopped
            // short, a counter that ran ahead of the files would hand the next
            // task an id nobody can see is taken.
            let mut saved = 0;
            for mut proj in store.projects() {
                if let Some(top) = tops.get(proj.code()) {
                    if proj.seq != *top {
                        proj.seq = *top;
                        if store.save_project(&proj).is_ok() {
                            saved += 1;
                        }
                    }
                }
            }
            store.git_commit(&format!(
                "wsp: migrate {} task(s) to per-project ids",
                map.len()
            ));
            println!("renamed {} task(s)", map.len());
            println!("rewrote {refs} reference(s) across {files} file(s)");
            println!("set the counter on {saved} project(s)");
            println!(
                "{}",
                p.dim(&format!(
                    "old ids go on resolving — {} records what became what",
                    store.ids_path().display()
                ))
            );
            println!(
                "{}",
                p.dim("source trees that cite task ids: wsp migrate --refs <path>")
            );
            0
        }
        Err(e) => {
            eprintln!("wsp: migration failed: {e}");
            eprintln!("wsp: the store is under git — `git -C {} status` shows how far it got", store.root.display());
            1
        }
    }
}

/// Apply the recorded renaming to a tree of source files.
///
/// Separate from the migration proper, and after it, because the map does not
/// exist until the store has moved. It is also the general tool: any repo whose
/// comments cite task ids — this one has 275 of them — can be brought forward
/// without the migration having to know where those repos are.
///
/// What it will not do is decide, for a file it cannot read, whether an id in
/// it is a citation or a fixture. Those files are named and left whole, which
/// is the answer being handed back to a person rather than guessed at.
///
/// Git history is deliberately not in scope. Ed settled that: commits before
/// the migration go on naming ids the tree no longer uses, and `ids.json` is
/// what makes that disagreement navigable rather than a dead end.
fn refs(store: &Store, dir: &str, args: &Args) -> i32 {
    let map = store.renamed_ids();
    if map.is_empty() {
        eprintln!("wsp: no renaming on record — run `wsp migrate` first");
        return 1;
    }
    let p = Paint::new();
    let dry = args.has("n") || args.has("dry-run");
    let root = crate::util::expand(dir);
    let mut files = 0usize;
    let mut total = 0usize;
    let mut data = 0usize;
    let mut unread: Vec<(PathBuf, usize)> = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            let path = e.path();
            let name = e.file_name();
            let name = name.to_string_lossy();
            // `.git` above all: rewriting objects is not rewriting references,
            // and `target` is build output that is regenerated anyway.
            if name.starts_with('.') || name == "target" || name == "node_modules" {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            // Every id in the file, asked before where in the file it sits: the
            // difference between this and what is rewritten is the count of
            // ids that are data, and it is worth saying out loud.
            let (_, all) = crate::store::substitute_tokens(&text, &map);
            if all == 0 {
                continue;
            }
            let how = Reads::of(&path, &text);
            if how == Reads::Unknown {
                unread.push((path, all));
                continue;
            }
            let (out, n) = rewrite(&text, &map, &how.spans(&text));
            data += all - n;
            if n == 0 {
                continue;
            }
            files += 1;
            total += n;
            if dry {
                let show = path.strip_prefix(&root).unwrap_or(&path).display();
                let rest = match all - n {
                    0 => String::new(),
                    k => p.dim(&format!("   ({k} in code)")),
                };
                println!("{n:4}  {show}{rest}");
            } else if let Err(e) = crate::store::write_atomic(&path, &out) {
                eprintln!("wsp: {}: {e}", path.display());
                return 1;
            }
        }
    }
    println!("{total} reference(s) across {files} file(s) under {}", root.display());
    if data > 0 {
        println!(
            "{}",
            p.dim(&format!("{data} more in code, left alone — a fixture value is data, not a citation"))
        );
    }
    if !unread.is_empty() {
        let held: usize = unread.iter().map(|(_, n)| n).sum();
        println!(
            "{}",
            p.dim(&format!(
                "{held} in {} file(s) this cannot read, left whole:",
                unread.len()
            ))
        );
        let shown = if args.has("all") { usize::MAX } else { 3 };
        for (path, n) in unread.iter().take(shown) {
            println!("{}", p.dim(&format!("  {n:4}  {}", path.strip_prefix(&root).unwrap_or(path).display())));
        }
        if unread.len() > shown {
            println!("{}", p.dim(&format!("  … and {} more (--all)", unread.len() - shown)));
        }
    }
    if dry {
        println!("{}", p.dim("nothing written — drop -n to apply"));
    }
    0
}

/// How to read a file for the one question this command asks of it: where in
/// it is a task id a *reference*?
///
/// It is never "everywhere". In prose it is everywhere; in code it is the
/// comments and nowhere else, because an id in code is a fixture value or a
/// literal whose shape is the thing under test; and in a file whose comments
/// this cannot find, it is nothing that can be established, so nothing is
/// touched and the file is named.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Reads {
    /// The whole file is prose — a README, a design note.
    Prose,
    /// `//` to the end of the line and `/* … */` in pairs: Rust, the C family.
    Slashes,
    /// `#` to the end of the line: shell, Python, TOML, YAML.
    Hash,
    /// Something whose comments this cannot find.
    Unknown,
}

impl Reads {
    /// Which of those a path is.
    ///
    /// By extension, because sniffing the contents of a source tree is a great
    /// deal of machinery to arrive at the same answer; and by `#!` line where
    /// there is no extension, which is every hook and shim in this repository.
    fn of(path: &Path, text: &str) -> Reads {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
        match ext.as_str() {
            "md" | "markdown" | "txt" | "text" | "rst" | "adoc" | "org" => Reads::Prose,
            "rs" | "c" | "h" | "cc" | "cpp" | "hpp" | "go" | "java" | "js" | "jsx" | "mjs"
            | "ts" | "tsx" | "swift" | "kt" | "scala" | "proto" => Reads::Slashes,
            "sh" | "bash" | "zsh" | "py" | "rb" | "pl" | "toml" | "yaml" | "yml" | "ini"
            | "conf" | "cfg" | "nix" => Reads::Hash,
            "" => {
                let first = text.lines().next().unwrap_or_default();
                let hashed = ["sh", "bash", "zsh", "python", "ruby", "perl", "awk"];
                match first.starts_with("#!") && hashed.iter().any(|w| first.contains(w)) {
                    true => Reads::Hash,
                    false => Reads::Unknown,
                }
            }
            _ => Reads::Unknown,
        }
    }

    /// The spans of `text` in which an id is a reference rather than data.
    fn spans(self, text: &str) -> Vec<(usize, usize)> {
        match self {
            Reads::Prose => vec![(0, text.len())],
            Reads::Unknown => Vec::new(),
            Reads::Slashes => slash_comments(text),
            Reads::Hash => hash_comments(text),
        }
    }
}

/// Rewrite ids inside `spans` and leave everything between them alone.
fn rewrite(
    text: &str,
    map: &BTreeMap<String, String>,
    spans: &[(usize, usize)],
) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut at = 0usize;
    let mut n = 0usize;
    for &(from, to) in spans {
        out.push_str(&text[at..from]);
        let (piece, k) = crate::store::substitute_tokens(&text[from..to], map);
        out.push_str(&piece);
        n += k;
        at = to;
    }
    out.push_str(&text[at..]);
    (out, n)
}

/// The comments in C-family source, found by walking past everything that can
/// hide one.
///
/// Literals are the whole difficulty, and each of them mis-read takes the rest
/// of the file with it: `"http://h"` holds a `//` that is not a comment, a raw
/// string holds anything at all, and `'"'` is a quote that opens nothing. So
/// this is a lexer rather than a search — and where it cannot tell (a `'` that
/// is a lifetime, a literal that never closes) it finds no comment rather than
/// inventing one, which loses a rewrite instead of making a wrong one.
fn slash_comments(text: &str) -> Vec<(usize, usize)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                let from = i;
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                out.push((from, i));
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let from = i;
                i += 2;
                // Rust nests these. C does not, but in C an inner `/*` is
                // already inside a comment, so counting is right for both.
                let mut depth = 1usize;
                while i < b.len() && depth > 0 {
                    if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                        depth += 1;
                        i += 2;
                    } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                out.push((from, i));
            }
            b'"' => i = end_of_string(text, i),
            b'\'' => i = end_of_char(text, i),
            // A raw string, but only where one can begin: the `r` of `for` is
            // not a prefix.
            b'r' | b'b' if !i.checked_sub(1).is_some_and(|prev| is_ident(b[prev])) => {
                i = end_of_raw(text, i).unwrap_or(i + 1);
            }
            _ => i += 1,
        }
    }
    out
}

/// A byte that can sit inside an identifier — so the `r` of `for` is one and
/// the `r` of `r"…"` is not.
fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// Past a `"…"` literal — or to the end of the file, when it never closes.
fn end_of_string(text: &str, at: usize) -> usize {
    let b = text.as_bytes();
    let mut i = at + 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return i + 1,
            _ => i += 1,
        }
    }
    b.len()
}

/// Past a `'…'` literal — or one byte on, when the quote is a lifetime.
///
/// The two are told apart by whether a closing quote arrives where a literal
/// would put one: `&'a str` is a lifetime and `'"'` is a literal. The second is
/// the one that costs, because read as an opening string quote it hides every
/// comment after it.
fn end_of_char(text: &str, at: usize) -> usize {
    let rest = &text[at + 1..];
    let inner = match rest.strip_prefix('\\') {
        Some(esc) => match esc.strip_prefix("u{") {
            Some(brace) => match brace.find('}') {
                Some(e) => at + 4 + e + 1,
                None => return at + 1,
            },
            None => match esc.chars().next() {
                Some(c) => at + 2 + c.len_utf8(),
                None => return at + 1,
            },
        },
        None => match rest.chars().next() {
            Some(c) => at + 1 + c.len_utf8(),
            None => return at + 1,
        },
    };
    match text[inner..].starts_with('\'') {
        true => inner + 1,
        false => at + 1,
    }
}

/// Past a raw string — `r"…"`, `r#"…"#`, `br#"…"#` — inside which `//` and `*/`
/// are text and nothing else.
fn end_of_raw(text: &str, at: usize) -> Option<usize> {
    let mut i = at;
    let mut rest = &text[at..];
    if let Some(r) = rest.strip_prefix('b') {
        rest = r;
        i += 1;
    }
    let rest = rest.strip_prefix('r')?;
    i += 1;
    let pounds = rest.bytes().take_while(|&c| c == b'#').count();
    if !rest[pounds..].starts_with('"') {
        return None;
    }
    i += pounds + 1;
    let close = format!("\"{}", "#".repeat(pounds));
    Some(match text[i..].find(&close) {
        Some(e) => i + e + close.len(),
        None => text.len(),
    })
}

/// The comments in `#`-commented source.
///
/// A `#` is a comment only where a word could begin, which is what keeps
/// `${name#prefix}` and `$#` and a url fragment from taking the rest of their
/// line with them; and only outside a quoted string, Python's triple quotes
/// included. A quote that never closes ends the search, for the same reason as
/// in [`slash_comments`]: a missed comment is a rewrite not made.
fn hash_comments(text: &str) -> Vec<(usize, usize)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            q @ (b'"' | b'\'') => {
                let three = [q; 3];
                if b[i..].starts_with(&three) {
                    let close = std::str::from_utf8(&three).unwrap_or_default().to_string();
                    i = match text[i + 3..].find(&close) {
                        Some(e) => i + 3 + e + 3,
                        None => b.len(),
                    };
                    continue;
                }
                let mut j = i + 1;
                while j < b.len() && b[j] != q {
                    j += if b[j] == b'\\' { 2 } else { 1 };
                }
                i = if j < b.len() { j + 1 } else { b.len() };
            }
            b'#' if i == 0 || matches!(b[i - 1], b' ' | b'\t' | b'\n' | b'\r') => {
                let from = i;
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                out.push((from, i));
            }
            _ => i += 1,
        }
    }
    out
}

/// Set or show a project's id prefix.
///
/// A code is what lets a descriptive slug stay descriptive while the id stays
/// short: `strata-prototype` sets `code=sp` and its tasks are `sp-062`. Ed's
/// own examples were `wsp-rob` and `mtw-rob`, which is the point — a code that
/// wants to carry the path may, but the id is never *derived* from a path,
/// because ancestry is mutable and an id must not be.
pub fn code(store: &Store, args: &Args) -> i32 {
    let index = crate::resolve::Index::new(store.projects());
    let Some(needle) = args.rest.first() else {
        let p = Paint::new();
        for proj in store.projects() {
            let marker = if proj.code_raw.is_empty() { p.dim("(slug)") } else { String::new() };
            println!("{:<20} {:<12} {}", proj.id, proj.code(), marker);
        }
        return 0;
    };
    let Some(found) = index.find(needle) else {
        eprintln!("wsp: no such project `{needle}`");
        return 1;
    };
    let mut proj: Project = match store.project(&found.id) {
        Some(p) => p,
        None => return 1,
    };

    let Some(new) = args.rest.get(1) else {
        println!("{} {}", proj.id, proj.code());
        return 0;
    };
    let new = crate::util::slugify(new);
    if new.is_empty() {
        eprintln!("wsp: `{}` does not reduce to a usable code", args.rest[1]);
        return 2;
    }
    if new.bytes().next().is_some_and(|b| b.is_ascii_digit()) {
        // `1-014` would make the whole id read as a number, and a bare suffix
        // could no longer be told from a whole id.
        eprintln!("wsp: a code cannot begin with a digit");
        return 2;
    }
    if crate::cmd_migrate::is_dated(&format!("{new}-001")) {
        eprintln!("wsp: `{new}` would produce ids indistinguishable from the dated scheme");
        return 2;
    }
    if new == proj.code() {
        println!("{} already numbers under {}", proj.id, new);
        return 0;
    }
    if let Some(why) = store.code_taken(&new, Some(&proj.id)) {
        eprintln!("wsp: `{new}` is not free: {why}");
        return 1;
    }

    let was = proj.code().to_string();
    proj.code_raw = new.clone();
    // The counter belongs to the *space*, and the space just changed. Starting
    // the new one from scratch is right; what would be wrong is carrying the
    // old count over, which would leave `sp-001`…`sp-062` unused for ever.
    proj.seq = 0;
    if let Err(e) = store.save_project(&proj) {
        eprintln!("wsp: write failed: {e}");
        return 1;
    }
    store.git_commit(&format!("wsp: {} numbers under {new}", proj.id));
    println!("{} now numbers under {new} (was {was})", proj.id);
    println!(
        "{}",
        Paint::new().dim("tasks already handed out keep their old prefix — an id names the task, not where it sits")
    );
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn renaming() -> BTreeMap<String, String> {
        [("t-260815-001", "core-001"), ("t-260815-002", "core-002")]
            .into_iter()
            .map(|(from, to)| (from.to_string(), to.to_string()))
            .collect()
    }

    /// What `wsp migrate --refs <dir>` would leave behind for one file.
    fn brought_forward(name: &str, text: &str) -> String {
        let path = PathBuf::from(name);
        let how = Reads::of(&path, text);
        rewrite(text, &renaming(), &how.spans(text)).0
    }

    /// The defect this was reopened for, in one file.
    ///
    /// Both lines hold the same id and only one of them is a citation. The
    /// second is a fixture: the test around it asserts on that literal, so a
    /// rewrite there does not follow the task, it changes what is being tested.
    #[test]
    fn an_id_in_code_is_data_and_only_an_id_in_a_comment_is_a_reference() {
        let out = brought_forward(
            "cmd_brief.rs",
            "    // t-260815-001 is why this exists\n    let t = Task::new(\"the member\", \"t-260815-001\");\n",
        );
        assert!(out.contains("// core-001 is why"), "the comment did not follow the task:\n{out}");
        assert!(out.contains("\"t-260815-001\")"), "a fixture value was rewritten:\n{out}");
    }

    /// The three literals that hide a comment, and the cost of mis-reading any
    /// of them: everything after it is read as the wrong kind of text. The last
    /// line is the assertion that the scanner recovered — a `'` read as opening
    /// a string would swallow the rest of the file.
    #[test]
    fn a_slash_or_a_quote_inside_a_literal_opens_nothing() {
        let out = brought_forward(
            "x.rs",
            concat!(
                "let u = \"http://h/t-260815-001\";\n",
                "let r = r#\"// t-260815-001\"#;\n",
                "let q = '\"';\n",
                "// t-260815-002\n",
            ),
        );
        assert!(out.contains("\"http://h/t-260815-001\""), "a url read as a comment:\n{out}");
        assert!(out.contains("r#\"// t-260815-001\"#"), "a raw string read as a comment:\n{out}");
        assert!(out.contains("// core-002"), "a literal swallowed the rest of the file:\n{out}");
    }

    /// A lifetime is a quote that never closes, and the file is full of them.
    #[test]
    fn a_lifetime_is_not_a_literal_that_never_ends() {
        let out = brought_forward("x.rs", "fn f<'a>(s: &'a str) {}\n// t-260815-001\n");
        assert!(out.contains("// core-001"), "a lifetime ended the search:\n{out}");
    }

    /// Rust nests block comments, so the first `*/` is not necessarily the end
    /// of one — and code after the real end is still code.
    #[test]
    fn a_block_comment_ends_where_it_closes_and_not_before() {
        let out = brought_forward(
            "x.rs",
            "/* t-260815-001 /* and */ t-260815-002 */\nlet a = \"t-260815-001\";\n",
        );
        assert!(out.contains("core-001 /* and */ core-002"), "a nested open was not counted:\n{out}");
        assert!(out.contains("\"t-260815-001\""), "code after the comment was rewritten:\n{out}");
    }

    /// Prose has no code in it, so the whole file is citation — this is the
    /// case the command already handled and must go on handling.
    #[test]
    fn a_prose_file_is_reference_all_the_way_through() {
        assert_eq!(
            brought_forward("README.md", "t-260815-001 and `t-260815-002`\n"),
            "core-001 and `core-002`\n",
        );
    }

    /// `#` is a comment where a word could begin and an operator everywhere
    /// else — which is the difference between a shell script and a shell script
    /// with its parameter expansions rewritten.
    #[test]
    fn a_hash_is_a_comment_only_where_a_word_could_begin() {
        let out = brought_forward(
            "wsp-session.sh",
            "id=${name#t-260815-001}   # t-260815-002\necho \"$#\"\n",
        );
        assert!(out.contains("${name#t-260815-001}"), "an expansion read as a comment:\n{out}");
        assert!(out.contains("# core-002"), "the comment did not follow the task:\n{out}");
    }

    /// A hook has no extension and is still a shell script.
    #[test]
    fn a_file_with_no_extension_is_read_by_its_shebang() {
        let hook = PathBuf::from("hooks/on-attention-raised");
        assert_eq!(Reads::of(&hook, "#!/usr/bin/env bash\n# t-260815-001\n"), Reads::Hash);
        assert_eq!(Reads::of(&PathBuf::from("Makefile"), "all:\n"), Reads::Unknown);
    }

    /// Guessing is how this broke in the first place. In a language whose
    /// comments this cannot find, an id might be either kind, and the answer to
    /// which is a person — so the file is left whole and named in the summary
    /// rather than quietly rewritten or quietly skipped.
    #[test]
    fn a_file_this_cannot_read_is_left_whole() {
        let sql = "-- t-260815-001\nselect 't-260815-002';\n";
        assert_eq!(brought_forward("queries.sql", sql), sql);
    }
}
