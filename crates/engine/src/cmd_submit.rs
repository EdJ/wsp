//! `wsp submit <row>`: a member's exit, validated whole and then taken
//! (`wsp-613`, row E2 of docs/exit-packet.md, §3 and §4).
//!
//! # What it reads
//!
//! The exit file a seat wrote: `--from FILE|-`, else `$WSP_EXIT`, else the
//! place wsp keeps for it ([`crate::closing::verdict_path`], the member's, which
//! a spawn also gives as `$WSP_VERDICT`). The kind is `$WSP_SEAT_KIND`'s, never
//! the file's, so a member cannot write a verdict.
//!
//! # A verifier's, a check's and a leg's (`wsp-614`, row E3)
//!
//! A verifier submits for the member it read: `verdict: holds|blocks`, its
//! `checked:` lines (`… — pass|fail`), and on a block a `cause:` and `owed:`.
//! A hold with an item not held or a check failed is refused, and so is a
//! block with neither; `owed:` is the `sent back:` reason. **`cause: env` is
//! no block**: the pass is recorded `could not verify`, the member stays at
//! review, and the run starts a new verifier. A barrier check submits for its
//! list: `go`/`hold` run as those verbs, a hold names its cause, and
//! `followup` stays a flag to the person. A leg's submit is `wsp leg done`
//! with the file as its baton (its done-when is optional); the row's last leg
//! submits as a member, and its leg is closed with it — done at review, back to
//! pending at a block. A running leg's seat submits its leg whatever
//! `$WSP_SEAT_KIND` says (`wsp-011`). `wsp verified`, a check seat's `worklist go`/`hold`
//! and `wsp leg done` run the same rules on what they are given.
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
//! row. Either way the seat is ended (D1, `wsp-615`): from this call it holds no
//! place under the cap, and [`crate::ending::end_exit`] ends it at its `Stop`,
//! the tree kept — a verifier's and a check's seat the same. The answer to a
//! decision ask reopens the row with the answer as its `sent back:` reason
//! ([`answered`]).

use crate::closing;
use crate::exit::{Ask, Exit, Kind, Outcome, Refusal, To};
use crate::message::{self, About, Message, Waiting};
use crate::model::{Status, Task};
use crate::store::Store;
use crate::util::{self, fold, Paint};
use crate::base::args::Args;
use std::path::{Path, PathBuf};

pub const VERBS: &[crate::verbs::Verb] = &[crate::verbs::Verb {
    names: &["submit"],
    run: submit,
    help: &[crate::verbs::Help {
        section: crate::verbs::Section::Tasks,
        order: 98,
        text: r##"  wsp submit <id> [--from FILE|-] [--dry-run]
                                    a seat's exit, from $WSP_EXIT: every refusal
                                    at once (section, line, what to write). A
                                    member's restores the project's ## Artefacts
                                    and goes to review; a verifier's is its
                                    verdict (a block names its cause; env is no
                                    block); a check's (<id> its list) is go,
                                    hold or a follow-up flag; a leg's is leg
                                    done. --dry-run only validates"##,
    }],
}];

const USAGE: &str = "wsp submit <id> [--from FILE|-] [--dry-run]   (the exit is read from $WSP_EXIT when no --from is given)";

/// The two full-suite commands the throttle already guards, refused as a
/// member's check whatever the project file says, unless it allows members.
const FULL_SUITE: &[&str] = &["cargo nextest run --workspace", "npm run check"];

/// The longest a member's check may be given.
const MEMBER_MAX_SECS: u64 = 30 * 60;

/// The shortest limit a member's check is written with. A member writes the
/// time its command took as `(<time>)`, and that was then its limit: `(1s)` on
/// `cargo test --test layering` cut the barrier's warm run off, 0.82s of it
/// cargo's build (`wsp-540`). `wsp check --set` still writes what it is given.
const MEMBER_MIN_SECS: u64 = 60;

/// `time` as a member's check is written: no shorter than [`MEMBER_MIN_SECS`].
fn member_limit(time: &str) -> String {
    match crate::check::limit_of(time) {
        Some(d) if d.as_secs() < MEMBER_MIN_SECS => format!("{MEMBER_MIN_SECS}s"),
        _ => time.to_string(),
    }
}

pub fn submit(store: &Store, args: &Args) -> i32 {
    // The helper an accepted exit leaves behind: its seat, ended at its `Stop`.
    if let Some(seat) = args.get("end-seat") {
        return end_at_stop(store, &seat);
    }
    let kind = std::env::var(crate::throttle::SEAT_KIND).ok();
    submit_as(store, args, kind.as_deref())
}

/// [`submit`] with the seat's kind given, so a test is not this seat.
pub fn submit_as(store: &Store, args: &Args, kind: Option<&str>) -> i32 {
    submit_from(store, args, kind, crate::place::my_pane())
}

/// [`submit_as`] with the seat given too: the pane a verifier's pass or a
/// leg is found by.
pub fn submit_from(store: &Store, args: &Args, kind: Option<&str>, me: Option<String>) -> i32 {
    let Some(needle) = args.rest.first() else {
        eprintln!("usage: {USAGE}");
        return 2;
    };
    let kind = match kind.filter(|k| !k.trim().is_empty()).map(|k| (k, Kind::of(k.trim()))) {
        None => Kind::Member,
        Some((_, Some(k))) => k,
        Some((word, None)) => {
            eprintln!("wsp: $WSP_SEAT_KIND is `{word}`, which is no seat kind (member, verifier, check, leg)");
            return 2;
        }
    };
    // A check's row is its list (or the barrier's subject), not a task.
    if kind == Kind::Check {
        return submit_check(store, args, needle);
    }
    let t = match store.task_or_why(needle) {
        Ok(t) => t,
        Err(why) => {
            eprintln!("{why}");
            return 1;
        }
    };
    // A seat whose claim expired does not land over the row's next holder
    // (`wsp-412`): refused before anything is read or written.
    if kind == Kind::Member {
        if let Some(why) = expired_claim_refusal(store, &t, me.as_deref()) {
            eprintln!("{why}");
            return 1;
        }
    }
    // The seat of a running leg submits its leg whatever kind its environment
    // says (`wsp-011`): a leg seat taken for a member closed the row and left
    // its leg `running` with no seat, and nothing reseated it.
    let kind = match kind {
        Kind::Member if leg_of(&t, me.as_deref()).is_some() => Kind::Leg,
        k => k,
    };
    match kind {
        Kind::Verifier => return submit_verifier(store, args, &t, me),
        Kind::Leg => {
            if let Some(code) = submit_leg(store, args, &t, me.clone()) {
                return code;
            }
            // The last leg of a row submits as a member: it ends the row.
        }
        _ => {}
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
    // The last leg's submit ends the row, which is refused while a sibling
    // still runs — said here, before anything moves.
    if let Some(name) = leg_of(&t, me.as_deref()) {
        if let Err(why) = crate::legs::end_by_review(&crate::legs::legs(&t), &name, "", &util::now_iso()) {
            eprintln!("wsp: {} leg {name} not submitted — {why}", t.id);
            return 1;
        }
    }
    if args.has("dry-run") {
        for line in plan_lines(&t, &checked, exit) {
            println!("{line}");
        }
        println!("wsp: {} would be submitted — dry run, nothing moved", t.id);
        return 0;
    }
    take(store, args, &t, &checked, exit, &text, &from, me.as_deref())
}

/// The refusal for a seat whose claim on `t` expired (`wsp-412`): `me` is not
/// the seat the row's claim names now, and the row's `## Log` carries the
/// `claim expired: held by <me> on …` line `wsp-411`'s pass writes. A row that
/// went on to another seat, or to nobody, is not this seat's to land over.
/// `None` for a seat that holds the row, a seat whose claim never expired, and
/// a caller outside any seat.
pub fn expired_claim_refusal(store: &Store, t: &Task, me: Option<&str>) -> Option<String> {
    let me = me.filter(|m| !m.is_empty())?;
    if holds_claim(store, &t.id, me) {
        return None;
    }
    let needle = format!("claim expired: held by {me} on ");
    let log = t.section("Log").unwrap_or_default();
    let line = log.lines().rev().find(|l| l.contains(&needle))?;
    let at = line.trim_start().trim_start_matches("- ").split_whitespace().next().unwrap_or("");
    let list = crate::cycle::list_of(store, t).map(|w| w.id).unwrap_or_else(|| "<list>".into());
    Some(format!(
        "wsp: your claim on {} expired at {at} and the row went back to todo; tell your governor (wsp tell governor:{list})",
        t.id
    ))
}

/// Whether `me` is the seat the claim on `row` names: bound to the row, or the
/// workspace the claim record carries.
fn holds_claim(store: &Store, row: &str, me: &str) -> bool {
    let Some(claim) = store.claims().get(row).cloned() else { return false };
    store.panes_for_task(row).iter().any(|p| p == me)
        || ["workspace_id", "workspace", "workspace_label", "pane_id"]
            .iter()
            .any(|k| claim.get(k).and_then(|x| x.as_str()) == Some(me))
}

/// The exit's text and where it came from: `--from FILE|-`, `$WSP_EXIT`, or
/// the member's verdict path, which must have been written in this run.
fn read_exit(store: &Store, args: &Args, t: &Task) -> Result<(String, String), i32> {
    let kept = closing::verdict_path(store, &t.id, closing::Kind::Member);
    read_exit_at(args, &t.id, kept, crate::repair::started_at(t))
}

/// [`read_exit`] for any kind: `kept` is the place wsp keeps the kind's file,
/// read when no `--from` and no `$WSP_EXIT` is given, and only when written
/// since `since` (the seat's start), so an earlier run's is never taken.
fn read_exit_at(args: &Args, id: &str, kept_at: PathBuf, since: Option<i64>) -> Result<(String, String), i32> {
    let given = args.get("from").filter(|f| f != "true").or_else(|| args.rest.get(1).filter(|a| *a == "-").cloned());
    let (src, kept) = match given {
        Some(src) => (src, false),
        None => match std::env::var("WSP_EXIT").ok().filter(|v| !v.trim().is_empty()) {
            Some(path) => (path, false),
            None => (kept_at.display().to_string(), true),
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
                eprintln!("wsp: no exit at {named} — write it with a heredoc (`cat > \"$WSP_EXIT\" <<'EOF'` … `EOF`), then `wsp submit {id}`");
                return Err(2);
            }
            Some(at) if since.is_some_and(|s| at < s) => {
                eprintln!("wsp: the exit at {named} is an earlier run's, written before this seat started on {id} — write this run's");
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
pub struct Checked {
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
pub fn refusal_lines(c: &Checked) -> Vec<String> {
    c.refusals
        .iter()
        .map(|r| match r.line {
            0 => format!("{} ({}) — {}", r.said, r.section, r.write),
            _ => r.to_string(),
        })
        .collect()
}

/// What `wsp submit --dry-run` would say of `text` as `kind`'s exit for `key`
/// (the member's row, the verifier's member, or a check's barrier subject):
/// `Ok` when it validates, else every refusal line (`wsp-616`). The tell at a
/// seat's stop carries these, and so does the flag when wsp's own close of an
/// exit is refused.
pub fn dry_run(store: &Store, kind: closing::Kind, key: &str, text: &str) -> Result<(), Vec<String>> {
    match kind {
        closing::Kind::Member => {
            let Some(t) = store.find_task(key) else { return Err(vec![format!("wsp: no row {key}")]) };
            let c = validate(store, &t, text, None);
            if c.refusals.is_empty() { Ok(()) } else { Err(refusal_lines(&c)) }
        }
        closing::Kind::Verifier => verifier_rules(text).map(|_| ()).map_err(|rs| printed(&rs)),
        closing::Kind::Check => check_rules_of(text).map(|_| ()).map_err(|rs| printed(&rs)),
    }
}

/// V0–V7 over `text` for row `t`, keeping every refusal.
pub fn validate(store: &Store, t: &Task, text: &str, anyway: Option<&str>) -> Checked {
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

/// The exit a bare `wsp review`'s words stand for, as [`validate`] reads it:
/// the words as they are when they are an exit already (a `summary:` or
/// `verdict:` first, as `$WSP_EXIT` is written), else the first line as the
/// summary and the rest as the account, with `owed: nothing` when no `owed:`
/// is there. The sections are left at their own lines, so a `done-when:` after
/// a caption is read as `wsp submit` reads it (`wsp-137`).
pub fn exit_of_review(account: &str) -> String {
    let account = account.trim();
    let first = account.lines().find(|l| !l.trim().is_empty()).and_then(section_heading);
    if matches!(first, Some("summary" | "verdict")) {
        return format!("{account}\n");
    }
    let (caption, rest) = account.split_once('\n').unwrap_or((account, ""));
    let owed = if account.lines().any(|l| section_heading(l) == Some("owed")) { "" } else { "\n\nowed: nothing" };
    format!("summary: {}\n\n{}{owed}\n", caption.trim(), rest.trim())
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
pub fn packet_items(t: &Task) -> Vec<String> {
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
    out.push("  the seat closing: no place under the cap from this call, ended at its Stop with its tree kept".into());
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
        let _ = crate::cmd_message::say_to_governor(store, scope, &q);
    }
    Ok(q.id)
}

/// An accepted exit's seat is closing, in this call (`wsp-615`, E4; D1): it
/// holds no place under the cap from here, and it is ended at its `Stop` — by
/// [`crate::ending::end_exit`], whose guards keep a governor seat and one typed
/// to since, and whose despawn keeps the tree. The next tick ends it where the
/// helper did not. `row` is where the ending is said: the member's, or the
/// verifier's member, or a check's barrier.
///
/// **Recorded before what the exit moves**, for a verifier's verdict and a
/// check's `go`/`hold`: from the record on, the older endings of those seats
/// step aside ([`crate::ending::exit_owns`]), so the seat has one ending and
/// not two racing for it. [`closing`] records, [`refused`] takes the record
/// back where the verb then failed, and this arranges the ending.
fn end_my_seat(store: &Store, row: &str, kind: &str, me: Option<&str>) -> Option<String> {
    let seat = closing(store, row, kind, me)?;
    if !crate::ending::exit_owns(store).contains(&seat) {
        return Some(format!("submit: could not record {seat} as closing — `wsp despawn --pane {seat} --keep-tree` ends it"));
    }
    let arranged = match cfg!(test) {
        true => Ok(()),
        false => crate::cycle::Seats::end_exit_when_idle(crate::cycle::seats(), store, &seat),
    };
    Some(match arranged {
        Ok(()) => format!("submit: the {kind} seat {seat} is closing — no place under the cap now, ended at its Stop"),
        Err(why) => format!("submit: the {kind} seat {seat} is closing — no place under the cap now; ended by the next tick ({why})"),
    })
}

/// Record this seat as closing on `row`'s exit, and answer which seat it is.
/// Already recorded is left as it was, so a retried exit keeps its first time.
fn closing(store: &Store, row: &str, kind: &str, me: Option<&str>) -> Option<String> {
    let seat = me.map(str::to_string).or_else(crate::place::my_pane)?;
    if !crate::ending::exit_owns(store).contains(&seat) {
        crate::ending::exited(store, &seat, row, kind);
    }
    Some(seat)
}

/// The verb the exit moved refused it: the seat is not closing after all.
fn refused(store: &Store, seat: Option<&str>) {
    if let Some(seat) = seat {
        crate::ending::forget_exit(store, seat);
    }
}

/// `wsp submit --end-seat <seat>`, the helper [`end_my_seat`] leaves: wait for
/// the seat's turn to end, then end it by [`crate::ending::end_exit`]. Through
/// the [`crate::cycle::Seats`] port, so this module reaches no seat verb.
fn end_at_stop(store: &Store, seat: &str) -> i32 {
    let fleet = crate::cycle::seats();
    crate::cycle::Seats::wait_idle(fleet, seat);
    match crate::ending::end_exit(store, fleet, seat) {
        Some(line) => println!("{line}"),
        None => println!("wsp: {seat} is still in a turn or unreadable — the next tick ends it"),
    }
    0
}

/// The answer to a decision ask a `not held` exit raised (D3): it reopens the
/// row, with the answer as its `sent back:` reason, on a fresh seat — and the
/// time from the ask to the decision is written on the row and as a
/// `decision-waited` event, so a slow decision is seen in the effort and the
/// retro. Nothing for any other question.
pub fn answered(store: &Store, q: &Message, answer: &str) {
    let Some(w) = q.waiting.as_ref().filter(|w| w.pane.is_empty() && !w.task.is_empty()) else { return };
    let Some(t) = store.find_task(&w.task) else { return };
    let ours = t.section("Log").is_some_and(|l| l.lines().any(|x| x.contains("asked (") && x.ends_with(&format!(" · {}", q.id))));
    if t.status() != Status::Blocked || !ours {
        return;
    }
    let secs = (util::epoch_secs() - util::epoch_of(&q.at)).max(0);
    log_on(store, &t.id, &[format!("decided: {} answered {}m after it was asked", q.id, secs / 60)]);
    store.log_event("decision-waited", serde_json::json!({ "id": t.id, "question": q.id, "asked": q.at, "secs": secs }));
    let answer = fold(answer);
    let reason = match answer.is_empty() {
        true => format!("decided ({})", q.id),
        false => answer,
    };
    let _ = crate::cmd_task::reopen(store, &Args::synth("reopen", &[&t.id, &reason], &[]));
}

fn take(store: &Store, args: &Args, t: &Task, c: &Checked, e: &Exit, text: &str, from: &str, me: Option<&str>) -> i32 {
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
        said.push(crate::repair::NOTHING_TO_LAND.into());
        if e.done_when.iter().flatten().any(|d| d.reason.contains('/') || d.reason.contains(".rs")) {
            eprintln!("  {}", Paint::new().yellow(&format!("{id}: the done-when cites a file while nothing is ahead of the trunk — committed?")));
        }
    }
    said.extend(c.full_suite.iter().cloned());
    log_on(store, &id, &std::mem::take(&mut said));
    // `checks:` is what writes `## Checks`.
    if !e.checks.is_empty() {
        let lines: String = e.checks.iter().map(|l| format!("- {} ({}): {}\n", l.label, member_limit(&l.time), l.command)).collect();
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
    if let Some(line) = close_my_leg(store, &id, &fold(text), matches!(outcome, Outcome::Blocked(_)), me) {
        said.push(line);
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
    if let Some(line) = end_my_seat(store, &id, "member", me) {
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

// ---- verifiers, checks and legs (`wsp-614`, row E3) ---------------------------

/// Whether a verdict or a baton is written as an exit: a `summary:` line of
/// its own. Prose without one is read the way its verb always read it.
pub fn looks_like_exit(text: &str) -> bool {
    text.lines().any(|l| section_heading(l) == Some("summary"))
}

/// The refusals as they are printed.
fn printed(rs: &[Refusal]) -> Vec<String> {
    refusal_lines(&Checked { refusals: rs.to_vec(), ..Checked::default() })
}

/// The verdict and what it judged: a verifier's `holds`/`blocks`, a check's
/// `go`/`hold`/`followup`. `structured` is an exit, whose done-when and
/// `checked:` lines are counted; prose has only its `checked:` items.
fn judge_rules(e: &Exit, text: &str, structured: bool) -> Vec<Refusal> {
    use crate::exit::{Cause, Verdict};
    let mut out = Vec::new();
    let Some(verdict) = e.verdict else { return out };
    let end = text.lines().count() + 1;
    let verdict_at = heading_line(text, "verdict").unwrap_or(1);
    let done_lines = item_lines(text, "done-when");
    let checked_lines = item_lines(text, "checked");
    let not_held: Vec<(usize, &crate::exit::Done)> =
        e.done_when.iter().flatten().enumerate().filter(|(_, d)| !d.held).map(|(i, d)| (done_lines.get(i).copied().unwrap_or(0), d)).collect();
    let failed: Vec<(usize, &crate::exit::Line)> = e
        .checks
        .iter()
        .enumerate()
        .filter(|(_, l)| l.ran.as_ref().is_some_and(|r| !r.passed))
        .map(|(i, l)| (checked_lines.get(i).copied().unwrap_or(0), l))
        .collect();
    let refuse = |line: usize, section: &str, said: String, write: &str| Refusal { line, section: section.into(), said, write: write.into() };
    match verdict {
        Verdict::Holds | Verdict::Go => {
            let w = verdict.word();
            let other = if verdict == Verdict::Holds { "blocks" } else { "hold" };
            for (n, d) in &not_held {
                out.push(refuse(
                    *n,
                    "done-when",
                    format!("\"{}\" is not held, and the verdict is `{w}`", util::truncate(&d.item, 60)),
                    &format!("`verdict: {other}` with its `cause:`, or the item as `- held: <item> — <the evidence>`"),
                ));
            }
            for (n, l) in &failed {
                out.push(refuse(
                    *n,
                    "checked",
                    format!("`{}` failed, and the verdict is `{w}`", l.label),
                    &format!("`verdict: {other}` with `cause: check`, or the line as it ran once it passes: `… — pass`"),
                ));
            }
            if let Some((c, _)) = &e.cause {
                out.push(refuse(
                    heading_line(text, "cause").unwrap_or(0),
                    "cause",
                    format!("`cause: {}` on a `{w}`", c.word()),
                    &format!("no `cause:` — a cause is a {other}'s"),
                ));
            }
        }
        Verdict::Blocks | Verdict::Hold => {
            let w = verdict.word();
            match &e.cause {
                None => out.push(refuse(
                    verdict_at,
                    "cause",
                    format!("`{w}` and no `cause:`"),
                    "`cause: work | check | tree | env — <one sentence>` after the checked lines (env: you could not verify, which is no block of the work)",
                )),
                Some((Cause::Env, _)) => {}
                Some(_) if verdict == Verdict::Blocks && structured => {
                    if not_held.is_empty() && failed.is_empty() {
                        out.push(refuse(
                            verdict_at,
                            "verdict",
                            "`blocks`, and no item is not held and no check failed".into(),
                            "the item as `- not held: <item> — <what is wrong, file:line>`, or the check as `… — fail`; or `cause: env` if you could not verify",
                        ));
                    }
                    if e.owed.as_ref().is_none_or(|o| o.is_empty()) {
                        out.push(refuse(
                            heading_line(text, "owed").unwrap_or(end),
                            "owed",
                            "a block owes nothing".into(),
                            "`owed:` and `- <what the member owes>` per line: it is the `sent back:` the member's next seat reads",
                        ));
                    }
                }
                Some(_) => {}
            }
        }
        Verdict::Followup => {}
    }
    out
}

/// What a verifier's exit records: a block's `owed:` is its `sent back:`, and
/// `cause: env` is a pass that could not verify.
fn judged_of(e: &Exit) -> crate::verification::Judged {
    use crate::exit::{Cause, Verdict};
    use crate::verification::State;
    let state = match (e.verdict, &e.cause) {
        (Some(Verdict::Blocks), Some((Cause::Env, _))) => State::CouldNotVerify,
        (Some(Verdict::Blocks), _) => State::Blocks,
        _ => State::Holds,
    };
    let mut text = e.account.trim().to_string();
    let mut tail = Vec::new();
    if !e.checks.is_empty() {
        tail.push("checked:".to_string());
        for l in &e.checks {
            let ran = l.ran.as_ref().map(|r| {
                format!(" — {}{}", if r.passed { "pass" } else { "fail" }, r.note.as_deref().map(|n| format!(" — {n}")).unwrap_or_default())
            });
            tail.push(format!("- {} ({}): {}{}", l.label, l.time, l.command, ran.unwrap_or_default()));
        }
    }
    if let Some((c, why)) = &e.cause {
        tail.push(format!("cause: {} — {why}", c.word()));
    }
    let owed: Vec<String> = e.owed.iter().flatten().cloned().collect();
    if !owed.is_empty() {
        tail.push("owed:".into());
        tail.extend(owed.iter().map(|o| format!("- {o}")));
    }
    if !tail.is_empty() {
        text = format!("{text}\n\n{}", tail.join("\n"));
    }
    let checked = e.done_when.iter().flatten().map(|d| crate::did::Checked { item: fold(&d.item), held: d.held }).collect();
    crate::verification::Judged {
        state,
        text,
        caption: e.summary.clone(),
        checked,
        owed: (state == State::Blocks && !owed.is_empty()).then(|| owed.join("; ")),
        note: match &e.cause {
            Some((Cause::Tree, why)) => Some(tree_note(why)),
            _ => None,
        },
    }
}

/// The line a `cause: tree` block leaves on the member: sent back, and a
/// defect in the gate that should have stopped it at the member's submit.
fn tree_note(why: &str) -> String {
    format!("submit: a `tree` block ({}) — commits missing or the tree dirty, which V1 should have refused at the member's submit: a defect in the gate", fold(why))
}

/// `text` with `verdict: <word>` on its first line, as the verb's flag gives
/// it: added when the text has none, refused when it says another word.
fn with_verdict(word: crate::exit::Verdict, text: &str) -> Result<String, Vec<String>> {
    let said = text.lines().find(|l| section_heading(l) == Some("verdict"));
    match said.map(|l| l.split_once(':').map(|(_, w)| w.trim()).unwrap_or_default()) {
        None => Ok(format!("verdict: {}\n{text}", word.word())),
        Some(w) if w == word.word() => Ok(text.to_string()),
        Some(w) => Err(vec![format!("line 1 (verdict): the file says `{w}` and the verb `{}` — write one verdict, the same in both", word.word())]),
    }
}

/// A verifier's exit, validated: the shape, the verdict against what it
/// judged, the cause and what is owed on a block, the summary and the account.
fn verifier_rules(text: &str) -> Result<Exit, Vec<Refusal>> {
    let e = Exit::parse(Kind::Verifier, text)?;
    let mut c = Checked::default();
    c.refusals.extend(judge_rules(&e, text, true));
    account_rules(text, &e, &mut c);
    c.refusals.sort_by_key(|r| r.line);
    if c.refusals.is_empty() { Ok(e) } else { Err(c.refusals) }
}

/// `wsp verified`'s verdict written as an exit, with the word typed.
pub fn verifier_exit(word: crate::exit::Verdict, text: &str) -> Result<crate::verification::Judged, Vec<String>> {
    let text = with_verdict(word, text)?;
    verifier_rules(&text).map(|e| judged_of(&e)).map_err(|rs| printed(&rs))
}

/// A `cause:` line at the start of a line, taken out of prose.
fn take_cause(text: &str) -> (String, Option<String>) {
    let mut cause = None;
    let kept: Vec<&str> = text
        .lines()
        .filter(|l| match l.strip_prefix("cause:").filter(|_| cause.is_none()) {
            Some(rest) => {
                cause = Some(rest.trim().to_string());
                false
            }
            None => true,
        })
        .collect();
    (kept.join("\n").trim().to_string(), cause)
}

/// A cause in the exit's grammar, `work | check | tree | env — <one sentence>`.
fn cause_of(said: &str) -> Result<(crate::exit::Cause, String), String> {
    let probe = format!("verdict: blocks\nsummary: s\n\na\n\ncause: {said}\n");
    match Exit::parse(Kind::Verifier, &probe) {
        Ok(e) => e.cause.ok_or_else(|| "no cause".to_string()),
        Err(rs) => Err(rs.into_iter().map(|r| format!("{} — write {}", r.said, r.write)).collect::<Vec<_>>().join("; ")),
    }
}

/// `wsp verified`'s verdict written as prose: its `checked:` items taken out,
/// and its cause from `--cause` or a `cause:` line. A block needs the cause;
/// a hold may not leave an item not held.
pub fn verifier_prose(word: crate::exit::Verdict, text: &str, flag: Option<&str>) -> Result<crate::verification::Judged, Vec<String>> {
    use crate::exit::{Cause, Done, Verdict};
    use crate::verification::State;
    let (prose, line) = take_cause(text);
    let (prose, checked) = crate::did::take_checked(&prose).map_err(|why| vec![format!("wsp: {why}")])?;
    let cause = match flag.map(str::to_string).or(line) {
        Some(said) => Some(cause_of(&said).map_err(|why| vec![format!("the verdict's cause (cause): {why}")])?),
        None => None,
    };
    let e = Exit {
        kind: Kind::Verifier,
        verdict: Some(word),
        summary: String::new(),
        account: prose.clone(),
        done_when: Some(checked.iter().map(|c| Done { held: c.held, item: c.item.clone(), reason: String::new() }).collect()),
        checks: Vec::new(),
        cause: cause.clone(),
        asks: Vec::new(),
        owed: None,
    };
    let rs: Vec<Refusal> = judge_rules(&e, "", false)
        .into_iter()
        .map(|mut r| {
            r.line = 0;
            if r.section == "cause" && e.cause.is_none() {
                r.write = "a line `cause: work | check | tree | env — <one sentence>` in the verdict, or `--cause \"work — …\"` (env: you could not verify, which is no block of the work)".into();
            }
            r
        })
        .collect();
    if !rs.is_empty() {
        return Err(printed(&rs));
    }
    let state = match (word, &cause) {
        (Verdict::Blocks, Some((Cause::Env, _))) => State::CouldNotVerify,
        (Verdict::Blocks, _) => State::Blocks,
        _ => State::Holds,
    };
    let text = match &cause {
        Some((c, why)) => format!("{prose}\n\ncause: {} — {why}", c.word()),
        None => prose,
    };
    Ok(crate::verification::Judged {
        state,
        caption: String::new(),
        text,
        checked,
        owed: None,
        note: match &cause {
            Some((Cause::Tree, why)) => Some(tree_note(why)),
            _ => None,
        },
    })
}

/// `wsp submit <member>` from a verifier's seat: the exit at `$WSP_EXIT` (or
/// the verifier's verdict path, written in this pass), validated whole and
/// recorded as `wsp verified` records a verdict.
fn submit_verifier(store: &Store, args: &Args, t: &Task, me: Option<String>) -> i32 {
    let since = crate::verification::passes(t)
        .into_iter()
        .filter(|p| p.state == crate::verification::State::Running)
        .rev()
        .find(|p| me.is_none() || p.pane == me)
        .map(|p| util::epoch_of(&p.at));
    let kept = closing::verdict_path(store, &t.id, closing::Kind::Verifier);
    let (text, from) = match read_exit_at(args, &t.id, kept, since) {
        Ok(v) => v,
        Err(code) => return code,
    };
    let e = match verifier_rules(&text) {
        Ok(e) => e,
        Err(rs) => {
            for l in printed(&rs) {
                eprintln!("{l}");
            }
            eprintln!("wsp: {} not submitted — {} refusal(s) in {from}; nothing recorded. Fix the file and `wsp submit {}` again", t.id, rs.len(), t.id);
            return 1;
        }
    };
    let judged = judged_of(&e);
    if args.has("dry-run") {
        let what = match judged.state {
            crate::verification::State::Holds => format!("  {} holds: the pass recorded, the row landed", t.id),
            crate::verification::State::Blocks => format!("  {} blocks: sent back to doing, owed: {}", t.id, judged.owed.clone().unwrap_or_default()),
            _ => format!("  {}: could not verify — no block; the row stays at review and the run starts a new verifier", t.id),
        };
        println!("{what}");
        println!("wsp: {} would be submitted — dry run, nothing moved", t.id);
        return 0;
    }
    let seat = closing(store, &t.id, "verifier", me.as_deref());
    let code = crate::verification::verdict_core(store, t.clone(), judged, me.clone());
    if code != 0 {
        refused(store, seat.as_deref());
    } else if let Some(line) = end_my_seat(store, &t.id, "verifier", seat.as_deref()) {
        log_on(store, &t.id, &[line]);
    }
    code
}

/// The list and barrier a check's `<row>` names: the list's id, or the
/// barrier's subject (`<list>-check-<n>`).
fn check_list(store: &Store, needle: &str) -> Option<crate::model::Worklist> {
    store.worklist(needle).or_else(|| crate::barrier::parse_subject(needle).and_then(|(l, _)| store.worklist(l)))
}

/// A check's exit, validated: the shape, the verdict against what it checked,
/// a hold's cause, the summary and the account.
fn check_rules_of(text: &str) -> Result<Exit, Vec<Refusal>> {
    let e = Exit::parse(Kind::Check, text)?;
    let mut c = Checked::default();
    c.refusals.extend(judge_rules(&e, text, true));
    account_rules(text, &e, &mut c);
    c.refusals.sort_by_key(|r| r.line);
    if c.refusals.is_empty() { Ok(e) } else { Err(c.refusals) }
}

/// The text a barrier's `go`/`hold` records from a check's exit: its account,
/// with what it checked and its cause after.
fn check_body(e: &Exit) -> String {
    let mut j = judged_of(e);
    for d in e.done_when.iter().flatten() {
        j.text.push_str(&format!("\n- {}: {} — {}", if d.held { "held" } else { "not held" }, d.item, d.reason));
    }
    j.text
}

/// `wsp submit <list>` from a barrier check's seat: `go` and `hold` are the
/// verbs, run with the exit's account; `followup` stays a flag to the person,
/// because the rows it names are not ones wsp can guess.
fn submit_check(store: &Store, args: &Args, needle: &str) -> i32 {
    use crate::exit::Verdict;
    let Some(w) = check_list(store, needle) else {
        eprintln!("wsp: no list `{needle}` — a check submits for its list (or `<list>-check-<n>`)");
        return 1;
    };
    let pos = crate::worklist::position(store, &w, crate::worklist::Reading::Landed);
    let at = pos.at.unwrap_or(0);
    let open = w.groups().into_iter().nth(at.saturating_sub(1)).and_then(|g| crate::barrier::running(&g).map(|c| util::epoch_of(&c.at)));
    let key = crate::barrier::subject(&w.id, at);
    let kept = closing::verdict_path(store, &key, closing::Kind::Check);
    let (text, from) = match read_exit_at(args, &key, kept.clone(), open) {
        Ok(v) => v,
        Err(code) => return code,
    };
    let e = match check_rules_of(&text) {
        Ok(e) => e,
        Err(rs) => {
            for l in printed(&rs) {
                eprintln!("{l}");
            }
            eprintln!("wsp: {} barrier {at} not submitted — {} refusal(s) in {from}; nothing moved. Fix the file and `wsp submit {}` again", w.id, rs.len(), w.id);
            return 1;
        }
    };
    let verdict = e.verdict.unwrap_or(Verdict::Followup);
    if args.has("dry-run") {
        let what = match verdict {
            Verdict::Go => "passed: `wsp worklist go`",
            Verdict::Hold => "held: `wsp worklist hold`",
            _ => "a follow-up, flagged to the person",
        };
        println!("  {} barrier {at} {what}, the exit's account as the verdict", w.id);
        println!("wsp: {} would be submitted — dry run, nothing moved", w.id);
        return 0;
    }
    let body = check_body(&e);
    if verdict == Verdict::Followup {
        use crate::message::{Kind as MKind, Party};
        let said = format!("{} barrier {at}: the check asks for a follow-up — {}\n\n{body}\n\nThe rows a follow-up names are not ones wsp guesses: `wsp worklist followup {}` with them, or `go`/`hold`.", w.id, e.summary, w.id);
        let m = Message::new(Party::wsp(), MKind::Note, &said).about(About::Scope(w.id.clone())).for_the_person();
        if let Err(why) = message::raise(store, &m) {
            eprintln!("wsp: {}: could not flag the follow-up: {why}", w.id);
            return 1;
        }
        crate::cycle::tell(store, &w, &format!("barrier {at}'s check asks for a follow-up: {} — the person is flagged", e.summary));
        println!("✓ {} barrier {at} submitted — a follow-up, flagged to the person; the barrier stays open", w.id);
        return 0;
    }
    let file = kept.with_extension("body");
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(why) = std::fs::write(&file, &body) {
        eprintln!("wsp: cannot write {}: {why}", util::contract(&file));
        return 1;
    }
    let path = file.display().to_string();
    let verb = verdict.word();
    let a = Args::synth("worklist", &[verb, w.id.as_str()], &[("from", &path), ("caption", &e.summary)]);
    // Recorded first: `go` arranges the ending of every check standing on the
    // barrier, and this one is its exit's to end.
    let seat = closing(store, &key, "check", None);
    let code = match verdict {
        Verdict::Go => crate::cmd_worklist::go(store, &a),
        _ => crate::cmd_worklist::hold(store, &a),
    };
    if code != 0 {
        refused(store, seat.as_deref());
    } else if let Some(line) = end_my_seat(store, &key, "check", seat.as_deref()) {
        crate::store::log_line(store, &format!("{} barrier {at}: {line}", w.id));
    }
    code
}

/// `worklist go`/`hold` from a check's seat: its words run the check's
/// validator. An exit is read with the verb as its verdict; prose holds only
/// with a `cause:` line. `Err` is the exit code, the refusals printed.
pub fn check_seat_words(verb: &str, said: &str) -> Result<(), i32> {
    let kind = std::env::var(crate::throttle::SEAT_KIND).ok();
    check_words_as(kind.as_deref(), verb, said)
}

/// [`check_seat_words`] with the seat's kind given.
pub fn check_words_as(kind: Option<&str>, verb: &str, said: &str) -> Result<(), i32> {
    use crate::exit::Verdict;
    if kind.map(str::trim) != Some("check") {
        return Ok(());
    }
    let word = match verb {
        "go" => Verdict::Go,
        "hold" => Verdict::Hold,
        _ => return Ok(()),
    };
    let lines = if looks_like_exit(said) {
        let text = match with_verdict(word, said) {
            Ok(t) => t,
            Err(lines) => return refuse_words(verb, &lines),
        };
        match check_rules_of(&text) {
            Ok(_) => return Ok(()),
            Err(rs) => printed(&rs),
        }
    } else {
        let (_, line) = take_cause(said);
        match (word, line) {
            (Verdict::Hold, None) => vec![
                "the hold (cause): a check's hold names its cause — write a line `cause: work | check | tree | env — <one sentence>` in the reason".to_string(),
            ],
            (Verdict::Hold, Some(c)) => match cause_of(&c) {
                Ok(_) => return Ok(()),
                Err(why) => vec![format!("the hold's cause (cause): {why}")],
            },
            _ => return Ok(()),
        }
    };
    refuse_words(verb, &lines)
}

fn refuse_words(verb: &str, lines: &[String]) -> Result<(), i32> {
    for l in lines {
        eprintln!("{l}");
    }
    eprintln!("wsp: `{verb}` refused — {} refusal(s); nothing moved. Fix the verdict and give it again", lines.len());
    Err(2)
}

/// A leg's exit, validated: the shape (its done-when is the leg order's, so
/// optional), the checks' scope, the summary and the account, and `owed:`.
fn leg_rules(store: &Store, t: &Task, text: &str) -> Result<Exit, Vec<Refusal>> {
    let e = Exit::parse(Kind::Leg, text)?;
    let mut c = Checked::default();
    check_rules(store, t, text, &e, &mut c);
    account_rules(text, &e, &mut c);
    if e.owed.is_none() {
        c.refusals.push(Refusal {
            line: text.lines().count() + 1,
            section: "owed".into(),
            said: "the exit has no `owed:`".into(),
            write: "`owed: nothing`, or `owed:` and `- <what the next leg is left>` per line".into(),
        });
    }
    c.refusals.sort_by_key(|r| r.line);
    if c.refusals.is_empty() { Ok(e) } else { Err(c.refusals) }
}

/// `wsp leg done`'s baton, when it is written as an exit: validated as a leg's,
/// and its summary the caption when none was typed. `Ok(None)` for a baton in
/// prose, which is taken as it always was.
pub fn leg_baton(store: &Store, t: &Task, baton: &str) -> Result<Option<String>, String> {
    if !looks_like_exit(baton) {
        return Ok(None);
    }
    leg_rules(store, t, baton).map(|e| Some(e.summary)).map_err(|rs| {
        let lines = printed(&rs);
        format!("the baton is an exit and {} line(s) of it are refused:\n{}", lines.len(), lines.join("\n"))
    })
}

/// The running leg of `t` whose seat is `me`, if `me` is one.
fn leg_of(t: &Task, me: Option<&str>) -> Option<String> {
    crate::legs::actor_of(&crate::legs::legs(t), me)
}

/// The last leg's end, when its exit has moved the row: in review, the leg is
/// done ([`crate::leg_seat::on_review`], with the seat that submitted rather
/// than the pane `review` reads for itself, which may name no leg); blocked,
/// the leg goes back to pending, so the run seats it again once the row is
/// answered rather than reading it as running in a seat that is closing.
/// Nothing, for a seat that is no leg's. The line for the row's log.
fn close_my_leg(store: &Store, id: &str, account: &str, blocked: bool, me: Option<&str>) -> Option<String> {
    let t = store.find_task(id)?;
    let name = leg_of(&t, me)?;
    if blocked {
        crate::leg_seat::reset(store, id, &name)?;
        return Some(format!("submit: leg {name} is pending again — its exit blocked the row, and the run seats it once the row is answered"));
    }
    match crate::leg_seat::on_review(store, id, account, me) {
        Ok(()) => {
            store.git_commit(&format!("wsp: {id} leg {name} done by its submit"));
            None
        }
        Err(why) => Some(format!("submit: leg {name} could not be closed: {}", fold(&why))),
    }
}

/// `wsp submit <row>` from a leg's seat: `wsp leg done`, with the exit file as
/// the baton. `None` when this seat's leg is the row's last, which submits as
/// a member.
fn submit_leg(store: &Store, args: &Args, t: &Task, me: Option<String>) -> Option<i32> {
    let legs = crate::legs::legs(t);
    let Some(name) = crate::legs::actor_of(&legs, me.as_deref()) else {
        eprintln!("wsp: {} is not the seat of a running leg of {} — only a leg's own seat submits it", me.as_deref().unwrap_or("this process"), t.id);
        return Some(1);
    };
    if crate::legs::lasts(&legs).contains(&name) {
        return None;
    }
    let started = legs.iter().find(|l| l.name == name).and_then(|l| l.started.as_deref()).map(util::epoch_of);
    let kept = closing::baton_path(store, &t.id, &name);
    let (text, from) = match read_exit_at(args, &t.id, kept.clone(), started) {
        Ok(v) => v,
        Err(code) => return Some(code),
    };
    let e = match leg_rules(store, t, &text) {
        Ok(e) => e,
        Err(rs) => {
            for l in printed(&rs) {
                eprintln!("{l}");
            }
            eprintln!("wsp: {} leg {name} not submitted — {} refusal(s) in {from}; the leg is still open. Fix the file and `wsp submit {}` again", t.id, rs.len(), t.id);
            return Some(1);
        }
    };
    if args.has("dry-run") {
        println!("  {} leg {name} done, the exit as its baton · {}", t.id, e.summary);
        println!("wsp: {} would be submitted — dry run, nothing moved", t.id);
        return Some(0);
    }
    // The file is the baton: written where the leg's baton is kept, and read from there.
    if let Some(dir) = kept.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(why) = std::fs::write(&kept, &text) {
        eprintln!("wsp: cannot write the baton {}: {why}", util::contract(&kept));
        return Some(1);
    }
    let path = kept.display().to_string();
    let a = Args::synth("leg", &["done", t.id.as_str()], &[("from", &path), ("caption", &e.summary)]);
    Some(crate::leg_seat::cmd_done(store, &a, me.as_deref()))
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
        let args = Args::parse(std::iter::once("submit").chain(rest).map(String::from).collect());
        submit_as(store, &args, Some("member"))
    }

    fn log(store: &Store) -> String {
        store.task("wsp-101").unwrap().section("Log").unwrap_or_default()
    }

    /// Give wsp-101 the legs `build` (done) and `finish`, running in `seat`.
    fn legged(store: &Store, seat: &str) {
        use crate::legs::{Leg, State};
        let mut t = store.task("wsp-101").unwrap();
        let mut finish = Leg::pending("finish", "submit the row");
        finish.state = State::Running;
        finish.pane = Some(seat.into());
        finish.started = Some("2026-11-17T06:00:00Z".into());
        let build = Leg { state: State::Done, caption: Some("built".into()), ..Leg::pending("build", "build it") };
        crate::legs::write(&mut t, &[build, finish]);
        store.save_task(&t).unwrap();
    }

    fn submit_in(env: &crate::util::Isolated, store: &Store, text: &str, kind: &str, seat: &str) -> i32 {
        let file = env.path("exit.md");
        std::fs::write(&file, text).unwrap();
        let path = file.display().to_string();
        let args = Args::parse(["submit", "wsp-101", "--from", path.as_str()].iter().map(|s| s.to_string()).collect());
        submit_from(store, &args, Some(kind), Some(seat.into()))
    }

    /// `wsp-011`, the wsp-001 shape: the last leg's seat submits with its kind
    /// read as `member` (and with no pane `review` can find for itself). The
    /// row goes to review and the leg reads done, not `running` with no seat.
    #[test]
    fn a_last_legs_submit_taken_for_a_members_closes_its_leg() {
        let (env, store, _tree) = member("submit-leg-last", ARTEFACTS);
        legged(&store, "cpd-121");
        for kind in ["member", "leg"] {
            assert_eq!(submit_in(&env, &store, &exit_with("- unit (1m): true\n", "- note: nothing\n"), kind, "cpd-121"), 0, "{}", log(&store));
            let t = store.task("wsp-101").unwrap();
            assert_eq!(t.status(), Status::Review);
            let finish = crate::legs::legs(&t).into_iter().find(|l| l.name == "finish").unwrap();
            assert_eq!(finish.state, crate::legs::State::Done, "kind {kind}: {}", t.section("Legs").unwrap_or_default());
            assert!(finish.closed.is_some());
            // Again, from the start, with the seat's kind said right.
            let mut t = store.task("wsp-101").unwrap();
            t.status_raw = "doing".into();
            store.save_task(&t).unwrap();
            crate::ending::forget_exit(&store, "cpd-121");
            legged(&store, "cpd-121");
        }
    }

    /// The last leg's exit that blocks the row puts the leg back to pending, so
    /// the run seats it again once the row is answered.
    #[test]
    fn a_last_legs_blocking_submit_leaves_its_leg_pending() {
        let (env, store, _tree) = member("submit-leg-block", ARTEFACTS);
        legged(&store, "cpd-122");
        let blocking = exit_with("- unit (1m): true\n", "- person: decide between the two layers?\n")
            .replace("- held: the work — work.txt", "- not held: the work — it waits on which layer owns it");
        assert_eq!(submit_in(&env, &store, &blocking, "member", "cpd-122"), 0, "{}", log(&store));
        let t = store.task("wsp-101").unwrap();
        assert_eq!(t.status(), Status::Blocked);
        let finish = crate::legs::legs(&t).into_iter().find(|l| l.name == "finish").unwrap();
        assert_eq!((finish.state, finish.pane), (crate::legs::State::Pending, None));
        assert!(log(&store).contains("leg finish is pending again"), "{}", log(&store));
    }

    #[test]
    fn a_last_legs_submit_while_a_sibling_runs_is_refused_before_anything_moves() {
        let (env, store, _tree) = member("submit-leg-sibling", ARTEFACTS);
        legged(&store, "cpd-123");
        let mut t = store.task("wsp-101").unwrap();
        let mut ls = crate::legs::legs(&t);
        ls[0].state = crate::legs::State::Running;
        ls[0].pane = Some("cpd-124".into());
        ls[1].after = Some(vec![]);
        crate::legs::write(&mut t, &ls);
        store.save_task(&t).unwrap();
        assert_eq!(submit_in(&env, &store, &exit_with("- unit (1m): true\n", "- note: nothing\n"), "member", "cpd-123"), 1);
        assert_eq!(store.task("wsp-101").unwrap().status(), Status::Doing, "nothing moved");
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

    /// `wsp-540`: a member's measured `(1s)` is not the limit a barrier then
    /// cuts its check off at; a longer time is written as given.
    #[test]
    fn a_members_measured_second_is_not_written_as_its_checks_limit() {
        let (env, store, _tree) = member("submit-floor", "");
        let e = exit_with("- layering (1s): true\n- long (5m): true\n", "- note: fine\n");
        assert_eq!(run(&env, &store, &e, &[]), 0);
        let checks = store.task("wsp-101").unwrap().section("Checks").unwrap_or_default();
        assert!(checks.contains("- layering (60s): true") && checks.contains("- long (5m): true"), "{checks}");
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

    /// `wsp-412`: seat A's claim expires and is released; seat B takes the row.
    /// A's submit is refused in the words the packet gives and the row is as it
    /// was; B's is accepted. A seat that never held it is not told it expired.
    #[test]
    fn a_seat_whose_claim_expired_cannot_submit_over_the_next_holder() {
        std::env::set_var("WSP_BACKEND_SOCKET_PATH", "/nonexistent/wsp-submit-tests.sock");
        let (env, store, _tree) = member("submit-expired", ARTEFACTS);
        let hold = |seat: &str| {
            store.set_binding(seat, serde_json::json!({ "task_id": "wsp-101", "pane_id": seat, "workspace_id": seat }));
            store.set_claim("wsp-101", serde_json::json!({ "workspace_id": seat, "cwd": "/tmp" }));
        };
        hold("seat-a");
        let why = "claim expired: held by seat-a on this machine, last renewed 2026-11-10T10:00:00Z";
        crate::hub_lease::tests::lose(&store, "wsp-101", why).unwrap();
        assert_eq!(store.task("wsp-101").unwrap().status_raw, "todo");
        hold("seat-b");
        let mut t = store.task("wsp-101").unwrap();
        t.status_raw = "doing".into();
        store.save_task(&t).unwrap();

        let text = exit_with("- unit (2m): true\n", "- note: none\n");
        let before = store.task("wsp-101").unwrap().render();
        assert_eq!(submit_in(&env, &store, &text, "member", "seat-a"), 1);
        let after = store.task("wsp-101").unwrap().render();
        assert_eq!(before, after, "the row is unchanged");
        let said = expired_claim_refusal(&store, &store.task("wsp-101").unwrap(), Some("seat-a")).unwrap();
        assert!(
            said.starts_with("wsp: your claim on wsp-101 expired at ")
                && said.ends_with(" and the row went back to todo; tell your governor (wsp tell governor:run)"),
            "{said}"
        );
        assert!(expired_claim_refusal(&store, &store.task("wsp-101").unwrap(), Some("seat-c")).is_none());
        assert_eq!(store.task("wsp-101").unwrap().status_raw, "doing");

        assert_eq!(submit_in(&env, &store, &text, "member", "seat-b"), 0);
        assert_eq!(store.task("wsp-101").unwrap().status_raw, "review");
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

    /// D3 (`wsp-615`): the answer to a `not held` exit's decision ask reopens
    /// the row with the answer as its `sent back:` reason, and the wait from the
    /// ask to the decision is written on it. An answer to any other question
    /// moves nothing.
    #[test]
    fn the_answer_to_a_not_held_exits_ask_reopens_the_row_with_it_as_the_reason() {
        let (env, store, _tree) = member("submit-answered", ARTEFACTS);
        let text = "summary: half done\n\nThe API half is in; the UI half needs a call.\n\ndone-when:\n- not held: the work — the UI half\n\nasks:\n- person: Either keep the old panel or drop it?\n\nowed:\n- the UI half\n";
        assert_eq!(run(&env, &store, text, &[]), 0);
        let q = crate::message::raised_to_the_person(&store).into_iter().next().expect("the decision ask");
        let mut other = q.clone();
        other.id = "q-not-this-one".into();
        answered(&store, &other, "drop it");
        assert_eq!(store.task("wsp-101").unwrap().status_raw, "blocked", "not the row's ask, so nothing moves");
        answered(&store, &q, "keep the old panel");
        let t = store.task("wsp-101").unwrap();
        assert_ne!(t.status_raw, "blocked", "the answer reopens the row");
        let log = t.section("Log").unwrap();
        assert!(log.contains(&format!("decided: {} answered 0m after it was asked", q.id)), "the wait is on the row: {log}");
        assert!(log.contains("sent back: keep the old panel"), "the answer is its reason: {log}");
        answered(&store, &q, "keep the old panel");
        assert_eq!(store.task("wsp-101").unwrap().section("Log").unwrap().matches("decided:").count(), 1, "once");
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
    fn a_word_that_is_no_seat_kind_is_refused() {
        let (_env, store, _tree) = member("submit-kind", ARTEFACTS);
        let args = Args::synth("submit", &["wsp-101"], &[]);
        assert_eq!(submit_as(&store, &args, Some("janitor")), 2);
        assert_eq!(store.task("wsp-101").unwrap().status_raw, "doing");
    }

    #[test]
    fn a_bare_review_from_a_run_seat_warns_with_the_validators_refusals_and_outside_a_run_says_nothing() {
        let (_env, store, tree) = member("submit-warn", ARTEFACTS);
        std::fs::write(tree.join("stray.txt"), "x\n").unwrap();
        let args = Args::synth("review", &["wsp-101"], &[]);
        assert!(crate::worklist::verified_by_a_run(&store, "wsp-101"));
        let warned = crate::cmd_task::exit_warnings(&store, &args, "did it\nand left nothing");
        assert!(!warned.is_empty(), "a run seat's dirty tree is warned of");
        // Outside a run: unchanged, whatever the tree holds.
        let mut w = store.worklists().into_iter().next().unwrap();
        w.set_status(WorklistStatus::Done);
        store.save_worklist(&w).unwrap();
        assert!(!crate::worklist::verified_by_a_run(&store, "wsp-101"));
        assert!(crate::cmd_task::exit_warnings(&store, &args, "did it").is_empty());
    }

    /// `wsp-137`, wsp-039's shape: one account, with its `done-when:` after a
    /// caption (or after a `summary:` line, as `$WSP_EXIT` is written), read by
    /// review's check and by `wsp submit --dry-run` — and the two agree. The
    /// account comes in on a file, as `wsp review <id> --from FILE` reads it,
    /// so a fold that loses its headings is caught here; a truly missing
    /// `done-when:` still warns.
    #[test]
    fn reviews_check_and_submits_dry_run_read_one_account_alike() {
        let (env, store, _tree) = member("submit-review-agree", ARTEFACTS);
        let mut t = store.task("wsp-101").unwrap();
        t.body = "## Overview\nDo the work.\n\nDone when:\n- the work is in work.txt\n".into();
        store.save_task(&t).unwrap();
        let review_of = |account: &str| {
            let file = env.path("account.md");
            std::fs::write(&file, account).unwrap();
            let path = file.display().to_string();
            let args = Args::synth("review", &["wsp-101", "--from", &path], &[]);
            let (folded, kept, _) = crate::cmd_task::review_payload(&args, "usage").unwrap();
            assert!(!folded.contains('\n'), "the log still gets one line");
            crate::cmd_task::exit_warnings(&store, &args, &kept)
        };
        let submit_of = |exit: &str| dry_run(&store, closing::Kind::Member, "wsp-101", exit).err().unwrap_or_default();
        let body = "It wrote work.txt and left nothing.\n\ndone-when:\n- held: the work is in work.txt — work.txt:1\n\nchecks:\n- unit (120s): true\n\nasks:\n- note: nothing to decide\n";
        // The exit as written to $WSP_EXIT: the same file to both.
        let exit = format!("summary: the work is in\n\n{body}\nowed: nothing\n");
        assert_eq!(submit_of(&exit), Vec::<String>::new(), "submit takes it");
        assert_eq!(review_of(&exit), Vec::<String>::new(), "and review warns of nothing");
        // A caption first, the way a bare review is written.
        let captioned = format!("wsp-101: the work is in\n\n{body}");
        assert_eq!(review_of(&captioned), Vec::<String>::new(), "no false `no done-when:`");
        assert_eq!(review_of(&captioned), submit_of(&exit_of_review(&captioned)));
        // No done-when at all: both say so.
        let bare = "wsp-101: the work is in\n\nIt wrote work.txt.\n";
        let warned = review_of(bare);
        assert!(warned.iter().any(|l| l.contains("no `done-when:`")), "{warned:#?}");
        assert_eq!(warned, submit_of(&exit_of_review(bare)));
    }
}
