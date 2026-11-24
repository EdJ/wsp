//! The host seam's guard (`docs/agent-manager.md` section 6.2, step M0's
//! done-when line).
//!
//! M0 put every OS operation a seat needs behind one trait, wire's
//! `host::Host`, whose only implementation runs in this process. M1 moved that
//! implementation into a crate of its own, `wsp-host` (`crates/host`, wsp-020),
//! which the agent manager can link, and left the trait in the wire as an
//! adapter over it. This test is what keeps the OS there. It reads the product
//! code of `src/` and `crates/*/src/` and fails when any of these is reached
//! outside `crates/host/src/`:
//!
//! - **a process probe or a signal**: `ps`, `pgrep`, `lsof`, `kill`, `pkill`,
//!   `killall`, run as a `Command`, written into a shell line, or declared as a
//!   C function (`kill`, `killpg`, `proc_*`);
//! - **a fact probe**: `df`, `du`, `sysctl`, `pmset`, `vm_stat`,
//!   `memory_pressure`, `ioreg`, the same three ways (`sysctl`, `getloadavg`,
//!   `statfs` as C functions);
//! - **a screen read**: running `compound-render`, or naming
//!   `host::compound_render_binary` and the helpers that drive it;
//! - **a transcript or hook-log read**: building the path Claude Code or pi
//!   writes a session under, naming `hooks.jsonl`, asking `opencode` for an
//!   `export` or its `db`, or naming the host's transcript helpers;
//! - **one of the host's own free functions that does any of the above**
//!   (`signal_group`, `tree_of`, `tally_burn`, ...), called or imported from
//!   outside it, which reaches the OS past the trait.
//!
//! And it fails when **"herdr"** appears anywhere under `src/` or `crates/`,
//! tests and comments included (wsp-120: there is no herdr executor, D15).
//!
//! **Tree work has no exception** (M3, wsp-057). Every run of a tree's work is
//! a job, sent through `Host::run_job`, under both `WSP_JOBS` choices. So in
//! tree work's code (`crates/trees/src/`, `src/check.rs`, `src/barrier_run.rs`)
//! the guard also fails:
//!
//! - **a spawn of a tree's tools**: `Command::new` of `sh`, `bash`, `cargo` or
//!   `rustc` (the shell a step runs in and the throttled tools), which only
//!   the job runner (`crates/host/src/`) may start;
//! - **a place built by hand**: a `HostDir`, `TreeRef` or `Place` literal,
//!   which is a job's place made from a path on wsp's machine. A place is
//!   taken from `job_place` (`place_of`, `dir_on`, `dir_under`, `own_root`),
//!   the one module that makes them.
//!
//! `crates/trees/src/treecopy.rs`, the remote-agents copy, stays under T2
//! (wsp-093) until M4 retires it, and is not read by this clause. Nothing of
//! tree work is on the list beside it, which must hold no tree-work line.
//!
//! **What is product code.** Comments are not. String literals are kept,
//! because a probe hides in one, but a probe word in a string counts only in
//! shell position (see [`shell_hits`]). An item under `#[cfg(test)]`, or under
//! a `cfg` that names `test` or the `test-support` feature, is test code, and
//! so is a file declared as a test module (`#[cfg(test)] mod tests;`).
//!
//! **The list beside it.** What the code still does outside the host, with
//! its reason, is `docs/host-seam-allowed.txt`, one `path: kind` per line. As
//! with the layering ratchet (`tests/layering.rs`), the test fails on a hit
//! the list does not have (a new one), on a line the code no longer has
//! (delete it: the list only shrinks), and on a line the trunk's copy lacks.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const ALLOW: &str = "docs/host-seam-allowed.txt";
/// The one crate allowed to do all of it: the in-process host.
const HOST: &str = "crates/host/src/";

/// Programs whose run is a probe, a signal or a screen read.
const PROBES: &[&str] = &[
    "ps",
    "pgrep",
    "lsof",
    "kill",
    "pkill",
    "killall",
    "df",
    "du",
    "sysctl",
    "pmset",
    "vm_stat",
    "memory_pressure",
    "ioreg",
    "compound-render",
];

/// C functions whose declaration is a probe or a signal.
const FFI: &[&str] = &[
    "kill",
    "killpg",
    "sysctl",
    "sysctlbyname",
    "getloadavg",
    "statfs",
    "statvfs",
    "fstatfs",
    "proc_pidinfo",
    "proc_listpids",
    "proc_pidpath",
    "host_statistics",
    "host_statistics64",
];

/// The host's free functions that run a probe, a signal, a screen read or a
/// transcript read themselves. Reaching one by name skips the trait.
const HOST_FNS: &[&str] = &[
    "signal_group",
    "tree_of",
    "close_pane_soon",
    "compound_render_binary",
    "type_and_submit",
    "claude_transcript",
    "pi_session_file",
    "opencode_rows",
    "tally_burn",
    "bank_burn",
    "burn_under",
    "last_written",
];

/// Calls whose string arguments are a command handed to a kill guard as its
/// payload, to see whether the guard refuses it: the guard reads the text and
/// nothing runs it. `wsp doctor` probes the seat hook with a broad `pkill`
/// through `guard_refuses` (wsp-428, wsp-501). A literal anywhere else, a
/// `sh(...)` included, is read as before.
const GUARD_PROBES: &[&str] = &["guard_refuses"];

/// The paths the host's free functions are reached by: the host crate and its
/// module, and the wire modules that hand them on (`pub use`).
const HOST_MODS: &[&str] = &["wsp_host", "inproc", "host", "place_super", "place_compound"];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

// ---------------------------------------------------------------- the lexer

#[derive(Clone, Copy, PartialEq, Debug)]
enum K {
    Code,
    Str,
    Comment,
}

/// One kind per byte: code, inside a string or char literal (its quotes
/// included), or inside a comment.
fn lex(s: &str) -> Vec<K> {
    let b = s.as_bytes();
    let mut k = vec![K::Code; b.len()];
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                k[i] = K::Comment;
                i += 1;
            }
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            let mut depth = 0;
            while i < b.len() {
                if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                    depth += 1;
                    k[i] = K::Comment;
                    k[i + 1] = K::Comment;
                    i += 2;
                } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                    depth -= 1;
                    k[i] = K::Comment;
                    k[i + 1] = K::Comment;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    k[i] = K::Comment;
                    i += 1;
                }
            }
        } else if c == b'r' && (i == 0 || !ident(b[i - 1])) && matches!(b.get(i + 1), Some(b'"') | Some(b'#')) {
            // r"…" or r#"…"#
            let mut j = i + 1;
            while b.get(j) == Some(&b'#') {
                j += 1;
            }
            if b.get(j) != Some(&b'"') {
                i += 1;
                continue;
            }
            let hashes = j - i - 1;
            let mut end = j + 1;
            loop {
                if end >= b.len() {
                    break;
                }
                if b[end] == b'"' && b[end + 1..].iter().take(hashes).filter(|&&h| h == b'#').count() == hashes {
                    end += 1 + hashes;
                    break;
                }
                end += 1;
            }
            for x in &mut k[i..end.min(b.len())] {
                *x = K::Str;
            }
            i = end;
        } else if c == b'"' {
            let start = i;
            i += 1;
            while i < b.len() && b[i] != b'"' {
                i += if b[i] == b'\\' { 2 } else { 1 };
            }
            i += 1;
            for x in &mut k[start..i.min(b.len())] {
                *x = K::Str;
            }
        } else if c == b'\'' {
            // a char literal, or a lifetime / label
            let start = i;
            if b.get(i + 1) == Some(&b'\\') {
                let mut j = i + 2;
                while j < b.len() && b[j] != b'\'' {
                    j += 1;
                }
                for x in &mut k[start..(j + 1).min(b.len())] {
                    *x = K::Str;
                }
                i = j + 1;
            } else {
                let ch = s[i + 1..].chars().next().map(char::len_utf8).unwrap_or(1);
                if b.get(i + 1 + ch) == Some(&b'\'') {
                    for x in &mut k[start..i + 2 + ch] {
                        *x = K::Str;
                    }
                    i += 2 + ch;
                } else {
                    i += 1;
                }
            }
        } else {
            i += 1;
        }
    }
    k
}

/// One source file, read for the guard.
struct Src {
    text: String,
    kind: Vec<K>,
    /// Whether each byte is product code (not under a test `cfg`).
    product: Vec<bool>,
    /// `mod x;` declared under a test `cfg`.
    test_mods: Vec<String>,
}

impl Src {
    fn new(text: String) -> Src {
        let kind = lex(&text);
        let mut s = Src { product: vec![true; text.len()], kind, text, test_mods: Vec::new() };
        s.mark_tests();
        s
    }

    fn code(&self, i: usize) -> bool {
        self.kind[i] == K::Code
    }

    /// Code with every literal and comment blanked to spaces, byte offsets
    /// kept (a blanked character becomes as many spaces as it had bytes).
    fn skeleton(&self) -> String {
        let mut out = String::with_capacity(self.text.len());
        for (i, c) in self.text.char_indices() {
            if self.kind[i] == K::Code || c == '\n' {
                out.push(c);
            } else {
                out.extend(std::iter::repeat_n(' ', c.len_utf8()));
            }
        }
        out
    }

    /// Every string literal in product code: its byte range and its text,
    /// quotes and prefixes dropped.
    fn literals(&self) -> Vec<(usize, usize, &str)> {
        let mut out = Vec::new();
        let mut i = 0;
        let b = self.text.as_bytes();
        while i < b.len() {
            if self.kind[i] == K::Str && (i == 0 || self.kind[i - 1] != K::Str) {
                let mut j = i;
                while j < b.len() && self.kind[j] == K::Str {
                    j += 1;
                }
                let raw = &self.text[i..j];
                if let (Some(open), Some(close)) = (raw.find('"'), raw.rfind('"')) {
                    if close > open && self.product[i] {
                        out.push((i, j, &raw[open + 1..close]));
                    }
                }
                i = j;
            } else {
                i += 1;
            }
        }
        out
    }

    /// Mark each item under a test `cfg` as not product, and note the test
    /// modules declared out of line.
    fn mark_tests(&mut self) {
        let sk = self.skeleton();
        let b = sk.as_bytes();
        // `#![cfg(test)]` at the top: the whole file.
        if let Some(at) = sk.find("#![cfg(") {
            if sk[..at].trim().is_empty() && test_cfg(&self.text, at + 2) {
                self.product.iter_mut().for_each(|p| *p = false);
                return;
            }
        }
        let mut from = 0;
        while let Some(i) = sk[from..].find("#[cfg") {
            let at = from + i;
            from = at + 1;
            if !test_cfg(&self.text, at + 1) {
                continue;
            }
            let Some(end) = item_end(b, at) else { continue };
            let item = &sk[at..end];
            if let Some(name) = out_of_line_mod(item) {
                self.test_mods.push(name);
            }
            for p in &mut self.product[at..end] {
                *p = false;
            }
            from = end;
        }
    }
}

/// Whether the attribute at `at` (pointing at its `[`) is a `cfg` that holds
/// for tests only: it names `test` or the `test-support` feature, and is not
/// a `not(...)`.
fn test_cfg(text: &str, at: usize) -> bool {
    let rest = &text[at..];
    let Some(close) = matching(rest.as_bytes(), 0, b'[', b']') else { return false };
    let attr = &rest[..close];
    let pred = attr.trim_start_matches('[').trim_start();
    if !pred.starts_with("cfg") || pred.starts_with("cfg_attr") {
        return false;
    }
    if pred.contains("not(") {
        return false;
    }
    let words = pred.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-'));
    let named = words.clone().any(|w| w == "test");
    named || pred.contains("\"test-support\"")
}

/// The index of the bracket closing the one at `open`.
fn matching(b: &[u8], open: usize, l: u8, r: u8) -> Option<usize> {
    let mut depth = 0i32;
    for (j, &c) in b.iter().enumerate().skip(open) {
        if c == l {
            depth += 1;
        } else if c == r {
            depth -= 1;
            if depth == 0 {
                return Some(j);
            }
        }
    }
    None
}

/// Where the item that an attribute at `at` (its `#`) belongs to ends: past
/// its block, or past its `;`. Read on the skeleton, so braces in strings and
/// comments are gone.
fn item_end(b: &[u8], at: usize) -> Option<usize> {
    let mut j = matching(b, at + 1, b'[', b']')? + 1;
    let (mut paren, mut square) = (0i32, 0i32);
    while j < b.len() {
        match b[j] {
            b'(' => paren += 1,
            b')' => paren -= 1,
            b'[' => square += 1,
            b']' => square -= 1,
            b';' if paren == 0 && square == 0 => return Some(j + 1),
            b'{' if paren == 0 && square == 0 => {
                let close = matching(b, j, b'{', b'}')?;
                let mut k = close + 1;
                while k < b.len() && (b[k] == b' ' || b[k] == b'\t') {
                    k += 1;
                }
                return Some(if b.get(k) == Some(&b';') { k + 1 } else { close + 1 });
            }
            _ => {}
        }
        j += 1;
    }
    None
}

/// `mod name;` (any visibility) in an item: its name.
fn out_of_line_mod(item: &str) -> Option<String> {
    let after_attr = item[item.find(']')? + 1..].trim();
    let t = after_attr.trim_start_matches("pub(crate) ").trim_start_matches("pub ");
    let t = t.strip_prefix("mod ")?;
    let name: String = t.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    t[name.len()..].trim_start().starts_with(';').then_some(name)
}

// ---------------------------------------------------------------- the hits

fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// Every product-code offset of `pat` in `sk` that starts and ends on a word
/// boundary.
fn words<'a>(src: &'a Src, sk: &'a str, pat: &'a str) -> impl Iterator<Item = usize> + 'a {
    let b = sk.as_bytes();
    sk.match_indices(pat).map(|(i, _)| i).filter(move |&i| {
        let before = i == 0 || !is_word(b[i - 1]);
        let after = b.get(i + pat.len()).is_none_or(|&c| !is_word(c));
        before && after && src.product[i] && src.code(i)
    })
}

/// `Command::new("<probe>")`: a probe run.
fn command_hits(src: &Src, sk: &str, out: &mut BTreeSet<String>) {
    for at in words(src, sk, "Command::new") {
        let rest = &src.text[at + "Command::new".len()..];
        let Some(lit) = rest.trim_start().strip_prefix('(') else { continue };
        let lit = lit.trim_start();
        let Some(body) = lit.strip_prefix('"') else { continue };
        let Some(end) = body.find('"') else { continue };
        let prog = body[..end].rsplit('/').next().unwrap_or("");
        if PROBES.contains(&prog) {
            out.insert(format!("runs {prog}"));
        }
    }
}

/// A probe word in shell position inside a string: at the literal's start, or
/// after `;`, `|`, `&`, `(`, a newline, or `while`/`until`/`if`/`do`/`then`/
/// `!`/`exec`/`xargs`, and followed by an argument. The bare word alone
/// (`"ps"`, a label or one element of an argv) is not a shell line, and
/// neither is one inside a markdown code span: that is a message showing a
/// person (or an agent) a command, which nothing here runs. Nor is an
/// argument of a [`GUARD_PROBES`] call: a kill guard's payload.
fn shell_hits(src: &Src, sk: &str, out: &mut BTreeSet<String>) {
    for (at, _, lit) in src.literals() {
        if callee(sk, at).is_some_and(|f| GUARD_PROBES.contains(&f)) {
            continue;
        }
        let b = lit.as_bytes();
        for prog in PROBES {
            for (i, _) in lit.match_indices(prog) {
                let end = i + prog.len();
                let word_end = b.get(end).is_some_and(|&c| c == b' ');
                let has_arg = word_end && lit[end..].trim_start().chars().next().is_some_and(|c| !c.is_whitespace());
                // Inside a markdown code span (an odd count of backticks
                // before it): prose showing a person a command, not one run.
                let in_span = lit[..i].matches('`').count() % 2 == 1;
                if in_span || !has_arg || (i > 0 && (is_word(b[i - 1]) || b[i - 1] == b'-' || b[i - 1] == b'/' || b[i - 1] == b'.')) {
                    continue;
                }
                let before = lit[..i].trim_end();
                let lead = before.is_empty()
                    || before.ends_with([';', '|', '&', '(', '\n'])
                    || before.ends_with("\\n")
                    || ["while", "until", "if", "do", "then", "!", "exec", "xargs", "sudo"]
                        .iter()
                        .any(|kw| before.ends_with(kw) && before[..before.len() - kw.len()].chars().last().is_none_or(|c| !c.is_alphanumeric()));
                if lead {
                    out.insert(format!("shell {prog}"));
                }
            }
        }
    }
}

/// The name of the call whose parens hold offset `at` directly (`f(a, "x")`
/// gives `f`; `f(g("x"))` gives `g`), read on the skeleton. `None` outside any
/// parens, or where the nearest open bracket is a `[` or `{` (an array, a
/// block, a `json!` body).
fn callee(sk: &str, at: usize) -> Option<&str> {
    let b = sk.as_bytes();
    let mut depth = 0i32;
    let mut j = at;
    while j > 0 {
        j -= 1;
        match b[j] {
            b')' | b']' | b'}' => depth += 1,
            b'(' | b'[' | b'{' if depth > 0 => depth -= 1,
            b'[' | b'{' => return None,
            b'(' => {
                let end = sk[..j].trim_end().len();
                let start = sk[..end].rfind(|c: char| !(c.is_alphanumeric() || c == '_')).map_or(0, |k| k + 1);
                return (start < end).then(|| &sk[start..end]);
            }
            b';' if depth == 0 => return None,
            _ => {}
        }
    }
    None
}

/// `extern "C" { fn kill(...) }` and the like.
fn ffi_hits(src: &Src, sk: &str, out: &mut BTreeSet<String>) {
    let b = sk.as_bytes();
    for at in words(src, sk, "extern") {
        let Some(open) = sk[at..].find('{').map(|o| at + o) else { continue };
        // `extern "C" fn f()` (a definition) has no block before its parens
        if sk[at..open].contains('(') {
            continue;
        }
        let Some(close) = matching(b, open, b'{', b'}') else { continue };
        let block = &sk[open..close];
        for f in FFI {
            if block.split("fn ").skip(1).any(|d| d.trim_start().starts_with(f) && d.trim_start()[f.len()..].trim_start().starts_with('(')) {
                out.insert(format!("declares {f}"));
            }
        }
    }
    for f in FFI {
        if words(src, sk, &format!("libc::{f}")).next().is_some() {
            out.insert(format!("declares {f}"));
        }
    }
    if words(src, sk, "nix::sys::signal").next().is_some() {
        out.insert("declares kill".into());
    }
}

/// The host's own OS functions, reached past the trait: a path through one of
/// [`HOST_MODS`], a `use` that imports one, or a bare call to one so imported.
fn host_fn_hits(src: &Src, sk: &str, out: &mut BTreeSet<String>) {
    let b = sk.as_bytes();
    let mut imported = BTreeSet::new();
    for at in words(src, sk, "use") {
        let Some(semi) = sk[at..].find(';') else { continue };
        let stmt = &sk[at..at + semi];
        if !HOST_MODS.iter().any(|m| stmt.contains(&format!("{m}::"))) {
            continue;
        }
        for f in HOST_FNS {
            let named = stmt.match_indices(f).any(|(i, _)| {
                let before = i == 0 || !is_word(stmt.as_bytes()[i - 1]);
                let after = stmt.as_bytes().get(i + f.len()).is_none_or(|&c| !is_word(c));
                before && after
            });
            if named {
                imported.insert(*f);
                out.insert(format!("imports host::{f}"));
            }
        }
    }
    for f in HOST_FNS {
        for at in words(src, sk, f) {
            let rest = sk[at + f.len()..].trim_start();
            if !rest.starts_with('(') {
                continue;
            }
            let head = sk[..at].trim_end();
            let through = HOST_MODS.iter().any(|m| head.ends_with(&format!("{m}::")));
            let bare = !head.ends_with('.') && !head.ends_with("::") && !head.ends_with("fn");
            if through || (bare && imported.contains(f)) {
                out.insert(format!("calls host::{f}"));
            }
        }
    }
    let _ = b;
}

/// Where a session's transcript or a seat's hook log is: Claude Code's
/// `~/.claude/projects`, pi's `~/.pi/agent/sessions` (or
/// `$PI_CODING_AGENT_DIR`), `hooks.jsonl`, and `opencode export` or `db`.
fn transcript_hits(src: &Src, sk: &str, out: &mut BTreeSet<String>) {
    let lits = src.literals();
    for (_, _, lit) in &lits {
        if lit.contains(".claude/projects") {
            out.insert("reads transcripts (~/.claude/projects)".into());
        }
        if lit.contains(".pi/agent/sessions") {
            out.insert("reads transcripts (pi sessions)".into());
        }
        if lit.contains("hooks.jsonl") {
            out.insert("reads the hook log (hooks.jsonl)".into());
        }
    }
    // `.join(".claude").join("projects")`: both literals in one statement.
    let b = sk.as_bytes();
    let stmt_of = |at: usize| {
        let start = sk[..at].rfind([';', '{', '}']).map_or(0, |s| s + 1);
        let end = sk[at..].find([';', '{', '}']).map_or(b.len(), |e| at + e);
        (start, end)
    };
    for (s, _, lit) in &lits {
        let (a, z) = stmt_of(*s);
        let in_stmt = |want: &str| lits.iter().any(|(t, _, l)| *t >= a && *t < z && *l == want);
        if *lit == ".claude" && in_stmt("projects") {
            out.insert("reads transcripts (~/.claude/projects)".into());
        }
        // pi's sessions under its agent directory: `"sessions"` joined in a
        // statement that names the directory (a literal, or `pi_agent_dir`).
        if *lit == "sessions" {
            let stmt = &sk[a..z];
            let pi = lits.iter().any(|(t, _, l)| *t >= a && *t < z && (l.contains(".pi") || *l == "PI_CODING_AGENT_DIR" || *l == "agent"))
                || stmt.contains("pi_agent_dir")
                || stmt.contains("agent_dir");
            if pi {
                out.insert("reads transcripts (pi sessions)".into());
            }
        }
    }
    for at in words(src, sk, "HOOKS_FILE") {
        let _ = at;
        out.insert("reads the hook log (hooks.jsonl)".into());
    }
    for at in words(src, sk, "opencode_bin") {
        let (a, z) = stmt_of(at);
        if lits.iter().any(|(t, _, l)| *t >= a && *t < z && (*l == "export" || *l == "db")) {
            out.insert("reads transcripts (opencode export/db)".into());
        }
    }
}

/// Every hit in one file's product code.
fn hits(src: &Src) -> BTreeSet<String> {
    let sk = src.skeleton();
    let mut out = BTreeSet::new();
    command_hits(src, &sk, &mut out);
    shell_hits(src, &sk, &mut out);
    ffi_hits(src, &sk, &mut out);
    host_fn_hits(src, &sk, &mut out);
    transcript_hits(src, &sk, &mut out);
    out
}

// ---------------------------------------------------------------- the walk

/// Every `.rs` file under `dir`, sorted.
fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// The source roots: `src/` and each `crates/<name>/src/`.
fn roots(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![root.join("src")];
    let mut crates: Vec<PathBuf> = fs::read_dir(root.join("crates")).unwrap().flatten().map(|e| e.path().join("src")).filter(|p| p.is_dir()).collect();
    crates.sort();
    out.extend(crates);
    out
}

/// The directory a file's child modules live in.
fn child_dir(file: &Path) -> PathBuf {
    let stem = file.file_stem().unwrap().to_string_lossy();
    let parent = file.parent().unwrap();
    if matches!(&*stem, "lib" | "main" | "mod") {
        parent.to_path_buf()
    } else {
        parent.join(&*stem)
    }
}

/// `path: kind` → hit, over every product file outside the host crate.
fn scan(root: &Path) -> BTreeSet<(String, String)> {
    let mut found = BTreeSet::new();
    for r in roots(root) {
        let files = rust_files(&r);
        let mut srcs: BTreeMap<PathBuf, Src> = BTreeMap::new();
        for f in &files {
            srcs.insert(f.clone(), Src::new(fs::read_to_string(f).unwrap()));
        }
        // Files declared as test modules, and everything under them.
        let mut test_files: BTreeSet<PathBuf> = BTreeSet::new();
        for (f, s) in &srcs {
            for m in &s.test_mods {
                let dir = child_dir(f);
                test_files.insert(dir.join(format!("{m}.rs")));
                test_files.insert(dir.join(m));
            }
        }
        for (f, s) in &srcs {
            let rel = f.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            if rel.starts_with(HOST) || test_files.iter().any(|t| f == t || f.starts_with(t)) {
                continue;
            }
            for h in hits(s) {
                found.insert((rel.clone(), h));
            }
        }
    }
    found
}

/// `path: kind  # why`, blank lines and `#` lines skipped.
fn parse_allow(text: &str) -> BTreeSet<(String, String)> {
    text.lines()
        .map(|l| l.split(" #").next().unwrap_or("").trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once(": ").map(|(p, k)| (p.trim().to_string(), k.trim().to_string())))
        .collect()
}

#[test]
fn nothing_outside_the_in_process_host_reaches_the_os_for_a_seat() {
    let root = root();
    let found = scan(&root);
    let allow_text = fs::read_to_string(root.join(ALLOW)).unwrap_or_default();
    let allow = parse_allow(&allow_text);
    let new: Vec<_> = found.difference(&allow).collect();
    let gone: Vec<_> = allow.difference(&found).collect();
    println!("host seam: {} hits outside {HOST}, {} allowed", found.len(), allow.len());
    assert!(
        new.is_empty() && gone.is_empty(),
        "the host seam (docs/agent-manager.md 6.2, M0):\n{}{}",
        if new.is_empty() {
            String::new()
        } else {
            format!(
                "reached outside {HOST} (go through `host::Host`):\n{}",
                new.iter().map(|(p, k)| format!("  {p}: {k}\n")).collect::<String>()
            )
        },
        if gone.is_empty() {
            String::new()
        } else {
            format!(
                "gone from the code, so delete them from {ALLOW} (it only shrinks):\n{}",
                gone.iter().map(|(p, k)| format!("  {p}: {k}\n")).collect::<String>()
            )
        },
    );

    // The trunk's list bounds this one: no line it lacks.
    let trunk = std::process::Command::new("git").args(["show", &format!("master:{ALLOW}")]).current_dir(&root).output();
    if let Ok(o) = trunk {
        if o.status.success() {
            let theirs = parse_allow(&String::from_utf8_lossy(&o.stdout));
            let grown: Vec<_> = allow.difference(&theirs).collect();
            assert!(
                grown.is_empty(),
                "{ALLOW} has lines the trunk's copy does not; the list only shrinks:\n{}",
                grown.iter().map(|(p, k)| format!("  {p}: {k}\n")).collect::<String>()
            );
        }
    }
}

// ------------------------------------------------- tree work (M3, wsp-057)

/// Tree work's code: where every run of a tree's work is a job.
const TREE_WORK: &[&str] = &["crates/trees/src/", "src/check.rs", "src/barrier_run.rs"];

/// Kept under T2 (wsp-093): the remote-agents copy, which M4 retires.
const TREE_WORK_KEPT: &[&str] = &["crates/trees/src/treecopy.rs"];

/// The one module that makes a job's place.
const PLACES: &str = "crates/trees/src/job_place.rs";

/// The programs a tree's work runs: the shell a step is, and the throttled
/// tools. Only the job runner starts them.
const TREE_TOOLS: &[&str] = &["sh", "bash", "cargo", "rustc"];

/// The types a job's place is made of.
const PLACE_TYPES: &[&str] = &["HostDir", "TreeRef", "Place"];

/// What one tree-work file does outside a job: a tool spawned, or a place
/// built by hand (`PLACES` may build them).
fn tree_hits(src: &Src, places_here: bool) -> BTreeSet<String> {
    let sk = src.skeleton();
    let mut out = BTreeSet::new();
    for at in words(src, &sk, "Command::new") {
        let rest = &src.text[at + "Command::new".len()..];
        let Some(lit) = rest.trim_start().strip_prefix('(') else { continue };
        let Some(body) = lit.trim_start().strip_prefix('"') else { continue };
        let Some(end) = body.find('"') else { continue };
        let prog = body[..end].rsplit('/').next().unwrap_or("");
        if TREE_TOOLS.contains(&prog) {
            out.insert(format!("spawns {prog} outside a job"));
        }
    }
    if !places_here {
        for ty in PLACE_TYPES {
            for at in words(src, &sk, ty) {
                if !sk[at + ty.len()..].trim_start().starts_with('{') {
                    continue;
                }
                // A declaration or a signature, not a literal.
                let before = sk[..at].trim_end();
                if ["struct", "enum", "impl", "for", "->", "union", "trait"].iter().any(|w| before.ends_with(w)) {
                    continue;
                }
                out.insert(format!("builds a {ty} by hand"));
            }
        }
    }
    out
}

/// `path: kind` for every tree-work file's product code.
fn scan_tree_work(root: &Path) -> BTreeSet<(String, String)> {
    let mut found = BTreeSet::new();
    for r in roots(root) {
        let files = rust_files(&r);
        let mut srcs: BTreeMap<PathBuf, Src> = BTreeMap::new();
        for f in &files {
            srcs.insert(f.clone(), Src::new(fs::read_to_string(f).unwrap()));
        }
        let mut test_files: BTreeSet<PathBuf> = BTreeSet::new();
        for (f, s) in &srcs {
            for m in &s.test_mods {
                let dir = child_dir(f);
                test_files.insert(dir.join(format!("{m}.rs")));
                test_files.insert(dir.join(m));
            }
        }
        for (f, s) in &srcs {
            let rel = f.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            let tree_work = TREE_WORK.iter().any(|t| rel.starts_with(t));
            if !tree_work || TREE_WORK_KEPT.contains(&rel.as_str()) || test_files.iter().any(|t| f == t || f.starts_with(t)) {
                continue;
            }
            for h in tree_hits(s, rel == PLACES) {
                found.insert((rel.clone(), h));
            }
        }
    }
    found
}

#[test]
fn tree_work_runs_nothing_outside_a_job_and_builds_no_place_by_hand() {
    let root = root();
    let found = scan_tree_work(&root);
    assert!(
        found.is_empty(),
        "tree work outside a job (docs/agent-manager.md 6.2, M3; wsp-057): send it as a job through `Host::run_job`, its place from `job_place`:\n{}",
        found.iter().map(|(p, k)| format!("  {p}: {k}\n")).collect::<String>()
    );
    let allow = parse_allow(&fs::read_to_string(root.join(ALLOW)).unwrap_or_default());
    let tree_lines: Vec<_> = allow.iter().filter(|(p, _)| TREE_WORK.iter().chain(TREE_WORK_KEPT).any(|t| p.starts_with(t))).collect();
    assert!(tree_lines.is_empty(), "{ALLOW} holds tree-work lines; it must hold none:\n{tree_lines:?}");
}

#[test]
fn the_tree_work_clause_reads_spawns_and_hand_built_places() {
    let kinds = |code: &str, places_here: bool| tree_hits(&Src::new(code.to_string()), places_here).into_iter().collect::<Vec<_>>();
    assert_eq!(kinds("fn f() { Command::new(\"cargo\").arg(\"build\").status(); }", false), ["spawns cargo outside a job"]);
    assert_eq!(kinds("fn f() { std::process::Command::new(\"/bin/sh\").arg(\"-c\"); }", false), ["spawns sh outside a job"]);
    assert_eq!(kinds("fn f() { let at = JobAt::Dir(HostDir { host: h, path }); }", false), ["builds a HostDir by hand"]);
    assert_eq!(kinds("fn f() { Place { at, dir, row, kind } }", false), ["builds a Place by hand"]);
    assert!(kinds("fn f() { let at = HostDir { host: h, path }; }", true).is_empty(), "job_place makes them");
    assert!(kinds("pub struct Place { at: JobAt }\nimpl Place { }\nfn g() -> Place { place_of(w, k, t).unwrap() }", false).is_empty());
    assert!(kinds("fn f() { Command::new(\"git\").arg(\"status\"); let v = [\"sh\", \"-c\", \"x\"]; }", false).is_empty(), "git, and a job's argv");
    assert!(kinds("#[cfg(test)]\nmod tests { fn f() { Command::new(\"sh\"); } }", false).is_empty(), "test code");
}

#[test]
fn herdr_is_gone_from_src_and_crates() {
    let root = root();
    let mut left = Vec::new();
    for dir in ["src", "crates"] {
        let mut stack = vec![root.join(dir)];
        while let Some(d) = stack.pop() {
            let Ok(rd) = fs::read_dir(&d) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    if p.file_name().is_some_and(|n| n == "target") {
                        continue;
                    }
                    stack.push(p);
                    continue;
                }
                let Ok(text) = fs::read(&p) else { continue };
                let text = String::from_utf8_lossy(&text).to_lowercase();
                let n = text.matches("herdr").count();
                if n > 0 {
                    left.push(format!("  {}: {n}", p.strip_prefix(&root).unwrap().display()));
                }
                if p.file_name().is_some_and(|f| f.to_string_lossy().to_lowercase().contains("herdr")) {
                    left.push(format!("  {}: in its name", p.strip_prefix(&root).unwrap().display()));
                }
            }
        }
    }
    assert!(left.is_empty(), "\"herdr\" is back under src/ or crates/ (wsp-120; there is no herdr executor, D15):\n{}", left.join("\n"));
}

/// The guard is only as good as its reading, so each kind of hit, and each
/// way code is not product, is shown on a fragment.
#[test]
fn the_guard_reads_probes_in_product_code_and_not_in_tests_comments_or_prose() {
    let kinds = |s: &str| hits(&Src::new(s.to_string())).into_iter().collect::<Vec<_>>();

    // Each probe, each way.
    assert_eq!(kinds("fn f() { Command::new(\"ps\").arg(\"-A\"); }"), ["runs ps"]);
    assert_eq!(kinds("fn f() { Command::new( \"/usr/sbin/lsof\" ); }"), ["runs lsof"]);
    assert_eq!(kinds("fn f() { ssh(\"cd x; ps -axo command= | grep y\"); }"), ["shell ps"]);
    assert_eq!(kinds("fn f() { sh(\"while kill -0 1; do sleep 1; done\"); }"), ["shell kill"]);
    assert_eq!(kinds("fn f() { sh(\"ioreg -r -n IODisplayWrangler\"); }"), ["shell ioreg"]);
    // A kill guard's payload is read by the guard, not run (wsp-501); the same
    // text run, or merely near the probe, is still a hit.
    assert!(kinds("fn f(p: &Path) -> bool { guard_refuses(p, \"pkill -f yes\") }").is_empty(), "a kill guard's payload");
    assert_eq!(kinds("fn f() { sh(\"pkill -f yes\"); }"), ["shell pkill"]);
    assert_eq!(kinds("fn f(p: &Path) { guard_refuses(p, &sh(\"pkill -f yes\")); }"), ["shell pkill"]);
    assert_eq!(kinds("fn f(p: &Path) { if guard_refuses(p, \"x\") { sh(\"pkill -f yes\"); } }"), ["shell pkill"]);
    assert_eq!(kinds("fn f(p: &Path) { guard_refuses(p, &[\"pkill -f yes\"]); }"), ["shell pkill"]);
    assert!(
        kinds("fn f() { say(\"wait on a pid (`while kill -0 $pid; do sleep 5; done`), or `kill {pid}` first\"); }").is_empty(),
        "a command in a code span is prose for a person"
    );
    assert_eq!(kinds("extern \"C\" { fn kill(pid: i32, sig: i32) -> i32; }"), ["declares kill"]);
    assert_eq!(kinds("extern \"C\" { fn sysctlbyname(n: *const u8) -> i32; }"), ["declares sysctlbyname"]);
    assert_eq!(kinds("fn f() { unsafe { libc::getloadavg(p, 3) }; }"), ["declares getloadavg"]);
    assert_eq!(kinds("fn f() { crate::host::signal_group(1, \"KILL\"); }"), ["calls host::signal_group"]);
    assert_eq!(
        kinds("use crate::host::{tree_of, Host};\nfn f() { tree_of(1); }"),
        ["calls host::tree_of", "imports host::tree_of"]
    );
    assert_eq!(kinds("fn f() { let b = host::compound_render_binary(); }"), ["calls host::compound_render_binary"]);
    // The same functions by the host crate's own paths (wsp-020).
    assert_eq!(kinds("fn f() { wsp_host::inproc::signal_group(1, \"KILL\"); }"), ["calls host::signal_group"]);
    assert_eq!(
        kinds("use wsp_host::inproc::{tally_burn, InProcess};\nfn f() { tally_burn(&d, &p); }"),
        ["calls host::tally_burn", "imports host::tally_burn"]
    );
    assert!(kinds("pub use wsp_host::inproc::*;\nfn f(h: &InProcess) { h.tree_of(1); }").is_empty(), "the adapter's re-export and its method calls");
    assert_eq!(kinds("fn f() { home.join(\".claude\").join(\"projects\"); }"), ["reads transcripts (~/.claude/projects)"]);
    assert_eq!(kinds("fn f() { read(\"/u/.claude/projects/x.jsonl\"); }"), ["reads transcripts (~/.claude/projects)"]);
    assert_eq!(kinds("fn f() { home.join(\".pi\").join(\"agent\").join(\"sessions\"); }"), ["reads transcripts (pi sessions)"]);
    assert_eq!(kinds("fn f() { let s = agent_dir.join(\"sessions\").join(&d); }"), ["reads transcripts (pi sessions)"]);
    assert!(kinds("fn f() { home.join(\".pi\").join(\"agent\").join(\"extensions\"); }").is_empty(), "pi's extension is not a transcript");
    assert_eq!(kinds("fn f() { dir.join(HOOKS_FILE); }"), ["reads the hook log (hooks.jsonl)"]);
    assert_eq!(kinds("fn f() { captured(opencode_bin(), &[\"export\", s]); }"), ["reads transcripts (opencode export/db)"]);

    // Not probes: the trait, a held child, an argv element, a label, another
    // module's function of the same name, opencode asked for something else.
    assert!(kinds("fn f(h: &dyn Host) { h.signal_group(1, \"KILL\"); h.tree_of(2); LOCAL.disk_free(p); }").is_empty());
    assert!(kinds("fn f() { child.kill(); }").is_empty());
    assert!(kinds("fn f() { calls(\"ps\"); let s = \"kill\"; }").is_empty());
    assert!(kinds("fn tree_of(s: &Store) {} fn g() { tree_of(s); }").is_empty());
    assert!(kinds("fn f() { captured(opencode_bin(), &[\"models\"]); }").is_empty());
    assert!(kinds("fn f() { let s = \"psst kill\"; let t = \"skill -0\"; let u = \"--ps 1\"; }").is_empty());
    assert!(kinds("fn f() -> &'static str { let c = '\"'; \"x\" }").is_empty());

    // Not product: comments, test items, test-support items, a test file.
    assert!(kinds("// Command::new(\"ps\")\n/* Command::new(\"lsof\") */ fn f() {}").is_empty());
    assert!(kinds("#[cfg(test)]\nmod tests {\n fn f() { Command::new(\"ps\"); }\n}").is_empty());
    assert!(kinds("#[cfg(any(test, feature = \"test-support\"))]\nimpl Drop for X { fn drop(&mut self) { signal_group(1, \"KILL\"); } }").is_empty());
    assert!(kinds("#[cfg(feature = \"test-support\")]\nfn f() { Command::new(\"kill\"); }").is_empty());
    assert!(kinds("#![cfg(test)]\nfn f() { Command::new(\"ps\"); }").is_empty());
    assert_eq!(kinds("#[cfg(not(test))]\nfn f() { Command::new(\"ps\"); }"), ["runs ps"]);
    assert_eq!(
        kinds("#[cfg(test)]\nuse {a, b};\nfn f() { Command::new(\"df\"); }"),
        ["runs df"],
        "a test-only `use {{a, b}};` ends at its `;`, not at its brace"
    );
    let s = Src::new("#[cfg(test)]\nmod tests;\n#[cfg(test)]\npub(crate) mod fixtures;\nmod real;".into());
    assert_eq!(s.test_mods, ["tests", "fixtures"]);
}
