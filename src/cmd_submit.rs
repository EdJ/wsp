//! `wsp submit <row>`: a member's exit, validated whole and then taken
//! (`wsp-613`, row E2 of docs/exit-packet.md, §3 and §4).
//!
//! # What it reads
//!
//! The exit file a seat wrote: `--from FILE|-`, else `$WSP_EXIT`, else the
//! place wsp keeps for it ([`crate::closing::verdict_path`], the member's, which
//! a spawn also gives as `$WSP_VERDICT`). The kind is `$WSP_SEAT_KIND`'s, never
//! the file's, so a member cannot write a verdict; only a member's exit is
//! taken here so far, and another kind is told the verb it still has.
//!
//! # Every refusal at once
//!
//! [`validate`] runs V0–V7 and keeps going: the file's shape ([`Exit::parse`],
//! V0 and the strict check-line grammar of V3), the tree (V1: dirty paths that
//! are the project's `## Artefacts` are restored by wsp itself, anything else
//! is refused by name, and treecopy's mismatch runs as in review), the commit
//! a verifier already blocked (V2, wsp-529), each check's scope and time (V3),
//! the done-when against the row's own bullets (V4), the asks (V5: a decision
//! ask that asks nothing is filed as a note and said so), the summary and the
//! account (V6) and what is owed (V7). Each refusal names its line, its
//! section and the text to write; nothing moves until there are none, and
//! `--dry-run` stops there with what it would do.
//!
//! # The full suite is the project's to allow (D4)
//!
//! A member's check may not be one of the project's barrier or end steps, and
//! may not be a full-suite command — the two the throttle guards, and any the
//! project's `## Full suite` names — unless that section says `members:
//! allowed`. Then it is let through and logged, as `WSP_FULL_SUITE` is. A
//! member's own `full-suite:` line stays refused ([`crate::exit`]).
//!
//! # What an accepted exit does
//!
//! The outcome is read from the file ([`Exit::outcome`]). Every item held, or
//! not held with only `owed:` for it, runs **the review core**
//! ([`crate::cmd_task::accept`], the same gates `wsp review` has) with the file
//! whole as the account, after `checks:` has written the row's `## Checks`. A
//! `not held` with a decision ask blocks the row on that ask, routed through
//! [`crate::waiting::route`]. `owed:` lines that open `person:` are the steps
//! owed to the person (compounding-470): the row still goes on, they are logged
//! by name and its governor is told they belong to the list's closing person
//! row. Either way the seat is ended once it is not mid-turn (D1).

use crate::closing;
use crate::exit::{Ask, Exit, Kind, Outcome, Refusal, To};
use crate::message::{self, About, Message, Waiting};
use crate::model::{Status, Task};
use crate::store::Store;
use crate::util::{self, fold, Paint};
use crate::Args;
use std::path::{Path, PathBuf};

pub const VERBS: &[crate::verbs::Verb] = &[crate::verbs::Verb {
    names: &["submit"],
    run: submit,
    help: &[crate::verbs::Help {
        section: crate::verbs::Section::Tasks,
        order: 98,
        text: r##"  wsp submit <id> [--from FILE|-] [--dry-run]
                                    a member's exit, from $WSP_EXIT: every refusal
                                    at once (section, line, what to write), the
                                    project's ## Artefacts restored, then review
                                    with the file as the account and the seat
                                    ended; --dry-run only validates"##,
    }],
}];

const USAGE: &str = "wsp submit <id> [--from FILE|-] [--dry-run]   (the exit is read from $WSP_EXIT when no --from is given)";

/// The two full-suite commands the throttle already guards, refused as a
/// member's check whatever the project file says, unless it allows members.
const FULL_SUITE: &[&str] = &["cargo nextest run --workspace", "npm run check"];

/// The longest a member's check may be given.
const MEMBER_MAX_SECS: u64 = 30 * 60;

pub fn submit(store: &Store, args: &Args) -> i32 {
    let kind = std::env::var(crate::throttle::SEAT_KIND).ok();
    submit_as(store, args, kind.as_deref())
}

/// [`submit`] with the seat's kind given, so a test is not this seat.
pub(crate) fn submit_as(store: &Store, args: &Args, kind: Option<&str>) -> i32 {
    let Some(needle) = args.rest.first() else {
        eprintln!("usage: {USAGE}");
        return 2;
    };
    let t = match store.task_or_why(needle) {
        Ok(t) => t,
        Err(why) => {
            eprintln!("{why}");
            return 1;
        }
    };
    match kind.filter(|k| !k.trim().is_empty()).map(|k| (k, Kind::of(k.trim()))) {
        None | Some((_, Some(Kind::Member))) => {}
        Some((_, Some(other))) => {
            eprintln!(
                "wsp: wsp submit takes a member's exit so far — a {} seat closes with {} until its exit is built",
                other.word(),
                match other {
                    Kind::Verifier => "`wsp verified`",
                    Kind::Check => "`wsp worklist go|hold|followup`",
                    _ => "`wsp leg done`",
                }
            );
            return 2;
        }
        Some((word, None)) => {
            eprintln!("wsp: $WSP_SEAT_KIND is `{word}`, which is no seat kind (member, verifier, check, leg)");
            return 2;
        }
    }
    let (text, from) = match read_exit(store, args, &t) {
        Ok(v) => v,
        Err(code) => return code,
    };
    let checked = validate(store, &t, &text, args.get("anyway").as_deref());
    let p = Paint::new();
    for w in &checked.warnings {
        eprintln!("  {}", p.yellow(w));
    }
    if !checked.refusals.is_empty() {
        for line in refusal_lines(&checked) {
            eprintln!("{line}");
        }
        eprintln!(
            "wsp: {} not submitted — {} refusal(s) in {from}; nothing moved. Fix the file and `wsp submit {}` again (`--dry-run` checks it first)",
            t.id,
            checked.refusals.len(),
            t.id
        );
        return 1;
    }
    let Some(exit) = checked.exit.as_ref() else { return 1 };
    if args.has("dry-run") {
        for line in plan_lines(&t, &checked, exit) {
            println!("{line}");
        }
        println!("wsp: {} would be submitted — dry run, nothing moved", t.id);
        return 0;
    }
    take(store, args, &t, &checked, exit, &text, &from)
}

/// The exit's text and where it came from: `--from FILE|-`, `$WSP_EXIT`, or
/// the member's verdict path, which must have been written in this run.
fn read_exit(store: &Store, args: &Args, t: &Task) -> Result<(String, String), i32> {
    let given = args.get("from").filter(|f| f != "true").or_else(|| args.rest.get(1).filter(|a| *a == "-").cloned());
    let (src, kept) = match given {
        Some(src) => (src, false),
        None => match std::env::var("WSP_EXIT").ok().filter(|v| !v.trim().is_empty()) {
            Some(path) => (path, false),
            None => (closing::verdict_path(store, &t.id, closing::Kind::Member).display().to_string(), true),
        },
    };
    let named = if src == "-" { "stdin".to_string() } else { util::contract(&util::expand(&src)) };
    if src == "-" && util::stdin_is_tty() {
        eprintln!("wsp: nothing is piped in — `-` reads the exit from a stream");
        return Err(2);
    }
    if kept {
        let path = PathBuf::from(&src);
        match closing::written_at(&path) {
            None => {
                eprintln!("wsp: no exit at {named} — write it with a heredoc (`cat > \"$WSP_EXIT\" <<'EOF'` … `EOF`), then `wsp submit {}`", t.id);
                return Err(2);
            }
            Some(at) if crate::repair::started_at(t).is_some_and(|s| at < s) => {
                eprintln!("wsp: the exit at {named} is an earlier run's, written before {} started — write this run's", t.id);
                return Err(2);
            }
            Some(_) => {}
        }
    }
    match util::read_source(&src) {
        Ok(text) if text.trim().is_empty() => {
            eprintln!("wsp: nothing in {named} — nothing submitted");
            Err(2)
        }
        Ok(text) => Ok((text, named)),
        Err(e) => {
            eprintln!("wsp: cannot read {named}: {e}");
            Err(1)
        }
    }
}

// ---- the validator ----------------------------------------------------------

/// What [`validate`] found: the exit when its shape held, every refusal, and
/// what would be done.
#[derive(Debug, Default)]
pub(crate) struct Checked {
    pub exit: Option<Exit>,
    pub refusals: Vec<Refusal>,
    /// Said and never refused.
    pub warnings: Vec<String>,
    /// The row's tree, when it has one.
    pub tree: Option<PathBuf>,
    /// The dirty paths that are the project's artefacts: restored by wsp.
    pub restore: Vec<(String, String)>,
    /// Decision asks that ask nothing, filed as notes: their lines.
    pub as_notes: Vec<usize>,
    /// A full-suite check the project allows a member: the log line for each.
    pub full_suite: Vec<String>,
}

/// A refusal of the tree or the commits rather than of a line of the file.
fn tree_refusal(section: &str, said: String, write: &str) -> Refusal {
    Refusal { line: 0, section: section.into(), said, write: write.into() }
}

/// Every refusal as it is printed: a line's at its line, the tree's whole.
pub(crate) fn refusal_lines(c: &Checked) -> Vec<String> {
    c.refusals
        .iter()
        .map(|r| match r.line {
            0 => format!("{} ({}) — {}", r.said, r.section, r.write),
            _ => r.to_string(),
        })
        .collect()
}

/// V0–V7 over `text` for row `t`, keeping every refusal.
pub(crate) fn validate(store: &Store, t: &Task, text: &str, anyway: Option<&str>) -> Checked {
    let mut c = Checked::default();
    // V0 and V3's grammar: the shape, every line at once.
    let parsed = Exit::parse(Kind::Member, text);
    let mut exit = match parsed {
        Ok(e) => Some(e),
        Err(rs) => {
            c.refusals.extend(rs);
            None
        }
    };
    // V1 and V2 do not need the file to have held.
    tree_rules(store, t, anyway, &mut c);
    if let Some(e) = exit.as_mut() {
        check_rules(store, t, text, e, &mut c);
        done_rules(t, text, e, &mut c);
        ask_rules(text, e, &mut c);
        account_rules(text, e, &mut c);
        owed_rules(text, e, &mut c);
    }
    c.refusals.sort_by_key(|r| r.line);
    c.exit = exit;
    c
}

/// V1, the tree, and V2's blocked commit.
fn tree_rules(store: &Store, t: &Task, anyway: Option<&str>, c: &mut Checked) {
    let Some(tree) = crate::cmd_checkout::member_tree(store, &t.id) else { return };
    let shown = util::contract(&tree);
    if let Err(why) = crate::treecopy::review_refusal(store, &t.id, &tree) {
        c.refusals.push(tree_refusal("tree", format!("wsp: {why}"), "`wsp verify` again in the tree, then submit"));
    }
    let globs = t.project.as_deref().map(|p| crate::check::artefact_globs(store, p)).unwrap_or_default();
    let (restore, other): (Vec<_>, Vec<_>) = crate::cmd_checkout::dirty_files(&tree)
        .into_iter()
        .partition(|(_, path)| globs.iter().any(|g| util::captures(g, path).is_some()));
    if !other.is_empty() {
        c.refusals.push(tree_refusal(
            "tree",
            crate::cmd_checkout::dirty_submit_refusal(&shown, &t.id, &other),
            "a clean tree: commit it (wsp commit-help)",
        ));
    }
    c.restore = restore;
    // V2: the commit a verifier already blocked, put up again unchanged.
    if anyway.is_none_or(|w| w == "true" || w.trim().is_empty()) {
        if let Some(head) = git(&tree, &["rev-parse", "HEAD"]) {
            if let Some(pass) = crate::cmd_task::blocked_at(t, &head) {
                let said = crate::cmd_task::blocked_refusal(&t.id, &pass).join("\n");
                c.refusals.push(tree_refusal("commits", said, "a new commit with the fix, or `--anyway \"why\"` to put it up as it is"));
            }
        }
    }
    c.tree = Some(tree);
}

/// The 1-based lines of the `- ` items under heading `name:`, in order.
fn item_lines(text: &str, name: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut inside = false;
    for (i, l) in text.lines().enumerate() {
        if let Some(h) = section_heading(l) {
            inside = h == name;
            continue;
        }
        if inside && l.trim_start().starts_with('-') {
            out.push(i + 1);
        }
    }
    out
}

/// The line a heading `name:` is on.
fn heading_line(text: &str, name: &str) -> Option<usize> {
    text.lines().position(|l| section_heading(l) == Some(name)).map(|i| i + 1)
}

/// The section a line opens, when it is one of the exit's headings.
fn section_heading(l: &str) -> Option<&str> {
    const NAMES: [&str; 8] = ["verdict", "summary", "done-when", "checks", "checked", "cause", "asks", "owed"];
    if l.starts_with(char::is_whitespace) {
        return None;
    }
    let (name, rest) = l.trim_end().split_once(':')?;
    (NAMES.contains(&name) && (rest.is_empty() || rest.starts_with(' '))).then_some(name)
}

/// A command as the scope rules compare it: its words, single-spaced.
fn words(command: &str) -> String {
    command.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// V3 past the grammar: the time, `sh -n`, the first word, and the scope.
fn check_rules(store: &Store, t: &Task, text: &str, e: &Exit, c: &mut Checked) {
    let project = t.project.as_deref().unwrap_or_default();
    let held_back: Vec<(String, &str)> = crate::check::barrier_steps(store, project)
        .into_iter()
        .map(|s| (s, "barrier"))
        .chain(crate::check::end_steps(store, project).into_iter().map(|s| (s, "end")))
        .map(|(s, at)| (words(&s.command), at))
        .collect();
    let suite = crate::check::full_suite(store, project);
    let guarded = |cmd: &str| FULL_SUITE.iter().any(|f| words(f) == cmd) || suite.names(cmd);
    let lines = item_lines(text, "checks");
    for (i, l) in e.checks.iter().enumerate() {
        let n = lines.get(i).copied().unwrap_or(0);
        let cmd = words(&l.command);
        let refuse = |said: String, write: &str| Refusal { line: n, section: "checks".into(), said, write: write.into() };
        if let Some((_, at)) = held_back.iter().find(|(s, _)| *s == cmd) {
            c.refusals.push(refuse(
                format!("\"{}\" is one of the project's `at {at}` steps, which the {at} runs", l.command),
                "the narrow command for what you touched",
            ));
            continue;
        }
        if guarded(&cmd) {
            if suite.members_allowed {
                c.full_suite.push(format!(
                    "full-suite: `{}` as {}'s check `{}` — the project's ## Full suite allows members it",
                    l.command, t.id, l.label
                ));
            } else {
                c.refusals.push(refuse(
                    format!("\"{}\" is the barrier's full suite", l.command),
                    "the narrow command for what you touched — the full suite runs at the barrier, and only the project's ## Full suite (`members: allowed`) lets a member run it",
                ));
                continue;
            }
        }
        if crate::check::limit_of(&l.time).is_some_and(|d| d.as_secs() > MEMBER_MAX_SECS) {
            c.refusals.push(refuse(format!("`{}` is given {}, past a member's 30m", l.label, l.time), "a time of 30m or less, for a narrower command"));
        }
        if let Some(why) = crate::check::unparsable(&l.command) {
            c.refusals.push(refuse(format!("`{}` cannot start under `sh`: {why}", l.label), "a command `sh -n` reads"));
        } else if !guarded(&cmd) {
            // A full-suite command the project allows is its own program to name.
            if let Some(why) = crate::check::unrunnable(&l.command) {
                c.refusals.push(refuse(format!("`{}`: {why}", l.label), "a builtin, a program on PATH, or a path inside the tree"));
            }
        }
    }
}

/// The row's own done-when items, when it gives them as bullets under a
/// `### Done when` heading or a bare `Done when:` line. Prose is not counted.
pub(crate) fn packet_items(t: &Task) -> Vec<String> {
    let body = t.section("Overview").unwrap_or_else(|| t.body.clone());
    let lines: Vec<&str> = body.lines().collect();
    let Some(at) = lines.iter().position(|l| {
        let l = l.trim().to_lowercase();
        l.trim_start_matches('#').trim() == "done when" || l == "done when:"
    }) else {
        return Vec::new();
    };
    let mut items: Vec<String> = Vec::new();
    for l in &lines[at + 1..] {
        let s = l.trim();
        if s.is_empty() {
            if items.is_empty() {
                continue;
            }
            break;
        }
        if s.starts_with('#') {
            break;
        }
        match s.strip_prefix("- ").or_else(|| s.strip_prefix("* ")) {
            Some(item) if !l.starts_with("  ") => items.push(item.trim().to_string()),
            _ if items.is_empty() => return Vec::new(),
            _ => {}
        }
    }
    items
}

/// The first words of an item, as a done-when line must open with them.
fn first_words(s: &str, n: usize) -> Vec<String> {
    s.split_whitespace()
        .map(|w| w.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase())
        .filter(|w| !w.is_empty())
        .take(n)
        .collect()
}

/// V4: the count of the row's bullets, and each line opening with its item.
fn done_rules(t: &Task, text: &str, e: &Exit, c: &mut Checked) {
    let items = packet_items(t);
    let done = e.done_when.as_deref().unwrap_or_default();
    if items.is_empty() {
        return;
    }
    let at = heading_line(text, "done-when").unwrap_or(0);
    if items.len() != done.len() {
        c.refusals.push(Refusal {
            line: at,
            section: "done-when".into(),
            said: format!("the row's Done when has {} item(s) and this has {}", items.len(), done.len()),
            write: format!("one `- held:` or `- not held:` line per item, in order: {}", items.iter().map(|i| format!("\"{}\"", util::truncate(i, 40))).collect::<Vec<_>>().join(", ")),
        });
        return;
    }
    let lines = item_lines(text, "done-when");
    for (i, (d, item)) in done.iter().zip(&items).enumerate() {
        let want = first_words(item, 3);
        if first_words(&d.item, want.len()) != want {
            c.refusals.push(Refusal {
                line: lines.get(i).copied().unwrap_or(at),
                section: "done-when".into(),
                said: format!("\"{}\" is not the row's item {}", util::truncate(&d.item, 60), i + 1),
                write: format!("the item as the packet says it: \"{}\"", util::truncate(item, 80)),
            });
        }
    }
}

/// Whether an ask's words ask anything, by the rule a turn's last words are
/// read by ([`crate::waiting::Words::asks`]).
fn asks_something(text: &str) -> bool {
    crate::waiting::last_words(text).is_some_and(|w| w.asks)
}

/// V5: a decision ask that asks nothing becomes a note, said so.
fn ask_rules(text: &str, e: &mut Exit, c: &mut Checked) {
    let lines = item_lines(text, "asks");
    for (i, a) in e.asks.iter_mut().enumerate() {
        if a.decides() && !asks_something(&a.text) {
            let n = lines.get(i).copied().unwrap_or(0);
            c.warnings.push(format!(
                "line {n} (asks): the `{}:` ask asks nothing (no `?`, no \"Need …\", no \"Either … or\"), so it is filed as a note, not a question",
                a.to.word()
            ));
            c.as_notes.push(n);
            a.to = To::Note;
        }
    }
}

/// V6: the summary under 100 characters, and an account.
fn account_rules(text: &str, e: &Exit, c: &mut Checked) {
    let at = heading_line(text, "summary").unwrap_or(1);
    let n = e.summary.chars().count();
    if n >= 100 {
        c.refusals.push(Refusal {
            line: at,
            section: "summary".into(),
            said: format!("the summary is {n} characters"),
            write: "one line under 100 characters: what is true now — the rest goes in the account under it".into(),
        });
    }
    if e.account.trim().is_empty() {
        c.refusals.push(Refusal {
            line: at + 1,
            section: "the account".into(),
            said: "the exit has no account".into(),
            write: "under `summary:`, in prose: what you did, what you left, what the next reader needs".into(),
        });
    }
}

/// V7: `owed:` is there, and every `not held` is owed or waits on a decision.
fn owed_rules(text: &str, e: &Exit, c: &mut Checked) {
    let end = text.lines().count() + 1;
    if e.owed.is_none() {
        c.refusals.push(Refusal {
            line: end,
            section: "owed".into(),
            said: "the exit has no `owed:`".into(),
            write: "`owed: nothing`, or `owed:` and `- <what is left>` per line (`- person: <step>` for one only the person can do)".into(),
        });
        return;
    }
    if e.outcome() == Outcome::Unowed {
        let lines = item_lines(text, "done-when");
        for (i, d) in e.done_when.iter().flatten().enumerate().filter(|(_, d)| !d.held) {
            c.refusals.push(Refusal {
                line: lines.get(i).copied().unwrap_or(end),
                section: "done-when".into(),
                said: format!("\"{}\" is not held, and nothing is owed for it or asked", util::truncate(&d.item, 60)),
                write: "an `owed:` line for what is left, or a `- governor:` / `- person:` ask it waits on".into(),
            });
        }
    }
}

/// The `owed:` lines that are the person's: `person: <step>`.
fn owed_to_the_person(e: &Exit) -> Vec<String> {
    e.owed.iter().flatten().filter_map(|o| o.strip_prefix("person:").map(|s| s.trim().to_string())).filter(|s| !s.is_empty()).collect()
}

/// What an accepted exit would do, as `--dry-run` says it.
fn plan_lines(t: &Task, c: &Checked, e: &Exit) -> Vec<String> {
    let mut out = Vec::new();
    for (code, path) in &c.restore {
        let how = if code == "??" { "remove (untracked)" } else { "git checkout --" };
        out.push(format!("  restore {path} — {how}, one of the project's ## Artefacts"));
    }
    if !e.checks.is_empty() {
        out.push(format!("  write {}'s ## Checks: {}", t.id, e.checks.iter().map(|l| l.label.as_str()).collect::<Vec<_>>().join(", ")));
    }
    for l in &c.full_suite {
        out.push(format!("  log {l}"));
    }
    match e.outcome() {
        Outcome::Ready => out.push(format!("  {} to review, the file as its account; the verifier runs", t.id)),
        Outcome::Partial => out.push(format!("  {} to review as partial, owed: {}", t.id, e.owed.iter().flatten().cloned().collect::<Vec<_>>().join("; "))),
        Outcome::Blocked(asks) => out.push(format!("  {} blocked on {} ask(s), routed: {}", t.id, asks.len(), asks.iter().map(|a| util::truncate(&a.text, 60)).collect::<Vec<_>>().join("; "))),
        Outcome::Unowed => {}
    }
    for a in e.asks.iter().filter(|a| !a.decides()) {
        out.push(format!("  note on {}: {}", t.id, util::truncate(&a.text, 80)));
    }
    for s in owed_to_the_person(e) {
        out.push(format!("  owed to the person: {s} — its governor is told, for the list's closing person row"));
    }
    out.push("  the seat ended once it is not mid-turn".into());
    out
}

// ---- taking it ---------------------------------------------------------------

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = wsp_base::calls::Command::new("git").arg("-C").arg(dir).args(args).env_remove("GIT_INDEX_FILE").output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Put the project's artefacts back: a tracked path checked out, an untracked
/// one removed by its exact path. The paths put back, or why one could not be.
fn restore(tree: &Path, paths: &[(String, String)]) -> Result<Vec<String>, String> {
    let mut done = Vec::new();
    for (code, path) in paths {
        if code == "??" {
            std::fs::remove_file(tree.join(path)).map_err(|e| format!("cannot remove {path}: {e}"))?;
        } else if git(tree, &["checkout", "--", path]).is_none() {
            return Err(format!("`git checkout -- {path}` failed in {}", util::contract(tree)));
        }
        done.push(path.clone());
    }
    Ok(done)
}

/// Log `lines` on the row, under the lock.
fn log_on(store: &Store, id: &str, lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    let id = id.to_string();
    store.locked(|| {
        let Some(mut t) = store.find_task(&id) else { return false };
        for l in lines {
            t.log(l);
        }
        t.touch();
        store.save_task(&t).is_ok()
    });
}

/// Raise a decision ask about `t`, to whom [`crate::waiting::route`] names:
/// the person for a `person:` ask, else the seat that answers for the row.
fn raise(store: &Store, t: &Task, ask: &Ask) -> Result<String, String> {
    let (me, _) = crate::cmd_message::whoami(store);
    // Nobody sits still behind it: the seat is ended, and the answer reopens the row.
    let mut q = Message::question(me, message::Kind::Note, &ask.text, Waiting::new("", &t.id)).about(About::Task(t.id.clone()));
    let routed = crate::waiting::route(store, &q);
    match (ask.to, &routed) {
        (To::Governor, crate::waiting::On::Seat(scope)) => q.for_seat = Some(scope.clone()),
        _ => q = q.for_the_person(),
    }
    message::raise(store, &q).map_err(|e| e.to_string())?;
    if q.for_person {
        crate::cmd_message::asked_of_the_person(store, &q, &t.id);
    }
    if let crate::waiting::On::Seat(scope) = &routed {
        let _ = crate::wake::say(store, scope, &crate::cmd_message::for_a_governor(&q), Some(&q.id));
    }
    Ok(q.id)
}

/// End this seat once it is not mid-turn (D1), unless it governs something.
fn end_my_seat(store: &Store, id: &str) -> Option<String> {
    let seat = crate::place::my_pane()?;
    if let Some(scope) = crate::place::governs(&store.governors(), &crate::place::Seat::new(&seat)) {
        return Some(format!("submit: the seat {seat} governs {scope}, so it is not ended"));
    }
    Some(match crate::cycle::end_seat_when_idle(store, &seat) {
        Ok(()) => format!("submit: the seat {seat} is ended once it is not mid-turn"),
        Err(why) => format!("submit: could not arrange the end of {seat}: {why} — `wsp despawn --pane {seat} --keep-tree` ends it ({id})"),
    })
}

fn take(store: &Store, args: &Args, t: &Task, c: &Checked, e: &Exit, text: &str, from: &str) -> i32 {
    let id = t.id.clone();
    let mut said: Vec<String> = Vec::new();
    // V1: the artefacts, put back by wsp.
    if let (Some(tree), false) = (c.tree.as_deref(), c.restore.is_empty()) {
        match restore(tree, &c.restore) {
            Ok(paths) => said.push(format!("submit: restored the project's ## Artefacts: {}", paths.join(", "))),
            Err(why) => {
                eprintln!("wsp: {id} not submitted — {why}");
                return 1;
            }
        }
        let left = crate::cmd_checkout::dirty_files(tree);
        if !left.is_empty() {
            log_on(store, &id, &said);
            eprintln!("{}", crate::cmd_checkout::dirty_submit_refusal(&util::contract(tree), &id, &left));
            return 1;
        }
    }
    // V2: nothing ahead is accepted and said.
    let nothing = c.tree.as_deref().is_some_and(|tree| matches!(crate::cmd_checkout::ready_from(store, &id, tree), Ok(None)));
    if nothing {
        said.push("submit: nothing to land".into());
        if e.done_when.iter().flatten().any(|d| d.reason.contains('/') || d.reason.contains(".rs")) {
            eprintln!("  {}", Paint::new().yellow(&format!("{id}: the done-when cites a file while nothing is ahead of the trunk — committed?")));
        }
    }
    said.extend(c.full_suite.iter().cloned());
    log_on(store, &id, &std::mem::take(&mut said));
    // `checks:` is what writes `## Checks`.
    if !e.checks.is_empty() {
        let lines: String = e.checks.iter().map(|l| format!("- {} ({}): {}\n", l.label, l.time, l.command)).collect();
        let ok = store.locked(|| {
            let Some(mut t) = store.find_task(&id) else { return false };
            crate::model::set_section_in(&mut t.body, crate::check::SECTION, lines.trim_end());
            store.save_task(&t).is_ok()
        });
        if !ok {
            eprintln!("wsp: {id}: could not write ## Checks");
            return 1;
        }
    }
    let outcome = e.outcome();
    match &outcome {
        Outcome::Blocked(asks) => {
            let mut raised = Vec::new();
            for a in asks {
                match raise(store, t, a) {
                    Ok(q) => raised.push(format!("asked ({}): {} · {q}", a.to.word(), fold(&a.text))),
                    Err(why) => {
                        eprintln!("wsp: {id}: could not raise the ask: {why}");
                        return 1;
                    }
                }
            }
            let first = asks.first().map(|a| fold(&a.text)).unwrap_or_default();
            let ok = store.locked(|| {
                let Some(mut t) = store.find_task(&id) else { return false };
                t.set_status(Status::Blocked);
                t.log(&format!("blocked: {first}"));
                for r in &raised {
                    t.log(r);
                }
                t.log(&format!("submit: the exit, whole: {}", fold(text)));
                t.touch();
                store.save_task(&t).is_ok()
            });
            if !ok {
                eprintln!("wsp: {id}: write failed");
                return 1;
            }
            store.git_commit(&format!("wsp: blocked {id} — {}", t.title));
            crate::cycle::poke_task(store, &id, "blocked");
        }
        _ => {
            let flags: Vec<(&str, String)> = args.get("anyway").map(|w| vec![("anyway", w)]).unwrap_or_default();
            let flags: Vec<(&str, &str)> = flags.iter().map(|(k, v)| (*k, v.as_str())).collect();
            let core = Args::synth("review", &[&id], &flags);
            let src = format!("{} characters from {from}, by wsp submit", text.len());
            let code = crate::cmd_task::accept(store, &core, &fold(text), Some(&src));
            if code != 0 {
                return code;
            }
        }
    }
    // What the file asks for besides the outcome.
    for a in e.asks.iter().filter(|a| !a.decides()) {
        said.push(format!("note: {}", fold(&a.text)));
    }
    if !c.as_notes.is_empty() {
        let n = c.as_notes.iter().map(usize::to_string).collect::<Vec<_>>().join(", ");
        said.push(format!("submit: the decision ask(s) at line {n} asked nothing, so they are filed as notes"));
    }
    if outcome == Outcome::Partial {
        said.push(format!("partial: owed — {}", e.owed.iter().flatten().cloned().collect::<Vec<_>>().join("; ")));
    }
    let person = owed_to_the_person(e);
    for s in &person {
        said.push(format!("owed to the person: {s}"));
    }
    if let (false, Some(w)) = (person.is_empty(), crate::cycle::list_of(store, t)) {
        crate::cycle::tell(
            store,
            &w,
            &format!("{id} is submitted with {} step(s) owed to the person, for the list's closing person row: {}", person.len(), person.join("; ")),
        );
    }
    if let Some(line) = end_my_seat(store, &id) {
        said.push(line);
    }
    log_on(store, &id, &said);
    for n in &c.as_notes {
        println!("  line {n}: that ask asked nothing, so it is filed on {id} as a note — a question has a `?`, \"Need …\" or \"Either … or\"");
    }
    let word = match outcome {
        Outcome::Blocked(_) => "blocked on its ask, routed",
        Outcome::Partial => "in review as partial",
        _ => "in review",
    };
    println!("✓ {id} submitted — {word}; the exit is its account");
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Group, Project, Worklist, WorklistStatus};

    fn git_at(dir: &Path, args: &[&str]) -> String {
        let out = wsp_base::calls::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    /// A member `wsp-101` of a running group, in a real repository, its tree a
    /// commit ahead of master holding `shot.png` and `work.txt`; its project's
    /// file is `project`, a fixture and never one of Ed's.
    fn member(tag: &str, project: &str) -> (crate::util::Isolated, Store, PathBuf) {
        let env = crate::util::isolated(tag);
        let store = Store::at(env.home(), env.state());
        store.ensure_dirs().unwrap();
        let repo = env.path("repo");
        std::fs::create_dir_all(&repo).unwrap();
        crate::testkit::repo(&repo);
        git_at(&repo, &["commit", "--quiet", "--allow-empty", "-m", "first"]);
        let tree = repo.join(".worktrees").join("wsp-101");
        git_at(&repo, &["worktree", "add", "--quiet", "-b", "wsp-101", &tree.display().to_string(), "master"]);
        std::fs::create_dir_all(tree.join("shots")).unwrap();
        std::fs::write(tree.join("shots/a.png"), "old\n").unwrap();
        std::fs::write(tree.join("work.txt"), "mine\n").unwrap();
        git_at(&tree, &["add", "."]);
        git_at(&tree, &["commit", "--quiet", "-m", "the work"]);
        let mut p = Project::new("p");
        p.roots = vec![repo.display().to_string()];
        p.body = project.into();
        store.save_project(&p).unwrap();
        let mut t = Task::new("a member", "wsp-101");
        t.project = Some("p".into());
        t.status_raw = "doing".into();
        t.caption = Some("A member that did its work".into());
        store.save_task(&t).unwrap();
        let mut w = Worklist::new("run", "run");
        w.set_status(WorklistStatus::Running);
        w.set_groups(&[Group { members: vec!["wsp-101".into()], agent: "claude".into(), ..Default::default() }]);
        store.save_worklist(&w).unwrap();
        (env, store, tree)
    }

    const ARTEFACTS: &str = "## Artefacts\n- shots/*.png — rewritten by the checks\n- out/*.log\n";

    fn exit_with(checks: &str, asks: &str) -> String {
        format!(
            "summary: the member did its work\n\nIt wrote work.txt and left nothing.\n\ndone-when:\n- held: the work — work.txt\n\nchecks:\n{checks}\nasks:\n{asks}\nowed: nothing\n"
        )
    }

    /// Submit `text` for wsp-101 as a member seat, from a file.
    fn run(env: &crate::util::Isolated, store: &Store, text: &str, flags: &[&str]) -> i32 {
        let file = env.path("exit.md");
        std::fs::write(&file, text).unwrap();
        let path = file.display().to_string();
        let mut rest = vec!["wsp-101", "--from", path.as_str()];
        rest.extend_from_slice(flags);
        let args = crate::Args::parse(std::iter::once("submit").chain(rest).map(String::from).collect());
        submit_as(store, &args, Some("member"))
    }

    fn log(store: &Store) -> String {
        store.task("wsp-101").unwrap().section("Log").unwrap_or_default()
    }

    #[test]
    fn a_tree_dirty_only_with_the_projects_artefacts_is_restored_and_accepted_with_the_paths_logged() {
        let (env, store, tree) = member("submit-artefacts", ARTEFACTS);
        std::fs::write(tree.join("shots/a.png"), "rewritten\n").unwrap();
        std::fs::create_dir_all(tree.join("out")).unwrap();
        std::fs::write(tree.join("out/run.log"), "noise\n").unwrap();
        let text = exit_with("- unit (2m): true\n", "- note: nothing to decide\n");
        assert_eq!(run(&env, &store, &text, &[]), 0);
        assert_eq!(std::fs::read_to_string(tree.join("shots/a.png")).unwrap(), "old\n", "the tracked artefact is checked out");
        assert!(!tree.join("out/run.log").exists(), "the untracked one is removed");
        assert!(crate::cmd_checkout::dirty_files(&tree).is_empty());
        let t = store.task("wsp-101").unwrap();
        assert_eq!(t.status_raw, "review");
        let log = log(&store);
        assert!(log.contains("restored the project's ## Artefacts") && log.contains("shots/a.png") && log.contains("out/run.log"), "{log}");
    }

    #[test]
    fn another_dirty_path_is_refused_by_name_and_nothing_is_restored() {
        let (env, store, tree) = member("submit-dirty", ARTEFACTS);
        std::fs::write(tree.join("shots/a.png"), "rewritten\n").unwrap();
        std::fs::write(tree.join("work.txt"), "more, uncommitted\n").unwrap();
        std::fs::write(tree.join("loose.rs"), "fn x() {}\n").unwrap();
        let t = store.task("wsp-101").unwrap();
        let c = validate(&store, &t, &exit_with("- unit (2m): true\n", "- note: fine\n"), None);
        let lines = refusal_lines(&c).join("\n");
        assert_eq!(c.refusals.len(), 1, "{lines}");
        assert!(lines.contains("work.txt") && lines.contains("loose.rs") && lines.contains("wsp commit-help"), "{lines}");
        assert!(!lines.contains("shots/a.png"), "an artefact is not refused: {lines}");
        assert_eq!(c.restore.iter().map(|(_, p)| p.as_str()).collect::<Vec<_>>(), vec!["shots/a.png"]);
        assert_eq!(run(&env, &store, &exit_with("- unit (2m): true\n", "- note: fine\n"), &[]), 1);
        assert_eq!(std::fs::read_to_string(tree.join("shots/a.png")).unwrap(), "rewritten\n", "nothing is half-taken");
        assert_eq!(store.task("wsp-101").unwrap().status_raw, "doing");
    }

    #[test]
    fn three_faults_in_one_file_are_three_refusal_lines_in_one_run() {
        let (env, store, _tree) = member("submit-three", ARTEFACTS);
        let text = "summary: the member did its work\n\nIt did it.\n\nTests:\n\ndone-when:\n- held: the work — work.txt\n\nchecks:\n- unit: true\n\nasks:\n- someone: decide\n\nowed: nothing\n";
        let t = store.task("wsp-101").unwrap();
        let c = validate(&store, &t, text, None);
        let lines = refusal_lines(&c);
        assert_eq!(lines.len(), 3, "{lines:#?}");
        assert!(lines[0].starts_with("line 5 ") && lines[0].contains("\"Tests:\" is not a section"), "{lines:#?}");
        assert!(lines[1].starts_with("line 11 ") && lines[1].contains("(checks)"), "{lines:#?}");
        assert!(lines[2].starts_with("line 14 ") && lines[2].contains("(asks)"), "{lines:#?}");
        assert!(lines.iter().all(|l| l.contains(" — write ")), "each names the text to write: {lines:#?}");
        assert_eq!(run(&env, &store, text, &[]), 1);
        assert_eq!(store.task("wsp-101").unwrap().status_raw, "doing");
    }

    #[test]
    fn the_full_suite_is_refused_as_a_members_check_unless_the_projects_d4_key_allows_it() {
        let (env, store, _tree) = member("submit-suite", ARTEFACTS);
        let suite = exit_with("- everything (20m): npm run check\n", "- note: fine\n");
        let t = store.task("wsp-101").unwrap();
        let c = validate(&store, &t, &suite, None);
        let lines = refusal_lines(&c).join("\n");
        assert!(lines.contains("\"npm run check\" is the barrier's full suite") && lines.contains("line 9 "), "{lines}");
        // A member's own `full-suite:` line is still refused (E1).
        let own = exit_with("- full-suite: I touched a shared module\n- everything (20m): npm run check\n", "- note: fine\n");
        let c = validate(&store, &t, &own, None);
        assert!(refusal_lines(&c).iter().any(|l| l.contains("full-suite override is not an exit's line")), "{:#?}", refusal_lines(&c));

        // The project's ## Full suite with `members: allowed` lets it through, logged.
        let mut p = store.project("p").unwrap();
        p.body = format!("{ARTEFACTS}\n## Full suite\n- `npm run check`\nmembers: allowed\n");
        store.save_project(&p).unwrap();
        let c = validate(&store, &t, &suite, None);
        assert!(c.refusals.is_empty(), "{:#?}", refusal_lines(&c));
        assert_eq!(c.full_suite.len(), 1);
        let c = validate(&store, &t, &own, None);
        assert!(refusal_lines(&c).iter().any(|l| l.contains("full-suite override")), "the line stays refused whatever the project says");
        assert_eq!(run(&env, &store, &suite, &[]), 0);
        let log = log(&store);
        assert!(log.contains("full-suite: `npm run check`") && log.contains("allows members"), "{log}");
        let checks = store.task("wsp-101").unwrap().section("Checks").unwrap_or_default();
        assert!(checks.contains("- everything (20m): npm run check"), "`checks:` writes ## Checks: {checks}");
    }

    #[test]
    fn a_barrier_step_is_refused_as_a_members_check() {
        let (_env, store, _tree) = member("submit-barrier", "## Checks\n- whole (20m) at barrier: cargo test --workspace\n");
        let t = store.task("wsp-101").unwrap();
        let c = validate(&store, &t, &exit_with("- whole (20m): cargo test --workspace\n", "- note: fine\n"), None);
        assert!(refusal_lines(&c).iter().any(|l| l.contains("`at barrier` steps")), "{:#?}", refusal_lines(&c));
    }

    #[test]
    fn a_person_ask_that_asks_nothing_is_filed_as_a_note() {
        let (env, store, _tree) = member("submit-note", ARTEFACTS);
        let text = exit_with("- unit (2m): true\n", "- person: ready for the verifier, all green\n");
        let t = store.task("wsp-101").unwrap();
        let c = validate(&store, &t, &text, None);
        assert!(c.refusals.is_empty(), "{:#?}", refusal_lines(&c));
        assert_eq!(c.as_notes, vec![12]);
        assert!(c.exit.as_ref().unwrap().asks.iter().all(|a| !a.decides()));
        assert_eq!(run(&env, &store, &text, &[]), 0);
        assert!(crate::message::raised_to_the_person(&store).is_empty(), "no question was raised");
        let log = log(&store);
        assert!(log.contains("note: ready for the verifier, all green") && log.contains("filed as notes"), "{log}");
        assert_eq!(store.task("wsp-101").unwrap().status_raw, "review");
    }

    #[test]
    fn an_accepted_exit_moves_the_row_to_review_with_the_file_as_its_account() {
        let (env, store, tree) = member("submit-accept", ARTEFACTS);
        let tip = git_at(&tree, &["rev-parse", "HEAD"]);
        let text = exit_with("- unit (2m): true\n", "- note: the next row can start\n");
        assert_eq!(run(&env, &store, &text, &["--dry-run"]), 0);
        assert_eq!(store.task("wsp-101").unwrap().status_raw, "doing", "a dry run moves nothing");
        assert_eq!(run(&env, &store, &text, &[]), 0);
        let t = store.task("wsp-101").unwrap();
        assert_eq!(t.status_raw, "review");
        assert_eq!(crate::repair::ready(&t).as_deref(), Some(tip.as_str()), "ready at the tree's commit, as review marks it");
        let log = t.section("Log").unwrap();
        assert!(log.contains(&format!("review: {}", fold(&text))), "the file whole is the account: {log}");
        assert!(log.contains("note: the next row can start"), "{log}");
    }

    #[test]
    fn a_not_held_with_a_decision_ask_blocks_the_row_on_it() {
        let (env, store, _tree) = member("submit-block", ARTEFACTS);
        let text = "summary: half done\n\nThe API half is in; the UI half needs a call.\n\ndone-when:\n- not held: the work — the UI half\n\nasks:\n- person: Either keep the old panel or drop it?\n\nowed:\n- the UI half\n";
        assert_eq!(run(&env, &store, text, &[]), 0);
        let t = store.task("wsp-101").unwrap();
        assert_eq!(t.status_raw, "blocked");
        assert_eq!(crate::message::raised_to_the_person(&store).len(), 1);
    }

    #[test]
    fn steps_owed_to_the_person_leave_the_row_going_on_and_are_logged_by_name() {
        let (env, store, _tree) = member("submit-owed-person", ARTEFACTS);
        let text = "summary: all but the hands-on half\n\nThe native half needs a person at the device.\n\ndone-when:\n- not held: the work — the in-app drive\n\nowed:\n- person: drive the native half in the app\n";
        assert_eq!(run(&env, &store, text, &[]), 0);
        let t = store.task("wsp-101").unwrap();
        assert_eq!(t.status_raw, "review", "the row goes on; the person's steps are owed apart");
        let log = t.section("Log").unwrap();
        assert!(log.contains("owed to the person: drive the native half in the app") && log.contains("partial: owed"), "{log}");
    }

    #[test]
    fn a_not_held_with_nothing_owed_is_refused_and_the_rows_done_when_bullets_are_counted() {
        let (_env, store, _tree) = member("submit-v4", ARTEFACTS);
        let mut t = store.task("wsp-101").unwrap();
        t.body = "## Overview\nDo it.\n\n### Done when\n- the API answers\n- the panel draws it\n".into();
        store.save_task(&t).unwrap();
        let t = store.task("wsp-101").unwrap();
        assert_eq!(packet_items(&t), vec!["the API answers", "the panel draws it"]);
        let one = "summary: s\n\nAn account.\n\ndone-when:\n- not held: the API answers — not yet\n\nowed: nothing\n";
        let lines = refusal_lines(&validate(&store, &t, one, None));
        assert!(lines.iter().any(|l| l.contains("has 2 item(s) and this has 1")), "{lines:#?}");
        let two = "summary: s\n\nAn account.\n\ndone-when:\n- held: the API answers — api.rs:9\n- not held: the panel draws it — no time\n\nowed: nothing\n";
        let lines = refusal_lines(&validate(&store, &t, two, None));
        assert_eq!(lines.len(), 1, "{lines:#?}");
        assert!(lines[0].starts_with("line 7 ") && lines[0].contains("nothing is owed for it"), "{lines:#?}");
        let wrong = "summary: s\n\nAn account.\n\ndone-when:\n- held: the API answers — api.rs:9\n- held: something else — x\n\nowed: nothing\n";
        let lines = refusal_lines(&validate(&store, &t, wrong, None));
        assert!(lines.iter().any(|l| l.contains("is not the row's item 2")), "{lines:#?}");
    }

    #[test]
    fn a_kind_other_than_a_member_is_told_its_verb() {
        let (_env, store, _tree) = member("submit-kind", ARTEFACTS);
        let args = crate::Args::synth("submit", &["wsp-101"], &[]);
        assert_eq!(submit_as(&store, &args, Some("verifier")), 2);
        assert_eq!(store.task("wsp-101").unwrap().status_raw, "doing");
    }
}
