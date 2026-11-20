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

/// The narrow order's last sentence, which [`order_for`] swaps for
/// [`SEARCH_RULE`] when the packet's files miss its subject.
const NARROW: &str = "Read nothing the packet does not name.";

/// What a seat whose packet misses its subject is told instead of [`NARROW`]
/// (`wsp-039`): wsp-016's packet named the wrong files, its order forbade
/// reading others, and the seat stopped to ask where the code was.
pub const SEARCH_RULE: &str = "The files this packet names may not hold what the row describes: \
     read those first, and if the subject is not in them, search the tree for the names the row gives \
     (`git grep`) and read only what that search finds, then say in your exit which files outside \
     the packet you read and why.";

/// The order for a packet row, chosen on its packet: the narrow order when the
/// files it names hold what its overview describes, else one that allows a
/// bounded search and says to report it ([`SEARCH_RULE`]). A row not found is
/// given the narrow order.
pub fn order_in(store: &Store, task: &str, flow: crate::cmd_checkout::Flow) -> String {
    match store.find_task(task) {
        Some(t) => order_for(&t, &tree_root(store, &t), flow),
        None => order(task, flow),
    }
}

/// [`order_in`] for a row in hand, reading its named files under `root`.
pub fn order_for(t: &Task, root: &Path, flow: crate::cmd_checkout::Flow) -> String {
    let narrow = order(&t.id, flow);
    match misses_subject(t, root) {
        true => narrow.replace(NARROW, SEARCH_RULE),
        false => narrow,
    }
}

/// The files a packet names: its `file:` lines, the paths on its context pack's
/// `likely files` line, and any path its prose or title spells out.
pub fn named_files(t: &Task) -> Vec<String> {
    let Some(p) = of(t) else { return Vec::new() };
    let mut out: Vec<String> = p.files.clone();
    for line in p.text.lines() {
        if let Some(rest) = line.trim().strip_prefix("likely files") {
            out.extend(rest.split(',').filter_map(|f| f.split_whitespace().next()).map(|f| f.to_string()));
        }
    }
    let prose = format!("{}\n{}\n{}", t.title, brief(t), p.text);
    out.extend(prose.split_whitespace().filter_map(path_in));
    let mut seen = std::collections::HashSet::new();
    out.retain(|f| inside(f) && seen.insert(f.clone()));
    out
}

/// A word that spells a path (`src/a.rs`, `(src/packet.rs:193`), bare.
fn path_in(word: &str) -> Option<String> {
    let w = word.trim_matches(|c: char| "`'\"()[]{}<>,;".contains(c));
    let w = w.split(':').next().unwrap_or(w).trim_end_matches('.');
    let (stem, ext) = w.rsplit_once('.')?;
    let ok = w.contains('/')
        && !w.contains("//")
        && !stem.is_empty()
        && (1..=5).contains(&ext.len())
        && ext.chars().all(|c| c.is_ascii_alphanumeric())
        && w.chars().all(|c| c.is_ascii_alphanumeric() || "/._-".contains(c));
    ok.then(|| w.to_string())
}

/// The sections a row's brief is written in: an overview, or, for a row
/// written from evidence (a retro row, wsp-016's shape), its evidence, fix and
/// done-when, which carry the names where there is no overview at all.
pub const BRIEF: &[&str] = &["Overview", "Evidence", "Fix", "Done when"];

/// The row's brief: each of [`BRIEF`] it has, in that order.
fn brief(t: &Task) -> String {
    BRIEF.iter().filter_map(|s| t.section(s)).collect::<Vec<_>>().join("\n")
}

/// The names the row's brief is about ([`BRIEF`]): words in backticks that read
/// as an identifier (`wall_bucket`, `Store::open`), and bare words with `_` or
/// `::` in them. Commands, flags and paths are not names.
pub fn subject(t: &Task) -> Vec<String> {
    let overview = brief(t);
    let ident = |w: &str| {
        let w = w.trim_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '_'));
        let ok = w.len() >= 4
            && w.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && w.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
            && !w.contains(":::");
        ok.then(|| w.to_string())
    };
    let mut out: Vec<String> = overview.split('`').skip(1).step_by(2).filter(|s| !s.contains(char::is_whitespace) && !s.starts_with('-')).filter_map(ident).collect();
    out.extend(
        overview
            .split('`')
            .step_by(2)
            .flat_map(|s| s.split_whitespace())
            .filter(|w| path_in(w).is_none() && (w.contains('_') || w.contains("::")))
            .filter_map(ident),
    );
    let mut seen = std::collections::HashSet::new();
    out.retain(|w| seen.insert(w.clone()));
    out
}

/// A name only code spells: one with `_` or `::` in it, or a lower letter
/// followed by a capital (`wall_buckets`, `Store::open`, `wallBuckets`). A
/// plain word in backticks (`running`, `finish`) is in half the files of a
/// tree, so it says nothing about whether these are the right ones.
pub fn distinctive(name: &str) -> bool {
    name.contains('_')
        || name.contains("::")
        || name.as_bytes().windows(2).any(|w| w[0].is_ascii_lowercase() && w[1].is_ascii_uppercase())
}

/// Whether the files a packet names miss what its brief describes, read from
/// the files it names that can be read under `root` (the governor's rule after
/// wsp-039's second block):
///
/// - When the brief has [`distinctive`] names, only those are judged, and the
///   files miss when **any one** of them is in none of the files. A common word
///   in backticks cannot clear a row (wsp-016's `running` and `finish` were in
///   src/cycle.rs a hundred times each), and one name found does not clear the
///   rest (a file with `wall_buckets` but not `legs_running_seconds` misses).
/// - Otherwise the plain names are judged, and the files miss when not one of
///   them is in any file.
///
/// A brief that names nothing gives nothing to miss, and a row with no packet is
/// not spawned short: both keep the narrow order.
pub fn misses_subject(t: &Task, root: &Path) -> bool {
    if of(t).is_none() {
        return false;
    }
    let names = subject(t);
    if names.is_empty() {
        return false;
    }
    let bodies: Vec<String> = named_files(t).iter().filter_map(|f| std::fs::read_to_string(root.join(f)).ok()).collect();
    let found = |n: &String| {
        let last = n.rsplit("::").next().unwrap_or(n);
        bodies.iter().any(|b| b.contains(last))
    };
    let code: Vec<&String> = names.iter().filter(|n| distinctive(n)).collect();
    match code.is_empty() {
        false => code.into_iter().any(|n| !found(n)),
        true => !names.iter().any(found),
    }
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

/// How a leg ends, on one ASCII line for its order: `wsp submit`, which is
/// `leg done` for a leg and the row's close for the leg nothing waits on.
pub fn leg_finish(task: &str, last: bool) -> String {
    match last {
        true => format!("You are the last leg: commit, then {}", crate::closing::how_to_close(task, true)),
        false => format!("Commit, then {}", crate::closing::how_to_close(task, false)),
    }
}

/// A leg's packet, printed: the file its order names, read as a `## Packet`
/// section is (its `file:` lines inlined), with the leg's exit template — a
/// member's for the last leg, which submits as one (`wsp-617`).
pub fn build_leg(task: &str, leg: &str, path: &str, root: &Path, last: bool) -> Result<String, String> {
    let at = if Path::new(path).is_absolute() { PathBuf::from(path) } else { root.join(path) };
    let text = std::fs::read_to_string(&at)
        .map_err(|e| format!("leg {leg}'s `packet: {path}` cannot be read from {}: {e}", util::contract(root)))?;
    let p = parse(&text);
    let kind = if last { crate::exit::Kind::Member } else { crate::exit::Kind::Leg };
    let exit = crate::closing::exit_template(kind, task, &[]);
    render(&format!("# Packet · {task} · leg {leg}\n"), &p, root, &format!("{task} leg {leg}"), &exit)
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
    let Some(mut p) = of(t) else { return Err(format!("{} has no ## {SECTION} section", t.id)) };
    if misses_subject(t, root) {
        p.text = format!("{}\n\n{SEARCH_RULE}", p.text).trim().to_string();
    }
    let head = match owed(t) {
        Some(owed) => format!("# Packet · {} · {}\n\n{owed}\n## The original brief\nWhat the row first asked, for what it names. Whatever of it already landed is done.\n", t.id, t.title),
        None => format!("# Packet · {} · {}\n", t.id, t.title),
    };
    let exit = crate::closing::exit_template(crate::exit::Kind::Member, &t.id, &crate::cmd_submit::packet_items(t));
    render(&head, &p, root, &t.id, &exit)
}

/// Bytes of the owed block: the newest reason is always in (cut to this if it
/// has to be), older ones follow while they fit.
const OWED_CAP: usize = 4_000;

/// What a reopened row is owed, to open its packet (`compounding-220`).
///
/// **The packet is saved once and the row's story moves on.** A packet is the
/// overview and the files it names, generated at the row's first spawn and kept;
/// `wsp reopen <id> "what is owed"` writes its reason to the row's log and to a
/// pane, and a seat that is re-seated afterwards (`wsp-304`) is told to read the
/// packet and nothing else. So the second seat of compounding-189 read the first
/// brief, re-reported a commit that had landed, and the verifier blocked it
/// again — the reason sat in the log, one verb away, unread. Read from the log
/// at every print, then, so no one rewrites an overview by hand for it.
///
/// The `sent back:` lines of the row's log, newest first — a reopen's, a
/// verifier's block, a failed check's — under a heading that says this is what is
/// owed and that an earlier landed commit is not it. Only while the row is at
/// `doing`: a row nobody sent back, or one that has gone back to review, is
/// printed as it always was.
pub fn owed(t: &Task) -> Option<String> {
    if t.status() != crate::model::Status::Doing {
        return None;
    }
    let log = t.section("Log").unwrap_or_default();
    let mut reasons: Vec<(String, String)> = log
        .lines()
        .filter_map(|l| l.trim().strip_prefix("- "))
        .filter_map(|l| {
            let (stamp, said) = match l.split_once(' ') {
                Some((stamp, said)) if util::is_stamp(stamp) => (stamp.get(..10).unwrap_or(stamp).to_string(), said),
                _ => (String::new(), l),
            };
            let why = said.strip_prefix(crate::cycle::SENT_BACK)?.trim();
            (!why.is_empty()).then(|| (stamp, why.to_string()))
        })
        .collect();
    if reasons.is_empty() {
        return None;
    }
    reasons.reverse();
    let mut out = String::from(
        "## What is owed now\n\
         This row was reopened. What follows is what is owed, newest first, and it is the work: \
         a commit already on this row is not it, so do not redo or re-report one — \
         read `git log` and do only what is named here.\n\n",
    );
    let mut used = 0;
    for (n, (stamp, why)) in reasons.iter().enumerate() {
        let line = match stamp.is_empty() {
            true => format!("{}. {why}\n", n + 1),
            false => format!("{}. ({stamp}) {why}\n", n + 1),
        };
        if n > 0 && used + line.len() > OWED_CAP {
            out.push_str(&format!("({} older send-back(s) are in `wsp show {} --log`)\n", reasons.len() - n, t.id));
            break;
        }
        let line = match line.len() > OWED_CAP {
            true => format!("{}\n", util::truncate(line.trim_end(), OWED_CAP)),
            false => line,
        };
        used += line.len();
        out.push_str(&line);
    }
    Some(out)
}

/// What a packet that names a `cargo test` says of how much to run (`wsp-389`):
/// the narrow filter is the member's, the full suite the barrier's.
pub const TEST_SCOPE: &str = "Tests: run only the one command above, or a narrower filter of it, for what you changed — never the full `cargo test`. The full suite runs once per group, at the barrier and (where the project declares it) in the verifier's tier; in your seat the full `cargo nextest run --workspace` is refused until you give a reason (`WSP_FULL_SUITE=\"<why>\"`, for a shared module, a fixture every test reads or a build-wide setting); the reason goes in the row's log and the verifier sees it, and a run is noted in `wsp wip`. Otherwise run your row's checks (`wsp check <row>`), name the full suite in your review summary and leave it to the barrier.";

/// The one rule every packet carries on waiting (`wsp-611`): three seats in
/// eighteen hours held to the tool's 600 s limit by a loop whose `pgrep -f`
/// found its own shell.
pub const WAIT_RULE: &str = "Waiting: never wait on `pgrep -f \"<string>\"` — the pattern is on the loop's own line, so it finds its own shell and never ends; wait on a pid (`wait <pid>`, or `while kill -0 <pid>; do sleep 5; done`) or on a `run_in_background` command's notification.";

/// The print: a heading, the author's words, the seat's exit template
/// (`wsp-617`), then each named file inline, refused over [`CAP`].
fn render(head: &str, p: &Packet, root: &Path, who: &str, exit: &str) -> Result<String, String> {
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
    out.push_str(&format!("\n{WAIT_RULE}\n"));
    if !exit.is_empty() {
        out.push_str(&format!("\n{exit}"));
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
                                    also starts the leg short; a row sent back (`wsp reopen`) has
                                    its `sent back:` reasons printed first, newest first, under
                                    "What is owed now""##,
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
    fn every_packet_says_never_to_wait_on_a_pgrep_and_to_wait_on_a_pid() {
        let dir = std::env::temp_dir().join(format!("wsp-packet-wait-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "fn a() {}\n").unwrap();
        for body in ["file: src/a.rs\nverify  cargo test a::", "file: src/a.rs\nSave prints ok."] {
            let text = build(&row(body), &dir).unwrap();
            assert_eq!(text.matches(WAIT_RULE).count(), 1, "{text}");
        }
        assert!(WAIT_RULE.contains("never wait on `pgrep -f \"<string>\"`") && WAIT_RULE.contains("wait on a pid"));
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

    /// `compounding-220`: the second seat of a reopened row read the first
    /// brief and re-reported a commit that had landed. The packet opens with
    /// what is owed, newest first, read from the log, before the original brief.
    /// `wsp-617`: the packet carries the member's exit template after the brief
    /// and before the files, its done-when pre-filled from the row's bullets;
    /// with no files it is the packet's end.
    #[test]
    fn a_packet_carries_the_member_template_with_the_rows_done_when_pre_filled() {
        let dir = std::env::temp_dir().join(format!("wsp-packet-exit-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "fn a() {}\n").unwrap();
        let mut t = Task::new("a row", "wsp-9");
        t.body = "## Overview\nMake a.\n\n### Done when\n- a returns 1\n- the test covers it\n\n## Packet\nMake a return 1.\nfile: src/a.rs\n".into();
        let out = build(&t, &dir).unwrap();
        let tmpl = crate::closing::exit_template(crate::exit::Kind::Member, "wsp-9", &["a returns 1".into(), "the test covers it".into()]);
        let (words, at, file) = (out.find("Make a return 1.").unwrap(), out.find(&tmpl).expect("the member template"), out.find("## src/a.rs").unwrap());
        assert!(words < at && at < file, "after the brief, before the files: {out}");
        assert!(out.contains("done-when:\n- held: a returns 1 — \n- held: the test covers it — \n"), "{out}");
        assert!(out.contains("`wsp submit wsp-9 --dry-run` once"), "{out}");

        t.body = "## Overview\nMake a.\n\nDone when:\n- a returns 1\n\n## Packet\nMake a return 1.\n".into();
        let bare = build(&t, &dir).unwrap();
        assert!(bare.ends_with(&crate::closing::exit_template(crate::exit::Kind::Member, "wsp-9", &["a returns 1".into()])), "{bare}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_reopened_rows_packet_opens_with_what_is_owed_newest_first_before_the_original_brief() {
        let dir = std::env::temp_dir();
        let mut t = row("Add the flag.");
        t.set_status(crate::model::Status::Doing);
        t.body.push_str("\n## Log\n- 2026-11-08 sent back: the first gap\n- 2026-11-08 review: fixed it\n- 2026-11-09T07:48:00Z sent back: the verifier blocked at 53f9831: wire the second half\n- 2026-11-09 claimed by pane cpd-9\n");
        let out = build(&t, &dir).unwrap();
        let (owed_at, brief_at) = (out.find("## What is owed now").expect(&out), out.find("Add the flag.").unwrap());
        assert!(owed_at < brief_at && out.starts_with("# Packet · t-1 · Add a flag\n"), "{out}");
        assert!(out.contains("not it"), "it says an earlier landed commit is not the work: {out}");
        let (new, old) = (out.find("wire the second half").unwrap(), out.find("the first gap").unwrap());
        assert!(new < old && new < brief_at, "newest first: {out}");
        assert!(out.contains("(2026-11-09) the verifier blocked"), "{out}");
    }

    #[test]
    fn a_row_not_sent_back_or_back_at_review_prints_as_it_always_did() {
        let dir = std::env::temp_dir();
        let plain = build(&row("Add the flag."), &dir).unwrap();
        let exit = crate::closing::exit_template(crate::exit::Kind::Member, "t-1", &[]);
        assert_eq!(plain, format!("# Packet · t-1 · Add a flag\n\nAdd the flag.\n\n{WAIT_RULE}\n\n{exit}"));
        let mut t = row("Add the flag.");
        t.body.push_str("\n## Log\n- 2026-11-08 sent back: the first gap\n");
        t.set_status(crate::model::Status::Review);
        assert_eq!(build(&t, &dir).unwrap(), plain, "answered: at review");
        t.set_status(crate::model::Status::Doing);
        t.body = t.body.replace("sent back: the first gap", "claimed by pane x");
        assert_eq!(build(&t, &dir).unwrap(), plain, "doing, never sent back");
    }

    #[test]
    fn the_owed_block_is_bounded_and_the_newest_reason_always_survives() {
        let mut t = row("x");
        t.set_status(crate::model::Status::Doing);
        let mut log = String::from("\n## Log\n");
        for i in 0..20 {
            log.push_str(&format!("- 2026-11-09 sent back: reason number {i} {}\n", "y".repeat(800)));
        }
        t.body.push_str(&log);
        let out = owed(&t).unwrap();
        assert!(out.len() < OWED_CAP + 1_000, "{}", out.len());
        assert!(out.contains("reason number 19") && !out.contains("reason number 0 "), "{out}");
        assert!(out.contains("older send-back(s) are in `wsp show t-1 --log`"), "{out}");
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
        // The leg's exit template, after its words and before its files (`wsp-617`).
        let exit = crate::closing::exit_template(crate::exit::Kind::Leg, "t-1", &[]);
        let (words, tmpl, file) = (mid.find("Make a return 1.").unwrap(), mid.find(&exit).expect("the leg template"), mid.find("## src/a.rs").unwrap());
        assert!(words < tmpl && tmpl < file, "{mid}");
        assert!(mid.contains("wsp submit t-1") && mid.contains("closes the leg") && !mid.contains("\ndone-when:\n"), "{mid}");
        let last = build_leg("t-1", "hint", "change.md", &dir, true).unwrap();
        assert!(last.contains(&crate::closing::exit_template(crate::exit::Kind::Member, "t-1", &[])), "the last leg submits as a member: {last}");
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
        assert!(o.contains("`wsp packet t-1 --leg hint`") && o.contains("`wsp submit t-1` alone") && o.contains("leg done") && !o.contains('\n'), "{o}");
        assert!(leg_order("t-1", "hint", true).contains("last leg") && leg_order("t-1", "hint", true).contains("ends the row"));
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

    /// A tree under the temp dir with `files` written in it, for the scope tests.
    fn tree(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wsp-packet-{name}-{}", std::process::id()));
        for (f, body) in files {
            std::fs::create_dir_all(dir.join(f).parent().unwrap()).unwrap();
            std::fs::write(dir.join(f), body).unwrap();
        }
        dir
    }

    fn retro(overview: &str, packet: &str) -> Task {
        let mut t = Task::default();
        t.id = "t-1".into();
        t.title = "Timing retro".into();
        t.body = format!("## Overview\n{overview}\n\n## {SECTION}\n{packet}\n");
        t
    }

    /// `wsp-039`: wsp-016's packet named files that did not hold its subject,
    /// and its order forbade reading others. A row whose files miss what its
    /// overview names is told it may search, bounded to what the search finds,
    /// and to say so in its exit; a row whose files hold it keeps the narrow order.
    #[test]
    fn a_row_whose_files_miss_its_subject_may_search_and_report_it_and_a_right_one_stays_narrow() {
        let dir = tree("scope", &[
            ("src/a.rs", "pub fn a() {}\n"),
            ("src/retro.rs", "pub fn retro() {}\n"),
            ("src/wallbuckets.rs", "pub fn wall_bucket() {}\n"),
        ]);
        let overview = "The `wall_bucket` totals count a barrier twice.";
        let missed = retro(overview, "file: src/a.rs\nlikely files  src/retro.rs (identifier), src/gone.rs (precedent)\nverify  cargo test retro::");
        assert!(misses_subject(&missed, &dir));
        let o = order_for(&missed, &dir, Flow::of(true));
        assert!(o.contains(SEARCH_RULE) && !o.contains(NARROW), "{o}");
        assert!(o.contains("search the tree") && o.contains("read only what that search finds") && o.contains("say in your exit"), "{o}");
        assert!(o.contains("`wsp packet t-1`") && o.contains("`wsp review t-1 ") && !o.contains('\n') && o.is_ascii(), "{o}");
        let printed = build(&missed, &dir).unwrap();
        assert_eq!(printed.matches(SEARCH_RULE).count(), 1, "the print says it too: {printed}");

        for right in [
            retro(overview, "file: src/wallbuckets.rs\nverify  cargo test wallbuckets::"),
            retro(overview, "likely files  src/wallbuckets.rs (named+identifier), src/a.rs (identifier)"),
            retro("Seen in src/wallbuckets.rs:12: `wall_bucket` counts twice.", "file: src/a.rs"),
            retro("Make the size show.", "file: src/a.rs"),
        ] {
            assert!(!misses_subject(&right, &dir), "{}", right.body);
            let o = order_for(&right, &dir, Flow::of(true));
            assert_eq!(o, order("t-1", Flow::of(true)), "the narrow order: {}", right.body);
            assert!(!build(&right, &dir).unwrap().contains(SEARCH_RULE));
        }

        assert!(!misses_subject(&retro("Make the size show.", "Make the size show."), &dir), "nothing named, nothing missed");
        assert!(misses_subject(&retro(overview, "The totals are wrong."), &dir), "a name and no file to hold it");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// wsp-016's own shape: written from evidence, with no `## Overview`, its
    /// names in `## Evidence` and `## Fix`, and a packet that is the context pack
    /// alone. Its likely files lacked the names, so it is told it may search; the
    /// same row with a likely file that holds them keeps the narrow order.
    #[test]
    fn a_row_written_from_evidence_with_no_overview_is_judged_on_its_evidence() {
        let dir = tree("evidence", &[
            ("src/cycle.rs", "pub fn cycle() {}\n"),
            ("src/wallbuckets.rs", "pub fn wall_buckets() {}\npub fn legs_running_seconds() {}\n"),
        ]);
        let row = |likely: &str| {
            let mut t = Task::default();
            t.id = "t-1".into();
            t.title = "Timing retro: legs are booked as wall".into();
            t.body = format!(
                "## Evidence\nwall_buckets books 3h of legs_running_seconds as wall.\n\n\
                 ## Fix\nCount legs once in wall_buckets.\n\n## Done when\nA test shows it.\n\n\
                 ## {SECTION}\nCONTEXT PACK  wsp@43be8f17  built from 9 finished tasks  kind: routine\n\
                 likely files  {likely}\nverify  cargo test cycle::\n"
            );
            t
        };
        let missed = row("src/cycle.rs (identifier), src/cmd_agent.rs (precedent)");
        assert!(missed.section("Overview").is_none());
        assert!(subject(&missed).contains(&"wall_buckets".to_string()), "{:?}", subject(&missed));
        assert!(misses_subject(&missed, &dir));
        let o = order_for(&missed, &dir, Flow::of(true));
        assert!(o.contains(SEARCH_RULE) && !o.contains(NARROW), "{o}");
        assert_eq!(build(&missed, &dir).unwrap().matches(SEARCH_RULE).count(), 1);

        let right = row("src/wallbuckets.rs (named), src/cycle.rs (identifier)");
        assert!(!misses_subject(&right, &dir));
        assert_eq!(order_for(&right, &dir, Flow::of(true)), order("t-1", Flow::of(true)));
        assert!(!build(&right, &dir).unwrap().contains(SEARCH_RULE));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// wsp-016's brief, verbatim: its Evidence and Done when carry `finish` and
    /// `running` in backticks, words src/cycle.rs holds a hundred times each,
    /// beside `wall_buckets` and `legs_running_seconds`, which none of its likely
    /// files held. The common words must not clear it.
    const WSP_016: &str = "## Evidence\n\
effort.json books a leg that has no seat as working. wsp-001's `finish` leg (cpd-121) ended at its Stop at 06:39:28Z as a member exit, and the leg line read `running` until it was edited by hand about 07:45 (wsp-011 has the cause). For that hour nobody worked, yet group 1's wall_buckets say working 7774 s of 8974 s (without the park), of which legs_running_seconds 7178 s. The legs that ran took about 32 min of seat time. cpd-121 reads 14139 s on 35 turns. So the list's \"working 226m (80 %)\" holds about 65 min that was a stall, and the wall's biggest single cause reads as work.\n\
The same wall also lists two stalls as \"nothing the logs name\": wsp-004's step did not run (545 s, the capped-reopen case in wsp-011) and wsp-001's (318 s), 14 min together, though the log says why.\n\
Search: `wsp ls --all`, `wsp find --all` for \"legs running\", \"gap\", \"idle\", \"unclaimed\"; no open row covers the measurement (wsp-011 covers the stuck leg itself and has this noted).\n\
\n\
## Fix\n\
In the wall buckets, count a leg as running only while its seat is alive (a binding, or a model call or tool in its transcript); a leg whose seat has ended is waiting, named \"a leg's seat ended, the leg not closed\" with the seat and the time it ended. And name the wsp-004-style gap where the log has a reopen at the leg cap with no member seated.\n\
\n\
## Done when\n\
A test over a leg record whose seat ended at T and whose line stays `running` to T+60 min: the 60 min land in waiting with that cause, not in working or legs_running_seconds. Re-reading m0-agent-manager's effort.json gives group 1 working near 7774 s less about 3900 s.\n";

    #[test]
    fn wsp_016s_own_brief_is_not_cleared_by_a_common_word_its_files_hold() {
        let dir = tree("wsp016", &[
            ("src/cycle.rs", "// a leg is running until its finish\npub fn running() -> bool { true }\npub fn finish() {}\n"),
            ("src/cmd_agent.rs", "pub fn agent() { let running = 1; let _ = running; }\n"),
            ("src/wallbuckets.rs", "pub fn wall_buckets() {}\n"),
            ("src/legs.rs", "pub fn wall_buckets() {}\npub fn legs_running_seconds() {}\n"),
        ]);
        let row = |likely: &str| {
            let mut t = Task::default();
            t.id = "wsp-016".into();
            t.title = "effort.json books a leg whose seat ended as working".into();
            t.body = format!(
                "{WSP_016}\n## {SECTION}\nCONTEXT PACK  wsp@43be8f17  built from 9 finished tasks  kind: routine\n\
                 likely files  {likely}\nverify  cargo test cycle::\n"
            );
            t
        };
        let missed = row("src/cycle.rs (identifier), src/cmd_agent.rs (precedent)");
        let names = subject(&missed);
        assert!(["finish", "running", "wall_buckets", "legs_running_seconds"].iter().all(|n| names.contains(&n.to_string())), "{names:?}");
        assert!(misses_subject(&missed, &dir), "`running` and `finish` are in src/cycle.rs; wall_buckets is not");
        let o = order_for(&missed, &dir, Flow::of(true));
        assert!(o.contains(SEARCH_RULE) && !o.contains(NARROW), "{o}");
        assert_eq!(build(&missed, &dir).unwrap().matches(SEARCH_RULE).count(), 1);

        let half = row("src/cycle.rs (identifier), src/wallbuckets.rs (named)");
        assert!(misses_subject(&half, &dir), "wall_buckets is held; legs_running_seconds is in none of them");
        assert!(order_for(&half, &dir, Flow::of(true)).contains(SEARCH_RULE));
        assert_eq!(build(&half, &dir).unwrap().matches(SEARCH_RULE).count(), 1);

        let right = row("src/cycle.rs (identifier), src/wallbuckets.rs (named), src/legs.rs (identifier)");
        assert!(!misses_subject(&right, &dir));
        assert_eq!(order_for(&right, &dir, Flow::of(true)), order("wsp-016", Flow::of(true)));
        assert!(!build(&right, &dir).unwrap().contains(SEARCH_RULE));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_name_only_code_spells_is_distinctive_and_a_plain_word_is_not() {
        for n in ["wall_buckets", "Store::open", "wallBuckets", "_x"] {
            assert!(distinctive(n), "{n}");
        }
        for n in ["running", "finish", "Store", "WSP"] {
            assert!(!distinctive(n), "{n}");
        }
        let dir = tree("plain", &[("src/cycle.rs", "pub fn running() {}\n")]);
        let plain = retro("The leg line reads `running`.", "file: src/cycle.rs");
        assert!(!misses_subject(&plain, &dir), "with nothing more specific named, a plain word still counts");
        assert!(misses_subject(&retro("The leg line reads `finish`.", "file: src/cycle.rs"), &dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_subject_is_the_overviews_names_not_its_commands_flags_or_paths() {
        let t = retro("`wall_bucket` and `Store::open` in src/x.rs; run `cargo build --workspace` with `--full`; the seat_gone line.", "x");
        assert_eq!(subject(&t), ["wall_bucket", "Store::open", "seat_gone"]);
        let t = retro("See (src/packet.rs:193 and :318).", "likely files  src/b.rs (identifier), src/c.rs (precedent)\nfile: src/a.rs");
        assert_eq!(named_files(&t), ["src/a.rs", "src/b.rs", "src/c.rs", "src/packet.rs"]);
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
            let _ = wsp_base::calls::Command::new("chmod").arg("+x").arg(&script).status();
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
        let out = wsp_base::calls::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
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
        assert!(printed.contains("pub fn a() {}") && printed.contains("wsp submit pj-001"), "{printed}");
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
