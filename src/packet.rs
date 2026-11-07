//! `wsp packet` — a row's packet: everything one change needs, in one printout.
//!
//! Round four (compounding-033) briefed a change as a packet — the files it
//! touches inline, the cases that cover it, the change as expected results, and
//! one command that builds and verifies — and the agent spent about a quarter
//! of the tokens it spent finding its own way. A row that carries a `## Packet`
//! section is spawned with the short start (`agent_commands::SHORT_START`) and
//! is told to run this verb as its first act. A row without one that reads as
//! small work is given one at spawn (`packet_gen`); the rest spawn as they always
//! did.
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

/// The claim line's clause for a packet of `bytes`: `packet 4210`, or `packet`
/// when the size is not known.
pub fn clause(bytes: Option<u64>) -> String {
    match bytes {
        Some(b) => format!("{CLAUSE} {b}"),
        None => CLAUSE.to_string(),
    }
}

/// `Some(size)` for a packet clause, `Some(None)` for one without a size (a claim
/// from before sizes were written), `None` for any other clause.
pub fn clause_bytes(c: &str) -> Option<Option<u64>> {
    let rest = c.trim().strip_prefix(CLAUSE)?;
    if rest.is_empty() {
        return Some(None);
    }
    rest.strip_prefix(' ').and_then(|n| n.trim().parse().ok()).map(Some)
}

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

/// The first line of a section `wsp packet` wrote, so `--regen` can tell it from
/// one a person wrote by hand. A comment: it is never part of the brief.
pub const GENERATED: &str = "<!-- wsp: generated packet -->";

pub fn parse(section: &str) -> Packet {
    let mut p = Packet::default();
    let mut kept: Vec<&str> = Vec::new();
    for line in section.lines() {
        if line.trim() == GENERATED {
            continue;
        }
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

/// The row's packet, when it has one with anything in it — or one that says
/// `packet: off`, which is a section too: a row that declined is not given one.
pub fn of(t: &Task) -> Option<Packet> {
    let p = parse(&t.section(SECTION)?);
    (!p.text.is_empty() || !p.files.is_empty() || p.off).then_some(p)
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

/// Whether a spawn of this row starts short, and with how many bytes of packet.
///
/// A row with a `## Packet` (and no `packet: off`) starts short. A row without
/// one that reads as routine or extension work is given one first — generated
/// from its overview and the project's context pack and written into the row,
/// where a person can see it. A packet that cannot be printed (a file gone, over
/// the cap) is not started short, since the order would point at a verb that
/// refuses; the reason goes to stderr and the row is spawned as ever.
pub fn at_spawn(store: &Store, task: Option<&str>, member: bool, full: bool, no_packet: bool, kind_short: bool) -> Option<u64> {
    if !member || full || no_packet || !kind_short {
        return None;
    }
    let mut t = store.find_task(task?)?;
    if t.section(SECTION).is_none_or(|s| s.trim().is_empty()) && crate::packet_gen::eligible(&t).is_ok() {
        let root = tree_root(store, &t);
        let made = crate::packet_gen::generate(store, &t, &root);
        match &made.pack {
            // A pack that names no file still leaves the overview, which is the
            // brief: the row is given a packet of the overview alone rather than
            // left to a start that hangs on a hook delivering it (`wsp-446`).
            Ok(_) => {
                if let Err(why) = crate::packet_gen::write(store, &mut t, &made.section) {
                    eprintln!("wsp: {} is not given a packet: {why}", t.id);
                }
            }
            Err(why) => eprintln!("wsp: {} is not given a packet: {why}", t.id),
        }
        t = store.find_task(&t.id)?;
    }
    if !short_start(Some(&t), member, full, no_packet, kind_short) {
        return None;
    }
    match build(&t, &tree_root(store, &t)) {
        Ok(text) => Some(text.len() as u64),
        Err(why) => {
            eprintln!("wsp: {} is not started short: {why}", t.id);
            None
        }
    }
}

/// The work order a packet row is told, one line because that is what an order
/// is. The packet itself is fetched, not typed: a pane takes a line, not twenty
/// kilobytes of pasted source.
pub fn order(task: &str, flow: crate::cmd_checkout::Flow) -> String {
    format!(
        "Your packet is the whole brief. Run `wsp packet {task}` now, read what it prints, \
         do the change it describes, run its one command. {} Read nothing the packet does not name.",
        flow.order_end(task)
    )
}

// ---- a leg's packet ---------------------------------------------------------------

/// The order key a leg names its packet on: `packet: spikes/x/change-hint.md`,
/// a line of its own, relative to the leg's tree (or absolute).
/// `wsp leg add … --packet FILE` writes it.
pub const LEG_KEY: &str = "packet:";

/// The packet a leg's order names, when it names one. `packet: off` is not a
/// path: a leg that says so is spawned the ordinary way.
pub fn leg_path(order: &str) -> Option<String> {
    order.lines().find_map(|l| {
        let v = l.trim().trim_start_matches("- ").trim().strip_prefix(LEG_KEY)?.trim();
        (!v.is_empty() && !v.eq_ignore_ascii_case("off")).then(|| v.to_string())
    })
}

/// Whether a leg's order says `packet: off`: it is spawned the ordinary way and
/// is not given a packet.
pub fn leg_declined(order: &str) -> bool {
    order.lines().any(|l| {
        l.trim().trim_start_matches("- ").trim().strip_prefix(LEG_KEY).is_some_and(|v| v.trim().eq_ignore_ascii_case("off"))
    })
}

/// Where a generated leg packet is kept: in the store, beside the row, so a
/// person can read and edit it before the leg runs.
pub fn leg_file(store: &Store, task: &str, leg: &str) -> PathBuf {
    store.root.join("packets").join(format!("{task}.{leg}.md"))
}

/// A leg that has no packet and has not declined one is given one at spawn:
/// generated from its order, the legs before it and the row, written to
/// [`leg_file`] and named on the leg's order as `packet: FILE`, which is what
/// `wsp show` and `wsp packet --leg` read. A leg that reads as judgement, or
/// whose pack names no file, is left as it was; the reason goes to stderr.
pub fn give_leg(store: &Store, task: &str, leg: &str, root: &Path, full: bool, no_packet: bool, kind_short: bool) {
    if full || no_packet || !kind_short {
        return;
    }
    // The pack can take two minutes: the lock is held only to write the order.
    let Some(t) = store.find_task(task) else { return };
    let all = crate::legs::legs(&t);
    let Some(leg_at) = all.iter().find(|l| l.name == leg) else { return };
    if crate::packet_gen::leg_eligible(leg_at).is_err() {
        return;
    }
    let made = crate::packet_gen::generate_leg(store, &t, leg_at, root);
    match &made.pack {
        Ok(0) => return eprintln!("wsp: {task} leg {leg} is not given a packet: the pack found no file this leg will touch"),
        Err(why) => return eprintln!("wsp: {task} leg {leg} is not given a packet: {why}"),
        Ok(_) => {}
    }
    let file = leg_file(store, task, leg);
    let wrote = file
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|_| std::fs::write(&file, format!("{GENERATED}\n{}\n", made.section)));
    if let Err(e) = wrote {
        return eprintln!("wsp: {task} leg {leg} is not given a packet: {}: {e}", util::contract(&file));
    }
    store.locked(|| {
        let Some(mut t) = store.find_task(task) else { return };
        let mut all = crate::legs::legs(&t);
        let Some(at) = all.iter().position(|l| l.name == leg) else { return };
        if crate::packet_gen::leg_eligible(&all[at]).is_err() {
            return;
        }
        all[at].order = format!("{LEG_KEY} {}\n\n{}", file.display(), all[at].order);
        crate::legs::write(&mut t, &all);
        if let Err(e) = store.save_task(&t) {
            return eprintln!("wsp: {task} leg {leg} is not given a packet: {e}");
        }
        store.wrote(&file);
        store.git_commit(&format!("wsp: packet {task} leg {leg} — generated"));
    });
}

/// How a leg ends: `leg done`, or `review` for the leg nothing waits on.
pub fn leg_finish(task: &str, last: bool) -> String {
    match last {
        true => format!("You are the last leg: commit, then `wsp review {task} \"...\"`, as a Bash call of its own, ends the row."),
        false => format!(
            "Commit, write your baton with a heredoc into $WSP_BATON (`cat > \"$WSP_BATON\" <<'EOF'` ... `EOF`), then end with \
             `wsp leg done {task} --caption \"...\"` as a Bash call of its own, never chained after another command."
        ),
    }
}

/// A leg's packet, printed: the file its order names, read as a `## Packet`
/// section is (its `file:` lines inlined), then the leg's finish line.
pub fn build_leg(task: &str, leg: &str, path: &str, root: &Path, last: bool) -> Result<String, String> {
    let at = if Path::new(path).is_absolute() { PathBuf::from(path) } else { root.join(path) };
    let text = std::fs::read_to_string(&at)
        .map_err(|e| format!("leg {leg}'s `packet: {path}` cannot be read from {}: {e}", util::contract(root)))?;
    let p = parse(&text);
    render(&format!("# Packet · {task} · leg {leg}\n"), &p, root, &format!("{task} leg {leg}"), &leg_finish(task, last))
}

/// A leg spawn's decision: the size of its packet when it starts short, `None`
/// when it starts as ever. A packet that cannot be printed is said on stderr.
pub fn at_leg_spawn(task: &str, leg: &str, order: &str, root: &Path, last: bool, full: bool, no_packet: bool, kind_short: bool) -> Option<u64> {
    if full || no_packet || !kind_short {
        return None;
    }
    let path = leg_path(order)?;
    match build_leg(task, leg, &path, root, last) {
        Ok(text) => Some(text.len() as u64),
        Err(why) => {
            eprintln!("wsp: {task} leg {leg} is not started short: {why}");
            None
        }
    }
}

/// A short-started leg's work order: the packet, then the finish line.
pub fn leg_order(task: &str, leg: &str, last: bool) -> String {
    format!(
        "You are leg {leg} of {task}. Your packet is the whole brief. Run `wsp packet {task} --leg {leg}` now, \
         read what it prints, do the change it describes, run its one command. {} \
         Read nothing the packet does not name.",
        leg_finish(task, last)
    )
}

/// The leg this seat is running on `t`, if any — so a bare `wsp packet <row>`
/// in a leg's seat reads the leg's packet, as the short start's system prompt
/// tells it to.
fn my_leg(t: &Task) -> Option<String> {
    let pane = crate::place::my_pane()?;
    crate::legs::actor_of(&crate::legs::legs(t), Some(&pane))
}

/// A path the packet may inline: relative, and staying inside the tree.
fn inside(path: &str) -> bool {
    let p = Path::new(path);
    !p.is_absolute() && p.components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

pub fn build(t: &Task, root: &Path) -> Result<String, String> {
    let Some(p) = of(t) else { return Err(format!("{} has no ## {SECTION} section", t.id)) };
    render(&format!("# Packet · {} · {}\n", t.id, t.title), &p, root, &t.id, "")
}

/// What a packet that names a `cargo test` says of how much to run (`wsp-389`):
/// the narrow filter is the member's, the full suite the barrier's.
pub const TEST_SCOPE: &str = "Tests: run only the one command above, or a narrower filter of it, for what you changed — never the full `cargo test`. The full suite runs once per group, at the barrier and (where the project declares it) in the verifier's tier; a full-suite run in your seat is noted in `wsp wip`. If the full suite matters for your change, name it in your review summary and leave it to the barrier.";

/// The print: a heading, the author's words, each named file inline, and a tail
/// (a leg's finish line), refused over [`CAP`].
fn render(head: &str, p: &Packet, root: &Path, who: &str, tail: &str) -> Result<String, String> {
    let mut parts: Vec<(String, String)> = Vec::new();
    for f in &p.files {
        if !inside(f) {
            return Err(format!("`file: {f}` leaves the tree — a packet names paths relative to it"));
        }
        let body = std::fs::read_to_string(root.join(f))
            .map_err(|e| format!("`file: {f}` cannot be read from {}: {e}", util::contract(root)))?;
        parts.push((f.clone(), body));
    }
    let mut out = head.to_string();
    if !p.text.is_empty() {
        out.push_str(&format!("\n{}\n", p.text));
    }
    if p.text.contains("cargo test") {
        out.push_str(&format!("\n{TEST_SCOPE}\n"));
    }
    for (path, body) in &parts {
        out.push_str(&format!("\n## {path}\n```\n{}\n```\n", body.trim_end()));
    }
    if !tail.is_empty() {
        out.push_str(&format!("\n{tail}\n"));
    }
    if out.len() > CAP {
        let mut sizes: Vec<(usize, &str)> = parts.iter().map(|(f, b)| (b.len(), f.as_str())).collect();
        sizes.sort_by(|a, b| b.0.cmp(&a.0));
        let largest: Vec<String> = sizes.iter().take(3).map(|(n, f)| format!("{f} {n}")).collect();
        return Err(format!(
            "the packet for {} is {} bytes and the cap is {CAP}; the prose is {} and the largest files are {}",
            who,
            out.len(),
            p.text.len(),
            if largest.is_empty() { "none".to_string() } else { largest.join(", ") }
        ));
    }
    Ok(out)
}

/// The project's own checkout, where a spawn reads the packet's files before the
/// agent has a tree.
fn tree_root(store: &Store, t: &Task) -> PathBuf {
    t.project
        .as_deref()
        .and_then(|p| crate::resolve::Index::new(store.projects()).root_for(p, &t.named_paths()))
        .map(|r| util::expand(&r))
        .unwrap_or_default()
}

/// Where the named files are read from: `--root`, else the repository the
/// caller stands in (the agent's own tree), else the project's root.
fn root_for(store: &Store, t: &Task, args: &Args) -> PathBuf {
    if let Some(r) = args.get("root") {
        return util::expand(&r);
    }
    let here = std::env::current_dir().unwrap_or_default();
    crate::cmd_verify::toplevel(&here).unwrap_or_else(|| {
        let root = tree_root(store, t);
        if root.as_os_str().is_empty() { here } else { root }
    })
}

/// `wsp packet <task> [--root DIR]` prints it, writing one first for a row that
/// has none (`--regen` replaces a generated one, `--regen --force` a hand-written one); `--from FILE` or a bare `-`
/// writes the section from a file or stdin, so prose never passes through a
/// shell string.
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
        let text = crate::util::read_source(&from).map_err(|e| format!("{from}: {e}"));
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
    let root = root_for(store, &t, args);
    if let Some(leg) = args.get("leg").filter(|l| l != "true").or_else(|| my_leg(&t)) {
        let all = crate::legs::legs(&t);
        let Some(l) = all.iter().find(|l| l.name == leg) else {
            eprintln!("wsp: {} has no leg called {leg}", t.id);
            return 1;
        };
        let Some(path) = leg_path(&l.order) else {
            if args.get("leg").is_some() {
                eprintln!("wsp: leg {leg} of {} names no packet (an order line `{LEG_KEY} FILE`)", t.id);
                return 1;
            }
            // A leg with no packet of its own, asked bare: the row's, as ever.
            return print_row(store, &mut t, &root, args);
        };
        let last = crate::legs::lasts(&all).first() == Some(&leg);
        return match build_leg(&t.id, &leg, &path, &root, last) {
            Ok(text) => {
                print!("{text}");
                0
            }
            Err(e) => {
                eprintln!("wsp: {e}");
                1
            }
        };
    }
    print_row(store, &mut t, &root, args)
}

/// The row's own packet, written first when it has none (or `--regen`).
fn print_row(store: &Store, t: &mut Task, root: &Path, args: &Args) -> i32 {
    let has = t.section(SECTION).is_some_and(|s| !s.trim().is_empty());
    if !has || args.has("regen") {
        if has && of(t).is_some_and(|p| p.off) {
            eprintln!("wsp: {} declined a packet (`packet: off`); remove that line to have one made", t.id);
            return 1;
        }
        if has && !t.section(SECTION).is_some_and(|s| s.lines().any(|l| l.trim() == GENERATED)) && !args.has("force") {
            eprintln!("wsp: {} has a packet written by hand; `wsp packet {} --regen --force` replaces it", t.id, t.id);
            return 1;
        }
        let made = crate::packet_gen::generate(store, t, root);
        match &made.pack {
            Ok(n) => eprintln!("wsp: {} read as {} work; the pack expects {n} file(s) to change", t.id, made.kind.name()),
            Err(why) => eprintln!("wsp: no context pack ({why}); the packet is the overview and the files it names"),
        }
        if let Err(why) = crate::packet_gen::write(store, t, &made.section) {
            eprintln!("wsp: {why}");
            return 1;
        }
        eprintln!("wsp: wrote ## {SECTION} on {} — edit it, or `packet: off` to decline", t.id);
    }
    match build(t, root) {
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

/// The verbs this module answers to, each with the help it carries. `main` and
/// the help text are assembled from these; nothing in `main.rs` names them.
pub const VERBS: &[crate::verbs::Verb] = &[
    crate::verbs::Verb {
        names: &["packet"],
        run: packet,
        help: &[
            crate::verbs::Help {
                section: crate::verbs::Section::Agents,
                order: 40,
                text: r##"  wsp packet <id> [--regen [--force]|--from FILE|-]
                                    print a row's ## Packet and the files it names, under a cap;
                                    a row with none gets one generated (the overview, the context
                                    pack, the likely files) and written; a small row is spawned
                                    with one and the short start unless `--no-packet`;
                                    `--leg NAME` prints that leg's packet (an order line
                                    `packet: FILE`, or `wsp leg add … --packet FILE`), which
                                    also starts the leg short"##,
            },
        ],
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd_checkout::Flow;

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
    fn a_packet_naming_a_cargo_test_says_the_full_suite_is_the_barriers() {
        let dir = std::env::temp_dir().join(format!("wsp-packet-scope-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "fn a() {}\n").unwrap();
        let with = build(&row("file: src/a.rs\nverify  cargo test a::"), &dir).unwrap();
        assert!(with.contains(TEST_SCOPE), "{with}");
        let without = build(&row("file: src/a.rs\nSave prints ok."), &dir).unwrap();
        assert!(!without.contains(TEST_SCOPE), "{without}");
        let _ = std::fs::remove_dir_all(&dir);
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
    fn a_leg_names_its_packet_on_an_order_line_and_off_is_not_a_path() {
        assert_eq!(leg_path("packet: spikes/p/change-hint.md\n\nDo the hint."), Some("spikes/p/change-hint.md".into()));
        assert_eq!(leg_path("Do it.\n- packet: a.md"), Some("a.md".into()));
        assert_eq!(leg_path("Read the call sites."), None);
        assert_eq!(leg_path("packet: off"), None);
        assert_eq!(leg_path("packet:"), None);
    }

    #[test]
    fn a_legs_packet_is_its_file_inlined_then_its_finish_line_and_it_starts_short() {
        let dir = std::env::temp_dir().join(format!("wsp-leg-packet-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "fn a() {}\n").unwrap();
        std::fs::write(dir.join("change.md"), "Make a return 1.\nfile: src/a.rs\n").unwrap();
        let mid = build_leg("t-1", "hint", "change.md", &dir, false).unwrap();
        assert!(mid.starts_with("# Packet · t-1 · leg hint\n"), "{mid}");
        assert!(mid.contains("Make a return 1.") && mid.contains("## src/a.rs\n```\nfn a() {}\n```"), "{mid}");
        assert!(mid.trim_end().ends_with(&leg_finish("t-1", false)) && mid.contains("wsp leg done t-1"), "{mid}");
        let last = build_leg("t-1", "hint", "change.md", &dir, true).unwrap();
        assert!(last.contains("wsp review t-1") && !last.contains("leg done"), "{last}");
        assert!(build_leg("t-1", "hint", "gone.md", &dir, false).unwrap_err().contains("gone.md"));

        let order = "packet: change.md\n\nthe hint";
        let size = at_leg_spawn("t-1", "hint", order, &dir, false, false, false, true);
        assert_eq!(size, Some(mid.len() as u64), "a leg with a packet starts short with its size");
        assert_eq!(at_leg_spawn("t-1", "hint", "the hint", &dir, false, false, false, true), None, "no packet line");
        assert_eq!(at_leg_spawn("t-1", "hint", order, &dir, false, true, false, true), None, "--full");
        assert_eq!(at_leg_spawn("t-1", "hint", order, &dir, false, false, true, true), None, "--no-packet");
        assert_eq!(at_leg_spawn("t-1", "hint", order, &dir, false, false, false, false), None, "a kind with no short start");
        assert_eq!(at_leg_spawn("t-1", "hint", "packet: gone.md", &dir, false, false, false, true), None, "unprintable");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_legs_order_names_its_packet_verb_and_its_finish() {
        let o = leg_order("t-1", "hint", false);
        assert!(o.contains("`wsp packet t-1 --leg hint`") && o.contains("wsp leg done t-1") && !o.contains('\n'), "{o}");
        assert!(leg_order("t-1", "hint", true).contains("wsp review t-1"));
        assert!(o.is_ascii(), "a spawn once hung on one character");
    }

    #[test]
    fn the_order_is_one_line_naming_the_verb_and_the_row() {
        let o = order("wsp-9", Flow::Own);
        assert!(!o.contains('\n') && o.contains("`wsp packet wsp-9`"));
        assert!(o.contains("`wsp land wsp-9`") && o.contains("`wsp review wsp-9 "), "{o}");
        assert!(!o.contains("as your brief says"), "a short start carries almost no brief: {o}");
        assert!(o.is_ascii(), "a spawn once hung on one character");
    }

    /// `wsp-379`: the order and the banner are one decision. A verified member
    /// is told to end with `wsp review` and never to land, in both; a plain row
    /// is told to land, in both.
    #[test]
    fn a_verified_member_and_a_plain_row_are_each_told_one_flow_by_order_and_banner() {
        let verified = order("wsp-9", Flow::of(true));
        assert!(verified.contains("`wsp review wsp-9 ") && verified.contains("never `wsp land`"), "{verified}");
        assert!(!verified.contains("run `wsp land"), "{verified}");
        assert_eq!(Flow::of(true).banner(), crate::cmd_checkout::READY_FLOW);
        assert!(Flow::of(true).banner().contains("Never `wsp land`"));
        assert!(Flow::of(true).tree().contains("a verifier lands it"));

        let plain = order("wsp-9", Flow::of(false));
        assert!(plain.contains("run `wsp land wsp-9`") && plain.contains("`wsp review wsp-9 "), "{plain}");
        assert!(!plain.contains("never `wsp land`"), "{plain}");
        assert_eq!(Flow::of(false).banner(), crate::cmd_checkout::LANDING);
        assert!(Flow::of(false).tree().contains("`wsp land` puts it on the trunk"));
        assert!(verified.is_ascii() && plain.is_ascii());
    }

    // ---- a row with no packet, at spawn ------------------------------------

    /// A stand-in for the context pack builder: the same arguments, canned output.
    struct FakePack(PathBuf);

    impl FakePack {
        fn with(dir: &Path, likely: &str) -> FakePack {
            let script = dir.join("ixv2");
            let body = format!(
                "#!/bin/sh\ncat >/dev/null\ncase \"$*\" in\n*\"--depth meta\"*) echo '{{\"est_code_lines\":10,\"precedents\":[],\"likely\":[{likely}],\"recipe\":null}}';;\n*) printf 'CONTEXT PACK  compound@abc  built from 1 finished tasks  kind: x\\nlikely files  src/a.rs (identifier)\\n';;\nesac\n"
            );
            std::fs::write(&script, body).unwrap();
            let _ = std::process::Command::new("chmod").arg("+x").arg(&script).status();
            std::env::set_var("WSP_PACK_BIN", &script);
            FakePack(script)
        }
    }

    impl Drop for FakePack {
        fn drop(&mut self) {
            std::env::remove_var("WSP_PACK_BIN");
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// A project `pj` whose repository has `src/a.rs`, and one row with `title`
    /// and `section` (when given) as its `## Packet`.
    fn world(name: &str, title: &str, section: Option<&str>) -> (crate::util::Isolated, Store, PathBuf) {
        let env = crate::util::isolated(&format!("packet-gen-{name}"));
        let repo = env.path("repo");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/a.rs"), "pub fn a() {}\n").unwrap();
        std::fs::write(repo.join("Cargo.toml"), "[package]\nname = \"pj\"\n").unwrap();
        crate::testkit::repo(&repo);
        git(&repo, &["add", "."]);
        git(&repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "--quiet", "-m", "pj-001: first"]);
        let store = Store::open();
        let mut p = crate::model::Project::new("pj");
        p.roots = vec![repo.display().to_string()];
        store.save_project(&p).unwrap();
        let mut t = Task::new(title, "pj-001");
        t.project = Some("pj".into());
        t.body = format!("## Overview\nShow the size in `src/a.rs`.\n{}", section.map(|s| format!("\n## {SECTION}\n{s}\n")).unwrap_or_default());
        store.save_task(&t).unwrap();
        (env, store, repo)
    }

    fn spawn(store: &Store, full: bool, no_packet: bool) -> Option<u64> {
        at_spawn(store, Some("pj-001"), true, full, no_packet, true)
    }

    #[test]
    fn a_small_row_with_no_packet_is_given_one_and_starts_short_with_its_size() {
        let (env, store, _) = world("small", "Show the packet size", None);
        let _builder = FakePack::with(&env.path(""), "\"src/a.rs\"");
        let bytes = spawn(&store, false, false).expect("a packet row");
        let t = store.find_task("pj-001").unwrap();
        let written = t.section(SECTION).expect("written into the row, where a person can edit it");
        assert!(written.contains("file: src/a.rs") && written.contains("CONTEXT PACK  pj@abc"), "{written}");
        assert!(written.contains("verify  cargo test a::"), "the pack named no test, so the first file's tests are: {written}");
        let printed = build(&t, &tree_root(&store, &t)).unwrap();
        assert_eq!(bytes as usize, printed.len(), "the size recorded is the size the agent is handed");
        assert!(printed.contains("pub fn a() {}") && printed.len() <= CAP);
    }

    #[test]
    fn a_row_that_asked_for_the_ordinary_start_is_left_alone() {
        let (env, store, _) = world("left", "Show the packet size", None);
        let _builder = FakePack::with(&env.path(""), "\"src/a.rs\"");
        assert!(spawn(&store, true, false).is_none() && spawn(&store, false, true).is_none(), "--full and --no-packet");
        assert!(at_spawn(&store, Some("pj-001"), false, false, false, true).is_none(), "a leg, a verifier or a check");
        assert!(at_spawn(&store, Some("pj-001"), true, false, false, false).is_none(), "a kind with no short start");
        assert!(store.find_task("pj-001").unwrap().section(SECTION).is_none(), "and none of them wrote a section");
    }

    #[test]
    fn a_row_that_declined_a_packet_is_left_alone() {
        let (env, store, _) = world("declined", "Show the packet size", Some("packet: off"));
        let _builder = FakePack::with(&env.path(""), "\"src/a.rs\"");
        assert!(spawn(&store, false, false).is_none());
        assert_eq!(store.find_task("pj-001").unwrap().section(SECTION).unwrap().trim(), "packet: off");
    }

    /// `pj-001` with two legs: `scout` (done, with a caption) and `edit`, whose
    /// order is `order`.
    fn with_legs(store: &Store, order: &str) {
        let mut t = store.find_task("pj-001").unwrap();
        let mut scout = crate::legs::Leg::pending("scout", "look at it");
        scout.state = crate::legs::State::Done;
        scout.caption = Some("found the size is computed in a.rs".into());
        crate::legs::write(&mut t, &[scout, crate::legs::Leg::pending("edit", order)]);
        store.save_task(&t).unwrap();
    }

    fn edit_order(store: &Store) -> String {
        crate::legs::legs(&store.find_task("pj-001").unwrap()).into_iter().find(|l| l.name == "edit").unwrap().order
    }

    #[test]
    fn a_leg_with_no_packet_is_given_one_beside_the_row_and_starts_short() {
        let (env, store, repo) = world("leg-given", "Show the packet size", None);
        std::fs::write(repo.join("src/b.rs"), "pub fn b() {}\n").unwrap();
        let _builder = FakePack::with(&env.path(""), "\"src/b.rs\", \"src/a.rs\"");
        with_legs(&store, "Print the size in `src/a.rs`.");
        give_leg(&store, "pj-001", "edit", &repo, false, false, true);
        let order = edit_order(&store);
        let path = leg_path(&order).expect("the order names the packet it was given");
        assert_eq!(PathBuf::from(&path), leg_file(&store, "pj-001", "edit"));
        assert!(order.contains("Print the size"), "the author's order is kept: {order}");
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.starts_with(GENERATED), "{written}");
        assert!(written.find("file: src/a.rs").unwrap() < written.find("file: src/b.rs").unwrap(), "the order's file leads: {written}");
        assert!(written.contains("found the size is computed in a.rs"), "the earlier leg's account: {written}");
        assert!(written.contains("Show the packet size") || written.contains("pj-001"), "the row: {written}");
        let printed = build_leg("pj-001", "edit", &path, &repo, true).unwrap();
        assert_eq!(at_leg_spawn("pj-001", "edit", &order, &repo, true, false, false, true), Some(printed.len() as u64));
        assert!(printed.contains("pub fn a() {}") && printed.contains("wsp review pj-001"), "{printed}");
        let again = edit_order(&store);
        give_leg(&store, "pj-001", "edit", &repo, false, false, true);
        assert_eq!(edit_order(&store), again, "a leg that has one is not given a second");
    }

    #[test]
    fn a_leg_that_declined_or_was_spawned_full_is_left_alone() {
        let (env, store, repo) = world("leg-left", "Show the packet size", None);
        let _builder = FakePack::with(&env.path(""), "\"src/a.rs\"");
        with_legs(&store, "packet: off\n\nPrint the size in `src/a.rs`.");
        give_leg(&store, "pj-001", "edit", &repo, false, false, true);
        assert_eq!(leg_path(&edit_order(&store)), None, "packet: off");
        assert_eq!(at_leg_spawn("pj-001", "edit", &edit_order(&store), &repo, true, false, false, true), None);
        with_legs(&store, "Print the size in `src/a.rs`.");
        for (full, no_packet, short) in [(true, false, true), (false, true, true), (false, false, false)] {
            give_leg(&store, "pj-001", "edit", &repo, full, no_packet, short);
            assert_eq!(leg_path(&edit_order(&store)), None, "full {full} no_packet {no_packet} short {short}");
        }
        assert!(!leg_file(&store, "pj-001", "edit").exists());
    }

    #[test]
    fn a_leg_that_reads_as_judgement_is_left_alone() {
        let (env, store, repo) = world("leg-judged", "Show the packet size", None);
        let _builder = FakePack::with(&env.path(""), "\"src/a.rs\"");
        with_legs(&store, "Investigate why the size is wrong and measure the cost; this is a spike.");
        give_leg(&store, "pj-001", "edit", &repo, false, false, true);
        assert_eq!(leg_path(&edit_order(&store)), None, "discovery work is not briefed by a packet");
    }

    #[test]
    fn a_leg_whose_pack_names_no_file_is_left_alone() {
        let (env, store, repo) = world("leg-nofiles", "Show the packet size", None);
        let _builder = FakePack::with(&env.path(""), "");
        with_legs(&store, "Print the size.");
        give_leg(&store, "pj-001", "edit", &repo, false, false, true);
        assert_eq!(leg_path(&edit_order(&store)), None, "a pack that names no file");
    }

    #[test]
    fn a_novel_row_is_not_given_one() {
        let (env, store, _) = world("novel", "Measure why the daemon is slow", None);
        let _builder = FakePack::with(&env.path(""), "\"src/a.rs\"");
        assert!(spawn(&store, false, false).is_none());
        assert!(store.find_task("pj-001").unwrap().section(SECTION).is_none(), "novel work is spawned as today");
    }

    #[test]
    fn a_row_a_person_wrote_a_packet_for_keeps_it_whatever_kind_it_is() {
        let (_env, store, _) = world("authored", "Measure why the daemon is slow", Some("Do it.\nfile: src/a.rs"));
        let bytes = spawn(&store, false, false).expect("an authored packet starts short");
        let t = store.find_task("pj-001").unwrap();
        assert_eq!(t.section(SECTION).unwrap().trim(), "Do it.\nfile: src/a.rs");
        assert_eq!(bytes as usize, build(&t, &tree_root(&store, &t)).unwrap().len());
    }

    #[test]
    fn no_builder_leaves_the_row_to_the_ordinary_start() {
        let (_env, store, _) = world("nobuilder", "Show the packet size", None);
        std::env::set_var("WSP_PACK_BIN", "/nonexistent/ixv2");
        assert!(spawn(&store, false, false).is_none());
        std::env::remove_var("WSP_PACK_BIN");
        assert!(store.find_task("pj-001").unwrap().section(SECTION).is_none());
    }

    #[test]
    fn a_pack_that_names_no_file_still_gives_the_row_its_overview_as_the_packet() {
        let (env, store, _) = world("nofiles", "Show the packet size", None);
        let _builder = FakePack::with(&env.path(""), "");
        let bytes = spawn(&store, false, false).expect("a short start, not an empty one");
        let t = store.find_task("pj-001").unwrap();
        let written = t.section(SECTION).expect("written into the row");
        assert!(!written.contains("file:"), "{written}");
        let printed = build(&t, &tree_root(&store, &t)).unwrap();
        assert_eq!(bytes as usize, printed.len());
        assert!(printed.contains("Show the size in"), "the packet the agent is handed holds the overview: {printed}");
    }

    #[test]
    fn a_packet_that_cannot_be_printed_is_not_started_short() {
        let (_env, store, _) = world("missing", "Show the packet size", Some("file: src/gone.rs\nDo it."));
        assert!(spawn(&store, false, false).is_none(), "the order would point at a verb that refuses");
    }

    #[test]
    fn the_claim_clause_carries_the_size_and_reads_back_old_claims_too() {
        assert_eq!(clause(Some(4210)), "packet 4210");
        assert_eq!(clause(None), "packet");
        assert_eq!(clause_bytes("packet 4210"), Some(Some(4210)));
        assert_eq!(clause_bytes("packet"), Some(None));
        assert_eq!(clause_bytes("packets"), None);
        assert_eq!(clause_bytes("spawned at sonnet/high"), None);
    }

    #[test]
    fn regen_refuses_a_hand_written_packet_unless_forced_and_the_marker_is_not_the_brief() {
        let env = crate::util::isolated("packet-regen-guard");
        let store = Store::open();
        let mut t = Task::new("Fix a flag", "pk-001");
        t.body = "## Overview\nFix the flag in src/a.rs.\n\n## Packet\nMy own words.\n".into();
        store.save_task(&t).unwrap();
        let run = |extra: &[&str]| {
            let mut argv: Vec<String> = ["packet", "pk-001", "--root"].iter().map(|s| s.to_string()).collect();
            argv.push(env.path("tree").display().to_string());
            argv.extend(extra.iter().map(|s| s.to_string()));
            packet(&store, &Args::parse(argv))
        };
        std::fs::create_dir_all(env.path("tree")).unwrap();
        let body = || match store.resolve_task("pk-001") {
            Found::One(t) => t.section(SECTION).unwrap_or_default(),
            _ => panic!("gone"),
        };
        assert_eq!(run(&["--regen"]), 1, "a hand-written packet is not replaced");
        assert_eq!(body().trim(), "My own words.");
        assert_eq!(run(&["--regen", "--force"]), 0);
        assert!(body().contains(GENERATED) && !body().contains("My own words."), "{}", body());
        assert_eq!(run(&["--regen"]), 0, "a generated packet is regenerated without the flag");
        let p = parse(&body());
        assert!(!p.text.contains("generated packet"), "the marker is not the brief: {}", p.text);
    }
}
