//! `wsp packet` — a row's packet: everything one change needs, in one printout.
//!
//! Round four (compounding-033) briefed a change as a packet — the files it
//! touches inline, the cases that cover it, the change as expected results, and
//! one command that builds and verifies — and the agent spent about a quarter
//! of the tokens it spent finding its own way. A row that carries a `## Packet`
//! section is spawned with the short start (`agent_commands::SHORT_START`) and
//! is told to run this verb as its first act; a row without one spawns as it
//! always did.
//!
//! The section is prose, plus two directives, each on a line of its own:
//!
//! ```text
//! file: src/lean.rs        inline this file, relative to the tree
//! packet: off              keep the section, spawn the row the ordinary way
//! attention: 1.50          opt in to the attention harness, capped at $1.50 an attempt
//! ```
//!
//! Nothing else is read as structure: the cases, the change and the command are
//! the author's words, printed as written. **The print is capped and refuses
//! over it** rather than truncating — a packet that silently lacks the end of a
//! file briefs the agent on a function that is not there, which is worse than
//! no packet. The refusal names the largest parts so the author can cut one.
//!
//! Project-agnostic on purpose: paths are relative to the tree the agent
//! stands in, and nothing here knows what language the files are.

use std::path::{Component, Path, PathBuf};

use crate::model::Task;
use crate::store::{Found, Store};
use crate::util;
use crate::Args;

pub const SECTION: &str = "Packet";

/// Bytes of the whole print. About 4k tokens: the short start is worth having
/// only while the packet stays near the size of what it replaces.
pub const CAP: usize = 16_000;

/// The claim line's clause for an attempt that ran as a packet — read back by
/// `cmd_attempts`, so cost by kind can tell the two apart.
pub const CLAUSE: &str = "packet";

/// What an `attention: on` row may spend an attempt when it names no figure.
pub const ATTENTION_DEFAULT_CENTS: u64 = 200;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Packet {
    /// The author's words, directive lines removed.
    pub text: String,
    pub files: Vec<String>,
    pub off: bool,
    /// The row opted in to the attention harness (`wsp spawn --kind attention`):
    /// the most one attempt may spend, in cents. Nothing else opts a row in.
    pub attention_cents: Option<u64>,
}

/// `on`, `2`, `$1.50` — a cap in cents, or `None` for anything that is not one.
fn cap_cents(word: &str) -> Option<u64> {
    let word = word.trim().trim_start_matches('$');
    if word.eq_ignore_ascii_case("on") {
        return Some(ATTENTION_DEFAULT_CENTS);
    }
    let dollars: f64 = word.parse().ok()?;
    (dollars.is_finite() && dollars > 0.0).then(|| (dollars * 100.0).round() as u64).filter(|c| *c > 0)
}

pub fn parse(section: &str) -> Packet {
    let mut p = Packet::default();
    let mut kept: Vec<&str> = Vec::new();
    for line in section.lines() {
        let bare = line.trim().trim_start_matches("- ").trim();
        if let Some(path) = bare.strip_prefix("file:") {
            let path = path.trim();
            if !path.is_empty() {
                p.files.push(path.to_string());
                continue;
            }
        }
        if bare.eq_ignore_ascii_case("packet: off") {
            p.off = true;
            continue;
        }
        if let Some(cap) = bare.strip_prefix("attention:") {
            if let Some(cents) = cap_cents(cap) {
                p.attention_cents = Some(cents);
                continue;
            }
        }
        kept.push(line);
    }
    p.text = kept.join("\n").trim().to_string();
    p
}

/// The row's packet, when it has one with anything in it.
pub fn of(t: &Task) -> Option<Packet> {
    let p = parse(&t.section(SECTION)?);
    (!p.text.is_empty() || !p.files.is_empty()).then_some(p)
}

/// Whether a spawn of this row takes the short start: it has a packet and has
/// not opted out.
pub fn spawns_short(t: &Task) -> bool {
    of(t).is_some_and(|p| !p.off)
}

/// The spawn's decision, in one place so a test can ask it every way.
/// `member` is a plain member's agent: not a leg, a verifier or a check, each
/// of which carries an order of its own. `--full` and `--no-packet` both opt
/// out, as does the row's own `packet: off`, and a kind with no short start
/// is spawned as ever.
pub fn short_start(t: Option<&Task>, member: bool, full: bool, no_packet: bool, kind_short: bool) -> bool {
    member && !full && !no_packet && kind_short && t.is_some_and(spawns_short)
}

/// The work order a packet row is told, one line because that is what an order
/// is. The packet itself is fetched, not typed: a pane takes a line, not twenty
/// kilobytes of pasted source.
pub fn order(task: &str) -> String {
    format!(
        "Your packet is the whole brief. Run `wsp packet {task}` now, read what it prints, \
         do the change it describes, run its one command, then land and review the row as \
         your brief says. Read nothing the packet does not name."
    )
}

/// A path the packet may inline: relative, and staying inside the tree.
fn inside(path: &str) -> bool {
    let p = Path::new(path);
    !p.is_absolute() && p.components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

pub fn build(t: &Task, root: &Path) -> Result<String, String> {
    let Some(p) = of(t) else { return Err(format!("{} has no ## {SECTION} section", t.id)) };
    let mut parts: Vec<(String, String)> = Vec::new();
    for f in &p.files {
        if !inside(f) {
            return Err(format!("`file: {f}` leaves the tree — a packet names paths relative to it"));
        }
        let body = std::fs::read_to_string(root.join(f))
            .map_err(|e| format!("`file: {f}` cannot be read from {}: {e}", util::contract(root)))?;
        parts.push((f.clone(), body));
    }
    let mut out = format!("# Packet · {} · {}\n", t.id, t.title);
    if !p.text.is_empty() {
        out.push_str(&format!("\n{}\n", p.text));
    }
    for (path, body) in &parts {
        out.push_str(&format!("\n## {path}\n```\n{}\n```\n", body.trim_end()));
    }
    if out.len() > CAP {
        let mut sizes: Vec<(usize, &str)> = parts.iter().map(|(f, b)| (b.len(), f.as_str())).collect();
        sizes.sort_by(|a, b| b.0.cmp(&a.0));
        let largest: Vec<String> = sizes.iter().take(3).map(|(n, f)| format!("{f} {n}")).collect();
        return Err(format!(
            "the packet for {} is {} bytes and the cap is {CAP}; the prose is {} and the largest files are {}",
            t.id,
            out.len(),
            p.text.len(),
            if largest.is_empty() { "none".to_string() } else { largest.join(", ") }
        ));
    }
    Ok(out)
}

/// Where the named files are read from: `--root`, else the repository the
/// caller stands in (the agent's own tree), else the project's root.
fn root_for(store: &Store, t: &Task, args: &Args) -> PathBuf {
    if let Some(r) = args.get("root") {
        return util::expand(&r);
    }
    let here = std::env::current_dir().unwrap_or_default();
    crate::cmd_verify::toplevel(&here)
        .or_else(|| {
            let project = t.project.as_deref()?;
            crate::resolve::Index::new(store.projects()).root_of(project).map(|r| util::expand(&r))
        })
        .unwrap_or(here)
}

/// `wsp packet <task> [--root DIR]` prints it; `--from FILE` or a bare `-` writes
/// the section from a file or stdin, so prose never passes through a shell string.
pub fn packet(store: &Store, args: &Args) -> i32 {
    let Some(needle) = args.rest.iter().find(|a| *a != "-").cloned().or_else(|| std::env::var("WSP_TASK").ok().filter(|t| !t.is_empty())) else {
        eprintln!("usage: wsp packet <task> [--root DIR] | wsp packet <task> --from FILE | wsp packet <task> -");
        return 2;
    };
    let mut t = match store.resolve_task(&needle) {
        Found::One(t) => *t,
        other => {
            eprintln!("{}", other.why(&needle).unwrap_or_default());
            return 1;
        }
    };
    if let Some(from) = crate::cmd_task::prose_source(args) {
        let text = crate::cmd_task::read_source(&from).map_err(|e| format!("{from}: {e}"));
        let text = match text {
            Ok(t) => t,
            Err(e) => {
                eprintln!("wsp: {e}");
                return 1;
            }
        };
        crate::model::set_section_in(&mut t.body, SECTION, text.trim());
        if let Err(e) = store.save_task(&t) {
            eprintln!("wsp: {e}");
            return 1;
        }
        store.git_commit(&format!("wsp: packet {} — {}", t.id, t.title));
        println!("{} packet set", t.id);
        return 0;
    }
    match build(&t, &root_for(store, &t, args)) {
        Ok(text) => {
            print!("{text}");
            0
        }
        Err(e) => {
            eprintln!("wsp: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(section: &str) -> Task {
        let mut t = Task::default();
        t.id = "t-1".into();
        t.title = "Add a flag".into();
        t.body = format!("## Overview\nx\n\n## {SECTION}\n{section}\n");
        t
    }

    #[test]
    fn directives_are_read_and_the_rest_is_the_authors_words() {
        let p = parse("- file: src/a.rs\nfile: src/b.rs\nWhen save runs, it prints ok.\npacket: off\nRun: cargo test a::");
        assert_eq!(p.files, ["src/a.rs", "src/b.rs"]);
        assert!(p.off);
        assert_eq!(p.text, "When save runs, it prints ok.\nRun: cargo test a::");
    }

    #[test]
    fn the_attention_line_is_a_cap_in_cents_and_is_not_the_authors_words() {
        for (line, cents) in [("attention: 2", 200), ("attention: $1.50", 150), ("attention: on", 200), ("- attention: 0.05", 5)] {
            let p = parse(&format!("Do it.\n{line}"));
            assert_eq!((p.attention_cents, p.text.as_str()), (Some(cents), "Do it."), "{line}");
        }
        for line in ["attention: 0", "attention: free", "attention: -2", "attention:"] {
            let p = parse(&format!("Do it.\n{line}"));
            assert_eq!(p.attention_cents, None, "{line}");
            assert!(p.text.contains(line), "a line that is not a cap stays the author's words: {line}");
        }
    }

    #[test]
    fn a_row_without_a_section_or_with_an_empty_one_has_no_packet() {
        let mut t = Task::default();
        t.body = "## Overview\nx\n".into();
        assert!(of(&t).is_none() && !spawns_short(&t));
        assert!(of(&row("")).is_none());
    }

    #[test]
    fn opting_out_keeps_the_packet_but_not_the_short_start() {
        let t = row("packet: off\nDo the thing.");
        assert!(of(&t).is_some());
        assert!(!spawns_short(&t));
        assert!(spawns_short(&row("Do the thing.")));
    }

    #[test]
    fn the_short_start_is_taken_only_by_a_plain_member_with_a_packet_that_nobody_declined() {
        let t = row("Do the thing.");
        assert!(short_start(Some(&t), true, false, false, true));
        assert!(!short_start(None, true, false, false, true), "no row");
        assert!(!short_start(Some(&t), false, false, false, true), "a leg, a verifier or a check");
        assert!(!short_start(Some(&t), true, true, false, true), "--full");
        assert!(!short_start(Some(&t), true, false, true, true), "--no-packet");
        assert!(!short_start(Some(&t), true, false, false, false), "a kind with no short start");
        assert!(!short_start(Some(&row("packet: off\nx")), true, false, false, true), "the row opted out");
        assert!(!short_start(Some(&row("")), true, false, false, true), "no packet");
    }

    #[test]
    fn the_print_has_the_prose_then_each_file_inline() {
        let dir = std::env::temp_dir().join(format!("wsp-packet-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "fn a() {}\n").unwrap();
        let out = build(&row("file: src/a.rs\nSave prints ok.\ncheck: ./go"), &dir).unwrap();
        assert!(out.starts_with("# Packet · t-1 · Add a flag\n"), "{out}");
        assert!(out.contains("Save prints ok.\ncheck: ./go"));
        assert!(out.contains("## src/a.rs\n```\nfn a() {}\n```"));
        assert!(out.find("Save prints ok").unwrap() < out.find("## src/a.rs").unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_is_missing_or_outside_the_tree_is_refused_by_name() {
        let dir = std::env::temp_dir();
        assert!(build(&row("file: no/such.rs\nx"), &dir).unwrap_err().contains("no/such.rs"));
        assert!(build(&row("file: ../etc/passwd\nx"), &dir).unwrap_err().contains("leaves the tree"));
        assert!(build(&row("file: /etc/passwd\nx"), &dir).unwrap_err().contains("leaves the tree"));
    }

    #[test]
    fn a_packet_over_the_cap_is_refused_not_cut_and_names_the_largest_file() {
        let dir = std::env::temp_dir().join(format!("wsp-packet-big-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("big.rs"), "x".repeat(CAP)).unwrap();
        let err = build(&row("file: big.rs\nx"), &dir).unwrap_err();
        assert!(err.contains("cap is 16000") && err.contains("big.rs 16000"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_order_is_one_line_naming_the_verb_and_the_row() {
        let o = order("wsp-9");
        assert!(!o.contains('\n') && o.contains("`wsp packet wsp-9`"));
    }
}
